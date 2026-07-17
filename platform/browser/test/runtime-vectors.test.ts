import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  AlphaMode,
  CollectionCompleteness,
  DataAttachmentRole,
  EndpointRole,
  KNOWN_ENDPOINT_CAPABILITIES,
  MAX_MESSAGE_BYTES,
  MAX_TRANSFER_SLOTS,
  MESSAGE_ID_HELLO,
  MESSAGE_ID_PROVIDE_DATA,
  MIN_COMPATIBLE_MINOR,
  NativeBackend,
  PixelFormat,
  PROTOCOL_MAJOR,
  PROTOCOL_MINOR,
  PROTOCOL_GENERATOR_VERSION,
  SCHEMA_HASH_HEX,
  SCHEMA_SHA256_HEX,
  SupportStatus,
  SurfaceCoordinateSpace,
  descriptorById,
  encodeCommandPayload,
  encodeCorrelationPayload,
  encodeHelloCommandPayload,
  encodeProvideDataCommandPayload,
  negotiateHandshake,
  validateCapabilityDecision,
  validateDataSegment,
  validateEndpointCapabilities,
  validateEnvelopeHeader,
  validateProvideDataCommand,
  validateProvideDataTransferLengths,
  validateSurfaceReadyEvent,
  validateSurfaceReclaimedEvent,
  type Command,
  type CommandEnvelope,
  type CompatibleHandshake,
  type Correlation,
  type PayloadCodecResult,
  type ProtocolHello,
} from "../generated/engine-protocol.js";
import {
  BROWSER_CONTROL_HEADER_BYTES,
  BrowserBoundaryError,
  BrowserCommandBoundary,
  type BrowserBoundaryErrorCode,
} from "../src/browser-command-boundary.js";
import {
  BrowserCommandAdmission,
} from "../src/browser-command-admission.js";
import {
  BrowserHandshakeError,
  type BrowserHandshakeErrorCode,
  negotiateBrowserHello,
} from "../src/browser-handshake.js";

interface VectorFile {
  readonly generator_version: string;
  readonly schema_sha256: string;
  readonly wire_schema_hash?: string;
  readonly minimum_compatible_minor?: number;
  readonly vectors: readonly unknown[];
}

const readVectorFile = async (name: string): Promise<VectorFile> => {
  const location = new URL(
    `../../../../protocol/generated/${name}`,
    import.meta.url,
  );
  const parsed: unknown = JSON.parse(await readFile(location, "utf8"));
  assert.ok(parsed !== null && typeof parsed === "object");

  const candidate = parsed as Partial<VectorFile>;
  assert.equal(typeof candidate.generator_version, "string");
  assert.equal(typeof candidate.schema_sha256, "string");
  assert.ok(Array.isArray(candidate.vectors));
  return candidate as VectorFile;
};

interface CompatibilityVector {
  readonly name: string;
  readonly local_major: number;
  readonly local_minor: number;
  readonly peer_major: number;
  readonly peer_minor: number;
  readonly peer_schema_hash: string;
  readonly peer_supported: string;
  readonly peer_mandatory: string;
  readonly expected?: string;
  readonly expected_error?: string;
}

const compatibilityVector = (value: unknown): CompatibilityVector => {
  if (value === null || typeof value !== "object") {
    throw new TypeError("compatibility vector must be an object");
  }
  const candidate = value as Partial<CompatibilityVector>;
  for (const field of [
    "name",
    "peer_schema_hash",
    "peer_supported",
    "peer_mandatory",
  ] as const) {
    if (typeof candidate[field] !== "string") {
      throw new TypeError(`compatibility vector ${field} must be a string`);
    }
  }
  for (const field of [
    "local_major",
    "local_minor",
    "peer_major",
    "peer_minor",
  ] as const) {
    if (typeof candidate[field] !== "number") {
      throw new TypeError(`compatibility vector ${field} must be a number`);
    }
  }
  if (
    (typeof candidate.expected === "string") ===
    (typeof candidate.expected_error === "string")
  ) {
    throw new TypeError(
      "compatibility vector must declare exactly one expected outcome",
    );
  }
  return candidate as CompatibilityVector;
};

