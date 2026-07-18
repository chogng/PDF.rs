import {
  MAX_MESSAGE_BYTES,
  MAX_TRANSFER_SLOTS,
} from "../generated/engine-protocol.js";
import {
  BrowserNativeWorkerLoader,
  nativeWorkerMaximumMemoryPages,
  wasmPageBytes,
} from "./browser-native-worker-loader.js";
import {
  BROWSER_CONTROL_HEADER_BYTES,
} from "./browser-command-boundary.js";
import type {
  BrowserNativeWorkerArtifact,
  BrowserNativeWorkerDispatch,
  BrowserNativeWorkerInstance,
  BrowserNativeWorkerLoaderRuntime,
  BrowserNativeWorkerPoll,
  BrowserNativeWorkerSupervisorIdentity,
} from "./browser-native-worker-loader.js";

const MAX_U64 = 0xffff_ffff_ffff_ffffn;
const MAX_ENTRY_QUEUE_CAPACITY = 4_096;
const MAX_ENTRY_TURN = 4_096;
export const BROWSER_NATIVE_WORKER_MAX_CONTROL_BYTES =
  MAX_MESSAGE_BYTES + BROWSER_CONTROL_HEADER_BYTES;
export const BROWSER_NATIVE_WORKER_MAX_TRANSFER_BYTES =
  nativeWorkerMaximumMemoryPages() * wasmPageBytes();
const START_KIND = "pdf-rs-native-worker-start-v1";

/** Stable, content-free failures raised by the Worker entry controller. */
export type BrowserNativeWorkerEntryFaultCode =
  | "InvalidConfiguration"
  | "InvalidBootstrap"
  | "InboundQueueOverflow"
  | "MessageError"
  | "NativeFailure"
  | "OutboundTransportFailure"
  | "SchedulerFailure";

/** An entry failure that never includes message, engine, or source data. */
export class BrowserNativeWorkerEntryError extends Error {
  readonly code: BrowserNativeWorkerEntryFaultCode;

  constructor(code: BrowserNativeWorkerEntryFaultCode) {
    super(code);
    this.name = "BrowserNativeWorkerEntryError";
    this.code = code;
  }
}

/** One strictly structured-cloneable start record, sent before Host Hello. */
export interface BrowserNativeWorkerStart {
  readonly kind: typeof START_KIND;
  readonly rendererEpoch: number;
  readonly worker: bigint;
  readonly workerEpoch: bigint;
}

/** Minimal module-Worker global used by the production entry and fake scopes. */
export interface BrowserNativeWorkerEntryScope {
  addEventListener(
    type: "message" | "messageerror",
    listener: EventListener,
  ): void;
  removeEventListener(
    type: "message" | "messageerror",
    listener: EventListener,
  ): void;
  postMessage(value: unknown[], transfer: ArrayBuffer[]): void;
  close(): void;
}

/** Worker global features needed by the one-call production installer. */
export interface BrowserNativeWorkerInstallationScope
  extends BrowserNativeWorkerEntryScope {
  setTimeout(callback: () => void, milliseconds: number): number;
  clearTimeout(handle: number): void;
}

/** Cancelable task scheduler; callbacks must never run inline from request. */
export interface BrowserNativeWorkerEntryScheduler {
  request(callback: () => void): number;
  cancel(handle: number): void;
}

/** Independent queue and actor-turn hard bounds. */
export interface BrowserNativeWorkerEntryLimits {
  readonly maxInboundMessages: number;
  readonly maxQueuedBytes: number;
  readonly maxTurn: number;
  readonly maxTransferBytes: number;
  readonly maxTransferSlots: number;
}

/** Loader surface consumed by the entry; production passes the real loader. */
export interface BrowserNativeWorkerEntryLoader {
  bootstrap(
    hostHelloFrame: Uint8Array,
    supervisorIdentity: BrowserNativeWorkerSupervisorIdentity,
  ): Promise<BrowserNativeWorkerEntryInstance>;
  close(): void;
}

