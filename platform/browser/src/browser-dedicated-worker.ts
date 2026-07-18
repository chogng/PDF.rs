import {
  BROWSER_NATIVE_WORKER_MAX_CONTROL_BYTES,
  BROWSER_NATIVE_WORKER_MAX_TRANSFER_BYTES,
  createBrowserNativeWorkerStart,
} from "./browser-native-worker-entry.js";
import {
  MAX_TRANSFER_SLOTS,
} from "../generated/engine-protocol.js";
import type {
  BrowserNativeWorkerSupervisorIdentity,
} from "./browser-native-worker-loader.js";
import type {
  BrowserWorkerFactory,
  BrowserWorkerHandlers,
  BrowserWorkerPort,
} from "./browser-worker-supervisor.js";

const MAX_U64 = 0xffff_ffff_ffff_ffffn;
const MAX_ENTRY_ARTIFACT_BYTES = 64 * 1_024;
const MAX_ENTRY_URL_BYTES = 4_096;
const MAX_WORKER_NAME_BYTES = 128;
const REGISTERED_ENTRY_FILE = "engine-worker-entry.generated.js";
const ENTRY_REFERENCES = new WeakSet<object>();
const ENTRY_REFERENCE_URLS = new WeakMap<object, string>();
const REFLECT_APPLY = Reflect.apply;
const URL_CONSTRUCTOR = URL;
const URL_TO_STRING = URL.prototype.toString;
const URL_PROTOCOL_GETTER =
  Object.getOwnPropertyDescriptor(URL.prototype, "protocol")?.get;
const URL_USERNAME_GETTER =
  Object.getOwnPropertyDescriptor(URL.prototype, "username")?.get;
const URL_PASSWORD_GETTER =
  Object.getOwnPropertyDescriptor(URL.prototype, "password")?.get;
const URL_PATHNAME_GETTER =
  Object.getOwnPropertyDescriptor(URL.prototype, "pathname")?.get;
const URL_SEARCH_GETTER =
  Object.getOwnPropertyDescriptor(URL.prototype, "search")?.get;
const URL_HASH_GETTER =
  Object.getOwnPropertyDescriptor(URL.prototype, "hash")?.get;
const ARRAY_BUFFER_BYTE_LENGTH_DESCRIPTOR =
  Object.getOwnPropertyDescriptor(
    ArrayBuffer.prototype,
    "byteLength",
  );
const ARRAY_BUFFER_RESIZABLE_DESCRIPTOR =
  Object.getOwnPropertyDescriptor(
    ArrayBuffer.prototype,
    "resizable",
  );
const ARRAY_BUFFER_RESIZE_DESCRIPTOR =
  Object.getOwnPropertyDescriptor(
    ArrayBuffer.prototype,
    "resize",
  );
const ARRAY_BUFFER_BYTE_LENGTH_GETTER =
  ARRAY_BUFFER_BYTE_LENGTH_DESCRIPTOR?.get;
// A realm without either resizable accessor or resize method predates
// resizable ArrayBuffer and therefore has only fixed backing stores. Any
// partial or malformed support snapshot fails closed.
const ARRAY_BUFFER_RESIZABLE_GETTER =
  ARRAY_BUFFER_RESIZABLE_DESCRIPTOR === undefined
  && ARRAY_BUFFER_RESIZE_DESCRIPTOR === undefined
    ? null
    : ARRAY_BUFFER_RESIZABLE_DESCRIPTOR?.get;

/** Minimal Host-realm Dedicated Worker used by the production port adapter. */
export interface BrowserDedicatedWorker {
  addEventListener(
    type: "message" | "messageerror" | "error",
    listener: EventListener,
  ): void;
  removeEventListener(
    type: "message" | "messageerror" | "error",
    listener: EventListener,
  ): void;
  postMessage(value: unknown, transfer: ArrayBuffer[]): void;
  terminate(): void;
}

/** Injectable constructor boundary for deterministic adapter tests. */
export interface BrowserDedicatedWorkerRuntime {
  construct(
    entryUrl: string | URL,
    workerName: string,
  ): BrowserDedicatedWorker;
}