const bytesFromHex = (value: string): Uint8Array => {
  if (!/^(?:[0-9a-f]{2})+$/u.test(value)) {
    throw new TypeError("hex value must contain complete lowercase bytes");
  }
  return Uint8Array.from(
    value.match(/.{2}/gu)?.map((byte) => Number.parseInt(byte, 16)) ?? [],
  );
};

const u64FromHex = (value: string): bigint => {
  if (!/^0x[0-9a-f]+$/u.test(value)) {
    throw new TypeError("capability value must be lowercase hexadecimal");
  }
  return BigInt(value);
};

type InvalidVector = Readonly<Record<string, unknown>> & {
  readonly name: string;
  readonly expected_error: string;
};

const invalidVector = (value: unknown): InvalidVector => {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("invalid vector must be an object");
  }
  const candidate = value as Readonly<Record<string, unknown>>;
  if (
    typeof candidate.name !== "string" ||
    typeof candidate.expected_error !== "string"
  ) {
    throw new TypeError("invalid vector outcome fields must be strings");
  }
  return candidate as InvalidVector;
};

const vectorString = (vector: InvalidVector, field: string): string => {
  const value = vector[field];
  if (typeof value !== "string") {
    throw new TypeError(`${vector.name}: ${field} must be a string`);
  }
  return value;
};

const vectorNumber = (vector: InvalidVector, field: string): number => {
  const value = vector[field];
  if (typeof value !== "number" || !Number.isSafeInteger(value)) {
    throw new TypeError(`${vector.name}: ${field} must be an integer`);
  }
  return value;
};

const vectorBigInt = (vector: InvalidVector, field: string): bigint => {
  const value = vectorString(vector, field);
  if (!/^(?:0|[1-9][0-9]*)$/u.test(value)) {
    throw new TypeError(`${vector.name}: ${field} must be decimal u64 text`);
  }
  return BigInt(value);
};

const vectorNumbers = (
  vector: InvalidVector,
  field: string,
): readonly number[] => {
  const value = vector[field];
  if (
    !Array.isArray(value) ||
    !value.every((entry) => Number.isSafeInteger(entry))
  ) {
    throw new TypeError(`${vector.name}: ${field} must be an integer array`);
  }
  return value as number[];
};

const vectorRecord = (
  vector: InvalidVector,
  field: string,
): Readonly<Record<string, unknown>> => {
  const value = vector[field];
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError(`${vector.name}: ${field} must be an object`);
  }
  return value as Readonly<Record<string, unknown>>;
};

interface ParsedWireHeader {
  readonly major: number;
  readonly minor: number;
  readonly message_type: number;
  readonly flags: number;
  readonly payload_len: number;
  readonly sequence: bigint;
}

const wireBytes = (vector: InvalidVector): Uint8Array =>
  bytesFromHex(vectorString(vector, "frame_hex"));

const parseWireHeader = (vector: InvalidVector): ParsedWireHeader => {
  const bytes = wireBytes(vector);
  if (bytes.byteLength < 20) {
    throw new RangeError(`${vector.name}: truncated desktop header`);
  }
  const view = new DataView(
    bytes.buffer,
    bytes.byteOffset,
    bytes.byteLength,
  );
  return {
    major: view.getUint16(0, true),
    minor: view.getUint16(2, true),
    message_type: view.getUint16(4, true),
    flags: view.getUint16(6, true),
    payload_len: view.getUint32(8, true),
    sequence: view.getBigUint64(12, true),
  };
};

const replayWireFrame = (vector: InvalidVector): string => {
  const bytes = wireBytes(vector);
  if (bytes.byteLength < 20) {
    return "TruncatedHeader";
  }
  const header = parseWireHeader(vector);
  if (bytes.byteLength !== 20 + header.payload_len) {
    return "FrameLengthMismatch";
  }
  if (header.sequence === 0n) {
    return "NonMonotonicSequence";
  }
  return validateEnvelopeHeader(header) ? "Accepted" : "InvalidHeader";
};

