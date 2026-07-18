import type {
  CompatibleHandshake,
  EnvelopeHeader,
  PendingCommandEnvelope,
  PendingEventEnvelope,
} from "../generated/engine-protocol.js";
import {
  beginValidateCommandEnvelope,
  beginValidateEventEnvelope,
  decodeCommandPayload,
  decodeEventPayload,
  descriptorById,
  EnvelopeSequenceTracker,
  isCompatibleHandshake,
  validateProvideDataTransferLengths,
} from "../generated/engine-protocol.js";
import {
  NATIVE_WORKER_ABI_HASH_WORDS,
  NATIVE_WORKER_ABI_VERSION,
  NATIVE_WORKER_FUNCTION_SIGNATURES,
} from "./native-worker-abi.generated.js";

const SHA256_HEX_LENGTH = 64;
const ENVELOPE_HEADER_BYTES = 20;
const WASM_PAGE_BYTES = 65_536;
const MAX_WASM_PAGES = 1_024;
const MAX_NATIVE_WORKER_ARTIFACT_BYTES = 64 * 1_024 * 1_024;
const MAX_U32 = 0xffff_ffff;

const EXPECTED_NATIVE_WORKER_IMPORTS = Object.freeze(
  [] as readonly WebAssembly.ModuleImportDescriptor[],
);

const EXPECTED_NATIVE_WORKER_EXPORTS = Object.freeze([
  Object.freeze({ name: "memory", kind: "memory" }),
  Object.freeze({
    name: "pdf_rs_worker_prepare_input",
    kind: "function",
  }),
  Object.freeze({
    name: "pdf_rs_worker_prepare_transfer",
    kind: "function",
  }),
  Object.freeze({
    name: "pdf_rs_worker_dispatch",
    kind: "function",
  }),
  Object.freeze({
    name: "pdf_rs_worker_poll",
    kind: "function",
  }),
  Object.freeze({
    name: "pdf_rs_worker_output_pointer",
    kind: "function",
  }),
  Object.freeze({
    name: "pdf_rs_worker_output_length",
    kind: "function",
  }),
  Object.freeze({
    name: "pdf_rs_worker_transfer_count",
    kind: "function",
  }),
  Object.freeze({
    name: "pdf_rs_worker_transfer_pointer",
    kind: "function",
  }),
  Object.freeze({
    name: "pdf_rs_worker_transfer_length",
    kind: "function",
  }),
  Object.freeze({
    name: "pdf_rs_worker_memory_epoch",
    kind: "function",
  }),
  Object.freeze({
    name: "pdf_rs_worker_shutdown",
    kind: "function",
  }),
  Object.freeze({
    name: "pdf_rs_worker_abi_version",
    kind: "function",
  }),
  ...NATIVE_WORKER_ABI_HASH_WORDS.map((_, index) => Object.freeze({
    name: `pdf_rs_worker_abi_hash_${index}`,
    kind: "function" as const,
  })),
  Object.freeze({ name: "main", kind: "function" }),
  Object.freeze({ name: "__data_end", kind: "global" }),
  Object.freeze({ name: "__heap_base", kind: "global" }),
] as const) satisfies readonly WebAssembly.ModuleExportDescriptor[];

export type BrowserNativeWorkerErrorCode =
  | "InvalidConfiguration"
  | "NegotiationRequired"
  | "NegotiationMismatch"
  | "InvalidLifecycle"
  | "ArtifactFetchFailure"
  | "ArtifactLengthMismatch"
  | "ArtifactDigestFailure"
  | "ArtifactHashMismatch"
  | "InvalidWasmModule"
  | "InvalidWasmImports"
  | "InvalidWasmExports"
  | "InvalidWasmMemory"
  | "InstantiationFailure"
  | "InvalidMessage"
  | "MessageLimit"
  | "TransferLimit"
  | "EngineRejected"
  | "EngineTrap";

export class BrowserNativeWorkerError extends Error {
  readonly code: BrowserNativeWorkerErrorCode;

  constructor(code: BrowserNativeWorkerErrorCode) {
    super(code);
    this.name = "BrowserNativeWorkerError";
    this.code = code;
  }
}

export interface BrowserNativeWorkerArtifact {
  readonly url: string | URL;
  readonly byteLength: number;
  readonly sha256: string;
  readonly minimumMemoryPages: number;
  readonly maximumMemoryPages: number;
}

export interface BrowserNativeWorkerLoaderRuntime {
  readonly fetch: (
    input: string | URL,
    signal: AbortSignal,
  ) => Promise<Pick<Response, "ok" | "headers" | "body">>;
  readonly digestSha256: (
    bytes: Uint8Array,
    signal: AbortSignal,
  ) => Promise<ArrayBuffer>;
  readonly compile: (
    bytes: Uint8Array,
    signal: AbortSignal,
  ) => Promise<WebAssembly.Module>;
  readonly instantiate: (
    module: WebAssembly.Module,
    imports: WebAssembly.Imports,
    signal: AbortSignal,
  ) => Promise<WebAssembly.Instance>;
}

export interface BrowserNativeWorkerDispatch {
  readonly frame: Uint8Array<ArrayBuffer>;
  readonly transfers: readonly ArrayBuffer[];
}

interface NativeWorkerExports {
  readonly memory: WebAssembly.Memory;
  readonly prepareInput: (length: number) => number;
  readonly prepareTransfer: (
    index: number,
    length: number,
  ) => number;
  readonly dispatch: (
    length: number,
    transferCount: number,
  ) => number;
  readonly poll: () => number;
  readonly outputPointer: () => number;
  readonly outputLength: () => number;
  readonly transferCount: () => number;
  readonly transferPointer: (index: number) => number;
  readonly transferLength: (index: number) => number;
  readonly memoryEpoch: () => number;
  readonly shutdown: () => number;
  readonly abiVersion: () => number;
  readonly abiHashWords: readonly (() => number)[];
}

export interface BrowserNativeWorkerMemoryLimits {
  readonly minimum: number;
  readonly maximum: number;
  readonly shared: boolean;
}

