import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const MAX_SURFACE_BYTES = 256 * 1024 * 1024;
const MAX_RESPONSE_LINE_BYTES = 16 * 1024;
export const FAST_CPU_CANARY_COHORT = "m4-r0-basic-page-local-v1";

export class PdfRsBridgeError extends Error {
  constructor(code) {
    super(code);
    this.name = "PdfRsBridgeError";
    this.code = code;
  }
}

export class PdfRsBridge {
  #child;
  #buffer = Buffer.alloc(0);
  #pending = new Map();
  #nextRequest = 1;
  #surface = undefined;
  #closed = false;

  constructor(options = {}) {
    const moduleDirectory = dirname(fileURLToPath(import.meta.url));
    const program = options.program
      ?? process.env.PDF_RS_ELECTRON_BRIDGE
      ?? resolve(moduleDirectory, "../../../target/debug/pdf-rs-electron-bridge");
    const rendererCohort = options.rendererCohort
      ?? process.env.PDF_RS_ELECTRON_RENDERER_COHORT;
    if (
      rendererCohort !== undefined
      && rendererCohort !== FAST_CPU_CANARY_COHORT
    ) {
      throw new PdfRsBridgeError("invalid-renderer-cohort");
    }
    const environment = { ...process.env };
    if (rendererCohort === FAST_CPU_CANARY_COHORT) {
      environment.PDF_RS_FAST_CPU_CANARY_V1 = rendererCohort;
    } else {
      delete environment.PDF_RS_FAST_CPU_CANARY_V1;
    }
    this.#child = spawn(program, ["--stdio"], {
      env: environment,
      stdio: ["pipe", "pipe", "inherit"],
      windowsHide: true,
    });
    this.#child.stdout.on("data", (chunk) => this.#accept(chunk));
    this.#child.on("error", () => this.#failAll("bridge-launch"));
    this.#child.on("close", () => this.#failAll("bridge-closed"));
  }

  get processId() {
    return this.#child.pid;
  }

  async open(path) {
    if (typeof path !== "string" || path.length === 0) {
      throw new PdfRsBridgeError("invalid-path");
    }
    const encoded = Buffer.from(path, "utf8").toString("hex");
    return this.#request(`OPEN {id} ${encoded}`, "OPENED");
  }

  async render(documentId, page, width, options = {}) {
    const generation = options.generation;
    const versioned = generation !== undefined;
    return this.#request(
      versioned
        ? `RENDER_V2 {id} ${integer(documentId)} ${positiveInteger(generation)} ${integer(page)} ${integer(width)}`
        : `RENDER {id} ${integer(documentId)} ${integer(page)} ${integer(width)}`,
      versioned ? "SURFACE_V2" : "SURFACE",
      options.signal,
    );
  }

  async close(documentId) {
    return this.#request(`CLOSE {id} ${integer(documentId)}`, "CLOSED");
  }

  async shutdown() {
    if (this.#closed) {
      return;
    }
    try {
      await this.#request("SHUTDOWN {id}", "BYE");
    } finally {
      this.#closed = true;
      this.#child.stdin.end();
    }
  }

  terminate() {
    this.#closed = true;
    if (this.#child.exitCode === null && !this.#child.killed) {
      this.#child.kill();
    }
  }

  #request(template, expected, signal) {
    if (this.#closed) {
      return Promise.reject(new PdfRsBridgeError("bridge-closed"));
    }
    if (signal?.aborted) {
      return Promise.reject(new PdfRsBridgeError("cancelled"));
    }
    const id = this.#nextRequest;
    this.#nextRequest += 1;
    if (!Number.isSafeInteger(id)) {
      return Promise.reject(new PdfRsBridgeError("request-exhausted"));
    }
    return new Promise((resolveRequest, rejectRequest) => {
      const abort = signal
        ? () => {
            if (!this.#pending.has(id)) {
              return;
            }
            void this.#request(`CANCEL {id} ${id}`, "CANCELLED").catch(
              () => undefined,
            );
          }
        : undefined;
      this.#pending.set(id, {
        expected,
        resolve: resolveRequest,
        reject: rejectRequest,
        signal,
        abort,
      });
      signal?.addEventListener("abort", abort, { once: true });
      const command = `${template.replace("{id}", String(id))}\n`;
      this.#child.stdin.write(command, "ascii", (error) => {
        if (!error) {
          return;
        }
        this.#reject(id, new PdfRsBridgeError("bridge-write"));
      });
    });
  }

  #accept(chunk) {
    if (this.#closed) {
      return;
    }
    try {
      while (chunk.length > 0) {
        if (this.#surface) {
          const remaining = this.#surface.length - this.#surface.received;
          if (remaining > 0) {
            const accepted = Math.min(remaining, chunk.length);
            chunk.copy(
              this.#surface.pixels,
              this.#surface.received,
              0,
              accepted,
            );
            this.#surface.received += accepted;
            chunk = chunk.subarray(accepted);
            if (this.#surface.received < this.#surface.length) {
              return;
            }
          }
          if (chunk.length === 0) {
            return;
          }
          if (chunk[0] !== 0x0a) {
            throw new PdfRsBridgeError("invalid-surface-frame");
          }
          const surface = this.#surface;
          this.#surface = undefined;
          chunk = chunk.subarray(1);
          this.#resolve(surface.request, surface.responseType, {
            documentId: surface.documentId,
            generation: surface.generation,
            page: surface.page,
            renderer: surface.renderer,
            width: surface.width,
            height: surface.height,
            stride: surface.stride,
            pixels: new Uint8ClampedArray(
              surface.pixels.buffer,
              surface.pixels.byteOffset,
              surface.pixels.byteLength,
            ),
          });
          continue;
        }
        const newline = chunk.indexOf(0x0a);
        if (newline === -1) {
          this.#appendResponseLine(chunk);
          return;
        }
        let line = chunk.subarray(0, newline);
        chunk = chunk.subarray(newline + 1);
        if (this.#buffer.length > 0) {
          if (this.#buffer.length + line.length > MAX_RESPONSE_LINE_BYTES) {
            throw new PdfRsBridgeError("invalid-response");
          }
          line = Buffer.concat([this.#buffer, line]);
          this.#buffer = Buffer.alloc(0);
        }
        const text = line.toString("ascii");
        this.#acceptLine(text);
      }
    } catch (error) {
      this.#failAll(error instanceof PdfRsBridgeError ? error.code : "invalid-frame");
      this.terminate();
    }
  }

  #appendResponseLine(chunk) {
    if (this.#buffer.length + chunk.length > MAX_RESPONSE_LINE_BYTES) {
      throw new PdfRsBridgeError("invalid-response");
    }
    this.#buffer = this.#buffer.length === 0
      ? chunk
      : Buffer.concat([this.#buffer, chunk]);
  }

  #acceptLine(line) {
    const fields = line.split(" ");
    const type = fields[0];
    const request = parsedInteger(fields[1]);
    if (!request) {
      throw new PdfRsBridgeError("invalid-response");
    }
    if (type === "ERROR" && fields.length === 3) {
      this.#reject(request, new PdfRsBridgeError(fields[2]));
      return;
    }
    if (type === "OPENED" && fields.length === 4) {
      this.#resolve(request, type, {
        documentId: parsedInteger(fields[2]),
        pageCount: parsedInteger(fields[3]),
      });
      return;
    }
    if (type === "CLOSED" && fields.length === 3) {
      this.#resolve(request, type, {
        documentId: parsedInteger(fields[2]),
      });
      return;
    }
    if (type === "CANCELLED" && fields.length === 3) {
      const target = parsedInteger(fields[2]);
      if (!target) {
        throw new PdfRsBridgeError("invalid-response");
      }
      this.#resolve(request, type, { target });
      return;
    }
    if (type === "BYE" && fields.length === 2) {
      this.#resolve(request, type, undefined);
      return;
    }
    if (
      (type === "SURFACE" && fields.length === 9)
      || (type === "SURFACE_V2" && fields.length === 10)
    ) {
      const versioned = type === "SURFACE_V2";
      const offset = versioned ? 1 : 0;
      const length = parsedInteger(fields[8 + offset]);
      if (!length || length > MAX_SURFACE_BYTES) {
        throw new PdfRsBridgeError("invalid-surface-length");
      }
      const renderer = fields[4 + offset];
      if (renderer !== "reference-cpu-v1" && renderer !== "fast-cpu-v1") {
        throw new PdfRsBridgeError("invalid-renderer");
      }
      this.#surface = {
        request,
        responseType: type,
        documentId: parsedInteger(fields[2]),
        generation: versioned ? parsedInteger(fields[3]) : undefined,
        page: parsedInteger(fields[3 + offset], true),
        renderer,
        width: parsedInteger(fields[5 + offset]),
        height: parsedInteger(fields[6 + offset]),
        stride: parsedInteger(fields[7 + offset]),
        length,
        received: 0,
        pixels: Buffer.allocUnsafe(length),
      };
      if (
        !this.#surface.documentId
        || (versioned && !this.#surface.generation)
        || this.#surface.page === undefined
        || !this.#surface.width
        || !this.#surface.height
        || !this.#surface.stride
        || this.#surface.stride * this.#surface.height !== length
      ) {
        throw new PdfRsBridgeError("invalid-surface-metadata");
      }
      return;
    }
    throw new PdfRsBridgeError("invalid-response");
  }

  #resolve(request, type, value) {
    const pending = this.#pending.get(request);
    if (!pending || pending.expected !== type) {
      throw new PdfRsBridgeError("unexpected-response");
    }
    this.#pending.delete(request);
    pending.signal?.removeEventListener("abort", pending.abort);
    pending.resolve(value);
  }

  #reject(request, error) {
    const pending = this.#pending.get(request);
    if (!pending) {
      return;
    }
    this.#pending.delete(request);
    pending.signal?.removeEventListener("abort", pending.abort);
    pending.reject(error);
  }

  #failAll(code) {
    if (this.#closed && this.#pending.size === 0) {
      return;
    }
    this.#closed = true;
    this.#buffer = Buffer.alloc(0);
    this.#surface = undefined;
    for (const pending of this.#pending.values()) {
      pending.signal?.removeEventListener("abort", pending.abort);
      pending.reject(new PdfRsBridgeError(code));
    }
    this.#pending.clear();
  }
}

function integer(value) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new PdfRsBridgeError("invalid-input");
  }
  return String(value);
}

function positiveInteger(value) {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new PdfRsBridgeError("invalid-input");
  }
  return String(value);
}

function parsedInteger(value, allowZero = false) {
  if (!/^(0|[1-9][0-9]*)$/.test(value ?? "")) {
    return undefined;
  }
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || (allowZero ? parsed < 0 : parsed <= 0)) {
    return undefined;
  }
  return parsed;
}