const protocolHello = (
  endpointRole: EndpointRole,
  capabilities: Readonly<{ supported: bigint; mandatory: bigint }> = {
    supported: KNOWN_ENDPOINT_CAPABILITIES,
    mandatory: 0n,
  },
): ProtocolHello => ({
  major: PROTOCOL_MAJOR,
  minor: PROTOCOL_MINOR,
  schema_hash: bytesFromHex(SCHEMA_HASH_HEX),
  endpoint_role: endpointRole,
  capabilities,
  max_message_bytes: MAX_MESSAGE_BYTES,
  max_transfer_slots: MAX_TRANSFER_SLOTS,
});

const browserConnection = (): CompatibleHandshake => {
  const connection = negotiateHandshake(
    protocolHello(EndpointRole.Host),
    protocolHello(EndpointRole.Engine),
  );
  if (connection === undefined) {
    throw new Error("runtime-vector handshake must be compatible");
  }
  return connection;
};

const unwrapPayload = <T>(result: PayloadCodecResult<T>): T => {
  if (result.ok) {
    return result.value;
  }
  throw new Error(`test payload could not be encoded: ${result.error.code}`);
};

const encodeCommandRecord = (command: Command): Uint8Array => {
  switch (command.type) {
    case "Hello":
      return unwrapPayload(encodeHelloCommandPayload(command.payload));
    case "ProvideData":
      return unwrapPayload(encodeProvideDataCommandPayload(command.payload));
    default:
      throw new Error("unsupported runtime-vector command");
  }
};

const binaryBrowserFrame = (
  command: Command,
  correlation: Correlation,
  resources: readonly unknown[] = [],
): unknown[] => {
  const correlationBytes = unwrapPayload(
    encodeCorrelationPayload(correlation),
  );
  const recordBytes = encodeCommandRecord(command);
  const payloadLength = correlationBytes.byteLength + recordBytes.byteLength;
  const messageType =
    command.type === "Hello"
      ? MESSAGE_ID_HELLO
      : MESSAGE_ID_PROVIDE_DATA;
  const envelope: CommandEnvelope = {
    header: {
      major: PROTOCOL_MAJOR,
      minor: PROTOCOL_MINOR,
      message_type: messageType,
      flags: 0,
      payload_len: payloadLength,
      sequence: 1n,
    },
    correlation,
    command,
  };
  const encoded = unwrapPayload(encodeCommandPayload(envelope));
  const control = new ArrayBuffer(
    BROWSER_CONTROL_HEADER_BYTES + encoded.bytes.byteLength,
  );
  const header = new DataView(control);
  header.setUint16(0, PROTOCOL_MAJOR, true);
  header.setUint16(2, PROTOCOL_MINOR, true);
  header.setUint16(4, encoded.messageId, true);
  header.setUint16(6, 0, true);
  header.setUint32(8, encoded.bytes.byteLength, true);
  header.setBigUint64(12, 1n, true);
  new Uint8Array(control, BROWSER_CONTROL_HEADER_BYTES).set(encoded.bytes);
  return [control, ...resources];
};

const helloFrame = (resources: readonly unknown[] = []): unknown[] =>
  binaryBrowserFrame(
    {
      type: "Hello",
      payload: {
        hello: protocolHello(EndpointRole.Host),
      },
    },
    { worker: 1n },
    resources,
  );

const provideDataFrame = (
  segments: readonly unknown[],
  transferLengths: readonly bigint[],
): unknown[] => {
  const payload = {
    ticket: 3n,
    source: {
      stable_id: new Uint8Array(32).fill(0x51),
      revision: 4n,
    },
    segments,
  };
  return binaryBrowserFrame(
    { type: "ProvideData", payload } as Command,
    { worker: 1n, session: 2n },
    transferLengths.map((length) => {
      const size = Number(length);
      if (!Number.isSafeInteger(size) || size < 0) {
        throw new TypeError("test transfer length cannot be represented");
      }
      return new ArrayBuffer(size);
    }),
  );
};

const boundaryOutcome = (frame: unknown): BrowserBoundaryErrorCode | "Accepted" => {
  try {
    const admission = new BrowserCommandAdmission("Ready", {
      maxSessions: 1,
      maxRequests: 1,
      maxSurfaces: 1,
    });
    admission.setSessionState(2n, "Ready");
    new BrowserCommandBoundary(
      1n,
      browserConnection(),
      admission,
    ).decode(frame);
    return "Accepted";
  } catch (error: unknown) {
    if (!(error instanceof BrowserBoundaryError)) {
      throw error;
    }
    return error.code;
  }
};