const copiedBytes = (
  bytes: Uint8Array,
): Uint8Array<ArrayBuffer> => {
  const copy = new Uint8Array(bytes.byteLength);
  copy.set(bytes);
  return copy;
};

const defaultRuntime = (): BrowserNativeWorkerLoaderRuntime => ({
  fetch: async (
    input: string | URL,
    signal: AbortSignal,
  ): Promise<Pick<Response, "ok" | "headers" | "body">> =>
    fetch(input, { signal }),
  digestSha256: async (
    bytes: Uint8Array,
    _signal: AbortSignal,
  ): Promise<ArrayBuffer> =>
    crypto.subtle.digest("SHA-256", copiedBytes(bytes)),
  compile: async (
    bytes: Uint8Array,
    _signal: AbortSignal,
  ): Promise<WebAssembly.Module> =>
    WebAssembly.compile(copiedBytes(bytes)),
  instantiate: async (
    module: WebAssembly.Module,
    imports: WebAssembly.Imports,
    _signal: AbortSignal,
  ): Promise<WebAssembly.Instance> =>
    WebAssembly.instantiate(module, imports),
});

const isSafePositive = (value: unknown, maximum: number): value is number =>
  typeof value === "number"
  && Number.isSafeInteger(value)
  && value > 0
  && value <= maximum;

const isU32 = (value: unknown): value is number =>
  typeof value === "number"
  && Number.isSafeInteger(value)
  && value >= 0
  && value <= MAX_U32;

const isFixedArrayBuffer = (
  value: unknown,
): value is ArrayBuffer => {
  try {
    return value instanceof ArrayBuffer
      && Object.getPrototypeOf(value) === ArrayBuffer.prototype
      && (
        Object.getOwnPropertyDescriptor(
          ArrayBuffer.prototype,
          "resizable",
        ) === undefined
        || value.resizable === false
      );
  } catch {
    return false;
  }
};

const exactDescriptors = <T extends {
  readonly name: string;
  readonly kind: string;
}>(
  actual: readonly T[],
  expected: readonly T[],
  includeModule: boolean,
): boolean => {
  if (actual.length !== expected.length) {
    return false;
  }
  const keys = (descriptors: readonly T[]): string[] =>
    descriptors.map((descriptor) => {
      const module = includeModule && "module" in descriptor
        ? String(descriptor.module)
        : "";
      return `${module}\u0000${descriptor.name}\u0000${descriptor.kind}`;
    }).sort();
  const actualKeys = keys(actual);
  const expectedKeys = keys(expected);
  return actualKeys.every(
    (key, index) => key === expectedKeys[index],
  );
};

const validateArtifact = (
  input: BrowserNativeWorkerArtifact,
): BrowserNativeWorkerArtifact => {
  try {
    const url = input.url;
    const byteLength = input.byteLength;
    const sha256 = input.sha256;
    const minimumMemoryPages = input.minimumMemoryPages;
    const maximumMemoryPages = input.maximumMemoryPages;
    if (
      (typeof url !== "string" && !(url instanceof URL))
      || !isSafePositive(
        byteLength,
        MAX_NATIVE_WORKER_ARTIFACT_BYTES,
      )
      || typeof sha256 !== "string"
      || !/^[0-9a-f]{64}$/u.test(sha256)
      || !isSafePositive(minimumMemoryPages, MAX_WASM_PAGES)
      || maximumMemoryPages !== MAX_WASM_PAGES
      || minimumMemoryPages > maximumMemoryPages
    ) {
      throw new BrowserNativeWorkerError("InvalidConfiguration");
    }
    return Object.freeze({
      url,
      byteLength,
      sha256,
      minimumMemoryPages,
      maximumMemoryPages,
    });
  } catch (error: unknown) {
    if (error instanceof BrowserNativeWorkerError) {
      throw error;
    }
    throw new BrowserNativeWorkerError("InvalidConfiguration");
  }
};

const readU32Leb = (
  bytes: Uint8Array,
  cursor: { value: number },
  end: number,
): number => {
  let result = 0;
  for (let shift = 0; shift < 35; shift += 7) {
    if (cursor.value >= end) {
      throw new BrowserNativeWorkerError("InvalidWasmModule");
    }
    const byte = bytes[cursor.value]!;
    cursor.value += 1;
    if (shift === 28 && (byte & 0xf0) !== 0) {
      throw new BrowserNativeWorkerError("InvalidWasmModule");
    }
    result |= (byte & 0x7f) << shift;
    if ((byte & 0x80) === 0) {
      return result >>> 0;
    }
  }
  throw new BrowserNativeWorkerError("InvalidWasmModule");
};

export const inspectNativeWorkerMemory = (
  bytes: Uint8Array,
): BrowserNativeWorkerMemoryLimits => {
  if (
    bytes.byteLength < 8
    || bytes[0] !== 0x00
    || bytes[1] !== 0x61
    || bytes[2] !== 0x73
    || bytes[3] !== 0x6d
    || bytes[4] !== 0x01
    || bytes[5] !== 0x00
    || bytes[6] !== 0x00
    || bytes[7] !== 0x00
  ) {
    throw new BrowserNativeWorkerError("InvalidWasmModule");
  }
  const cursor = { value: 8 };
  let memory: BrowserNativeWorkerMemoryLimits | undefined;
  while (cursor.value < bytes.byteLength) {
    const sectionId = bytes[cursor.value]!;
    cursor.value += 1;
    const sectionLength = readU32Leb(bytes, cursor, bytes.byteLength);
    const sectionEnd = cursor.value + sectionLength;
    if (
      !Number.isSafeInteger(sectionEnd)
      || sectionEnd > bytes.byteLength
    ) {
      throw new BrowserNativeWorkerError("InvalidWasmModule");
    }
    if (sectionId === 5) {
      if (memory !== undefined) {
        throw new BrowserNativeWorkerError("InvalidWasmMemory");
      }
      const count = readU32Leb(bytes, cursor, sectionEnd);
      if (count !== 1) {
        throw new BrowserNativeWorkerError("InvalidWasmMemory");
      }
      const flags = readU32Leb(bytes, cursor, sectionEnd);
      if ((flags & ~0x03) !== 0 || (flags & 0x01) === 0) {
        throw new BrowserNativeWorkerError("InvalidWasmMemory");
      }
      const minimum = readU32Leb(bytes, cursor, sectionEnd);
      const maximum = readU32Leb(bytes, cursor, sectionEnd);
      if (
        cursor.value !== sectionEnd
        || minimum === 0
        || maximum === 0
        || minimum > maximum
        || maximum > MAX_WASM_PAGES
      ) {
        throw new BrowserNativeWorkerError("InvalidWasmMemory");
      }
      memory = Object.freeze({
        minimum,
        maximum,
        shared: (flags & 0x02) !== 0,
      });
    }
    cursor.value = sectionEnd;
  }
  if (memory === undefined || memory.shared) {
    throw new BrowserNativeWorkerError("InvalidWasmMemory");
  }
  return memory;
};

