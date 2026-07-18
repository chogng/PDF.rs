import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const glue = await import(
  new URL("../dist/native/engine-worker.generated.js", import.meta.url)
);
const {
  BROWSER_NATIVE_WORKER_MAX_TRANSFER_BYTES,
  createBrowserNativeWorkerStart,
  installBrowserNativeWorkerEntry,
} = await import(
  new URL("../.test-dist/src/browser-native-worker-entry.js", import.meta.url)
);
const {
  negotiateBrowserHello,
} = await import(
  new URL("../.test-dist/src/browser-handshake.js", import.meta.url)
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

class FixtureScheduler {
  #next = 1;
  #callbacks = new Map();

  get pending() {
    return this.#callbacks.size;
  }

  request(callback) {
    const handle = this.#next;
    this.#next += 1;
    this.#callbacks.set(handle, callback);
    return handle;
  }

  cancel(handle) {
    this.#callbacks.delete(handle);
  }

  runOne() {
    const next = this.#callbacks.entries().next();
    if (next.done) {
      return false;
    }
    const [handle, callback] = next.value;
    this.#callbacks.delete(handle);
    callback();
    return true;
  }
}

class FixtureWorkerScope {
  #listeners = new Map();
  #scheduler;
  posted = [];
  closeCount = 0;

  constructor(scheduler) {
    this.#scheduler = scheduler;
  }

  addEventListener(type, listener) {
    let listeners = this.#listeners.get(type);
    if (listeners === undefined) {
      listeners = new Set();
      this.#listeners.set(type, listeners);
    }
    listeners.add(listener);
  }

  removeEventListener(type, listener) {
    this.#listeners.get(type)?.delete(listener);
  }

  postMessage(value, transfer) {
    assert.deepEqual(value, transfer);
    this.posted.push(structuredClone(value, { transfer }));
  }

  close() {
    this.closeCount += 1;
  }

  setTimeout(callback, milliseconds) {
    assert.equal(milliseconds, 0);
    return this.#scheduler.request(callback);
  }

  clearTimeout(handle) {
    this.#scheduler.cancel(handle);
  }

  emit(value, transfer = []) {
    const cloned = structuredClone(value, { transfer });
    const event = new MessageEvent("message", { data: cloned });
    for (const listener of this.#listeners.get("message") ?? []) {
      listener(event);
    }
  }

  emitPhysical(control, transfers = []) {
    const value = [control.buffer, ...transfers];
    this.emit(value, value);
  }
}

const scheduler = new FixtureScheduler();
const scope = new FixtureWorkerScope(scheduler);
const faults = [];
const controller = installBrowserNativeWorkerEntry({
  scope,
  artifact: glue.NATIVE_WORKER_ARTIFACT,
  runtime,
  limits: Object.freeze({
    maxInboundMessages: 64,
    maxQueuedBytes: BROWSER_NATIVE_WORKER_MAX_TRANSFER_BYTES,
    maxTurn: 64,
    maxTransferBytes: BROWSER_NATIVE_WORKER_MAX_TRANSFER_BYTES,
    maxTransferSlots: protocol.MAX_TRANSFER_SLOTS,
  }),
  fatal: (code) => {
    faults.push(code);
  },
});
const seen = [];
const takePosted = () => {
  while (scope.posted.length > 0) {
    const physical = scope.posted.shift();
    assert.ok(Array.isArray(physical));
    const [control, ...transfers] = physical;
    assert.ok(control instanceof ArrayBuffer);
    const event = decodeDispatch({
      frame: new Uint8Array(control),
      transfers,
    });
    assert.notEqual(event, undefined);
    seen.push(event);
  }
};
const driveUntil = async (predicate, label) => {
  for (let turn = 0; turn < MAX_FIXTURE_NATIVE_TURNS; turn += 1) {
    if (scheduler.runOne()) {
      takePosted();
    }
    takePosted();
    if (predicate(seen)) {
      return seen.at(-1);
    }
    if (faults.length > 0) {
      throw new Error(`Native Worker controller faulted: ${faults[0]}`);
    }
    if (
      controller.state !== "Loading"
      && scheduler.pending === 0
    ) {
      throw new Error(
        `Native Worker controller became idle while waiting for ${label}`,
      );
    }
    await new Promise((resolve) => setImmediate(resolve));
  }
  throw new Error(
    `Native Worker exceeded ${MAX_FIXTURE_NATIVE_TURNS} controller turns waiting for ${label}`,
  );
};
const send = (command, correlation, transfers = []) => {
  scope.emitPhysical(
    encodeFrame(command, correlation),
    transfers,
  );
};

scope.emit(createBrowserNativeWorkerStart(supervisorIdentity));
send(
  { type: "Hello", payload: { hello: hostHello } },
  { worker: supervisorIdentity.worker },
);
await driveUntil(
  (events) => events.some((event) => event.event.type === "EngineHello"),
  "EngineHello",
);
const engineHello = seen.find(
  (event) => event.event.type === "EngineHello",
);
assert.notEqual(engineHello, undefined);
assert.equal(
  engineHello.event.payload.hello.endpoint_role,
  protocol.EndpointRole.Engine,
);
const connection = negotiateBrowserHello(
  hostHello,
  engineHello.event.payload.hello,
);
send(
  {
    type: "HelloAccept",
    payload: {
      negotiated_minor: connection.minor,
      schema_hash: protocol.SCHEMA_HASH.slice(),
    },
  },
  { worker: supervisorIdentity.worker },
);
await driveUntil(
  (events) => events.some((event) => event.event.type === "Ready"),
  "Ready",
);
assert.equal(controller.state, "Ready");

const dispatch = (command, correlation, transfers = []) => {
  send(command, correlation, transfers);
};
const pollUntil = async (predicate, label) => {
  const existing = seen.at(-1);
  if (existing !== undefined && predicate(existing, seen)) {
    return existing;
  }
  await driveUntil(
    (events) => {
      const latest = events.at(-1);
      return latest !== undefined && predicate(latest, events);
    },
    label,
  );
  const matching = seen.findLast((event) => predicate(event, seen));
  if (matching === undefined) {
    throw new Error(`Native Worker did not publish ${label}`);
  }
  return matching;
};

/*
 * The full fixture below now drives the same controller physical-message path
 * as the supervisor. No direct BrowserNativeWorkerInstance dispatch or poll is
 * used after bootstrap.
 */

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
const needData = await pollUntil(
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
const documentReady = await pollUntil(
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
const pageMetrics = await pollUntil(
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
await pollUntil(
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
await pollUntil(
  (_event, events) =>
    events.some((event) => event.event.type === "SurfaceReleaseAcknowledged"),
  "SurfaceReleaseAcknowledged",
);

dispatch(
  { type: "CloseSession", payload: {} },
  { worker: supervisorIdentity.worker, session },
);
await pollUntil(
  (_event, events) =>
    events.some((event) => event.event.type === "CloseSessionAcknowledged")
    && events.some((event) => event.event.type === "SessionClosed"),
  "CloseSessionAcknowledged and SessionClosed",
);

dispatch(
  { type: "Shutdown", payload: { deadline_ms: 1_000 } },
  { worker: supervisorIdentity.worker },
);
await pollUntil(
  (_event, events) =>
    events.some((event) => event.event.type === "ShutdownAcknowledged")
    && events.some((event) => event.event.type === "WorkerStopped"),
  "ShutdownAcknowledged and WorkerStopped",
);
assert.equal(controller.state, "Closed");
assert.equal(scope.closeCount, 1);
assert.equal(scheduler.pending, 0);
assert.deepEqual(faults, []);

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
