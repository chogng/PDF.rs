import type {
  CompatibleHandshake,
  EnvelopeHeader,
  EventEnvelope,
} from "../generated/engine-protocol.js";
import {
  MAX_MESSAGE_BYTES,
  MAX_TRANSFER_SLOTS,
  MESSAGE_ID_ENGINE_HELLO,
  PROTOCOL_MAJOR,
  PROTOCOL_MINOR,
  beginValidateEventEnvelope,
  decodeEventPayload,
  EnvelopeSequenceTracker,
  isCompatibleHandshake,
  validateCorrelation,
  validateEngineHelloEvent,
  validateEnvelopeHeader,
  validateWorkerId,
} from "../generated/engine-protocol.js";
import {
  BROWSER_CONTROL_HEADER_BYTES,
} from "./browser-command-boundary.js";

/** Stable, content-free event-boundary rejection categories. */
export type BrowserEventBoundaryErrorCode =
  | "InvalidConfiguration"
  | "InvalidResourceTable"
  | "InvalidControlResource"
  | "InvalidHeader"
  | "InvalidPayloadLength"
  | "InvalidPayload"
  | "InvalidEnvelope"
  | "NonMonotonicSequence"
  | "StaleWorker"
  | "InvalidLifecycle";

/** Reports an event ingress failure without formatting untrusted content. */
export class BrowserEventBoundaryError extends Error {
  readonly code: BrowserEventBoundaryErrorCode;

  constructor(code: BrowserEventBoundaryErrorCode) {
    super(code);
    this.name = "BrowserEventBoundaryError";
    this.code = code;
  }
}

/** A fully decoded event whose OOB resources remain unadopted. */
export interface ValidatedBrowserEvent {
  readonly envelope: EventEnvelope;
  /**
   * Logical resource slots in wire order.
   *
   * M5-02 deliberately treats each value as opaque. M5-05 owns browser-object
   * type, extent, fence, transfer, release, and presentation validation.
   */
  readonly resources: readonly unknown[];
}

/** Read-only lifecycle validation performed before sequence commit. */
export type BrowserEventLifecycleValidator = (
  envelope: EventEnvelope,
) => BrowserEventBoundaryErrorCode | undefined;

/**
 * Performs host-specific EngineHello negotiation checks before receive
 * sequence ownership is committed.
 */
export type BrowserEngineHelloValidator = (
  envelope: EventEnvelope,
) => boolean;

const SEQUENCE_TRACKER_PROTOTYPE = EnvelopeSequenceTracker.prototype;
const ORIGINAL_SEQUENCE_PENDING =
  EnvelopeSequenceTracker.prototype.pending;
const ORIGINAL_SEQUENCE_LAST_ACCEPTED =
  Object.getOwnPropertyDescriptor(
    EnvelopeSequenceTracker.prototype,
    "lastAccepted",
  )?.get;

const isAuthenticSequenceTracker = (
  value: unknown,
): value is EnvelopeSequenceTracker => {
  if (
    typeof value !== "object"
    || value === null
    || ORIGINAL_SEQUENCE_LAST_ACCEPTED === undefined
  ) {
    return false;
  }
  try {
    if (Object.getPrototypeOf(value) !== SEQUENCE_TRACKER_PROTOTYPE) {
      return false;
    }
    const lastAccepted = Reflect.apply(
      ORIGINAL_SEQUENCE_LAST_ACCEPTED,
      value,
      [],
    ) as bigint | undefined;
    return (
      (lastAccepted === undefined || typeof lastAccepted === "bigint")
      && Reflect.apply(
        ORIGINAL_SEQUENCE_PENDING,
        value,
        [0n],
      ) === undefined
    );
  } catch {
    return false;
  }
};

const hasExactArrayShape = (value: unknown): value is unknown[] => {
  try {
    if (
      !Array.isArray(value)
      || Object.getPrototypeOf(value) !== Array.prototype
      || value.length === 0
      || value.length > MAX_TRANSFER_SLOTS + 1
    ) {
      return false;
    }
    const keys = Reflect.ownKeys(value);
    if (keys.length !== value.length + 1 || !keys.includes("length")) {
      return false;
    }
    for (let index = 0; index < value.length; index += 1) {
      const key = String(index);
      if (!keys.includes(key)) {
        return false;
      }
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (
        descriptor === undefined
        || !Object.prototype.hasOwnProperty.call(descriptor, "value")
      ) {
        return false;
      }
    }
    return true;
  } catch {
    return false;
  }
};