const readWasmName = (
  bytes: Uint8Array,
  cursor: { value: number },
  end: number,
): string => {
  const length = readU32Leb(bytes, cursor, end);
  const nameEnd = cursor.value + length;
  if (
    length > 256
    || !Number.isSafeInteger(nameEnd)
    || nameEnd > end
  ) {
    throw new BrowserNativeWorkerError("InvalidWasmModule");
  }
  try {
    const name = new TextDecoder("utf-8", { fatal: true }).decode(
      bytes.subarray(cursor.value, nameEnd),
    );
    cursor.value = nameEnd;
    return name;
  } catch {
    throw new BrowserNativeWorkerError("InvalidWasmModule");
  }
};

const wasmValueType = (value: number): string => {
  switch (value) {
    case 0x7f:
      return "i32";
    case 0x7e:
      return "i64";
    case 0x7d:
      return "f32";
    case 0x7c:
      return "f64";
    case 0x7b:
      return "v128";
    case 0x70:
      return "funcref";
    case 0x6f:
      return "externref";
    default:
      throw new BrowserNativeWorkerError("InvalidWasmModule");
  }
};

export const inspectNativeWorkerAbi = (
  bytes: Uint8Array,
): Readonly<Record<string, string>> => {
  if (
    bytes.byteLength < 8
    || bytes[0] !== 0x00
    || bytes[1] !== 0x61
    || bytes[2] !== 0x73
    || bytes[3] !== 0x6d
  ) {
    throw new BrowserNativeWorkerError("InvalidWasmModule");
  }
  const cursor = { value: 8 };
  const signatures: string[] = [];
  const functionTypes: number[] = [];
  const functionExports = new Map<string, number>();
  let importCount = 0;
  while (cursor.value < bytes.byteLength) {
    const sectionId = bytes[cursor.value]!;
    cursor.value += 1;
    const sectionLength = readU32Leb(
      bytes,
      cursor,
      bytes.byteLength,
    );
    const sectionEnd = cursor.value + sectionLength;
    if (
      !Number.isSafeInteger(sectionEnd)
      || sectionEnd > bytes.byteLength
    ) {
      throw new BrowserNativeWorkerError("InvalidWasmModule");
    }
    if (sectionId === 1) {
      const count = readU32Leb(bytes, cursor, sectionEnd);
      if (count > 65_536) {
        throw new BrowserNativeWorkerError("InvalidWasmModule");
      }
      for (let index = 0; index < count; index += 1) {
        if (bytes[cursor.value] !== 0x60) {
          throw new BrowserNativeWorkerError(
            "InvalidWasmModule",
          );
        }
        cursor.value += 1;
        const parameterCount = readU32Leb(
          bytes,
          cursor,
          sectionEnd,
        );
        if (parameterCount > 64) {
          throw new BrowserNativeWorkerError(
            "InvalidWasmModule",
          );
        }
        const parameters: string[] = [];
        for (
          let parameter = 0;
          parameter < parameterCount;
          parameter += 1
        ) {
          if (cursor.value >= sectionEnd) {
            throw new BrowserNativeWorkerError(
              "InvalidWasmModule",
            );
          }
          parameters.push(
            wasmValueType(bytes[cursor.value]!),
          );
          cursor.value += 1;
        }
        const resultCount = readU32Leb(
          bytes,
          cursor,
          sectionEnd,
        );
        if (resultCount > 16) {
          throw new BrowserNativeWorkerError(
            "InvalidWasmModule",
          );
        }
        const results: string[] = [];
        for (
          let result = 0;
          result < resultCount;
          result += 1
        ) {
          if (cursor.value >= sectionEnd) {
            throw new BrowserNativeWorkerError(
              "InvalidWasmModule",
            );
          }
          results.push(wasmValueType(bytes[cursor.value]!));
          cursor.value += 1;
        }
        signatures.push(
          `(${parameters.join(",")})->${
            results.length === 0
              ? "void"
              : results.join(",")
          }`,
        );
      }
      if (cursor.value !== sectionEnd) {
        throw new BrowserNativeWorkerError(
          "InvalidWasmModule",
        );
      }
    } else if (sectionId === 2) {
      importCount = readU32Leb(bytes, cursor, sectionEnd);
      if (importCount !== 0 || cursor.value !== sectionEnd) {
        throw new BrowserNativeWorkerError(
          "InvalidWasmImports",
        );
      }
    } else if (sectionId === 3) {
      const count = readU32Leb(bytes, cursor, sectionEnd);
      if (count > 1_000_000) {
        throw new BrowserNativeWorkerError(
          "InvalidWasmModule",
        );
      }
      for (let index = 0; index < count; index += 1) {
        functionTypes.push(
          readU32Leb(bytes, cursor, sectionEnd),
        );
      }
      if (cursor.value !== sectionEnd) {
        throw new BrowserNativeWorkerError(
          "InvalidWasmModule",
        );
      }
    } else if (sectionId === 7) {
      const count = readU32Leb(bytes, cursor, sectionEnd);
      if (count > 256) {
        throw new BrowserNativeWorkerError(
          "InvalidWasmExports",
        );
      }
      for (let index = 0; index < count; index += 1) {
        const name = readWasmName(bytes, cursor, sectionEnd);
        if (cursor.value >= sectionEnd) {
          throw new BrowserNativeWorkerError(
            "InvalidWasmModule",
          );
        }
        const kind = bytes[cursor.value]!;
        cursor.value += 1;
        const itemIndex = readU32Leb(
          bytes,
          cursor,
          sectionEnd,
        );
        if (kind === 0) {
          if (functionExports.has(name)) {
            throw new BrowserNativeWorkerError(
              "InvalidWasmExports",
            );
          }
          functionExports.set(name, itemIndex);
        }
      }
      if (cursor.value !== sectionEnd) {
        throw new BrowserNativeWorkerError(
          "InvalidWasmModule",
        );
      }
    }
    cursor.value = sectionEnd;
  }
  if (importCount !== 0) {
    throw new BrowserNativeWorkerError("InvalidWasmImports");
  }
  const actual: Record<string, string> = {};
  for (
    const [name, expected]
    of Object.entries(
      NATIVE_WORKER_FUNCTION_SIGNATURES,
    )
  ) {
    const functionIndex = functionExports.get(name);
    const typeIndex = functionIndex === undefined
      ? undefined
      : functionTypes[functionIndex];
    const signature = typeIndex === undefined
      ? undefined
      : signatures[typeIndex];
    if (signature !== expected) {
      throw new BrowserNativeWorkerError("InvalidWasmExports");
    }
    actual[name] = signature;
  }
  return Object.freeze(actual);
};

