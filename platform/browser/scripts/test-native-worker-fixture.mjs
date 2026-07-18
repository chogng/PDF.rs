import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const glue = await import(
  new URL("../dist/native/engine-worker.generated.js", import.meta.url)
);
const {
  BrowserNativeWorkerLoader,
} = await import(
  new URL("../.test-dist/src/browser-native-worker-loader.js", import.meta.url)
);
const protocol = await import(
  new URL("../.test-dist/generated/engine-protocol.js", import.meta.url)
);

const engine = new Uint8Array(await readFile(
  new URL("../dist/native/engine.wasm", import.meta.url),
));
// The production build uses panic=abort: Rust panic is a WebAssembly trap.
// Status 0xffff is reserved for unwind-capable/internal ABI probes only.
const runtime = {
  fetch: async () => ({
    ok: true,
    headers: new Headers({
      "content-length": String(engine.byteLength),
    }),
    body: new Response(engine.slice()).body,
  }),
  digestSha256: async (bytes) =>
    crypto.subtle.digest("SHA-256", bytes.slice()),
  compile: async (bytes) => WebAssembly.compile(bytes.slice()),
  instantiate: async (module, imports) =>
    WebAssembly.instantiate(module, imports),
};

const hello = (endpointRole) => ({
  major: protocol.PROTOCOL_MAJOR,
  minor: protocol.PROTOCOL_MINOR,
  schema_hash: protocol.SCHEMA_HASH.slice(),
  endpoint_role: endpointRole,
  capabilities: {
    supported: protocol.EndpointCapability.TransferableArrayBuffer,
    mandatory: endpointRole === protocol.EndpointRole.Engine
      ? protocol.EndpointCapability.TransferableArrayBuffer
      : 0n,
  },
  max_message_bytes: protocol.MAX_MESSAGE_BYTES,
  max_transfer_slots: protocol.MAX_TRANSFER_SLOTS,
});
const hostHello = hello(protocol.EndpointRole.Host);
const connection = protocol.negotiateHandshake(
  hostHello,
  hello(protocol.EndpointRole.Engine),
);
assert.notEqual(connection, undefined, "fixture handshake must negotiate");
const supervisorIdentity = Object.freeze({
  worker: 1n,
  workerEpoch: 1n,
  rendererEpoch: 1,
});
const MAX_FIXTURE_NATIVE_TURNS = 4_096;

const unwrap = (result) => {
  if (!result.ok) {
    throw new Error(`protocol codec failed: ${result.error.code}`);
  }
  return result.value;
};

const commandEncoder = (command) => {
  switch (command.type) {
    case "Hello":
      return protocol.encodeHelloCommandPayload(command.payload);
    case "HelloAccept":
      return protocol.encodeHelloAcceptCommandPayload(command.payload);
    case "Open":
      return protocol.encodeOpenCommandPayload(command.payload);
    case "ProvideData":
      return protocol.encodeProvideDataCommandPayload(command.payload);
    case "GetPageMetrics":
      return protocol.encodeGetPageMetricsCommandPayload(command.payload);
    case "SetViewport":
      return protocol.encodeSetViewportCommandPayload(command.payload);
    case "ReleaseSurface":
      return protocol.encodeReleaseSurfaceCommandPayload(command.payload);
    case "CloseSession":
      return protocol.encodeCloseSessionCommandPayload(command.payload);
    case "Shutdown":
      return protocol.encodeShutdownCommandPayload(command.payload);
    default:
      throw new Error(`unsupported fixture command: ${command.type}`);
  }
};

const descriptorFor = (kind, name) => {
  const descriptor = protocol.MESSAGE_DESCRIPTORS.find(
    (candidate) => candidate.kind === kind && candidate.name === name,
  );
  if (descriptor === undefined) {
    throw new Error(`missing ${kind} descriptor for ${name}`);
  }
  return descriptor;
};