/** Native instance surface consumed without reimplementing protocol semantics. */
export interface BrowserNativeWorkerEntryInstance {
  readonly engineHello: BrowserNativeWorkerDispatch;
  readonly ready: boolean;
  readonly stopped: boolean;
  accept(frame: Uint8Array): BrowserNativeWorkerDispatch;
  dispatch(
    frame: Uint8Array,
    transfers?: readonly ArrayBuffer[],
  ): BrowserNativeWorkerDispatch | undefined;
  poll(): BrowserNativeWorkerPoll;
}

/** Construction data for one Worker-realm controller. */
export interface BrowserNativeWorkerEntryConfiguration {
  readonly scope: BrowserNativeWorkerEntryScope;
  readonly scheduler: BrowserNativeWorkerEntryScheduler;
  readonly loader: BrowserNativeWorkerEntryLoader;
  readonly limits: BrowserNativeWorkerEntryLimits;
  readonly fatal?: (code: BrowserNativeWorkerEntryFaultCode) => void;
}

/** Inputs bound by the generated Worker bundle in M5-09. */
export interface BrowserNativeWorkerInstallationConfiguration {
  readonly scope: BrowserNativeWorkerInstallationScope;
  readonly artifact: BrowserNativeWorkerArtifact;
  readonly runtime?: BrowserNativeWorkerLoaderRuntime;
  readonly limits: BrowserNativeWorkerEntryLimits;
  readonly fatal?: (code: BrowserNativeWorkerEntryFaultCode) => void;
}

type EntryState =
  | "AwaitingStart"
  | "AwaitingHello"
  | "Loading"
  | "BootstrapReady"
  | "AwaitingAccept"
  | "Ready"
  | "Closed"
  | "Failed";

interface PhysicalMessage {
  readonly byteLength: number;
  readonly control: Uint8Array<ArrayBuffer>;
  readonly transfers: readonly ArrayBuffer[];
}

type InboundMessage =
  | Readonly<{
    readonly kind: "physical";
    readonly physical: PhysicalMessage;
  }>
  | Readonly<{
    readonly kind: "start";
    readonly start: BrowserNativeWorkerStart;
  }>;

interface SnapshotLimits {
  readonly maxInboundMessages: number;
  readonly maxQueuedBytes: number;
  readonly maxTurn: number;
  readonly maxTransferBytes: number;
  readonly maxTransferSlots: number;
}

type BootstrapCompletion =
  | Readonly<{
    readonly kind: "fulfilled";
    readonly instance: BrowserNativeWorkerEntryInstance;
  }>
  | Readonly<{
    readonly kind: "rejected";
  }>;

const isPositiveBound = (
  value: unknown,
  maximum: number,
): value is number =>
  typeof value === "number"
  && Number.isSafeInteger(value)
  && value > 0
  && value <= maximum;

const isU32 = (value: unknown): value is number =>
  typeof value === "number"
  && Number.isInteger(value)
  && value >= 0
  && value <= 0xffff_ffff;

const isFixedArrayBuffer = (
  value: unknown,
): value is ArrayBuffer => {
  if (!(value instanceof ArrayBuffer)) {
    return false;
  }
  const candidate = value as ArrayBuffer & {
    readonly resizable?: boolean;
  };
  return candidate.resizable !== true;
};

const exactOwnDataRecord = (
  value: unknown,
  names: readonly string[],
): Readonly<Record<string, PropertyDescriptor>> | undefined => {
  if (
    typeof value !== "object"
    || value === null
    || Object.getPrototypeOf(value) !== Object.prototype
  ) {
    return undefined;
  }
  const keys = Reflect.ownKeys(value);
  if (
    keys.some((key) => typeof key !== "string")
    || JSON.stringify(
      keys.map((key) => String(key)).sort(),
    ) !== JSON.stringify([...names].sort())
  ) {
    return undefined;
  }
  const descriptors: Record<string, PropertyDescriptor> =
    Object.create(null) as Record<string, PropertyDescriptor>;
  for (const name of names) {
    const descriptor = Object.getOwnPropertyDescriptor(value, name);
    if (
      descriptor === undefined
      || !Object.hasOwn(descriptor, "value")
      || descriptor.get !== undefined
      || descriptor.set !== undefined
    ) {
      return undefined;
    }
    descriptors[name] = descriptor;
  }
  return descriptors;
};