const bytesToHex = (bytes: Uint8Array): string => {
  let result = "";
  for (const byte of bytes) {
    result += byte.toString(16).padStart(2, "0");
  }
  return result;
};

const checkedRange = (
  pointer: number,
  length: number,
  byteLength: number,
): void => {
  if (
    !isU32(pointer)
    || !isU32(length)
    || pointer > byteLength - length
  ) {
    throw new BrowserNativeWorkerError("InvalidWasmMemory");
  }
};

const u32Export = (
  exports: WebAssembly.Exports,
  name: string,
): ((...values: number[]) => number) => {
  const value = exports[name];
  if (typeof value !== "function") {
    throw new BrowserNativeWorkerError("InvalidWasmExports");
  }
  return (...values: number[]): number => {
    if (!values.every(isU32)) {
      throw new BrowserNativeWorkerError("EngineRejected");
    }
    const result: unknown = value(...values);
    if (
      typeof result !== "number"
      || !Number.isInteger(result)
      || result < -0x8000_0000
      || result > 0x7fff_ffff
    ) {
      throw new BrowserNativeWorkerError("EngineRejected");
    }
    return result >>> 0;
  };
};

const nativeExports = (
  instance: WebAssembly.Instance,
): NativeWorkerExports => {
  const memory = instance.exports.memory;
  if (!(memory instanceof WebAssembly.Memory)) {
    throw new BrowserNativeWorkerError("InvalidWasmExports");
  }
  const abiHashWords = NATIVE_WORKER_ABI_HASH_WORDS.map(
    (_, index) => u32Export(
      instance.exports,
      `pdf_rs_worker_abi_hash_${index}`,
    ),
  );
  const exports = {
    memory,
    prepareInput: u32Export(
      instance.exports,
      "pdf_rs_worker_prepare_input",
    ),
    prepareTransfer: u32Export(
      instance.exports,
      "pdf_rs_worker_prepare_transfer",
    ) as (index: number, length: number) => number,
    dispatch: u32Export(
      instance.exports,
      "pdf_rs_worker_dispatch",
    ) as (length: number, transferCount: number) => number,
    poll: u32Export(instance.exports, "pdf_rs_worker_poll"),
    outputPointer: u32Export(
      instance.exports,
      "pdf_rs_worker_output_pointer",
    ),
    outputLength: u32Export(
      instance.exports,
      "pdf_rs_worker_output_length",
    ),
    transferCount: u32Export(
      instance.exports,
      "pdf_rs_worker_transfer_count",
    ),
    transferPointer: u32Export(
      instance.exports,
      "pdf_rs_worker_transfer_pointer",
    ),
    transferLength: u32Export(
      instance.exports,
      "pdf_rs_worker_transfer_length",
    ),
    memoryEpoch: u32Export(
      instance.exports,
      "pdf_rs_worker_memory_epoch",
    ),
    shutdown: u32Export(
      instance.exports,
      "pdf_rs_worker_shutdown",
    ),
    abiVersion: u32Export(
      instance.exports,
      "pdf_rs_worker_abi_version",
    ),
    abiHashWords: Object.freeze(abiHashWords),
  };
  if (
    exports.abiVersion() !== NATIVE_WORKER_ABI_VERSION
    || exports.abiHashWords.some(
      (word, index) =>
        word() !== NATIVE_WORKER_ABI_HASH_WORDS[index],
    )
  ) {
    throw new BrowserNativeWorkerError("InvalidWasmExports");
  }
  return exports;
};

const parseFrame = (
  frame: Uint8Array,
): Readonly<{
  header: EnvelopeHeader;
  payload: Uint8Array;
}> => {
  if (
    !(frame instanceof Uint8Array)
    || Object.getPrototypeOf(frame) !== Uint8Array.prototype
    || !isFixedArrayBuffer(frame.buffer)
    || (
      typeof SharedArrayBuffer !== "undefined"
      && frame.buffer instanceof SharedArrayBuffer
    )
    || frame.byteLength < ENVELOPE_HEADER_BYTES
  ) {
    throw new BrowserNativeWorkerError("InvalidMessage");
  }
  try {
    const view = new DataView(
      frame.buffer,
      frame.byteOffset,
      frame.byteLength,
    );
    const payloadLength = view.getUint32(8, true);
    if (payloadLength !== frame.byteLength - ENVELOPE_HEADER_BYTES) {
      throw new BrowserNativeWorkerError("InvalidMessage");
    }
    return Object.freeze({
      header: Object.freeze({
        major: view.getUint16(0, true),
        minor: view.getUint16(2, true),
        message_type: view.getUint16(4, true),
        flags: view.getUint16(6, true),
        payload_len: payloadLength,
        sequence: view.getBigUint64(12, true),
      }),
      payload: frame.subarray(ENVELOPE_HEADER_BYTES),
    });
  } catch (error: unknown) {
    if (error instanceof BrowserNativeWorkerError) {
      throw error;
    }
    throw new BrowserNativeWorkerError("InvalidMessage");
  }
};

