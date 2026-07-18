import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const MAX_SURFACE_BYTES = 256 * 1024 * 1024;

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
    this.#child = spawn(program, ["--stdio"], {
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

  async render(documentId, page, width) {
    return this.#request(
      `RENDER {id} ${integer(documentId)} ${integer(page)} ${integer(width)}`,
      "SURFACE",
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

  #request(template, expected) {
    if (this.#closed) {
      return Promise.reject(new PdfRsBridgeError("bridge-closed"));
    }
    const id = this.#nextRequest;
    this.#nextRequest += 1;
    if (!Number.isSafeInteger(id)) {
      return Promise.reject(new PdfRsBridgeError("request-exhausted"));
    }
    return new Promise((resolveRequest, rejectRequest) => {
      this.#pending.set(id, {
        expected,
        resolve: resolveRequest,
        reject: rejectRequest,
      });
      const command = `${template.replace("{id}", String(id))}\n`;
      this.#child.stdin.write(command, "ascii", (error) => {
        if (!error) {
          return;
        }
        const pending = this.#pending.get(id);
        this.#pending.delete(id);
        pending?.reject(new PdfRsBridgeError("bridge-write"));
      });
    });
  }

  #accept(chunk) {
    if (this.#closed) {
      return;
    }
    this.#buffer = Buffer.concat([this.#buffer, chunk]);
    try {
      while (this.#buffer.length > 0) {
        if (this.#surface) {
          if (this.#buffer.length < this.#surface.length + 1) {
            return;
          }
          const pixels = this.#buffer.subarray(0, this.#surface.length);
          if (this.#buffer[this.#surface.length] !== 0x0a) {
            throw new PdfRsBridgeError("invalid-surface-frame");
          }
          this.#buffer = this.#buffer.subarray(this.#surface.length + 1);
          const surface = this.#surface;
          this.#surface = undefined;
          this.#resolve(surface.request, "SURFACE", {
            documentId: surface.documentId,
            page: surface.page,
            renderer: surface.renderer,
            width: surface.width,
            height: surface.height,
            stride: surface.stride,
            pixels: Uint8Array.from(pixels),
          });
          continue;
        }
        const newline = this.#buffer.indexOf(0x0a);
        if (newline === -1) {
          return;
        }
        const line = this.#buffer.subarray(0, newline).toString("ascii");
        this.#buffer = this.#buffer.subarray(newline + 1);
        this.#acceptLine(line);
      }
    } catch (error) {
      this.#failAll(error instanceof PdfRsBridgeError ? error.code : "invalid-frame");
      this.terminate();
    }
  }

  #acceptLine(line) {
    const fields = line.split(" ");
    const type = fields[0];
    const request = parsedInteger(fields[1]);
    if (!request) {
      throw new PdfRsBridgeError("invalid-response");
    }
    if (type === "ERROR" && fields.length === 3) {
      const pending = this.#pending.get(request);
      this.#pending.delete(request);
      pending?.reject(new PdfRsBridgeError(fields[2]));
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
    if (type === "BYE" && fields.length === 2) {
      this.#resolve(request, type, undefined);
      return;
    }
    if (type === "SURFACE" && fields.length === 9) {
      const length = parsedInteger(fields[8]);
      if (!length || length > MAX_SURFACE_BYTES) {
        throw new PdfRsBridgeError("invalid-surface-length");
      }
      const renderer = fields[4];
      if (renderer !== "reference-cpu-v1" && renderer !== "fast-cpu-v1") {
        throw new PdfRsBridgeError("invalid-renderer");
      }
      this.#surface = {
        request,
        documentId: parsedInteger(fields[2]),
        page: parsedInteger(fields[3], true),
        renderer,
        width: parsedInteger(fields[5]),
        height: parsedInteger(fields[6]),
        stride: parsedInteger(fields[7]),
        length,
      };
      if (
        !this.#surface.documentId
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
    pending.resolve(value);
  }

  #failAll(code) {
    if (this.#closed && this.#pending.size === 0) {
      return;
    }
    this.#closed = true;
    for (const pending of this.#pending.values()) {
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
