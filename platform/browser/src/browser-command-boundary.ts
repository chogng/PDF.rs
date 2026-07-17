import type {
  CommandEnvelope,
  CompatibleHandshake,
  EnvelopeHeader,
} from "../generated/engine-protocol.js";
import {
  DataAttachmentRole,
  ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER,
  MAX_MESSAGE_BYTES,
  MAX_TRANSFER_SLOTS,
  PROTOCOL_MAJOR,
  PROTOCOL_MINOR,
  beginValidateCommandEnvelope,
  decodeCommandPayload,
  EnvelopeSequenceTracker,
  isCompatibleHandshake,
  validateEnvelopeHeader,
  validateWorkerId,
} from "../generated/engine-protocol.js";
import {
  isBrowserCommandAdmission,
  validateBrowserCommandAdmission,
  type BrowserCommandAdmission,
  type BrowserCommandAdmissionErrorCode,
} from "./browser-command-admission.js";

/** Bytes occupied by the canonical little-endian control header. */
export const BROWSER_CONTROL_HEADER_BYTES = 20 as const;

/**
 * A command accepted by the browser ingress boundary.
 *
 * `resources[n]` is logical protocol slot `n`; physical resource-table index
 * zero (the control frame) is deliberately not exposed as a logical resource.
 */
export interface ValidatedBrowserCommand {
  readonly envelope: CommandEnvelope;
  readonly resources: readonly ArrayBuffer[];
}

/** Stable, content-free browser-boundary rejection categories. */
export type BrowserBoundaryErrorCode =
  | "InvalidConfiguration"
  | "InvalidResourceTable"
  | "InvalidControlResource"
  | "InvalidHeader"
  | "InvalidPayloadLength"
  | "InvalidPayload"
  | "InvalidEnvelope"
  | "NonMonotonicSequence"
  | "StaleWorker"
  | BrowserCommandAdmissionErrorCode
  | "InvalidResourceBinding"
  | "InvalidResourceType"
  | "InvalidResourceLength"
  | "MissingCapability";

/**
 * An ingress error whose message never contains payload, identity, or codec
 * details. Callers may safely report `code` as a stable diagnostic category.
 */
export class BrowserBoundaryError extends Error {
  readonly code: BrowserBoundaryErrorCode;

  constructor(code: BrowserBoundaryErrorCode) {
    super(code);
    this.name = "BrowserBoundaryError";
    this.code = code;
  }
}