const validateInputFrame = (
  frame: Uint8Array,
  transfers: readonly ArrayBuffer[],
  connection: CompatibleHandshake,
  sequence: EnvelopeSequenceTracker,
): PendingCommandEnvelope => {
  const parsed = parseFrame(frame);
  const decoded = decodeCommandPayload(parsed.header, parsed.payload);
  if (!decoded.ok) {
    throw new BrowserNativeWorkerError("InvalidMessage");
  }
  const pending = beginValidateCommandEnvelope(
    decoded.value,
    transfers.length,
    parsed.payload.byteLength,
    connection,
    sequence,
  );
  if (pending === undefined) {
    throw new BrowserNativeWorkerError("InvalidMessage");
  }
  if (
    decoded.value.command.type === "ProvideData"
    && !validateProvideDataTransferLengths(
      decoded.value.command.payload,
      transfers.map((transfer) => BigInt(transfer.byteLength)),
    )
  ) {
    throw new BrowserNativeWorkerError("InvalidMessage");
  }
  return pending;
};

const validateOutputFrame = (
  frame: Uint8Array<ArrayBuffer>,
  transfers: readonly ArrayBuffer[],
  connection: CompatibleHandshake,
  sequence: EnvelopeSequenceTracker,
): PendingEventEnvelope => {
  const parsed = parseFrame(frame);
  const decoded = decodeEventPayload(parsed.header, parsed.payload);
  if (!decoded.ok) {
    throw new BrowserNativeWorkerError("InvalidMessage");
  }
  const pending = beginValidateEventEnvelope(
    decoded.value,
    transfers.length,
    parsed.payload.byteLength,
    connection,
    sequence,
  );
  if (pending === undefined) {
    throw new BrowserNativeWorkerError("InvalidMessage");
  }
  if (decoded.value.event.type === "SurfaceReady") {
    const transport = decoded.value.event.payload.transport;
    if (
      transport.kind !== "BrowserArrayBuffer"
      || transfers[transport.slot]?.byteLength
        !== Number(transport.buffer_length)
    ) {
      throw new BrowserNativeWorkerError("InvalidMessage");
    }
  } else if (transfers.length !== 0) {
    throw new BrowserNativeWorkerError("InvalidMessage");
  }
  return pending;
};

const bestEffortShutdown = (instance: WebAssembly.Instance): void => {
  try {
    const shutdown = instance.exports.pdf_rs_worker_shutdown;
    if (typeof shutdown === "function") {
      void shutdown();
    }
  } catch {
    // The instance is already rejected and must not escape the loader.
  }
};

const lifecycleAbort = (): BrowserNativeWorkerError =>
  new BrowserNativeWorkerError("InvalidLifecycle");

const awaitAbortable = async <T>(
  operation: Promise<T>,
  signal: AbortSignal,
  disposeLateValue?: (value: T) => void,
): Promise<T> => {
  if (signal.aborted) {
    void operation.then(disposeLateValue, () => undefined);
    throw lifecycleAbort();
  }
  return new Promise<T>((resolve, reject) => {
    let settled = false;
    const abort = (): void => {
      if (settled) {
        return;
      }
      settled = true;
      reject(lifecycleAbort());
    };
    signal.addEventListener("abort", abort, { once: true });
    void operation.then(
      (value) => {
        if (settled) {
          disposeLateValue?.(value);
          return;
        }
        settled = true;
        signal.removeEventListener("abort", abort);
        resolve(value);
      },
      (error: unknown) => {
        if (settled) {
          return;
        }
        settled = true;
        signal.removeEventListener("abort", abort);
        reject(error);
      },
    );
  });
};

const readBoundedArtifact = async (
  response: Pick<Response, "headers" | "body">,
  expectedLength: number,
  signal: AbortSignal,
): Promise<ArrayBuffer> => {
  let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
  const cancelReader = (): void => {
    void reader?.cancel().catch(() => undefined);
  };
  try {
    if (signal.aborted) {
      throw lifecycleAbort();
    }
    const contentLength = response.headers.get("content-length");
    if (
      contentLength === null
      || !/^(0|[1-9][0-9]*)$/u.test(contentLength)
      || Number(contentLength) !== expectedLength
    ) {
      throw new BrowserNativeWorkerError(
        "ArtifactLengthMismatch",
      );
    }
    if (response.body === null || response.body.locked) {
      throw new BrowserNativeWorkerError(
        "ArtifactFetchFailure",
      );
    }
    reader = response.body.getReader();
    signal.addEventListener("abort", cancelReader, { once: true });
    const target = new Uint8Array(expectedLength);
    let offset = 0;
    let chunks = 0;
    while (true) {
      const result = await reader.read();
      if (signal.aborted) {
        throw lifecycleAbort();
      }
      if (result.done) {
        if (offset !== expectedLength) {
          throw new BrowserNativeWorkerError(
            "ArtifactLengthMismatch",
          );
        }
        return target.buffer;
      }
      const chunk = result.value;
      chunks += 1;
      if (
        !(chunk instanceof Uint8Array)
        || Object.getPrototypeOf(chunk) !== Uint8Array.prototype
        || !isFixedArrayBuffer(chunk.buffer)
        || chunk.byteLength === 0
        || chunks > 1_048_576
        || chunk.byteLength > expectedLength - offset
      ) {
        throw new BrowserNativeWorkerError(
          chunk instanceof Uint8Array
            && chunk.byteLength > expectedLength - offset
            ? "ArtifactLengthMismatch"
            : "ArtifactFetchFailure",
        );
      }
      target.set(chunk, offset);
      offset += chunk.byteLength;
    }
  } catch (error: unknown) {
    if (reader !== undefined) {
      try {
        await reader.cancel();
      } catch {
        // The stable primary fetch/length failure remains visible.
      }
    }
    if (signal.aborted) {
      throw lifecycleAbort();
    }
    if (error instanceof BrowserNativeWorkerError) {
      throw error;
    }
    throw new BrowserNativeWorkerError("ArtifactFetchFailure");
  } finally {
    signal.removeEventListener("abort", cancelReader);
    try {
      reader?.releaseLock();
    } catch {
      // The stream is no longer used after this bounded read.
    }
  }
};

