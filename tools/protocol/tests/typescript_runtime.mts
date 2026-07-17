import { readFileSync } from "node:fs";

import {
  AlphaMode,
  CapabilityScopeKind,
  CollectionCompleteness,
  DataAttachmentRole,
  EndpointRole,
  EnvelopeSequenceTracker,
  KNOWN_ENDPOINT_CAPABILITIES,
  MESSAGE_ID_CLOSE_SESSION,
  MESSAGE_ID_PROVIDE_DATA,
  MESSAGE_ID_SET_VIEWPORT,
  MESSAGE_ID_SURFACE_READY,
  MIN_COMPATIBLE_MINOR,
  NativeBackend,
  OutputProfile,
  PageCoordinateSpace,
  PageRotation,
  PixelFormat,
  PROTOCOL_GENERATOR_VERSION,
  PROTOCOL_MAJOR,
  PROTOCOL_MINOR,
  QualityPolicy,
  SCHEMA_HASH,
  SCHEMA_HASH_HEX,
  SCHEMA_SHA256_HEX,
  SupportStatus,
  SurfaceCoordinateSpace,
  beginValidateCommandEnvelope,
  beginValidateCommandEnvelopeResult,
  beginValidateEventEnvelope,
  negotiateHandshake,
  negotiateHandshakeResult,
  validateCapabilityDecision,
  validateCorrelation,
  validateDataSegment,
  validateEndpointCapabilities,
  validateEnvelopeHeader,
  validateEnvelopeHeaderForMinor,
  validateProtocolHello,
  validateProvideDataTransferLengths,
  validateSurfaceReclaimedEvent,
  validateSurfaceTransport,
} from "../../../platform/browser/generated/engine-protocol.ts";

const expect = (condition: boolean, label: string): void => {
  if (!condition) throw new Error(`generated TypeScript runtime vector failed: ${label}`);
};
const bytes32 = (): Uint8Array => {
  const bytes = new Uint8Array(32);
  bytes[0] = 1;
  return bytes;
};

const header = (message_type: number, payload_len = 0) => ({
  major: PROTOCOL_MAJOR,
  minor: PROTOCOL_MINOR,
  message_type,
  flags: 0,
  payload_len,
  sequence: 1n,
});
const workerCorrelation = { worker: 1n };
const sessionCorrelation = { worker: 1n, session: 2n };
const knownCapabilities = KNOWN_ENDPOINT_CAPABILITIES;
const handshakeHello = (endpoint_role: EndpointRole) => ({
  major: PROTOCOL_MAJOR,
  minor: PROTOCOL_MINOR,
  schema_hash: SCHEMA_HASH,
  endpoint_role,
  capabilities: { supported: knownCapabilities, mandatory: 0n },
  max_message_bytes: 1_048_576,
  max_transfer_slots: 8,
});
const connection = negotiateHandshake(
  handshakeHello(EndpointRole.Host),
  handshakeHello(EndpointRole.Engine),
);
if (connection === undefined) {
  throw new Error("generated exact handshake must negotiate");
}
expect(
  negotiateHandshakeResult(
    handshakeHello(EndpointRole.Host),
    handshakeHello(EndpointRole.Engine),
  ).ok,
  "handshake result API accepts exact transcript",
);
const invalidHandshake = negotiateHandshakeResult(
  handshakeHello(EndpointRole.Host),
  { ...handshakeHello(EndpointRole.Engine), minor: PROTOCOL_MINOR + 1 },
);
expect(
  !invalidHandshake.ok && invalidHandshake.error.code === "InvalidHandshake",
  "handshake result API is stable and redacted",
);

