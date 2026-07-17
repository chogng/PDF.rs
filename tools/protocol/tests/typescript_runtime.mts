import {
  AlphaMode,
  BrowserTransferKind,
  EndpointRole,
  MESSAGE_ID_CLOSE_SESSION,
  MESSAGE_ID_PROVIDE_DATA,
  MESSAGE_ID_REGISTER_CANVAS,
  MESSAGE_ID_SURFACE_READY,
  NativeBackend,
  PixelFormat,
  PROTOCOL_MAJOR,
  PROTOCOL_MINOR,
  SurfaceCoordinateSpace,
  validateCommandEnvelope,
  validateEndpointCapabilities,
  validateEnvelopeHeader,
  validateEventEnvelope,
  validateProtocolHello,
  validateSurfaceTransport,
} from "../../../platform/browser/generated/engine-protocol.ts";

const expect = (condition: boolean, label: string): void => {
  if (!condition) throw new Error(`generated TypeScript runtime vector failed: ${label}`);
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

const close = {
  header: header(MESSAGE_ID_CLOSE_SESSION),
  correlation: sessionCorrelation,
  command: { type: "CloseSession", payload: {} },
};
expect(validateCommandEnvelope(close), "valid command envelope");
expect(
  !validateCommandEnvelope({
    ...close,
    header: header(MESSAGE_ID_REGISTER_CANVAS),
    correlation: workerCorrelation,
  }),
  "header id and command type mismatch",
);
expect(
  !validateCommandEnvelope({ ...close, correlation: workerCorrelation }),
  "missing session correlation",
);
expect(!validateCommandEnvelope(close, 1), "unexpected transfer slot");

const canvas = {
  header: header(MESSAGE_ID_REGISTER_CANVAS, 32),
  correlation: workerCorrelation,
  command: {
    type: "RegisterCanvas",
    payload: { canvas: 3n, transfer_slot: 0, width: 10, height: 10 },
  },
};
expect(validateCommandEnvelope(canvas, 1), "registered canvas transfer");
expect(!validateCommandEnvelope(canvas, 0), "missing registered canvas transfer");

const source = {
  stable_id: new Uint8Array(32),
  revision: 1n,
};
const segment = (slot: number) => ({
  range: { start: BigInt(slot), len: 1n },
  slot,
  byte_length: 1n,
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

const knownCapabilities = 0x3fn;
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
  schema_hash: new Uint8Array(16),
  endpoint_role: EndpointRole.Host,
  capabilities: { supported: knownCapabilities, mandatory: 0n },
  max_message_bytes: 1024,
  max_transfer_slots: 4,
};
expect(validateProtocolHello(hello), "bounded hello");
expect(!validateProtocolHello({ ...hello, max_message_bytes: 0 }), "zero message limit");
expect(
  !validateProtocolHello({ ...hello, max_transfer_slots: 65 }),
  "transfer limit above global ceiling",
);

const bytes32 = (): Uint8Array => {
  const bytes = new Uint8Array(32);
  bytes[0] = 1;
  return bytes;
};
const surfacePayload = (stride: number, byteLength: bigint) => ({
  metadata: {
    id: 5n,
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
    alpha: AlphaMode.Premultiplied,
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
  transport: { kind: "LocalMemory", region_length: byteLength, memory_epoch: 1 },
});
const surfaceEnvelope = (payload: ReturnType<typeof surfacePayload>) => ({
  header: header(MESSAGE_ID_SURFACE_READY, 1024),
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
      transport: { kind: "LocalMemory", region_length: 400n, memory_epoch: 0 },
    }),
  ),
  "zero local memory epoch",
);
expect(
  validateSurfaceTransport({
    kind: "BrowserTransfer",
    slot: 0,
    transfer_kind: BrowserTransferKind.ArrayBuffer,
    transfer_length: 400n,
  }),
  "browser transfer discriminant is distinct from transfer kind",
);