export class BrowserNativeWorkerInstance {
  readonly #connection: CompatibleHandshake;
  readonly #exports: NativeWorkerExports;
  readonly #minimumMemoryBytes: number;
  readonly #maximumMemoryBytes: number;
  readonly #inputSequence = new EnvelopeSequenceTracker();
  readonly #outputSequence = new EnvelopeSequenceTracker();
  #memoryBuffer: ArrayBuffer;
  #memoryByteLength: number;
  #memoryEpoch: number;
  #closed = false;

  constructor(
    connection: CompatibleHandshake,
    instance: WebAssembly.Instance,
    minimumMemoryPages: number,
    maximumMemoryPages: number,
  ) {
    if (!isCompatibleHandshake(connection)) {
      throw new BrowserNativeWorkerError("NegotiationRequired");
    }
    if (
      !isSafePositive(minimumMemoryPages, MAX_WASM_PAGES)
      || maximumMemoryPages !== MAX_WASM_PAGES
      || minimumMemoryPages > maximumMemoryPages
    ) {
      throw new BrowserNativeWorkerError("InvalidConfiguration");
    }
    this.#connection = connection;
    this.#minimumMemoryBytes = minimumMemoryPages * WASM_PAGE_BYTES;
    this.#maximumMemoryBytes = maximumMemoryPages * WASM_PAGE_BYTES;
    this.#exports = nativeExports(instance);
    const buffer = this.#exports.memory.buffer;
    const epoch = this.#exports.memoryEpoch();
    if (
      !isFixedArrayBuffer(buffer)
      || epoch === 0
      || buffer.byteLength < this.#minimumMemoryBytes
      || buffer.byteLength > this.#maximumMemoryBytes
      || buffer.byteLength % WASM_PAGE_BYTES !== 0
    ) {
      throw new BrowserNativeWorkerError("InvalidWasmMemory");
    }
    this.#memoryBuffer = buffer;
    this.#memoryByteLength = buffer.byteLength;
    this.#memoryEpoch = epoch;
  }

  get closed(): boolean {
    return this.#closed;
  }