let inputSequence = 1n;
const encodeFrame = (command, correlation) => {
  const descriptor = descriptorFor("command", command.type);
  const correlationBytes = unwrap(
    protocol.encodeCorrelationPayload(correlation),
  );
  const commandBytes = unwrap(commandEncoder(command));
  const payloadLength = correlationBytes.byteLength + commandBytes.byteLength;
  const header = {
    major: protocol.PROTOCOL_MAJOR,
    minor: protocol.PROTOCOL_MINOR,
    message_type: descriptor.id,
    flags: 0,
    payload_len: payloadLength,
    sequence: inputSequence,
  };
  inputSequence += 1n;
  const encoded = unwrap(protocol.encodeCommandPayload({
    header,
    correlation,
    command,
  }));
  assert.equal(encoded.bytes.byteLength, payloadLength);
  const frame = new Uint8Array(20 + payloadLength);
  const view = new DataView(frame.buffer);
  view.setUint16(0, header.major, true);
  view.setUint16(2, header.minor, true);
  view.setUint16(4, header.message_type, true);
  view.setUint16(6, header.flags, true);
  view.setUint32(8, header.payload_len, true);
  view.setBigUint64(12, header.sequence, true);
  frame.set(encoded.bytes, 20);
  return frame;
};

const decodeDispatch = (dispatch) => {
  if (dispatch === undefined) {
    return undefined;
  }
  const view = new DataView(
    dispatch.frame.buffer,
    dispatch.frame.byteOffset,
    dispatch.frame.byteLength,
  );
  const header = {
    major: view.getUint16(0, true),
    minor: view.getUint16(2, true),
    message_type: view.getUint16(4, true),
    flags: view.getUint16(6, true),
    payload_len: view.getUint32(8, true),
    sequence: view.getBigUint64(12, true),
  };
  const decoded = unwrap(
    protocol.decodeEventPayload(header, dispatch.frame.subarray(20)),
  );
  return Object.freeze({
    ...decoded,
    transfers: dispatch.transfers,
  });
};

const loader = glue.createNativeWorkerEngineLoader(
  BrowserNativeWorkerLoader,
  runtime,
);
const worker = await loader.load(connection, supervisorIdentity);
const seen = [];
const dispatch = (command, correlation, transfers = []) => {
  const event = decodeDispatch(
    worker.dispatch(encodeFrame(command, correlation), transfers),
  );
  if (event !== undefined) {
    seen.push(event);
  }
  return event;
};
const pollUntil = (predicate, label) => {
  const existing = seen.at(-1);
  if (existing !== undefined && predicate(existing, seen)) {
    return existing;
  }
  for (let turn = 0; turn < MAX_FIXTURE_NATIVE_TURNS; turn += 1) {
    const polled = worker.poll();
    const event = decodeDispatch(polled.output);
    if (event !== undefined) {
      seen.push(event);
      if (predicate(event, seen)) {
        return event;
      }
    }
    if (!polled.pending) {
      throw new Error(
        `Native Worker became idle while waiting for ${label}`,
      );
    }
  }
  throw new Error(
    `Native Worker exceeded ${MAX_FIXTURE_NATIVE_TURNS} turns waiting for ${label}`,
  );
};

const engineHello = dispatch(
  { type: "Hello", payload: { hello: hostHello } },
  { worker: supervisorIdentity.worker },
);
assert.equal(engineHello?.event.type, "EngineHello");
assert.equal(
  engineHello.event.payload.hello.endpoint_role,
  protocol.EndpointRole.Engine,
);

const ready = dispatch(
  {
    type: "HelloAccept",
    payload: {
      negotiated_minor: connection.minor,
      schema_hash: protocol.SCHEMA_HASH.slice(),
    },
  },
  { worker: supervisorIdentity.worker },
);
assert.equal(ready?.event.type, "Ready");

const pdf = new TextEncoder().encode("%PDF-1.7\n");
const source = {
  stable_id: new Uint8Array(32).fill(0x51),
  revision: 1n,
};
dispatch(
  {
    type: "Open",
    payload: {
      source: {
        identity: source,
        length: BigInt(pdf.byteLength),
        validator: new Uint8Array(32).fill(0x7a),
      },
    },
  },
  { worker: supervisorIdentity.worker, request: 1n },
);
const needData = pollUntil(
  (event) => event.event.type === "NeedData",
  "NeedData",
);
const session = needData.correlation.session;
assert.notEqual(session, undefined);
const dataTransfers = needData.event.payload.ranges.map((range) =>
  pdf.slice(
    Number(range.start),
    Number(range.start + range.len),
  ).buffer
);
dispatch(
  {
    type: "ProvideData",
    payload: {
      ticket: needData.event.payload.ticket,
      source,
      segments: needData.event.payload.ranges.map((range, slot) => ({
        range,
        slot,
        byte_length: range.len,
        role: protocol.DataAttachmentRole.ImmutableRangeBytes,
      })),
    },
  },
  { worker: supervisorIdentity.worker, session },
  dataTransfers,
);
const documentReady = pollUntil(
  (event) => event.event.type === "DocumentReady",
  "DocumentReady",
);
assert.equal(documentReady.event.payload.session, session);