/** Immutable construction data for a restart-aware module Worker factory. */
export interface BrowserDedicatedWorkerFactoryConfiguration {
  readonly entry: UnverifiedBrowserNativeWorkerEntryReference;
  readonly rendererEpoch: number;
  readonly workerNamePrefix: string;
  readonly runtime?: BrowserDedicatedWorkerRuntime;
}

/**
 * Opaque but unverified module URL ownership selected by an embedding.
 *
 * This brand prevents accidental raw-string construction only. It does not
 * prove same-origin deployment, runtime integrity, or that fetched bytes
 * install the controller; those properties remain browser-evidence gates.
 */
export interface UnverifiedBrowserNativeWorkerEntryReference {
  /**
   * A diagnostic copy only. Mutating this URL never changes the private,
   * canonical construction URL captured when the reference was created.
   */
  readonly url: URL;
}

/**
 * Structurally bounded artifact candidate consumed before constructing a URL.
 *
 * This public shape is not a trusted registry: arbitrary callers can construct
 * another candidate. The generated Host registration artifact and product
 * purity gate separately prove that the product call site supplies the
 * manifest's canonical tuple. Neither layer proves what a browser later
 * fetches, deployment origin, or runtime integrity.
 */
export interface BrowserNativeWorkerEntryArtifactCandidate {
  readonly byteLength: number;
  readonly sha256: string;
  readonly url: URL;
}

const defaultRuntime = (): BrowserDedicatedWorkerRuntime =>
  Object.freeze({
    construct: (
      entryUrl: string | URL,
      workerName: string,
    ): BrowserDedicatedWorker =>
      new Worker(
        entryUrl,
        Object.freeze({
          credentials: "same-origin",
          name: workerName,
          type: "module",
        }),
      ),
  });

const isU32 = (value: unknown): value is number =>
  typeof value === "number"
  && Number.isInteger(value)
  && value >= 0
  && value <= 0xffff_ffff;

/**
 * Takes an immutable URL from one exact structural artifact candidate.
 *
 * The product's generated Host module supplies the manifest-bound candidate,
 * but this public function does not authenticate arbitrary callers. The
 * returned brand remains deliberately unverified at runtime and does not
 * replace same-origin or network-trace evidence.
 */