  dispatch(
    frame: Uint8Array,
    transfers: readonly ArrayBuffer[] = Object.freeze([]),
  ): BrowserNativeWorkerDispatch | undefined {
    if (this.#closed) {
      throw new BrowserNativeWorkerError("InvalidLifecycle");
    }
    if (
      frame.byteLength
        > this.#connection.max_message_bytes + ENVELOPE_HEADER_BYTES
    ) {
      throw new BrowserNativeWorkerError("MessageLimit");
    }
    const transferBytes = this.#validateTransfers(transfers);
    const pending = validateInputFrame(
      frame,
      transferBytes,
      this.#connection,
      this.#inputSequence,
    );
    try {
      const pointer = this.#exports.prepareInput(frame.byteLength);
      this.#observeMemory();
      checkedRange(
        pointer,
        frame.byteLength,
        this.#memoryByteLength,
      );
      new Uint8Array(
        this.#memoryBuffer,
        pointer,
        frame.byteLength,
      ).set(frame);
      for (
        let index = 0;
        index < transferBytes.length;
        index += 1
      ) {
        const transfer = transferBytes[index]!;
        const transferPointer = this.#exports.prepareTransfer(
          index,
          transfer.byteLength,
        );
        this.#observeMemory();
        checkedRange(
          transferPointer,
          transfer.byteLength,
          this.#memoryByteLength,
        );
        new Uint8Array(
          this.#memoryBuffer,
          transferPointer,
          transfer.byteLength,
        ).set(new Uint8Array(transfer));
      }
      const status = this.#exports.dispatch(
        frame.byteLength,
        transferBytes.length,
      );
      this.#observeMemory();
      if (status !== 0 || !pending.commitSequence()) {
        this.#poison("EngineRejected");
      }
      return this.#readOutput();
    } catch (error: unknown) {
      this.#rethrowPoisoned(error);
    }
  }

  poll(): BrowserNativeWorkerDispatch | undefined {
    if (this.#closed) {
      throw new BrowserNativeWorkerError("InvalidLifecycle");
    }
    try {
      const status = this.#exports.poll();
      this.#observeMemory();
      if (status !== 0) {
        this.#poison("EngineRejected");
      }
      return this.#readOutput();
    } catch (error: unknown) {
      this.#rethrowPoisoned(error);
    }
  }

  shutdown(): void {
    if (this.#closed) {
      return;
    }
    this.#closed = true;
    try {
      if (this.#exports.shutdown() !== 0) {
        throw new BrowserNativeWorkerError("EngineRejected");
      }
    } catch (error: unknown) {
      if (error instanceof BrowserNativeWorkerError) {
        throw error;
      }
      throw new BrowserNativeWorkerError("EngineTrap");
    }
  }

  #validateTransfers(
    input: readonly ArrayBuffer[],
  ): readonly ArrayBuffer[] {
    if (
      !Array.isArray(input)
      || input.length > this.#connection.max_transfer_slots
    ) {
      throw new BrowserNativeWorkerError("TransferLimit");
    }
    const transfers: ArrayBuffer[] = [];
    let total = 0;
    for (const transfer of input) {
      if (!isFixedArrayBuffer(transfer)) {
        throw new BrowserNativeWorkerError("TransferLimit");
      }
      total += transfer.byteLength;
      if (
        !Number.isSafeInteger(total)
        || total > this.#maximumMemoryBytes
      ) {
        throw new BrowserNativeWorkerError("TransferLimit");
      }
      transfers.push(transfer);
    }
    return Object.freeze(transfers);
  }

  #observeMemory(): void {
    const epoch = this.#exports.memoryEpoch();
    const buffer = this.#exports.memory.buffer;
    if (
      !isFixedArrayBuffer(buffer)
      || epoch === 0
      || epoch < this.#memoryEpoch
      || buffer.byteLength < this.#minimumMemoryBytes
      || buffer.byteLength > this.#maximumMemoryBytes
      || buffer.byteLength % WASM_PAGE_BYTES !== 0
      || (
        (
          buffer !== this.#memoryBuffer
          || buffer.byteLength !== this.#memoryByteLength
        )
        && epoch <= this.#memoryEpoch
      )
    ) {
      throw new BrowserNativeWorkerError("InvalidWasmMemory");
    }
    this.#memoryBuffer = buffer;
    this.#memoryByteLength = buffer.byteLength;
    this.#memoryEpoch = epoch;
  }

  #readOutput(): BrowserNativeWorkerDispatch | undefined {
    const outputPointer = this.#exports.outputPointer();
    const outputLength = this.#exports.outputLength();
    const transferCount = this.#exports.transferCount();
    if (
      outputLength === 0
      && transferCount === 0
    ) {
      return undefined;
    }
    if (
      outputLength < ENVELOPE_HEADER_BYTES
      || outputLength
        > this.#connection.max_message_bytes + ENVELOPE_HEADER_BYTES
    ) {
      this.#poison("MessageLimit");
    }
    if (transferCount > this.#connection.max_transfer_slots) {
      this.#poison("TransferLimit");
    }
    this.#observeMemory();
    checkedRange(
      outputPointer,
      outputLength,
      this.#memoryByteLength,
    );
    const frame = new Uint8Array(
      this.#memoryBuffer,
      outputPointer,
      outputLength,
    ).slice();
    const transfers: ArrayBuffer[] = [];
    let total = 0;
    for (let index = 0; index < transferCount; index += 1) {
      const pointer = this.#exports.transferPointer(index);
      const length = this.#exports.transferLength(index);
      this.#observeMemory();
      checkedRange(pointer, length, this.#memoryByteLength);
      total += length;
      if (
        !Number.isSafeInteger(total)
        || total > this.#maximumMemoryBytes
      ) {
        this.#poison("TransferLimit");
      }
      transfers.push(
        new Uint8Array(
          this.#memoryBuffer,
          pointer,
          length,
        ).slice().buffer,
      );
    }
    const frozenTransfers = Object.freeze(transfers);
    const pending = validateOutputFrame(
      frame,
      frozenTransfers,
      this.#connection,
      this.#outputSequence,
    );
    if (!pending.commitSequence()) {
      this.#poison("InvalidMessage");
    }
    return Object.freeze({
      frame,
      transfers: frozenTransfers,
    });
  }

  #poison(code: BrowserNativeWorkerErrorCode): never {
    if (!this.#closed) {
      this.#closed = true;
      try {
        void this.#exports.shutdown();
      } catch {
        // The stable primary failure remains the externally visible code.
      }
    }
    throw new BrowserNativeWorkerError(code);
  }

  #rethrowPoisoned(error: unknown): never {
    if (error instanceof BrowserNativeWorkerError) {
      if (
        error.code === "MessageLimit"
        || error.code === "TransferLimit"
        || error.code === "EngineRejected"
        || error.code === "InvalidMessage"
        || error.code === "InvalidWasmMemory"
        || error.code === "InvalidWasmExports"
      ) {
        this.#poison(error.code);
      }
      throw error;
    }
    this.#poison("EngineTrap");
  }
}

export class BrowserNativeWorkerLoader {
  readonly #artifact: BrowserNativeWorkerArtifact;
  readonly #runtime: BrowserNativeWorkerLoaderRuntime;
  #connection: CompatibleHandshake | undefined;
  #load: Promise<BrowserNativeWorkerInstance> | undefined;
  readonly #abort = new AbortController();
  #closed = false;

  constructor(
    artifact: BrowserNativeWorkerArtifact,
    runtime: BrowserNativeWorkerLoaderRuntime = defaultRuntime(),
  ) {
    this.#artifact = validateArtifact(artifact);
    try {
      if (
        typeof runtime.fetch !== "function"
        || typeof runtime.digestSha256 !== "function"
        || typeof runtime.compile !== "function"
        || typeof runtime.instantiate !== "function"
      ) {
        throw new BrowserNativeWorkerError("InvalidConfiguration");
      }
    } catch (error: unknown) {
      if (error instanceof BrowserNativeWorkerError) {
        throw error;
      }
      throw new BrowserNativeWorkerError("InvalidConfiguration");
    }
    this.#runtime = runtime;
  }

  load(
    connection: CompatibleHandshake,
  ): Promise<BrowserNativeWorkerInstance> {
    if (this.#closed) {
      return Promise.reject(
        new BrowserNativeWorkerError("InvalidLifecycle"),
      );
    }
    if (!isCompatibleHandshake(connection)) {
      return Promise.reject(
        new BrowserNativeWorkerError("NegotiationRequired"),
      );
    }
    if (
      this.#connection !== undefined
      && connection !== this.#connection
    ) {
      return Promise.reject(
        new BrowserNativeWorkerError("NegotiationMismatch"),
      );
    }
    this.#connection ??= connection;
    this.#load ??= this.#loadOnce(connection);
    return this.#load;
  }