const startRecord = (
  value: unknown,
): BrowserNativeWorkerStart | undefined => {
  try {
    const descriptors = exactOwnDataRecord(value, [
      "kind",
      "rendererEpoch",
      "worker",
      "workerEpoch",
    ]);
    if (descriptors === undefined) {
      return undefined;
    }
    const kind = descriptors.kind?.value;
    const rendererEpoch = descriptors.rendererEpoch?.value;
    const worker = descriptors.worker?.value;
    const workerEpoch = descriptors.workerEpoch?.value;
    if (
      kind !== START_KIND
      || typeof worker !== "bigint"
      || worker <= 0n
      || worker > MAX_U64
      || typeof workerEpoch !== "bigint"
      || workerEpoch <= 0n
      || workerEpoch > MAX_U64
      || !isU32(rendererEpoch)
      || rendererEpoch === 0
    ) {
      return undefined;
    }
    const cloned = structuredClone(value);
    if (
      typeof cloned !== "object"
      || cloned === null
      || Object.getPrototypeOf(cloned) !== Object.prototype
    ) {
      return undefined;
    }
    return Object.freeze({
      kind: START_KIND,
      rendererEpoch,
      worker,
      workerEpoch,
    });
  } catch {
    return undefined;
  }
};

/** Builds the only accepted start record after taking an exact identity copy. */
export function createBrowserNativeWorkerStart(
  identity: BrowserNativeWorkerSupervisorIdentity,
): BrowserNativeWorkerStart {
  const record = Object.freeze({
    kind: START_KIND,
    rendererEpoch: identity.rendererEpoch,
    worker: identity.worker,
    workerEpoch: identity.workerEpoch,
  });
  const snapshot = startRecord(record);
  if (snapshot === undefined) {
    throw new BrowserNativeWorkerEntryError("InvalidConfiguration");
  }
  return snapshot;
}

const snapshotLimits = (
  limits: BrowserNativeWorkerEntryLimits,
): SnapshotLimits | undefined => {
  try {
    if (
      !isPositiveBound(
        limits.maxInboundMessages,
        MAX_ENTRY_QUEUE_CAPACITY,
      )
      || !isPositiveBound(
        limits.maxQueuedBytes,
        BROWSER_NATIVE_WORKER_MAX_TRANSFER_BYTES,
      )
      || !isPositiveBound(limits.maxTurn, MAX_ENTRY_TURN)
      || !isPositiveBound(
        limits.maxTransferBytes,
        BROWSER_NATIVE_WORKER_MAX_TRANSFER_BYTES,
      )
      || !isPositiveBound(
        limits.maxTransferSlots,
        MAX_TRANSFER_SLOTS,
      )
    ) {
      return undefined;
    }
    return Object.freeze({
      maxInboundMessages: limits.maxInboundMessages,
      maxQueuedBytes: limits.maxQueuedBytes,
      maxTurn: limits.maxTurn,
      maxTransferBytes: limits.maxTransferBytes,
      maxTransferSlots: limits.maxTransferSlots,
    });
  } catch {
    return undefined;
  }
};

const snapshotPhysicalMessage = (
  value: unknown,
  maximumTransfers: number,
  maximumTransferBytes: number,
  maximumQueuedBytes: number,
): PhysicalMessage | undefined => {
  try {
    if (
      !Array.isArray(value)
      || Object.getPrototypeOf(value) !== Array.prototype
      || value.length === 0
      || value.length > maximumTransfers + 1
    ) {
      return undefined;
    }
    const iterator = value.values();
    const first = iterator.next();
    if (
      first.done
      || !isFixedArrayBuffer(first.value)
      || first.value.byteLength
        > BROWSER_NATIVE_WORKER_MAX_CONTROL_BYTES
    ) {
      return undefined;
    }
    const transfers: ArrayBuffer[] = [];
    const seen = new Set<ArrayBuffer>([first.value]);
    let transferBytes = 0;
    let queuedBytes = first.value.byteLength;
    if (queuedBytes > maximumQueuedBytes) {
      return undefined;
    }
    for (
      let next = iterator.next();
      !next.done;
      next = iterator.next()
    ) {
      if (
        !isFixedArrayBuffer(next.value)
        || seen.has(next.value)
      ) {
        return undefined;
      }
      transferBytes += next.value.byteLength;
      if (
        !Number.isSafeInteger(transferBytes)
        || transferBytes > maximumTransferBytes
      ) {
        return undefined;
      }
      queuedBytes += next.value.byteLength;
      if (
        !Number.isSafeInteger(queuedBytes)
        || queuedBytes > maximumQueuedBytes
      ) {
        return undefined;
      }
      seen.add(next.value);
      transfers.push(next.value);
    }
    const control = new Uint8Array(first.value).slice();
    if (control.byteLength !== first.value.byteLength) {
      return undefined;
    }
    return Object.freeze({
      byteLength: queuedBytes,
      control,
      transfers: Object.freeze(transfers),
    });
  } catch {
    return undefined;
  }
};