const hasExactArrayShape = (value: unknown): value is unknown[] => {
  try {
    if (
      !Array.isArray(value)
      || Object.getPrototypeOf(value) !== Array.prototype
    ) {
      return false;
    }
    if (value.length === 0 || value.length > MAX_TRANSFER_SLOTS + 1) {
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
    throw new BrowserBoundaryError("InvalidResourceTable");
  }

  const result: unknown[] = [];
  try {
    for (let index = 0; index < value.length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (
        descriptor === undefined
        || !Object.prototype.hasOwnProperty.call(descriptor, "value")
      ) {
        throw new BrowserBoundaryError("InvalidResourceTable");
      }
      result.push(descriptor.value);
    }
  } catch (error: unknown) {
    if (error instanceof BrowserBoundaryError) {
      throw error;
    }
    throw new BrowserBoundaryError("InvalidResourceTable");
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

const parseControlHeader = (
  control: ArrayBuffer,
  connection: CompatibleHandshake,
): EnvelopeHeader => {
  if (control.byteLength < BROWSER_CONTROL_HEADER_BYTES) {
    throw new BrowserBoundaryError("InvalidPayloadLength");
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
    throw new BrowserBoundaryError("InvalidControlResource");
  }

  const actualPayloadBytes =
    control.byteLength - BROWSER_CONTROL_HEADER_BYTES;
  if (
    header.payload_len !== actualPayloadBytes
    || actualPayloadBytes > MAX_MESSAGE_BYTES
    || actualPayloadBytes > connection.max_message_bytes
  ) {
    throw new BrowserBoundaryError("InvalidPayloadLength");
  }
  if (
    header.major !== PROTOCOL_MAJOR
    || header.minor !== PROTOCOL_MINOR
    || header.minor !== connection.minor
    || !validateEnvelopeHeader(header)
  ) {
    throw new BrowserBoundaryError("InvalidHeader");
  }
  return header;
};

const validateProvideDataResources = (
  envelope: CommandEnvelope,
  control: ArrayBuffer,
  resources: readonly unknown[],
  connection: CompatibleHandshake,
): readonly ArrayBuffer[] => {
  if (envelope.command.type !== "ProvideData") {
    if (resources.length !== 0) {
      throw new BrowserBoundaryError("InvalidResourceBinding");
    }
    return Object.freeze([]);
  }

  const segments = envelope.command.payload.segments;
  if (
    (
      connection.capabilities
      & ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER
    ) === 0n
  ) {
    throw new BrowserBoundaryError("MissingCapability");
  }
  if (resources.length !== segments.length) {
    throw new BrowserBoundaryError("InvalidResourceBinding");
  }

  const adopted: ArrayBuffer[] = [];
  const identities = new Set<ArrayBuffer>([control]);
  for (let index = 0; index < segments.length; index += 1) {
    const segment = segments[index];
    const resource = resources[index];
    if (
      segment === undefined
      || segment.slot !== index
      || segment.role !== DataAttachmentRole.ImmutableRangeBytes
    ) {
      throw new BrowserBoundaryError("InvalidResourceBinding");
    }
    if (!isFixedArrayBuffer(resource)) {
      throw new BrowserBoundaryError("InvalidResourceType");
    }
    if (identities.has(resource)) {
      throw new BrowserBoundaryError("InvalidResourceBinding");
    }
    identities.add(resource);
    if (BigInt(resource.byteLength) !== segment.byte_length) {
      throw new BrowserBoundaryError("InvalidResourceLength");
    }
    adopted.push(resource);
  }
  return Object.freeze(adopted);
};

/**
 * Transactional browser command ingress.
 *
 * The input is the physical resource table presented to the command receiver.
 * Index zero must be the canonical binary control `ArrayBuffer`; logical slot
 * `n` is physical index `n + 1`. Transfer-list provenance is deliberately not
 * inferred at this receiver-side boundary. The receive-direction sequence is
 * committed only after framing, codec, generated envelope, Worker, lifecycle,
 * correlation, and resource checks have all succeeded.
 */
export class BrowserCommandBoundary {
  readonly #expectedWorker: bigint;
  readonly #connection: CompatibleHandshake;
  readonly #admission: BrowserCommandAdmission;
  readonly #sequence = new EnvelopeSequenceTracker();

  constructor(
    expectedWorker: bigint,
    connection: CompatibleHandshake,
    admission: BrowserCommandAdmission,
  ) {
    if (
      new.target !== BrowserCommandBoundary
      || !validateWorkerId(expectedWorker)
      || expectedWorker === 0n
      || !isCompatibleHandshake(connection)
      || connection.minor !== PROTOCOL_MINOR
      || !Number.isInteger(connection.max_message_bytes)
      || connection.max_message_bytes <= 0
      || connection.max_message_bytes > MAX_MESSAGE_BYTES
      || !Number.isInteger(connection.max_transfer_slots)
      || connection.max_transfer_slots <= 0
      || connection.max_transfer_slots > MAX_TRANSFER_SLOTS
      || typeof connection.capabilities !== "bigint"
      || !isBrowserCommandAdmission(admission)
    ) {
      throw new BrowserBoundaryError("InvalidConfiguration");
    }
    this.#expectedWorker = expectedWorker;
    this.#connection = connection;
    this.#admission = admission;
    Object.freeze(this);
  }

  /** Most recently accepted receive-direction sequence, if any. */
  get lastAcceptedSequence(): bigint | undefined {
    return this.#sequence.lastAccepted;
  }

  /** Validates one physical command resource table and commits its sequence. */
  decode(value: unknown): ValidatedBrowserCommand {
    const physicalResources = exactArrayValues(value);
    const control = physicalResources[0];
    if (!isFixedArrayBuffer(control)) {
      throw new BrowserBoundaryError("InvalidControlResource");
    }

    const header = parseControlHeader(control, this.#connection);
    if (
      this.#sequence.lastAccepted !== undefined
      && header.sequence <= this.#sequence.lastAccepted
    ) {
      throw new BrowserBoundaryError("NonMonotonicSequence");
    }

    const payload = new Uint8Array(
      control,
      BROWSER_CONTROL_HEADER_BYTES,
      header.payload_len,
    );
    const decoded = decodeCommandPayload(header, payload);
    if (!decoded.ok) {
      throw new BrowserBoundaryError("InvalidPayload");
    }

    const logicalResources = physicalResources.slice(1);
    const expectedLogicalResources =
      decoded.value.command.type === "ProvideData"
        ? decoded.value.command.payload.segments.length
        : 0;
    if (logicalResources.length !== expectedLogicalResources) {
      throw new BrowserBoundaryError("InvalidResourceBinding");
    }
    const pending = beginValidateCommandEnvelope(
      decoded.value,
      logicalResources.length,
      header.payload_len,
      this.#connection,
      this.#sequence,
    );
    if (pending === undefined) {
      throw new BrowserBoundaryError("InvalidEnvelope");
    }
    if (pending.envelope.correlation.worker !== this.#expectedWorker) {
      throw new BrowserBoundaryError("StaleWorker");
    }

    const admissionError = validateBrowserCommandAdmission(
      this.#admission,
      pending.envelope,
    );
    if (admissionError !== undefined) {
      throw new BrowserBoundaryError(admissionError);
    }
    const resources = validateProvideDataResources(
      pending.envelope,
      control,
      logicalResources,
      this.#connection,
    );
    const accepted = Object.freeze({
      envelope: pending.envelope,
      resources,
    });
    if (!pending.commitSequence()) {
      throw new BrowserBoundaryError("NonMonotonicSequence");
    }
    return accepted;
  }
}

Object.freeze(BrowserCommandBoundary.prototype);