const declaredPayloadLength = (value: unknown): number | undefined => {
  if (typeof value !== "object" || value === null) return undefined;
  const headerValue = Reflect.get(value, "header");
  if (typeof headerValue !== "object" || headerValue === null) return undefined;
  const payloadLength = Reflect.get(headerValue, "payload_len");
  return typeof payloadLength === "number" ? payloadLength : undefined;
};
const validateCommandEnvelope = (
  value: unknown,
  transferSlots = 0,
  negotiatedMinor = PROTOCOL_MINOR,
): boolean => {
  const payloadLength = declaredPayloadLength(value);
  return negotiatedMinor === connection.minor
    && payloadLength !== undefined
    && beginValidateCommandEnvelope(
      value,
      transferSlots,
      payloadLength,
      connection,
      new EnvelopeSequenceTracker(),
    ) !== undefined;
};
const validateEventEnvelope = (
  value: unknown,
  transferSlots?: number,
): boolean => {
  const payloadLength = declaredPayloadLength(value);
  const eventValue =
    typeof value === "object" && value !== null
      ? Reflect.get(value, "event")
      : undefined;
  const eventType =
    typeof eventValue === "object" && eventValue !== null
      ? Reflect.get(eventValue, "type")
      : undefined;
  const actualTransferSlots =
    transferSlots ?? (eventType === "SurfaceReady" ? 1 : 0);
  return payloadLength !== undefined
    && beginValidateEventEnvelope(
      value,
      actualTransferSlots,
      payloadLength,
      connection,
      new EnvelopeSequenceTracker(),
    ) !== undefined;
};

expect(validateEnvelopeHeader(header(MESSAGE_ID_CLOSE_SESSION)), "known header");
expect(!validateEnvelopeHeader(header(65535)), "unknown message id");
expect(
  !validateEnvelopeHeader({ ...header(MESSAGE_ID_CLOSE_SESSION), flags: 1 }),
  "unsupported flags",
);
expect(
  !validateEnvelopeHeader(header(MESSAGE_ID_CLOSE_SESSION, 65)),
  "message payload limit",
);
expect(
  !validateEnvelopeHeader({ ...header(MESSAGE_ID_CLOSE_SESSION), sequence: 0n }),
  "zero sequence",
);
expect(
  !validateEnvelopeHeader({
    ...header(MESSAGE_ID_CLOSE_SESSION),
    minor: MIN_COMPATIBLE_MINOR - 1,
  }),
  "minor below generated compatibility window",
);

const close = {
  header: header(MESSAGE_ID_CLOSE_SESSION),
  correlation: sessionCorrelation,
  command: { type: "CloseSession", payload: {} },
};
expect(validateCommandEnvelope(close), "valid command envelope");
const committedCloseSequence = new EnvelopeSequenceTracker();
const pendingClose = beginValidateCommandEnvelopeResult(
  close,
  0,
  close.header.payload_len,
  connection,
  committedCloseSequence,
);
expect(
  pendingClose.ok && pendingClose.value.commitSequence(),
  "envelope result API commits only explicitly",
);
const duplicateClose = beginValidateCommandEnvelopeResult(
  close,
  0,
  close.header.payload_len,
  connection,
  committedCloseSequence,
);
expect(
  !duplicateClose.ok
    && duplicateClose.error.code === "NonMonotonicSequence",
  "envelope result API reports stable sequence failure",
);
expect(
  validateEnvelopeHeaderForMinor(close.header, MIN_COMPATIBLE_MINOR),
  "header matches registered negotiated minor",
);
expect(
  !validateEnvelopeHeaderForMinor(close.header, MIN_COMPATIBLE_MINOR - 1),
  "header rejects an unregistered negotiated minor",
);
expect(
  !validateCommandEnvelope(close, 0, MIN_COMPATIBLE_MINOR - 1),
  "envelope rejects an unregistered negotiated minor",
);
expect(
  !validateCommandEnvelope({
    ...close,
    header: header(MESSAGE_ID_SET_VIEWPORT),
    correlation: workerCorrelation,
  }),
  "header id and command type mismatch",
);
expect(
  !validateCommandEnvelope({ ...close, correlation: workerCorrelation }),
  "missing session correlation",
);
expect(!validateCommandEnvelope(close, 1), "unexpected transfer slot");