export function createUnverifiedBrowserNativeWorkerEntryReference(
  candidate: BrowserNativeWorkerEntryArtifactCandidate,
): UnverifiedBrowserNativeWorkerEntryReference {
  let snapshot: URL;
  try {
    if (
      typeof candidate !== "object"
      || candidate === null
      || Object.getPrototypeOf(candidate) !== Object.prototype
    ) {
      throw new TypeError("InvalidEntry");
    }
    const keys = Reflect.ownKeys(candidate);
    const byteLengthDescriptor =
      Object.getOwnPropertyDescriptor(candidate, "byteLength");
    const sha256Descriptor =
      Object.getOwnPropertyDescriptor(candidate, "sha256");
    const urlDescriptor =
      Object.getOwnPropertyDescriptor(candidate, "url");
    if (
      keys.length !== 3
      || !keys.includes("byteLength")
      || !keys.includes("sha256")
      || !keys.includes("url")
      || byteLengthDescriptor === undefined
      || sha256Descriptor === undefined
      || urlDescriptor === undefined
      || !Object.hasOwn(byteLengthDescriptor, "value")
      || !Object.hasOwn(sha256Descriptor, "value")
      || !Object.hasOwn(urlDescriptor, "value")
      || byteLengthDescriptor.configurable !== false
      || byteLengthDescriptor.enumerable !== true
      || byteLengthDescriptor.writable !== false
      || sha256Descriptor.configurable !== false
      || sha256Descriptor.enumerable !== true
      || sha256Descriptor.writable !== false
      || urlDescriptor.configurable !== false
      || urlDescriptor.enumerable !== true
      || urlDescriptor.writable !== false
      || !Number.isSafeInteger(byteLengthDescriptor.value)
      || byteLengthDescriptor.value <= 0
      || byteLengthDescriptor.value > MAX_ENTRY_ARTIFACT_BYTES
      || typeof sha256Descriptor.value !== "string"
      || !/^[0-9a-f]{64}$/u.test(sha256Descriptor.value)
      || typeof URL_PROTOCOL_GETTER !== "function"
      || typeof URL_USERNAME_GETTER !== "function"
      || typeof URL_PASSWORD_GETTER !== "function"
      || typeof URL_PATHNAME_GETTER !== "function"
      || typeof URL_SEARCH_GETTER !== "function"
      || typeof URL_HASH_GETTER !== "function"
    ) {
      throw new TypeError("InvalidEntry");
    }
    const serialized = REFLECT_APPLY(
      URL_TO_STRING,
      urlDescriptor.value,
      [],
    );
    if (
      typeof serialized !== "string"
      ||
      new TextEncoder().encode(serialized).byteLength
        > MAX_ENTRY_URL_BYTES
    ) {
      throw new TypeError("InvalidEntry");
    }
    snapshot = new URL_CONSTRUCTOR(serialized);
    const protocol = REFLECT_APPLY(
      URL_PROTOCOL_GETTER,
      snapshot,
      [],
    );
    const username = REFLECT_APPLY(
      URL_USERNAME_GETTER,
      snapshot,
      [],
    );
    const password = REFLECT_APPLY(
      URL_PASSWORD_GETTER,
      snapshot,
      [],
    );
    const pathname = REFLECT_APPLY(
      URL_PATHNAME_GETTER,
      snapshot,
      [],
    );
    const search = REFLECT_APPLY(URL_SEARCH_GETTER, snapshot, []);
    const hash = REFLECT_APPLY(URL_HASH_GETTER, snapshot, []);
    if (
      protocol !== "https:"
      && protocol !== "http:"
      || username !== ""
      || password !== ""
      || search !== ""
      || hash !== ""
      || typeof pathname !== "string"
      || pathname.split("/").at(-1)
        !== REGISTERED_ENTRY_FILE
    ) {
      throw new TypeError("InvalidEntry");
    }
  } catch {
    throw new TypeError("InvalidEntry");
  }
  const canonical = REFLECT_APPLY(URL_TO_STRING, snapshot, []);
  if (typeof canonical !== "string") {
    throw new TypeError("InvalidEntry");
  }
  const reference = Object.freeze({ url: snapshot });
  ENTRY_REFERENCES.add(reference);
  ENTRY_REFERENCE_URLS.set(reference, canonical);
  return reference;
}

const snapshotEntryReference = (
  value: UnverifiedBrowserNativeWorkerEntryReference,
): string | undefined => {
  try {
    if (
      typeof value !== "object"
      || value === null
      || !ENTRY_REFERENCES.has(value)
      || Object.getPrototypeOf(value) !== Object.prototype
    ) {
      return undefined;
    }
    const keys = Reflect.ownKeys(value);
    const descriptor = Object.getOwnPropertyDescriptor(value, "url");
    if (
      keys.length !== 1
      || keys[0] !== "url"
      || descriptor === undefined
      || !Object.hasOwn(descriptor, "value")
    ) {
      return undefined;
    }
    const canonical = ENTRY_REFERENCE_URLS.get(value);
    if (canonical === undefined) {
      return undefined;
    }
    const snapshot = new URL_CONSTRUCTOR(canonical);
    const protocol = typeof URL_PROTOCOL_GETTER === "function"
      ? REFLECT_APPLY(URL_PROTOCOL_GETTER, snapshot, [])
      : undefined;
    const serialized = REFLECT_APPLY(URL_TO_STRING, snapshot, []);
    if (
      (
        protocol !== "https:"
        && protocol !== "http:"
      )
      || serialized !== canonical
      || new TextEncoder().encode(canonical).byteLength
        > MAX_ENTRY_URL_BYTES
    ) {
      return undefined;
    }
    return canonical;
  } catch {
    return undefined;
  }
};