dispatch(
  {
    type: "GetPageMetrics",
    payload: {
      document_revision: documentReady.event.payload.document_revision,
      start_index: 0,
      max_count: 1,
    },
  },
  { worker: supervisorIdentity.worker, session, request: 2n },
);
const pageMetrics = pollUntil(
  (event) => event.event.type === "PageMetrics",
  "PageMetrics",
);
assert.equal(pageMetrics.event.payload.pages.length, 1);
const geometry = pageMetrics.event.payload.pages[0].geometry;

dispatch(
  {
    type: "SetViewport",
    payload: {
      viewport: {
        generation: 1n,
        document_revision: documentReady.event.payload.document_revision,
        annotation_revision: 0n,
        zoom_numerator: 1,
        zoom_denominator: 1,
        visible_pages: [{
          page_index: 0,
          coordinate_space: protocol.PageCoordinateSpace.PdfPointsBottomLeft,
          geometry,
          clip_x_milli_points: geometry.crop_box_x_milli_points,
          clip_y_milli_points: geometry.crop_box_y_milli_points,
          clip_width_milli_points: Math.min(
            geometry.crop_box_width_milli_points,
            72_000,
          ),
          clip_height_milli_points: Math.min(
            geometry.crop_box_height_milli_points,
            72_000,
          ),
        }],
        quality: protocol.QualityPolicy.Full,
        output_profile: protocol.OutputProfile.Srgb,
        device_scale_milli: 1_000,
        rotation: protocol.PageRotation.Degrees0,
        optional_content_id: 0n,
      },
    },
  },
  { worker: supervisorIdentity.worker, session, generation: 1n },
);
pollUntil(
  (_event, events) =>
    events.some((event) => event.event.type === "CapabilityReported")
    && events.some((event) => event.event.type === "SurfaceReady")
    && events.some((event) => event.event.type === "GenerationCompleted"),
  "CapabilityReported, SurfaceReady, and GenerationCompleted",
);
const surface = seen.find((event) => event.event.type === "SurfaceReady");
assert.notEqual(surface, undefined);
assert.equal(surface.transfers.length, 1);
assert.equal(
  BigInt(surface.transfers[0].byteLength),
  surface.event.payload.transport.buffer_length,
);
assert.ok(new Uint8Array(surface.transfers[0]).some((byte) => byte !== 0));

dispatch(
  {
    type: "ReleaseSurface",
    payload: {
      surface: surface.event.payload.metadata.id,
      lease_token: surface.event.payload.metadata.lease_token,
    },
  },
  { worker: supervisorIdentity.worker, session },
);
pollUntil(
  (_event, events) =>
    events.some((event) => event.event.type === "SurfaceReleaseAcknowledged"),
  "SurfaceReleaseAcknowledged",
);

dispatch(
  { type: "CloseSession", payload: {} },
  { worker: supervisorIdentity.worker, session },
);
pollUntil(
  (_event, events) =>
    events.some((event) => event.event.type === "CloseSessionAcknowledged")
    && events.some((event) => event.event.type === "SessionClosed"),
  "CloseSessionAcknowledged and SessionClosed",
);

dispatch(
  { type: "Shutdown", payload: { deadline_ms: 1_000 } },
  { worker: supervisorIdentity.worker },
);
pollUntil(
  (_event, events) =>
    events.some((event) => event.event.type === "ShutdownAcknowledged")
    && events.some((event) => event.event.type === "WorkerStopped"),
  "ShutdownAcknowledged and WorkerStopped",
);
worker.shutdown();
assert.equal(worker.closed, true);
loader.close();

const requiredEvents = [
  "EngineHello",
  "Ready",
  "NeedData",
  "DocumentReady",
  "PageMetrics",
  "CapabilityReported",
  "SurfaceReady",
  "GenerationCompleted",
  "SurfaceReleaseAcknowledged",
  "CloseSessionAcknowledged",
  "SessionClosed",
  "ShutdownAcknowledged",
  "WorkerStopped",
];
for (const eventType of requiredEvents) {
  assert.ok(
    seen.some((event) => event.event.type === eventType),
    `missing lifecycle event ${eventType}`,
  );
}