const pageViewport = (page_index: number) => ({
  page_index,
  coordinate_space: PageCoordinateSpace.PdfPointsBottomLeft,
  geometry: {
    identity: bytes32(),
    media_box_x_milli_points: 0,
    media_box_y_milli_points: 0,
    media_box_width_milli_points: 612_000,
    media_box_height_milli_points: 792_000,
    crop_box_x_milli_points: 0,
    crop_box_y_milli_points: 0,
    crop_box_width_milli_points: 612_000,
    crop_box_height_milli_points: 792_000,
    intrinsic_rotation: PageRotation.Degrees0,
  },
  clip_x_milli_points: 0,
  clip_y_milli_points: 0,
  clip_width_milli_points: 612_000,
  clip_height_milli_points: 792_000,
});
const viewport = {
  generation: 7n,
  document_revision: 3n,
  annotation_revision: 0n,
  zoom_numerator: 3,
  zoom_denominator: 2,
  visible_pages: [pageViewport(0)],
  quality: QualityPolicy.Full,
  output_profile: OutputProfile.Srgb,
  device_scale_milli: 2_000,
  rotation: PageRotation.Degrees0,
  optional_content_id: 0n,
};
const setViewport = {
  header: header(MESSAGE_ID_SET_VIEWPORT),
  correlation: { worker: 1n, session: 2n, generation: 7n },
  command: { type: "SetViewport", payload: { viewport } },
};
expect(validateCommandEnvelope(setViewport), "canonical viewport command");
expect(
  !validateCommandEnvelope({
    ...setViewport,
    correlation: { ...setViewport.correlation, generation: 8n },
  }),
  "viewport generation matches correlation",
);
for (const invalidViewport of [
  { ...viewport, generation: 0n },
  { ...viewport, document_revision: 0n },
  { ...viewport, zoom_numerator: 0 },
  { ...viewport, zoom_denominator: 0 },
  { ...viewport, zoom_numerator: 6, zoom_denominator: 4 },
  { ...viewport, device_scale_milli: 0 },
  { ...viewport, visible_pages: [pageViewport(0), pageViewport(0)] },
  {
    ...viewport,
    visible_pages: [
      pageViewport(0),
      { ...pageViewport(1), geometry: pageViewport(0).geometry },
    ],
  },
]) {
  expect(
    !validateCommandEnvelope({
      ...setViewport,
      command: { type: "SetViewport", payload: { viewport: invalidViewport } },
    }),
    "invalid viewport semantics",
  );
}

const source = {
  stable_id: bytes32(),
  revision: 1n,
};
const segment = (slot: number) => ({
  range: { start: BigInt(slot * 2), len: 1n },
  slot,
  byte_length: 1n,
  role: DataAttachmentRole.ImmutableRangeBytes,
});
const provide = {
  header: header(MESSAGE_ID_PROVIDE_DATA, 128),
  correlation: sessionCorrelation,
  command: {
    type: "ProvideData",
    payload: { ticket: 4n, source, segments: [segment(0), segment(1)] },
  },
};
expect(validateCommandEnvelope(provide, 2), "canonical transfer slot coverage");
expect(
  validateProvideDataTransferLengths(provide.command.payload, [1n, 1n]),
  "canonical transfer byte lengths",
);
expect(
  !validateProvideDataTransferLengths(provide.command.payload, [1n, 2n]),
  "actual transfer byte length mismatch",
);
expect(
  !validateCommandEnvelope(
    {
      ...provide,
      command: {
        ...provide.command,
        payload: { ...provide.command.payload, segments: [segment(0), segment(0)] },
      },
    },
    2,
  ),
  "duplicate transfer slot",
);
expect(
  !validateCommandEnvelope(
    {
      ...provide,
      command: { ...provide.command, payload: { ...provide.command.payload, segments: [] } },
    },
    1,
  ),
  "unreferenced transfer slot",
);