const snapshotWorkerNamePrefix = (
  value: unknown,
): string | undefined => {
  if (
    typeof value !== "string"
    || value.length === 0
    || new TextEncoder().encode(value).byteLength
      > MAX_WORKER_NAME_BYTES / 2
  ) {
    return undefined;
  }
  return value;
};

interface CanonicalTransferTable {
  readonly value: ArrayBuffer[];
  readonly transfer: ArrayBuffer[];
}

interface FixedArrayBufferSnapshot {
  readonly value: ArrayBuffer;
  readonly byteLength: number;
}

const fixedArrayBufferSnapshot = (
  value: unknown,
): FixedArrayBufferSnapshot | undefined => {
  if (
    typeof ARRAY_BUFFER_BYTE_LENGTH_GETTER !== "function"
    || ARRAY_BUFFER_RESIZABLE_GETTER === undefined
  ) {
    return undefined;
  }
  try {
    const byteLength = REFLECT_APPLY(
      ARRAY_BUFFER_BYTE_LENGTH_GETTER,
      value,
      [],
    ) as unknown;
    const resizable = ARRAY_BUFFER_RESIZABLE_GETTER === null
      ? false
      : REFLECT_APPLY(
        ARRAY_BUFFER_RESIZABLE_GETTER,
        value,
        [],
      ) as unknown;
    if (
      typeof byteLength !== "number"
      || !Number.isSafeInteger(byteLength)
      || byteLength < 0
      || typeof resizable !== "boolean"
      || resizable
    ) {
      return undefined;
    }
    return Object.freeze({
      value: value as ArrayBuffer,
      byteLength,
    });
  } catch {
    return undefined;
  }
};

const exactArrayLength = (
  value: unknown[],
): number | undefined => {
  if (
    !Array.isArray(value)
    || Object.getPrototypeOf(value) !== Array.prototype
  ) {
    return undefined;
  }
  const descriptor = Object.getOwnPropertyDescriptor(value, "length");
  if (
    descriptor === undefined
    || !Object.hasOwn(descriptor, "value")
    || typeof descriptor.value !== "number"
    || !Number.isSafeInteger(descriptor.value)
    || descriptor.value <= 0
    || descriptor.value > MAX_TRANSFER_SLOTS + 1
    || descriptor.configurable !== false
    || descriptor.enumerable !== false
    || descriptor.writable !== true
  ) {
    return undefined;
  }
  const length = descriptor.value;
  const keys = Reflect.ownKeys(value);
  if (keys.length !== length + 1 || keys[length] !== "length") {
    return undefined;
  }
  for (let index = 0; index < length; index += 1) {
    if (keys[index] !== index.toString(10)) {
      return undefined;
    }
  }
  return length;
};

const exactTransferTable = (
  value: unknown[],
  transfer: ArrayBuffer[],
): CanonicalTransferTable | undefined => {
  try {
    const valueLength = exactArrayLength(value);
    const transferLength = exactArrayLength(transfer);
    if (
      valueLength === undefined
      || transferLength === undefined
      || valueLength !== transferLength
    ) {
      return undefined;
    }
    const canonicalValue: ArrayBuffer[] = [];
    const canonicalTransfer: ArrayBuffer[] = [];
    const seen = new Set<ArrayBuffer>();
    let physicalBytes = 0;
    let transferBytes = 0;
    for (let index = 0; index < valueLength; index += 1) {
      const valueDescriptor = Object.getOwnPropertyDescriptor(
        value,
        index.toString(10),
      );
      const transferDescriptor = Object.getOwnPropertyDescriptor(
        transfer,
        index.toString(10),
      );
      if (
        valueDescriptor === undefined
        || transferDescriptor === undefined
        || !Object.hasOwn(valueDescriptor, "value")
        || !Object.hasOwn(transferDescriptor, "value")
        || valueDescriptor.configurable !== true
        || valueDescriptor.enumerable !== true
        || valueDescriptor.writable !== true
        || transferDescriptor.configurable !== true
        || transferDescriptor.enumerable !== true
        || transferDescriptor.writable !== true
        || valueDescriptor.value !== transferDescriptor.value
      ) {
        return undefined;
      }
      const candidate = fixedArrayBufferSnapshot(
        valueDescriptor.value,
      );
      if (
        candidate === undefined
        || seen.has(candidate.value)
      ) {
        return undefined;
      }
      physicalBytes += candidate.byteLength;
      if (
        !Number.isSafeInteger(physicalBytes)
        || physicalBytes
          > BROWSER_NATIVE_WORKER_MAX_TRANSFER_BYTES
      ) {
        return undefined;
      }
      if (index === 0) {
        if (
          candidate.byteLength
            > BROWSER_NATIVE_WORKER_MAX_CONTROL_BYTES
        ) {
          return undefined;
        }
      } else {
        transferBytes += candidate.byteLength;
        if (
          !Number.isSafeInteger(transferBytes)
          || transferBytes
            > BROWSER_NATIVE_WORKER_MAX_TRANSFER_BYTES
        ) {
          return undefined;
        }
      }
      seen.add(candidate.value);
      canonicalValue.push(candidate.value);
      canonicalTransfer.push(candidate.value);
    }
    return Object.freeze({
      value: canonicalValue,
      transfer: canonicalTransfer,
    });
  } catch {
    return undefined;
  }
};