const mutateBrowserControl = (
  frame: readonly unknown[],
  mutate: (control: ArrayBuffer) => void,
): unknown[] => {
  const candidate = frame[0];
  if (!(candidate instanceof ArrayBuffer)) {
    throw new TypeError("test frame must begin with an ArrayBuffer");
  }
  const control = candidate.slice(0);
  mutate(control);
  return [control, ...frame.slice(1)];
};

const wireResourceTable = (vector: InvalidVector): unknown[] => {
  const bytes = wireBytes(vector);
  const control = new ArrayBuffer(bytes.byteLength);
  new Uint8Array(control).set(bytes);
  return [control];
};

const handshakeOutcome = (
  local: unknown,
  peer: unknown,
): BrowserHandshakeErrorCode | "Accepted" => {
  try {
    negotiateBrowserHello(local, peer);
    return "Accepted";
  } catch (error: unknown) {
    if (!(error instanceof BrowserHandshakeError)) {
      throw error;
    }
    return error.code;
  }
};

const validSurface = (): {
  metadata: Record<string, unknown>;
  transport: Record<string, unknown>;
} => ({
  metadata: {
    id: 1n,
    lease_token: 5n,
    owner: {
      worker: 1n,
      session: 2n,
    },
    generation: 3n,
    region: {
      page_index: 0,
      x: 0,
      y: 0,
      width: 1,
      height: 1,
      coordinate_space: SurfaceCoordinateSpace.DevicePixelsTopLeft,
    },
    width: 1,
    height: 1,
    stride: 4,
    format: PixelFormat.Rgba8,
    alpha: AlphaMode.Straight,
    byte_offset: 0n,
    byte_length: 4n,
    render_config: new Uint8Array(32).fill(1),
    renderer_epoch: 1,
    plan_id: 1n,
    plan_hash: new Uint8Array(32).fill(2),
    scene_hash: new Uint8Array(32).fill(3),
    decision_hash: new Uint8Array(32).fill(4),
    backend: NativeBackend.ReferenceCpu,
  },
  transport: {
    kind: "BrowserArrayBuffer",
    slot: 0,
    buffer_length: 4n,
  },
});

const capabilityRequirement = (id: number): unknown => ({
  id,
  capability: 7,
  parameter: 0n,
  context: {
    code: 1,
    value: 0n,
  },
  dependencies: [],
  scope: {
    kind: 1,
  },
  contributor_ids: [],
});

const capabilityDecision = (
  missingCount: number,
  missingTotal: number,
  completeness: CollectionCompleteness,
): unknown => ({
  decision_schema_version: 1,
  status: SupportStatus.Unsupported,
  profile: 1,
  profile_version: 1,
  policy_version: 1,
  subject: {
    source: {
      stable_id: new Uint8Array(32).fill(1),
      revision: 1n,
    },
    document_revision: 1n,
    revision_startxref: 10n,
    page_index: 0,
    page_object_number: 1,
    page_object_generation: 0,
    scene_schema_major: 1,
    scene_schema_minor: 0,
    scene_hash: new Uint8Array(32).fill(2),
  },
  missing: Array.from({ length: missingCount }, (_, index) =>
    capabilityRequirement(index + 1),
  ),
  missing_total: missingTotal,
  missing_completeness: completeness,
  contributors: [],
  contributors_total: 0,
  contributors_completeness: CollectionCompleteness.Complete,
  locations_total: missingTotal,
  locations_completeness:
    missingTotal === 0
      ? CollectionCompleteness.Complete
      : CollectionCompleteness.Truncated,
  evaluated_requirements: missingTotal,
  evaluated_dependencies: 0,
  evaluated_parameters: missingTotal,
  evaluated_commands: 0,
  evaluated_resources: 0,
  scope: {
    kind: 1,
  },
});