expect(
  validateEndpointCapabilities({ supported: knownCapabilities, mandatory: 1n }),
  "known mandatory capability",
);
expect(
  validateEndpointCapabilities({
    supported: knownCapabilities | (1n << 63n),
    mandatory: 0n,
  }),
  "unknown optional capability",
);
expect(
  !validateEndpointCapabilities({
    supported: knownCapabilities,
    mandatory: 1n << 63n,
  }),
  "unknown mandatory capability",
);
expect(
  !validateEndpointCapabilities({ supported: 0n, mandatory: 1n }),
  "mandatory capability not supported",
);
const hello = {
  major: PROTOCOL_MAJOR,
  minor: PROTOCOL_MINOR,
  schema_hash: SCHEMA_HASH,
  endpoint_role: EndpointRole.Host,
  capabilities: { supported: knownCapabilities, mandatory: 0n },
  max_message_bytes: 1024,
  max_transfer_slots: 4,
};
expect(validateProtocolHello(hello), "bounded hello");
expect(
  validateProtocolHello({ ...hello, minor: MIN_COMPATIBLE_MINOR }),
  "compatible older hello",
);
expect(
  !validateProtocolHello({ ...hello, minor: MIN_COMPATIBLE_MINOR - 1 }),
  "too-old hello",
);
expect(
  !validateProtocolHello({ ...hello, minor: PROTOCOL_MINOR + 1 }),
  "future hello",
);
expect(!validateProtocolHello({ ...hello, max_message_bytes: 0 }), "zero message limit");
expect(
  !validateProtocolHello({ ...hello, max_transfer_slots: 65 }),
  "transfer limit above global ceiling",
);

const surfacePayload = (stride: number, byteLength: bigint) => ({
  metadata: {
    id: 5n,
    lease_token: 7n,
    owner: { worker: 1n, session: 2n },
    generation: 3n,
    region: {
      page_index: 0,
      x: 0,
      y: 0,
      width: 100,
      height: 1,
      coordinate_space: SurfaceCoordinateSpace.DevicePixelsTopLeft,
    },
    width: 100,
    height: 1,
    stride,
    format: PixelFormat.Rgba8,
    alpha: AlphaMode.Straight,
    byte_offset: 0n,
    byte_length: byteLength,
    render_config: bytes32(),
    renderer_epoch: 1,
    plan_id: 6n,
    plan_hash: bytes32(),
    scene_hash: bytes32(),
    decision_hash: bytes32(),
    backend: NativeBackend.ReferenceCpu,
  },
  transport: {
    kind: "BrowserArrayBuffer",
    slot: 0,
    buffer_length: byteLength,
  },
});
const surfaceEnvelope = (payload: ReturnType<typeof surfacePayload>) => ({
  header: header(MESSAGE_ID_SURFACE_READY, 282),
  correlation: { worker: 1n, session: 2n, generation: 3n },
  event: { type: "SurfaceReady", payload },
});
expect(
  !validateEventEnvelope(surfaceEnvelope(surfacePayload(1, 1n))),
  "stride below RGBA8 width",
);
expect(
  validateEventEnvelope(surfaceEnvelope(surfacePayload(400, 400n))),
  "checked RGBA8 surface",
);
expect(
  !validateEventEnvelope(
    surfaceEnvelope({
      ...surfacePayload(400, 400n),
      metadata: { ...surfacePayload(400, 400n).metadata, id: 0n },
    }),
  ),
  "zero surface identity",
);
expect(
  !validateEventEnvelope(
    surfaceEnvelope({
      ...surfacePayload(400, 400n),
      metadata: {
        ...surfacePayload(400, 400n).metadata,
        render_config: new Uint8Array(32),
      },
    }),
  ),
  "zero surface binding hash",
);
expect(
  !validateEventEnvelope(
    surfaceEnvelope({
      ...surfacePayload(400, 400n),
      transport: {
        kind: "BrowserArrayBuffer",
        slot: 1,
        buffer_length: 400n,
      },
    }),
  ),
  "browser surface slot must bind the only logical resource",
);
expect(
  validateSurfaceTransport({
    kind: "BrowserArrayBuffer",
    slot: 0,
    buffer_length: 400n,
  }),
  "browser ArrayBuffer transport is generated",
);