const exactArrayValues = (value: unknown): readonly unknown[] => {
  if (!hasExactArrayShape(value)) {
    throw new BrowserEventBoundaryError("InvalidResourceTable");
  }
  const result: unknown[] = [];
  try {
    for (let index = 0; index < value.length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (
        descriptor === undefined
        || !Object.prototype.hasOwnProperty.call(descriptor, "value")
      ) {
        throw new BrowserEventBoundaryError("InvalidResourceTable");
      }
      result.push(descriptor.value);
    }
  } catch (error: unknown) {
    if (error instanceof BrowserEventBoundaryError) {
      throw error;
    }
    throw new BrowserEventBoundaryError("InvalidResourceTable");
  }
  return result;
};

const isFixedArrayBuffer = (value: unknown): value is ArrayBuffer => {
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

interface ParsedControl {
  readonly control: ArrayBuffer;
  readonly header: EnvelopeHeader;
  readonly payload: Uint8Array;
  readonly logicalResources: readonly unknown[];
}

const parseControl = (
  value: unknown,
  maximumMessageBytes: number,
): ParsedControl => {
  const physicalResources = exactArrayValues(value);
  const control = physicalResources[0];
  if (!isFixedArrayBuffer(control)) {
    throw new BrowserEventBoundaryError("InvalidControlResource");
  }
  if (control.byteLength < BROWSER_CONTROL_HEADER_BYTES) {
    throw new BrowserEventBoundaryError("InvalidPayloadLength");
  }

  let header: EnvelopeHeader;
  try {
    const view = new DataView(control);
    header = Object.freeze({
      major: view.getUint16(0, true),
      minor: view.getUint16(2, true),
      message_type: view.getUint16(4, true),
      flags: view.getUint16(6, true),
      payload_len: view.getUint32(8, true),
      sequence: view.getBigUint64(12, true),
    });
  } catch {
    throw new BrowserEventBoundaryError("InvalidControlResource");
  }

  const actualPayloadBytes =
    control.byteLength - BROWSER_CONTROL_HEADER_BYTES;
  if (
    header.payload_len !== actualPayloadBytes
    || actualPayloadBytes > MAX_MESSAGE_BYTES
    || actualPayloadBytes > maximumMessageBytes
  ) {
    throw new BrowserEventBoundaryError("InvalidPayloadLength");
  }
  if (
    header.major !== PROTOCOL_MAJOR
    || header.minor !== PROTOCOL_MINOR
    || !validateEnvelopeHeader(header)
  ) {
    throw new BrowserEventBoundaryError("InvalidHeader");
  }
  return Object.freeze({
    control,
    header,
    payload: new Uint8Array(
      control,
      BROWSER_CONTROL_HEADER_BYTES,
      header.payload_len,
    ),
    logicalResources: Object.freeze(physicalResources.slice(1)),
  });
};

/**
 * Decodes the one pre-negotiation EngineHello event.
 *
 * No browser resource is permitted or inspected. The receive sequence commits
 * only after the exact event, Worker epoch, and correlation shape validate.
 */
export function decodeBrowserEngineHello(
  value: unknown,
  expectedWorker: bigint,
  sequence: EnvelopeSequenceTracker,
  validateHandshake: BrowserEngineHelloValidator,
): EventEnvelope {
  if (
    !validateWorkerId(expectedWorker)
    || expectedWorker === 0n
    || !isAuthenticSequenceTracker(sequence)
    || typeof validateHandshake !== "function"
  ) {
    throw new BrowserEventBoundaryError("InvalidConfiguration");
  }
  const parsed = parseControl(value, MAX_MESSAGE_BYTES);
  if (
    parsed.logicalResources.length !== 0
    || parsed.header.message_type !== MESSAGE_ID_ENGINE_HELLO
  ) {
    throw new BrowserEventBoundaryError("InvalidEnvelope");
  }
  let pendingSequence: ReturnType<EnvelopeSequenceTracker["pending"]>;
  try {
    pendingSequence = Reflect.apply(
      ORIGINAL_SEQUENCE_PENDING,
      sequence,
      [parsed.header.sequence],
    ) as ReturnType<EnvelopeSequenceTracker["pending"]>;
  } catch {
    throw new BrowserEventBoundaryError("InvalidConfiguration");
  }
  if (pendingSequence === undefined) {
    throw new BrowserEventBoundaryError("NonMonotonicSequence");
  }
  const decoded = decodeEventPayload(parsed.header, parsed.payload);
  if (
    !decoded.ok
    || decoded.value.event.type !== "EngineHello"
    || !validateEngineHelloEvent(decoded.value.event.payload)
    || !validateCorrelation(decoded.value.correlation)
    || decoded.value.correlation.worker !== expectedWorker
    || decoded.value.correlation.session !== undefined
    || decoded.value.correlation.request !== undefined
    || decoded.value.correlation.generation !== undefined
  ) {
    throw new BrowserEventBoundaryError("InvalidEnvelope");
  }
  let handshakeAccepted = false;
  try {
    handshakeAccepted = validateHandshake(decoded.value);
  } catch {
    handshakeAccepted = false;
  }
  if (!handshakeAccepted) {
    throw new BrowserEventBoundaryError("InvalidLifecycle");
  }
  if (!pendingSequence.commit()) {
    throw new BrowserEventBoundaryError("NonMonotonicSequence");
  }
  return decoded.value;
}

/**
 * Transactional event ingress for the negotiated Host side.
 *
 * OOB values are counted but deliberately remain opaque until M5-05. Generated
 * framing, codec, correlation, capability, lifecycle, and sequence validation
 * all complete before the event is returned.
 */
export class BrowserEventBoundary {
  readonly #expectedWorker: bigint;
  readonly #connection: CompatibleHandshake;
  readonly #validateLifecycle: BrowserEventLifecycleValidator;
  readonly #sequence: EnvelopeSequenceTracker;

  constructor(
    expectedWorker: bigint,
    connection: CompatibleHandshake,
    sequence: EnvelopeSequenceTracker,
    validateLifecycle: BrowserEventLifecycleValidator,
  ) {
    if (
      new.target !== BrowserEventBoundary
      || !validateWorkerId(expectedWorker)
      || expectedWorker === 0n
      || !isCompatibleHandshake(connection)
      || !isAuthenticSequenceTracker(sequence)
      || typeof validateLifecycle !== "function"
    ) {
      throw new BrowserEventBoundaryError("InvalidConfiguration");
    }
    this.#expectedWorker = expectedWorker;
    this.#connection = connection;
    this.#sequence = sequence;
    this.#validateLifecycle = validateLifecycle;
    Object.freeze(this);
  }

  /** Most recently committed receive-direction sequence, if any. */
  get lastAcceptedSequence(): bigint | undefined {
    return this.#sequence.lastAccepted;
  }

  /** Decodes one physical resource table and commits its sequence last. */
  decode(value: unknown): ValidatedBrowserEvent {
    const parsed = parseControl(
      value,
      this.#connection.max_message_bytes,
    );
    if (
      parsed.logicalResources.length > this.#connection.max_transfer_slots
    ) {
      throw new BrowserEventBoundaryError("InvalidResourceTable");
    }
    const decoded = decodeEventPayload(parsed.header, parsed.payload);
    if (!decoded.ok) {
      throw new BrowserEventBoundaryError("InvalidPayload");
    }
    const pending = beginValidateEventEnvelope(
      decoded.value,
      parsed.logicalResources.length,
      parsed.header.payload_len,
      this.#connection,
      this.#sequence,
    );
    if (pending === undefined) {
      if (
        this.#sequence.lastAccepted !== undefined
        && parsed.header.sequence <= this.#sequence.lastAccepted
      ) {
        throw new BrowserEventBoundaryError("NonMonotonicSequence");
      }
      throw new BrowserEventBoundaryError("InvalidEnvelope");
    }
    if (pending.envelope.correlation.worker !== this.#expectedWorker) {
      throw new BrowserEventBoundaryError("StaleWorker");
    }
    let lifecycleError: BrowserEventBoundaryErrorCode | undefined;
    try {
      lifecycleError = this.#validateLifecycle(pending.envelope);
    } catch {
      throw new BrowserEventBoundaryError("InvalidLifecycle");
    }
    if (lifecycleError !== undefined) {
      throw new BrowserEventBoundaryError(lifecycleError);
    }
    const accepted = Object.freeze({
      envelope: pending.envelope,
      resources: parsed.logicalResources,
    });
    if (!pending.commitSequence()) {
      throw new BrowserEventBoundaryError("NonMonotonicSequence");
    }
    return accepted;
  }
}

Object.freeze(BrowserEventBoundary.prototype);