const replayProvideDataVector = (vector: InvalidVector): string => {
  const start = vectorBigInt(vector, "range_start");
  const length = vectorBigInt(vector, "range_len");
  const byteLength = vectorBigInt(vector, "byte_length");
  const transferLength = vectorBigInt(vector, "transfer_length");
  const segment = {
    range: {
      start,
      len: length,
    },
    slot: 0,
    byte_length: byteLength,
    role: DataAttachmentRole.ImmutableRangeBytes,
  };
  const command = {
    ticket: 3n,
    source: {
      stable_id: new Uint8Array(32).fill(0x51),
      revision: 4n,
    },
    segments: [segment],
  };

  let outcome = "Accepted";
  if (length === 0n || length !== byteLength) {
    outcome = "InvalidDataRange";
  } else if (start > 0xffff_ffff_ffff_ffffn - length) {
    outcome = "NumericOverflow";
  } else if (
    !validateProvideDataCommand(command) ||
    !validateProvideDataTransferLengths(command, [transferLength])
  ) {
    outcome = "InvalidTransferBinding";
  }

  assert.equal(
    validateDataSegment(segment),
    outcome === "Accepted" || outcome === "InvalidTransferBinding",
    vector.name,
  );
  assert.equal(
    boundaryOutcome(provideDataFrame([segment], [transferLength])),
    outcome === "InvalidTransferBinding"
      ? "InvalidResourceLength"
      : "InvalidEnvelope",
    vector.name,
  );
  return outcome;
};

const replayEndpointCapabilitiesVector = (vector: InvalidVector): string => {
  const mandatory = u64FromHex(vectorString(vector, "mandatory"));
  const supported =
    typeof vector.supported === "string"
      ? u64FromHex(vector.supported)
      : KNOWN_ENDPOINT_CAPABILITIES | mandatory;
  const capabilities = { supported, mandatory };

  assert.equal(validateEndpointCapabilities(capabilities), false, vector.name);
  assert.equal(
    handshakeOutcome(
      protocolHello(EndpointRole.Host),
      protocolHello(EndpointRole.Engine, capabilities),
    ),
    "InvalidHandshake",
    vector.name,
  );
  if ((mandatory & ~KNOWN_ENDPOINT_CAPABILITIES) !== 0n) {
    return "UnknownMandatoryCapability";
  }
  return (mandatory & ~supported) !== 0n
    ? "InvalidEndpointCapabilities"
    : "Accepted";
};