const validWorker = (value: bigint): boolean =>
  value > 0n && value <= MAX_U64;

/**
 * Creates the BrowserWorkerFactory consumed unchanged by the supervisor/reader.
 *
 * Each factory call constructs one module Worker, posts one exact identity
 * start record, and returns a port whose callbacks become inert on terminate.
 * The generated product boundary supplies this unverified reference, but the
 * reference is not a fetched-byte or runtime-integrity proof. Browsers expose
 * no DedicatedWorker normal-exit
 * event, so this adapter never synthesizes `onTerminated`: protocol
 * WorkerStopped closes cleanly, message/error paths fault, and Host teardown
 * calls the returned port's idempotent `terminate`.
 */
export function createBrowserDedicatedWorkerFactory(
  configuration: BrowserDedicatedWorkerFactoryConfiguration,
): BrowserWorkerFactory {
  let workerNamePrefix: string | undefined;
  let entryUrl: string | undefined;
  let construct: BrowserDedicatedWorkerRuntime["construct"];
  try {
    entryUrl = snapshotEntryReference(configuration.entry);
    workerNamePrefix = snapshotWorkerNamePrefix(
      configuration.workerNamePrefix,
    );
    const runtime = configuration.runtime ?? defaultRuntime();
    if (
      entryUrl === undefined
      || workerNamePrefix === undefined
      || !isU32(configuration.rendererEpoch)
      || configuration.rendererEpoch === 0
      || typeof runtime !== "object"
      || runtime === null
      || typeof runtime.construct !== "function"
    ) {
      throw new TypeError("InvalidConfiguration");
    }
    construct = runtime.construct.bind(runtime);
  } catch {
    throw new TypeError("InvalidConfiguration");
  }
  const rendererEpoch = configuration.rendererEpoch;
  return (worker: bigint): BrowserWorkerPort => {
    if (!validWorker(worker)) {
      throw new TypeError("InvalidWorker");
    }
    const identity: BrowserNativeWorkerSupervisorIdentity =
      Object.freeze({
        rendererEpoch,
        worker,
        workerEpoch: worker,
      });
    const workerName = `${workerNamePrefix}-${worker.toString(16)}`;
    if (
      new TextEncoder().encode(workerName).byteLength
        > MAX_WORKER_NAME_BYTES
    ) {
      throw new TypeError("InvalidWorker");
    }
    let dedicated: BrowserDedicatedWorker;
    let constructed: BrowserDedicatedWorker | undefined;
    try {
      dedicated = construct(
        new URL_CONSTRUCTOR(entryUrl),
        workerName,
      );
      constructed = dedicated;
      if (
        typeof dedicated !== "object"
        || dedicated === null
        || typeof dedicated.addEventListener !== "function"
        || typeof dedicated.removeEventListener !== "function"
        || typeof dedicated.postMessage !== "function"
        || typeof dedicated.terminate !== "function"
      ) {
        throw new TypeError("InvalidWorker");
      }
    } catch {
      try {
        constructed?.terminate();
      } catch {
        // Any object returned by the constructor is already rejected.
      }
      throw new TypeError("InvalidWorker");
    }
    let active = true;
    let handlers: BrowserWorkerHandlers | undefined;
    let listenersAttached = false;
    let startPosted = false;
    const onMessage: EventListener = (event: Event): void => {
      if (active && handlers !== undefined) {
        handlers.onMessage(
          event instanceof MessageEvent ? event.data : undefined,
        );
      }
    };
    const onMessageError: EventListener = (): void => {
      if (active && handlers !== undefined) {
        handlers.onMessageError();
      }
    };
    const onError: EventListener = (event: Event): void => {
      if (active && handlers !== undefined) {
        try {
          event.preventDefault();
        } catch {
          // Stable fault delivery does not depend on browser diagnostics.
        }
        handlers.onError();
      }
    };
    const detach = (): void => {
      if (!listenersAttached) {
        return;
      }
      listenersAttached = false;
      try {
        dedicated.removeEventListener("message", onMessage);
      } catch {
        // The port is already terminal at this ownership boundary.
      }
      try {
        dedicated.removeEventListener(
          "messageerror",
          onMessageError,
        );
      } catch {
        // The port is already terminal at this ownership boundary.
      }
      try {
        dedicated.removeEventListener("error", onError);
      } catch {
        // The port is already terminal at this ownership boundary.
      }
    };
    const terminate = (): void => {
      if (!active) {
        return;
      }
      active = false;
      handlers = undefined;
      detach();
      dedicated.terminate();
    };
    return Object.freeze({
      setHandlers: (next: BrowserWorkerHandlers): void => {
        if (!active || handlers !== undefined) {
          throw new TypeError("InvalidWorker");
        }
        if (
          typeof next !== "object"
          || next === null
          || typeof next.onMessage !== "function"
          || typeof next.onMessageError !== "function"
          || typeof next.onError !== "function"
          || typeof next.onTerminated !== "function"
        ) {
          terminate();
          throw new TypeError("InvalidWorker");
        }
        handlers = next;
        let messageAttempted = false;
        let messageErrorAttempted = false;
        let errorAttempted = false;
        try {
          messageAttempted = true;
          dedicated.addEventListener("message", onMessage);
          messageErrorAttempted = true;
          dedicated.addEventListener(
            "messageerror",
            onMessageError,
          );
          errorAttempted = true;
          dedicated.addEventListener("error", onError);
          listenersAttached = true;
          if (startPosted) {
            throw new TypeError("InvalidWorker");
          }
          dedicated.postMessage(
            createBrowserNativeWorkerStart(identity),
            [],
          );
          startPosted = true;
        } catch {
          if (!listenersAttached && messageAttempted) {
            try {
              dedicated.removeEventListener("message", onMessage);
            } catch {
              // Teardown continues through the remaining owners.
            }
          }
          if (!listenersAttached && messageErrorAttempted) {
            try {
              dedicated.removeEventListener(
                "messageerror",
                onMessageError,
              );
            } catch {
              // Teardown continues through the remaining owners.
            }
          }
          if (!listenersAttached && errorAttempted) {
            try {
              dedicated.removeEventListener("error", onError);
            } catch {
              // Teardown continues through the remaining owners.
            }
          }
          handlers = undefined;
          try {
            terminate();
          } catch {
            // Registration still fails closed.
          }
          throw new TypeError("InvalidWorker");
        }
      },
      postMessage: (
        value: unknown[],
        transfer: ArrayBuffer[],
      ): void => {
        if (!active || handlers === undefined) {
          throw new TypeError("InvalidWorker");
        }
        const canonical = exactTransferTable(value, transfer);
        if (canonical === undefined) {
          throw new TypeError("InvalidWorker");
        }
        dedicated.postMessage(
          canonical.value,
          canonical.transfer,
        );
      },
      terminate,
    });
  };
}