  close(): void {
    if (this.#closed) {
      return;
    }
    this.#closed = true;
    this.#abort.abort();
    void this.#load?.then(
      (worker) => {
        try {
          worker.shutdown();
        } catch {
          // close remains irreversible even when the rejected instance traps.
        }
      },
      () => undefined,
    );
  }

  #ensureOpen(): void {
    if (this.#closed || this.#abort.signal.aborted) {
      throw new BrowserNativeWorkerError("InvalidLifecycle");
    }
  }

  async #loadOnce(
    connection: CompatibleHandshake,
  ): Promise<BrowserNativeWorkerInstance> {
    const signal = this.#abort.signal;
    let response: Pick<Response, "ok" | "headers" | "body">;
    try {
      response = await awaitAbortable(
        this.#runtime.fetch(this.#artifact.url, signal),
        signal,
      );
      this.#ensureOpen();
      if (!response.ok) {
        throw new BrowserNativeWorkerError(
          "ArtifactFetchFailure",
        );
      }
    } catch (error: unknown) {
      if (error instanceof BrowserNativeWorkerError) {
        throw error;
      }
      throw new BrowserNativeWorkerError("ArtifactFetchFailure");
    }
    let buffer: ArrayBuffer;
    try {
      buffer = await awaitAbortable(
        readBoundedArtifact(
          response,
          this.#artifact.byteLength,
          signal,
        ),
        signal,
      );
      this.#ensureOpen();
      if (!isFixedArrayBuffer(buffer)) {
        throw new BrowserNativeWorkerError(
          "ArtifactFetchFailure",
        );
      }
    } catch (error: unknown) {
      if (error instanceof BrowserNativeWorkerError) {
        throw error;
      }
      throw new BrowserNativeWorkerError("ArtifactFetchFailure");
    }
    const bytes = new Uint8Array(buffer);
    if (bytes.byteLength !== this.#artifact.byteLength) {
      throw new BrowserNativeWorkerError(
        "ArtifactLengthMismatch",
      );
    }
    let digest: Uint8Array;
    try {
      const digestBuffer = await awaitAbortable(
        this.#runtime.digestSha256(bytes, signal),
        signal,
      );
      this.#ensureOpen();
      if (!isFixedArrayBuffer(digestBuffer)) {
        throw new BrowserNativeWorkerError(
          "ArtifactDigestFailure",
        );
      }
      digest = new Uint8Array(digestBuffer);
    } catch (error: unknown) {
      if (error instanceof BrowserNativeWorkerError) {
        throw error;
      }
      throw new BrowserNativeWorkerError(
        "ArtifactDigestFailure",
      );
    }
    if (
      digest.byteLength * 2 !== SHA256_HEX_LENGTH
      || bytesToHex(digest) !== this.#artifact.sha256
    ) {
      throw new BrowserNativeWorkerError("ArtifactHashMismatch");
    }
    const memory = inspectNativeWorkerMemory(bytes);
    if (
      memory.minimum !== this.#artifact.minimumMemoryPages
      || memory.maximum !== this.#artifact.maximumMemoryPages
    ) {
      throw new BrowserNativeWorkerError("InvalidWasmMemory");
    }
    let module: WebAssembly.Module;
    try {
      module = await awaitAbortable(
        this.#runtime.compile(bytes, signal),
        signal,
      );
      this.#ensureOpen();
    } catch (error: unknown) {
      if (error instanceof BrowserNativeWorkerError) {
        throw error;
      }
      throw new BrowserNativeWorkerError("InvalidWasmModule");
    }
    let imports: readonly WebAssembly.ModuleImportDescriptor[];
    let exports: readonly WebAssembly.ModuleExportDescriptor[];
    try {
      imports = WebAssembly.Module.imports(module);
    } catch {
      throw new BrowserNativeWorkerError("InvalidWasmImports");
    }
    try {
      exports = WebAssembly.Module.exports(module);
    } catch {
      throw new BrowserNativeWorkerError("InvalidWasmExports");
    }
    if (
      !exactDescriptors(
        imports,
        EXPECTED_NATIVE_WORKER_IMPORTS,
        true,
      )
    ) {
      throw new BrowserNativeWorkerError("InvalidWasmImports");
    }
    if (
      !exactDescriptors(
        exports,
        EXPECTED_NATIVE_WORKER_EXPORTS,
        false,
      )
    ) {
      throw new BrowserNativeWorkerError("InvalidWasmExports");
    }
    inspectNativeWorkerAbi(bytes);
    let instance: WebAssembly.Instance;
    try {
      instance = await awaitAbortable(
        this.#runtime.instantiate(module, {}, signal),
        signal,
        bestEffortShutdown,
      );
    } catch (error: unknown) {
      if (error instanceof BrowserNativeWorkerError) {
        throw error;
      }
      throw new BrowserNativeWorkerError(
        "InstantiationFailure",
      );
    }
    if (this.#closed) {
      bestEffortShutdown(instance);
      throw new BrowserNativeWorkerError("InvalidLifecycle");
    }
    try {
      const worker = new BrowserNativeWorkerInstance(
        connection,
        instance,
        this.#artifact.minimumMemoryPages,
        this.#artifact.maximumMemoryPages,
      );
      if (this.#closed) {
        try {
          worker.shutdown();
        } catch {
          // The loader still rejects the closed load operation.
        }
        throw new BrowserNativeWorkerError("InvalidLifecycle");
      }
      return worker;
    } catch (error: unknown) {
      bestEffortShutdown(instance);
      if (error instanceof BrowserNativeWorkerError) {
        throw error;
      }
      throw new BrowserNativeWorkerError("InvalidWasmExports");
    }
  }
}

export const nativeWorkerSha256 = async (
  bytes: Uint8Array,
): Promise<string> => bytesToHex(
  new Uint8Array(
    await crypto.subtle.digest("SHA-256", copiedBytes(bytes)),
  ),
);

export const nativeWorkerMaximumMemoryPages = (): number =>
  MAX_WASM_PAGES;

export const nativeWorkerMaximumArtifactBytes = (): number =>
  MAX_NATIVE_WORKER_ARTIFACT_BYTES;

export const nativeWorkerExpectedExports = (
): readonly WebAssembly.ModuleExportDescriptor[] =>
  EXPECTED_NATIVE_WORKER_EXPORTS;

export const wasmPageBytes = (): number => WASM_PAGE_BYTES;

export const nativeWorkerMessageDescriptor = (
  messageType: number,
) => descriptorById(messageType);