const replayInvalidVector = (vector: InvalidVector): string => {
  switch (vector.name) {
    case "truncated-header": {
      assert.equal(vectorNumber(vector, "transfer_slots"), 0, vector.name);
      assert.equal(
        boundaryOutcome(wireResourceTable(vector)),
        "InvalidPayloadLength",
        vector.name,
      );
      return replayWireFrame(vector);
    }
    case "payload-length-mismatch": {
      assert.equal(
        boundaryOutcome(wireResourceTable(vector)),
        "InvalidPayloadLength",
        vector.name,
      );
      return replayWireFrame(vector);
    }
    case "zero-sequence": {
      assert.equal(
        boundaryOutcome(wireResourceTable(vector)),
        "InvalidHeader",
        vector.name,
      );
      return replayWireFrame(vector);
    }
    case "unknown-message": {
      const frame = mutateBrowserControl(
        helloFrame(),
        (control) => {
          new DataView(control).setUint16(
            4,
            vectorNumber(vector, "message_type"),
            true,
          );
        },
      );
      assert.equal(boundaryOutcome(frame), "InvalidHeader", vector.name);
      return "UnknownMessage";
    }
    case "unsupported-flags": {
      const frame = mutateBrowserControl(
        helloFrame(),
        (control) => {
          const view = new DataView(control);
          view.setUint16(4, vectorNumber(vector, "message_type"), true);
          view.setUint16(6, vectorNumber(vector, "flags"), true);
        },
      );
      assert.equal(boundaryOutcome(frame), "InvalidHeader", vector.name);
      return "InvalidFlags";
    }
    case "missing-required-correlation": {
      vectorRecord(vector, "correlation");
      const frame = mutateBrowserControl(
        helloFrame(),
        (control) => {
          new Uint8Array(control).fill(
            0,
            BROWSER_CONTROL_HEADER_BYTES,
            BROWSER_CONTROL_HEADER_BYTES + 8,
          );
        },
      );
      assert.equal(boundaryOutcome(frame), "InvalidEnvelope", vector.name);
      return "InvalidCorrelation";
    }
    case "transfer-count-out-of-range": {
      const transferSlots = vectorNumber(vector, "transfer_slots");
      const frame = helloFrame(
        Array.from({ length: transferSlots }, () => new ArrayBuffer(0)),
      );
      assert.equal(
        vectorNumber(vector, "message_type"),
        MESSAGE_ID_HELLO,
        vector.name,
      );
      assert.equal(
        boundaryOutcome(frame),
        "InvalidResourceBinding",
        vector.name,
      );
      return "InvalidTransferCount";
    }
    case "provide-data-duplicate-slot": {
      const transferSlots = vectorNumber(vector, "transfer_slots");
      const slots = vectorNumbers(vector, "slots");
      const segments = slots.map((slot, index) => ({
        range: {
          start: BigInt(index * 4),
          len: 4n,
        },
        slot,
        byte_length: 4n,
        role: DataAttachmentRole.ImmutableRangeBytes,
      }));
      const frame = provideDataFrame(
        segments,
        Array.from({ length: transferSlots }, () => 4n),
      );
      assert.equal(
        vectorNumber(vector, "message_type"),
        MESSAGE_ID_PROVIDE_DATA,
        vector.name,
      );
      assert.equal(boundaryOutcome(frame), "InvalidEnvelope", vector.name);
      return "InvalidTransferBinding";
    }
    case "provide-data-zero-range":
    case "provide-data-range-overflow":
    case "provide-data-length-mismatch":
    case "provide-data-transfer-length-mismatch":
      return replayProvideDataVector(vector);
    case "surface-stride-too-small": {
      const width = vectorNumber(vector, "width");
      const height = vectorNumber(vector, "height");
      const stride = vectorNumber(vector, "stride");
      const byteLength = vectorBigInt(vector, "byte_length");
      const surface = validSurface();
      Object.assign(surface.metadata, {
        width,
        height,
        stride,
        byte_length: byteLength,
      });
      Object.assign(surface.transport, { buffer_length: byteLength });
      assert.equal(validateSurfaceReadyEvent(surface), false, vector.name);
      return stride < width * 4 ||
        stride % 4 !== 0 ||
        BigInt(stride) * BigInt(height) !== byteLength
        ? "InvalidSurfaceLayout"
        : "Accepted";
    }
    case "surface-range-overflow": {
      const byteOffset = vectorBigInt(vector, "byte_offset");
      const byteLength = vectorBigInt(vector, "byte_length");
      const regionLength = vectorBigInt(vector, "region_length");
      const surface = validSurface();
      Object.assign(surface.metadata, {
        byte_offset: byteOffset,
        byte_length: byteLength,
      });
      Object.assign(surface.transport, { buffer_length: regionLength });
      assert.equal(validateSurfaceReadyEvent(surface), false, vector.name);
      return byteOffset > 0xffff_ffff_ffff_ffffn - byteLength
        ? "NumericOverflow"
        : "Accepted";
    }
    case "surface-reclaimed-missing-reason": {
      const descriptor = descriptorById(
        vectorNumber(vector, "message_type"),
      );
      assert.equal(descriptor?.name, "SurfaceReclaimed", vector.name);
      const rawPayload = vectorRecord(vector, "payload");
      const rawSurface = rawPayload.surface;
      if (typeof rawSurface !== "number") {
        throw new TypeError(`${vector.name}: surface must be a number`);
      }
      const payload = { surface: BigInt(rawSurface) };
      assert.equal(
        validateSurfaceReclaimedEvent(payload),
        false,
        vector.name,
      );
      return "MissingRequiredField";
    }
    case "unknown-mandatory-capability":
    case "mandatory-not-supported-by-endpoint":
      return replayEndpointCapabilitiesVector(vector);
    case "silent-decision-truncation": {
      const missingCount = vectorNumber(vector, "missing_count");
      const missingTotal = vectorNumber(vector, "missing_total");
      const completenessText = vectorString(
        vector,
        "missing_completeness",
      );
      const completeness =
        completenessText === "Complete"
          ? CollectionCompleteness.Complete
          : completenessText === "Truncated"
            ? CollectionCompleteness.Truncated
            : undefined;
      if (completeness === undefined) {
        throw new TypeError(
          `${vector.name}: unknown collection completeness`,
        );
      }
      const decision = capabilityDecision(
        missingCount,
        missingTotal,
        completeness,
      );
      assert.equal(validateCapabilityDecision(decision), false, vector.name);
      return completeness === CollectionCompleteness.Complete &&
        missingCount !== missingTotal
        ? "InvalidCapabilityDecision"
        : "Accepted";
    }
    default:
      throw new TypeError(`unhandled invalid runtime vector ${vector.name}`);
  }
};