const defaultFatal = (
  code: BrowserNativeWorkerEntryFaultCode,
): never => {
  throw new BrowserNativeWorkerEntryError(code);
};

/**
 * Owns exactly one Native instance inside one Dedicated Worker epoch.
 *
 * Browser callbacks perform only bounded structural snapshot/byte charging and
 * queue an admitted start/physical record or a stable fault marker. Protocol
 * interpretation, bootstrap state commitment, Native dispatch, polling, and
 * output transfer all occur in cancelable bounded actor turns.
 */
export class BrowserNativeWorkerEntryController {
  readonly #scope: BrowserNativeWorkerEntryScope;
  readonly #scheduler: BrowserNativeWorkerEntryScheduler;
  readonly #loader: BrowserNativeWorkerEntryLoader;
  readonly #limits: SnapshotLimits;
  readonly #fatal: (code: BrowserNativeWorkerEntryFaultCode) => void;
  readonly #inbound: InboundMessage[] = [];
  readonly #messageListener: EventListener;
  readonly #messageErrorListener: EventListener;

  #state: EntryState = "AwaitingStart";
  #identity: BrowserNativeWorkerSupervisorIdentity | undefined;
  #instance: BrowserNativeWorkerEntryInstance | undefined;
  #bootstrapCompletion: BootstrapCompletion | undefined;
  #queuedPhysicalBytes = 0;
  #pendingFault: BrowserNativeWorkerEntryFaultCode | undefined;
  #pollRequired = false;
  #pollPending = false;
  #scheduledHandle: number | undefined;
  #scheduledToken: object | undefined;
  #listenersAttached = false;
  #loaderClosed = false;
  #scopeClosed = false;
  #fatalDelivered = false;