type JsonObject = { readonly [key: string]: unknown };
const jsonObject = (value: unknown, label: string): JsonObject => {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${label} is not an object`);
  }
  return value as JsonObject;
};
const jsonArray = (value: unknown, label: string): readonly unknown[] => {
  if (!Array.isArray(value)) throw new Error(`${label} is not an array`);
  return value;
};
const jsonString = (value: unknown, label: string): string => {
  if (typeof value !== "string") throw new Error(`${label} is not a string`);
  return value;
};
const jsonNumber = (value: unknown, label: string): number => {
  if (typeof value !== "number" || !Number.isInteger(value)) {
    throw new Error(`${label} is not an integer`);
  }
  return value;
};
const hexBytes = (value: string): Uint8Array => {
  if (!/^[0-9a-f]+$/.test(value) || value.length % 2 !== 0) {
    throw new Error("invalid generated lowercase hex");
  }
  return Uint8Array.from(
    Array.from({ length: value.length / 2 }, (_, index) =>
      Number.parseInt(value.slice(index * 2, index * 2 + 2), 16),
    ),
  );
};
const loadGeneratedJson = (relative: string): JsonObject => {
  const parsed: unknown = JSON.parse(
    readFileSync(new URL(relative, import.meta.url), "utf8"),
  );
  return jsonObject(parsed, relative);
};

const compatibilityDocument = loadGeneratedJson(
  "../../../protocol/generated/compatibility-vectors.json",
);
expect(
  jsonString(compatibilityDocument.generator_version, "generator_version") ===
    PROTOCOL_GENERATOR_VERSION &&
  jsonString(compatibilityDocument.schema_sha256, "schema_sha256") ===
    SCHEMA_SHA256_HEX &&
    jsonString(compatibilityDocument.wire_schema_hash, "wire_schema_hash") === SCHEMA_HASH_HEX,
  "compatibility document identities",
);
expect(
  jsonNumber(
    compatibilityDocument.minimum_compatible_minor,
    "minimum_compatible_minor",
  ) === MIN_COMPATIBLE_MINOR,
  "generated compatibility minimum",
);
const replayedCompatibility = new Set<string>();
for (const raw of jsonArray(compatibilityDocument.vectors, "compatibility vectors")) {
  const vector = jsonObject(raw, "compatibility vector");
  const name = jsonString(vector.name, "compatibility name");
  replayedCompatibility.add(name);
  expect(jsonNumber(vector.local_major, `${name}.local_major`) === PROTOCOL_MAJOR, name);
  expect(jsonNumber(vector.local_minor, `${name}.local_minor`) === PROTOCOL_MINOR, name);
  const supported = BigInt(jsonString(vector.peer_supported, `${name}.supported`));
  const mandatory = BigInt(jsonString(vector.peer_mandatory, `${name}.mandatory`));
  const peerHashHex = jsonString(vector.peer_schema_hash, `${name}.schema_hash`);
  const peer = {
    major: jsonNumber(vector.peer_major, `${name}.peer_major`),
    minor: jsonNumber(vector.peer_minor, `${name}.peer_minor`),
    schema_hash: hexBytes(peerHashHex),
    endpoint_role: EndpointRole.Engine,
    capabilities: { supported, mandatory },
    max_message_bytes: 1_048_576,
    max_transfer_slots: 8,
  };
  const valid = validateProtocolHello(peer);
  const expected =
    typeof vector.expected === "string"
      ? vector.expected
      : jsonString(vector.expected_error, `${name}.expected_error`);
  switch (expected) {
    case "ExactSchema":
      expect(valid && peerHashHex === SCHEMA_HASH_HEX, name);
      break;
    case "UnsupportedMajor":
    case "UnsupportedMinor":
      expect(!valid, name);
      break;
    case "IncompatibleSchema":
      expect(valid && peerHashHex !== SCHEMA_HASH_HEX, name);
      break;
    case "UnknownMandatoryCapability":
      expect(!valid && (mandatory & ~KNOWN_ENDPOINT_CAPABILITIES) !== 0n, name);
      break;
    case "InvalidEndpointCapabilities":
      expect(!valid && (mandatory & ~supported) !== 0n, name);
      break;
    default:
      throw new Error(`unregistered compatibility outcome ${expected}`);
  }
}
expect(replayedCompatibility.size === 8, "all compatibility vectors replayed");

const invalidDocument = loadGeneratedJson(
  "../../../protocol/generated/invalid-vectors.json",
);
expect(
  jsonString(invalidDocument.generator_version, "generator_version") ===
    PROTOCOL_GENERATOR_VERSION &&
    jsonString(invalidDocument.schema_sha256, "schema_sha256") ===
      SCHEMA_SHA256_HEX,
  "invalid document identities",
);
const replayedInvalid = new Set<string>();
for (const raw of jsonArray(invalidDocument.vectors, "invalid vectors")) {
  const vector = jsonObject(raw, "invalid vector");
  const name = jsonString(vector.name, "invalid name");
  replayedInvalid.add(name);
  switch (name) {
    case "truncated-header":
    case "payload-length-mismatch":
      // The Rust desktop decoder replays byte-framing vectors from this same file.
      break;
    case "zero-sequence":
      expect(
        !validateEnvelopeHeader({
          ...header(MESSAGE_ID_CLOSE_SESSION),
          sequence: 0n,
        }),
        name,
      );
      break;
    case "unknown-message":
      expect(
        !validateEnvelopeHeader(
          header(jsonNumber(vector.message_type, `${name}.message_type`)),
        ),
        name,
      );
      break;
    case "unsupported-flags":
      expect(
        !validateEnvelopeHeader({
          ...header(jsonNumber(vector.message_type, `${name}.message_type`)),
          flags: jsonNumber(vector.flags, `${name}.flags`),
        }),
        name,
      );
      break;
    case "missing-required-correlation":
      expect(!validateCorrelation(vector.correlation), name);
      break;
    case "transfer-count-out-of-range":
      expect(
        !validateCommandEnvelope(
          {
            header: header(jsonNumber(vector.message_type, `${name}.message_type`)),
            correlation: workerCorrelation,
            command: { type: "Hello", payload: { hello } },
          },
          jsonNumber(vector.transfer_slots, `${name}.transfer_slots`),
        ),
        name,
      );
      break;
    case "provide-data-duplicate-slot": {
      const slots = jsonArray(vector.slots, `${name}.slots`).map((slot) =>
        jsonNumber(slot, `${name}.slot`),
      );
      expect(
        !validateCommandEnvelope(
          {
            ...provide,
            command: {
              ...provide.command,
              payload: {
                ...provide.command.payload,
                segments: slots.map(segment),
              },
            },
          },
          jsonNumber(vector.transfer_slots, `${name}.transfer_slots`),
        ),
        name,
      );
      break;
    }
    case "provide-data-zero-range":
    case "provide-data-range-overflow":
    case "provide-data-length-mismatch":
    case "provide-data-transfer-length-mismatch": {
      const candidate = {
        range: {
          start: BigInt(jsonString(vector.range_start, `${name}.range_start`)),
          len: BigInt(jsonString(vector.range_len, `${name}.range_len`)),
        },
        slot: 0,
        byte_length: BigInt(
          jsonString(vector.byte_length, `${name}.byte_length`),
        ),
        role: DataAttachmentRole.ImmutableRangeBytes,
      };
      const transferLength = BigInt(
        jsonString(vector.transfer_length, `${name}.transfer_length`),
      );
      if (name === "provide-data-transfer-length-mismatch") {
        expect(
          validateDataSegment(candidate) &&
            !validateProvideDataTransferLengths(
              {
                ticket: 1n,
                source,
                segments: [candidate],
              },
              [transferLength],
            ),
          name,
        );
      } else {
        expect(!validateDataSegment(candidate), name);
      }
      break;
    }
    case "surface-stride-too-small": {
      const width = jsonNumber(vector.width, `${name}.width`);
      const height = jsonNumber(vector.height, `${name}.height`);
      const stride = jsonNumber(vector.stride, `${name}.stride`);
      const byteLength = BigInt(jsonString(vector.byte_length, `${name}.byte_length`));
      const payload = surfacePayload(stride, byteLength);
      payload.metadata.width = width;
      payload.metadata.height = height;
      payload.metadata.region.width = width;
      payload.metadata.region.height = height;
      expect(!validateEventEnvelope(surfaceEnvelope(payload)), name);
      break;
    }
    case "surface-range-overflow": {
      const byteOffset = BigInt(jsonString(vector.byte_offset, `${name}.byte_offset`));
      const byteLength = BigInt(jsonString(vector.byte_length, `${name}.byte_length`));
      const regionLength = BigInt(jsonString(vector.region_length, `${name}.region_length`));
      const payload = surfacePayload(4, byteLength);
      payload.metadata.width = 1;
      payload.metadata.height = 1;
      payload.metadata.region.width = 1;
      payload.metadata.region.height = 1;
      payload.metadata.byte_offset = byteOffset;
      payload.transport.buffer_length = regionLength;
      expect(!validateEventEnvelope(surfaceEnvelope(payload)), name);
      break;
    }
    case "surface-reclaimed-missing-reason":
      expect(!validateSurfaceReclaimedEvent(vector.payload), name);
      break;
    case "unknown-mandatory-capability": {
      const mandatory = BigInt(jsonString(vector.mandatory, `${name}.mandatory`));
      expect(
        !validateEndpointCapabilities({
          supported: KNOWN_ENDPOINT_CAPABILITIES,
          mandatory,
        }),
        name,
      );
      break;
    }
    case "mandatory-not-supported-by-endpoint":
      expect(
        !validateEndpointCapabilities({
          supported: BigInt(jsonString(vector.supported, `${name}.supported`)),
          mandatory: BigInt(jsonString(vector.mandatory, `${name}.mandatory`)),
        }),
        name,
      );
      break;
    case "silent-decision-truncation": {
      const missingCount = jsonNumber(vector.missing_count, `${name}.missing_count`);
      const decision = {
        decision_schema_version: 1,
        status: SupportStatus.Unsupported,
        profile: 1,
        profile_version: 1,
        policy_version: 1,
        subject: {
          source: { stable_id: bytes32(), revision: 1n },
          document_revision: 1n,
          revision_startxref: 1n,
          page_index: 0,
          page_object_number: 1,
          page_object_generation: 0,
          scene_schema_major: 1,
          scene_schema_minor: 0,
          scene_hash: bytes32(),
        },
        missing: Array.from({ length: missingCount }, (_, index) => ({
          id: index + 1,
          capability: 1,
          parameter: 0n,
          context: { code: 1, value: 0n },
          dependencies: [],
          scope: { kind: CapabilityScopeKind.Page, page: 0 },
          contributor_ids: [],
        })),
        missing_total: jsonNumber(vector.missing_total, `${name}.missing_total`),
        missing_completeness: CollectionCompleteness.Complete,
        contributors: [],
        contributors_total: 0,
        contributors_completeness: CollectionCompleteness.Complete,
        scope: { kind: CapabilityScopeKind.Page, page: 0 },
      };
      expect(!validateCapabilityDecision(decision), name);
      break;
    }
    default:
      throw new Error(`unregistered invalid vector ${name}`);
  }
}
expect(replayedInvalid.size === 18, "all invalid vectors replayed");