test("compatibility vectors execute through the browser handshake", async () => {
  const compatibility = await readVectorFile("compatibility-vectors.json");

  assert.equal(
    compatibility.generator_version,
    PROTOCOL_GENERATOR_VERSION,
  );
  assert.equal(compatibility.schema_sha256, SCHEMA_SHA256_HEX);
  assert.equal(compatibility.wire_schema_hash, SCHEMA_HASH_HEX);
  assert.equal(
    compatibility.minimum_compatible_minor,
    MIN_COMPATIBLE_MINOR,
  );
  assert.ok(compatibility.vectors.length > 0);

  const errorMap: Readonly<Record<string, BrowserHandshakeErrorCode>> = {
    UnsupportedMinor: "InvalidHandshake",
    IncompatibleSchema: "InvalidHandshake",
    UnknownMandatoryCapability: "InvalidHandshake",
    InvalidEndpointCapabilities: "InvalidHandshake",
    MissingMandatoryCapability: "InvalidHandshake",
    UnsupportedMajor: "InvalidHandshake",
  };
  for (const rawVector of compatibility.vectors) {
    const vector = compatibilityVector(rawVector);
    assert.equal(vector.local_major, PROTOCOL_MAJOR, vector.name);
    assert.equal(vector.local_minor, PROTOCOL_MINOR, vector.name);
    const local = {
      major: vector.local_major,
      minor: vector.local_minor,
      schema_hash: bytesFromHex(SCHEMA_HASH_HEX),
      endpoint_role: EndpointRole.Host,
      capabilities: {
        supported: KNOWN_ENDPOINT_CAPABILITIES,
        mandatory: 0n,
      },
      max_message_bytes: MAX_MESSAGE_BYTES,
      max_transfer_slots: MAX_TRANSFER_SLOTS,
    };
    const peer = {
      major: vector.peer_major,
      minor: vector.peer_minor,
      schema_hash: bytesFromHex(vector.peer_schema_hash),
      endpoint_role: EndpointRole.Engine,
      capabilities: {
        supported: u64FromHex(vector.peer_supported),
        mandatory: u64FromHex(vector.peer_mandatory),
      },
      max_message_bytes: MAX_MESSAGE_BYTES,
      max_transfer_slots: MAX_TRANSFER_SLOTS,
    };

    if (vector.expected !== undefined) {
      assert.equal(vector.expected, "ExactSchema", vector.name);
      const negotiated = negotiateBrowserHello(local, peer);
      assert.equal(negotiated.minor, vector.peer_minor, vector.name);
    } else {
      const expectedCode = errorMap[vector.expected_error ?? ""];
      assert.notEqual(expectedCode, undefined, vector.name);
      assert.throws(
        () => negotiateBrowserHello(local, peer),
        (error: unknown) =>
          error instanceof BrowserHandshakeError &&
          error.code === expectedCode,
        vector.name,
      );
    }
  }
});

test("invalid runtime vectors execute through browser validators", async () => {
  const invalid = await readVectorFile("invalid-vectors.json");

  assert.equal(invalid.generator_version, PROTOCOL_GENERATOR_VERSION);
  assert.equal(invalid.schema_sha256, SCHEMA_SHA256_HEX);
  assert.ok(invalid.vectors.length > 0);

  const replayed = new Set<string>();
  for (const rawVector of invalid.vectors) {
    const vector = invalidVector(rawVector);
    assert.equal(replayed.has(vector.name), false, vector.name);
    replayed.add(vector.name);
    assert.equal(
      replayInvalidVector(vector),
      vector.expected_error,
      vector.name,
    );
  }
});