  constructor(configuration: BrowserNativeWorkerEntryConfiguration) {
    const limits = snapshotLimits(configuration.limits);
    try {
      if (
        new.target !== BrowserNativeWorkerEntryController
        || limits === undefined
        || typeof configuration.scope !== "object"
        || configuration.scope === null
        || typeof configuration.scope.addEventListener !== "function"
        || typeof configuration.scope.removeEventListener !== "function"
        || typeof configuration.scope.postMessage !== "function"
        || typeof configuration.scope.close !== "function"
        || typeof configuration.scheduler !== "object"
        || configuration.scheduler === null
        || typeof configuration.scheduler.request !== "function"
        || typeof configuration.scheduler.cancel !== "function"
        || typeof configuration.loader !== "object"
        || configuration.loader === null
        || typeof configuration.loader.bootstrap !== "function"
        || typeof configuration.loader.close !== "function"
        || (
          configuration.fatal !== undefined
          && typeof configuration.fatal !== "function"
        )
      ) {
        throw new BrowserNativeWorkerEntryError(
          "InvalidConfiguration",
        );
      }
    } catch (error: unknown) {
      if (error instanceof BrowserNativeWorkerEntryError) {
        throw error;
      }
      throw new BrowserNativeWorkerEntryError("InvalidConfiguration");
    }
    this.#scope = configuration.scope;
    this.#scheduler = configuration.scheduler;
    this.#loader = configuration.loader;
    this.#limits = limits;
    this.#fatal = configuration.fatal ?? defaultFatal;
    this.#messageListener = (event: Event): void => {
      this.#queueMessage(
        event instanceof MessageEvent ? event.data : undefined,
      );
    };
    this.#messageErrorListener = (): void => {
      this.#queueFault("MessageError");
    };
    try {
      this.#scope.addEventListener(
        "message",
        this.#messageListener,
      );
      try {
        this.#scope.addEventListener(
          "messageerror",
          this.#messageErrorListener,
        );
      } catch {
        this.#scope.removeEventListener(
          "message",
          this.#messageListener,
        );
        throw new BrowserNativeWorkerEntryError(
          "InvalidConfiguration",
        );
      }
      this.#listenersAttached = true;
    } catch (error: unknown) {
      this.#closeLoader();
      if (error instanceof BrowserNativeWorkerEntryError) {
        throw error;
      }
      throw new BrowserNativeWorkerEntryError("InvalidConfiguration");
    }
  }

  get state(): EntryState {
    return this.#state;
  }

  get queueDepth(): number {
    return this.#inbound.length;
  }

  get queuedPhysicalBytes(): number {
    return this.#queuedPhysicalBytes;
  }

  get hasScheduledTurn(): boolean {
    return this.#scheduledToken !== undefined;
  }

  /** Idempotently releases this Worker realm and its loader ownership. */
  close(): void {
    if (this.#state === "Closed" || this.#state === "Failed") {
      return;
    }
    this.#state = "Closed";
    this.#release(true);
  }

  #queueMessage(value: unknown): void {
    if (this.#state === "Closed" || this.#state === "Failed") {
      return;
    }
    if (this.#inbound.length >= this.#limits.maxInboundMessages) {
      this.#pendingFault ??= "InboundQueueOverflow";
    } else {
      const start = startRecord(value);
      const physical = start === undefined
        ? snapshotPhysicalMessage(
          value,
          this.#limits.maxTransferSlots,
          this.#limits.maxTransferBytes,
          this.#limits.maxQueuedBytes - this.#queuedPhysicalBytes,
        )
        : undefined;
      if (start !== undefined) {
        this.#inbound.push(Object.freeze({
          kind: "start",
          start,
        }));
      } else if (physical !== undefined) {
        const nextQueuedBytes =
          this.#queuedPhysicalBytes + physical.byteLength;
        if (
          !Number.isSafeInteger(nextQueuedBytes)
          || nextQueuedBytes > this.#limits.maxQueuedBytes
        ) {
          this.#pendingFault ??= "InvalidBootstrap";
          this.#requestTurn();
          return;
        }
        this.#queuedPhysicalBytes = nextQueuedBytes;
        this.#inbound.push(Object.freeze({
          kind: "physical",
          physical,
        }));
      } else {
        this.#pendingFault ??= "InvalidBootstrap";
      }
    }
    this.#requestTurn();
  }

  #queueFault(code: BrowserNativeWorkerEntryFaultCode): void {
    if (this.#state === "Closed" || this.#state === "Failed") {
      return;
    }
    this.#pendingFault ??= code;
    this.#requestTurn();
  }

  #queueBootstrapCompletion(completion: BootstrapCompletion): void {
    if (this.#state === "Closed" || this.#state === "Failed") {
      return;
    }
    if (this.#bootstrapCompletion !== undefined) {
      this.#pendingFault ??= "NativeFailure";
    } else {
      this.#bootstrapCompletion = completion;
    }
    this.#requestTurn();
  }

  #requestTurn(): void {
    if (
      this.#state === "Closed"
      || this.#state === "Failed"
      || this.#scheduledToken !== undefined
    ) {
      return;
    }
    const token = {};
    this.#scheduledToken = token;
    let requested = true;
    let ranInline = false;
    let handle: number;
    try {
      handle = this.#scheduler.request((): void => {
        if (requested) {
          ranInline = true;
          return;
        }
        if (
          this.#scheduledToken !== token
          || this.#state === "Closed"
          || this.#state === "Failed"
        ) {
          return;
        }
        this.#scheduledToken = undefined;
        this.#scheduledHandle = undefined;
        this.#runTurn();
      });
    } catch {
      this.#scheduledToken = undefined;
      this.#fail("SchedulerFailure");
      return;
    } finally {
      requested = false;
    }
    if (
      ranInline
      || !Number.isSafeInteger(handle)
      || handle < 0
    ) {
      this.#scheduledToken = undefined;
      try {
        if (Number.isSafeInteger(handle) && handle >= 0) {
          this.#scheduler.cancel(handle);
        }
      } catch {
        // The stable scheduler failure remains primary.
      }
      this.#fail("SchedulerFailure");
      return;
    }
    this.#scheduledHandle = handle;
  }

  #runTurn(): void {
    if (this.#pendingFault !== undefined) {
      const fault = this.#pendingFault;
      this.#pendingFault = undefined;
      this.#fail(fault);
      return;
    }
    let work = 0;
    try {
      if (this.#bootstrapCompletion !== undefined) {
        const completion = this.#bootstrapCompletion;
        this.#bootstrapCompletion = undefined;
        if (
          this.#state !== "Loading"
          || completion.kind === "rejected"
        ) {
          this.#fail("NativeFailure");
          return;
        }
        const instance = completion.instance;
        if (
          typeof instance !== "object"
          || instance === null
          || typeof instance.accept !== "function"
          || typeof instance.dispatch !== "function"
          || typeof instance.poll !== "function"
          || instance.ready
          || instance.stopped
        ) {
          this.#fail("NativeFailure");
          return;
        }
        this.#instance = instance;
        this.#state = "BootstrapReady";
        work += 1;
      }
      if (
        work < this.#limits.maxTurn
        && this.#state === "BootstrapReady"
      ) {
        const instance = this.#instance;
        if (instance === undefined) {
          this.#fail("NativeFailure");
          return;
        }
        this.#postDispatch(instance.engineHello);
        this.#state = "AwaitingAccept";
        work += 1;
      }
      while (
        work < this.#limits.maxTurn
        && this.#inbound.length > 0
        && this.#state !== "Loading"
        && this.#state !== "Closed"
        && this.#state !== "Failed"
      ) {
        const inbound = this.#inbound.shift();
        if (inbound === undefined) {
          this.#fail("NativeFailure");
          return;
        }
        if (inbound.kind === "physical") {
          this.#queuedPhysicalBytes -= inbound.physical.byteLength;
          if (this.#queuedPhysicalBytes < 0) {
            this.#fail("NativeFailure");
            return;
          }
        }
        if (this.#state === "AwaitingStart") {
          if (inbound.kind !== "start") {
            this.#fail("InvalidBootstrap");
            return;
          }
          const start = inbound.start;
          this.#identity = Object.freeze({
            rendererEpoch: start.rendererEpoch,
            worker: start.worker,
            workerEpoch: start.workerEpoch,
          });
          this.#state = "AwaitingHello";
          work += 1;
          continue;
        }
        if (inbound.kind !== "physical") {
          this.#fail("InvalidBootstrap");
          return;
        }
        const physical = inbound.physical;
        if (this.#state === "AwaitingHello") {
          if (physical.transfers.length !== 0) {
            this.#fail("InvalidBootstrap");
            return;
          }
          this.#beginBootstrap(physical.control);
          work += 1;
          break;
        }
        const instance = this.#instance;
        if (instance === undefined) {
          this.#fail("NativeFailure");
          return;
        }
        if (this.#state === "AwaitingAccept") {
          if (physical.transfers.length !== 0) {
            this.#fail("InvalidBootstrap");
            return;
          }
          const ready = instance.accept(physical.control);
          if (!instance.ready) {
            this.#fail("NativeFailure");
            return;
          }
          this.#postDispatch(ready);
          this.#state = "Ready";
          work += 1;
          if (instance.stopped) {
            this.#complete();
            return;
          }
          continue;
        }
        if (this.#state !== "Ready") {
          this.#fail("InvalidBootstrap");
          return;
        }
        const output = instance.dispatch(
          physical.control,
          physical.transfers,
        );
        if (output !== undefined) {
          this.#postDispatch(output);
        }
        this.#pollRequired = true;
        work += 1;
        if (instance.stopped) {
          this.#complete();
          return;
        }
      }
      while (
        work < this.#limits.maxTurn
        && this.#state === "Ready"
        && (this.#pollRequired || this.#pollPending)
      ) {
        const instance = this.#instance;
        if (instance === undefined) {
          this.#fail("NativeFailure");
          return;
        }
        this.#pollRequired = false;
        const result = instance.poll();
        this.#pollPending = result.pending;
        if (result.output !== undefined) {
          this.#postDispatch(result.output);
        }
        work += 1;
        if (instance.stopped) {
          this.#complete();
          return;
        }
      }
    } catch (error: unknown) {
      if (this.#state === "Failed") {
        throw error;
      }
      this.#fail(
        error instanceof BrowserNativeWorkerEntryError
          ? error.code
          : "NativeFailure",
      );
      return;
    }
    if (
      this.#pendingFault !== undefined
      || this.#bootstrapCompletion !== undefined
      || this.#state === "BootstrapReady"
      || (
        this.#state !== "Loading"
        && this.#inbound.length > 0
      )
      || this.#pollRequired
      || this.#pollPending
    ) {
      this.#requestTurn();
    }
  }

  #beginBootstrap(hostHello: Uint8Array<ArrayBuffer>): void {
    const identity = this.#identity;
    if (identity === undefined || this.#state !== "AwaitingHello") {
      this.#fail("InvalidBootstrap");
      return;
    }
    const fixedHello = hostHello.slice();
    this.#state = "Loading";
    let loading: Promise<BrowserNativeWorkerEntryInstance>;
    try {
      loading = this.#loader.bootstrap(fixedHello, identity);
    } catch {
      this.#queueFault("NativeFailure");
      return;
    }
    void loading.then(
      (instance): void => {
        this.#queueBootstrapCompletion(Object.freeze({
          kind: "fulfilled",
          instance,
        }));
      },
      (): void => {
        this.#queueBootstrapCompletion(Object.freeze({
          kind: "rejected",
        }));
      },
    );
  }

  #postDispatch(dispatch: BrowserNativeWorkerDispatch): void {
    const frame = dispatch.frame;
    if (
      !(frame instanceof Uint8Array)
      || frame.byteOffset !== 0
      || frame.byteLength !== frame.buffer.byteLength
      || frame.byteLength > BROWSER_NATIVE_WORKER_MAX_CONTROL_BYTES
      || !isFixedArrayBuffer(frame.buffer)
      || !Array.isArray(dispatch.transfers)
      || dispatch.transfers.length > this.#limits.maxTransferSlots
    ) {
      throw new BrowserNativeWorkerEntryError("NativeFailure");
    }
    const resources: ArrayBuffer[] = [];
    const seen = new Set<ArrayBuffer>([frame.buffer]);
    let transferBytes = 0;
    for (const transfer of dispatch.transfers) {
      if (
        !isFixedArrayBuffer(transfer)
        || seen.has(transfer)
      ) {
        throw new BrowserNativeWorkerEntryError("NativeFailure");
      }
      transferBytes += transfer.byteLength;
      if (
        !Number.isSafeInteger(transferBytes)
        || transferBytes > this.#limits.maxTransferBytes
      ) {
        throw new BrowserNativeWorkerEntryError("NativeFailure");
      }
      seen.add(transfer);
      resources.push(transfer);
    }
    const value: unknown[] = [frame.buffer, ...resources];
    const transfer: ArrayBuffer[] = [frame.buffer, ...resources];
    try {
      this.#scope.postMessage(value, transfer);
    } catch {
      throw new BrowserNativeWorkerEntryError(
        "OutboundTransportFailure",
      );
    }
  }

  #complete(): void {
    if (this.#state === "Closed" || this.#state === "Failed") {
      return;
    }
    this.#state = "Closed";
    this.#release(true);
  }

  #fail(code: BrowserNativeWorkerEntryFaultCode): void {
    if (this.#state === "Closed" || this.#state === "Failed") {
      return;
    }
    this.#state = "Failed";
    this.#release(false);
    if (this.#fatalDelivered) {
      return;
    }
    this.#fatalDelivered = true;
    try {
      this.#fatal(code);
    } finally {
      this.#closeScope();
    }
  }

  #release(closeScope: boolean): void {
    this.#inbound.length = 0;
    this.#queuedPhysicalBytes = 0;
    this.#bootstrapCompletion = undefined;
    this.#pendingFault = undefined;
    this.#pollRequired = false;
    this.#pollPending = false;
    const scheduledHandle = this.#scheduledHandle;
    this.#scheduledHandle = undefined;
    this.#scheduledToken = undefined;
    if (scheduledHandle !== undefined) {
      try {
        this.#scheduler.cancel(scheduledHandle);
      } catch {
        // Ownership is already invalidated.
      }
    }
    if (this.#listenersAttached) {
      this.#listenersAttached = false;
      try {
        this.#scope.removeEventListener(
          "message",
          this.#messageListener,
        );
      } catch {
        // Teardown continues through independent owners.
      }
      try {
        this.#scope.removeEventListener(
          "messageerror",
          this.#messageErrorListener,
        );
      } catch {
        // Teardown continues through independent owners.
      }
    }
    this.#instance = undefined;
    this.#closeLoader();
    if (closeScope) {
      this.#closeScope();
    }
  }

  #closeScope(): void {
    if (this.#scopeClosed) {
      return;
    }
    this.#scopeClosed = true;
    try {
      this.#scope.close();
    } catch {
      // The controller remains terminal even if the realm rejects close.
    }
  }

  #closeLoader(): void {
    if (this.#loaderClosed) {
      return;
    }
    this.#loaderClosed = true;
    try {
      this.#loader.close();
    } catch {
      // Loader ownership is terminal regardless of best-effort shutdown.
    }
  }
}

/**
 * Installs the production Worker realm with the real loader and a cancelable
 * zero-delay task scheduler. The final bundle supplies only its generated,
 * hash-bound artifact record and the DedicatedWorkerGlobalScope adapter.
 */
export function installBrowserNativeWorkerEntry(
  configuration: BrowserNativeWorkerInstallationConfiguration,
): BrowserNativeWorkerEntryController {
  let request: (callback: () => void, milliseconds: number) => number;
  let cancel: (handle: number) => void;
  try {
    if (
      typeof configuration.scope !== "object"
      || configuration.scope === null
      || typeof configuration.scope.addEventListener !== "function"
      || typeof configuration.scope.removeEventListener !== "function"
      || typeof configuration.scope.postMessage !== "function"
      || typeof configuration.scope.close !== "function"
      || typeof configuration.scope.setTimeout !== "function"
      || typeof configuration.scope.clearTimeout !== "function"
      || snapshotLimits(configuration.limits) === undefined
      || (
        configuration.fatal !== undefined
        && typeof configuration.fatal !== "function"
      )
    ) {
      throw new BrowserNativeWorkerEntryError(
        "InvalidConfiguration",
      );
    }
    request = configuration.scope.setTimeout.bind(
      configuration.scope,
    );
    cancel = configuration.scope.clearTimeout.bind(
      configuration.scope,
    );
  } catch (error: unknown) {
    if (error instanceof BrowserNativeWorkerEntryError) {
      throw error;
    }
    throw new BrowserNativeWorkerEntryError("InvalidConfiguration");
  }
  const loader = configuration.runtime === undefined
    ? new BrowserNativeWorkerLoader(configuration.artifact)
    : new BrowserNativeWorkerLoader(
      configuration.artifact,
      configuration.runtime,
    );
  const scheduler: BrowserNativeWorkerEntryScheduler = Object.freeze({
    request: (callback: () => void): number =>
      request(callback, 0),
    cancel,
  });
  const base = {
    scope: configuration.scope,
    scheduler,
    loader,
    limits: configuration.limits,
  };
  return configuration.fatal === undefined
    ? new BrowserNativeWorkerEntryController(base)
    : new BrowserNativeWorkerEntryController({
      ...base,
      fatal: configuration.fatal,
    });
}

/**
 * Type-level proof that the real loader and instance satisfy the entry surface.
 * These assignments emit no JavaScript and prevent adapter drift.
 */
type LoaderCompatibility = BrowserNativeWorkerLoader extends
  BrowserNativeWorkerEntryLoader ? true : never;
type InstanceCompatibility = BrowserNativeWorkerInstance extends
  BrowserNativeWorkerEntryInstance ? true : never;
const LOADER_COMPATIBILITY: LoaderCompatibility = true;
const INSTANCE_COMPATIBILITY: InstanceCompatibility = true;
void LOADER_COMPATIBILITY;
void INSTANCE_COMPATIBILITY;
