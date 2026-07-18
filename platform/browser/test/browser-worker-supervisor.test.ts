import assert from "node:assert/strict";
import test from "node:test";

import {
  AlphaMode,
  CapabilityScopeKind,
  CapabilityProfileId,
  CollectionCompleteness,
  DataPriority,
  EndpointCapability,
  EndpointRole,
  EngineErrorCode,
  ErrorCategory,
  ErrorRecoverability,
  ErrorSeverity,
  GenerationCompletionStatus,
  MAX_MESSAGE_BYTES,
  MAX_TRANSFER_SLOTS,
  MESSAGE_DESCRIPTORS,
  NativeBackend,
  OutputProfile,
  PROTOCOL_MAJOR,
  PROTOCOL_MINOR,
  PageRotation,
  PixelFormat,
  QualityPolicy,
  SCHEMA_HASH,
  SupportStatus,
  SurfaceCoordinateSpace,
  SurfaceReclaimReason,
  encodeCapabilityReportedEventPayload,
  decodeCommandPayload,
  encodeCorrelationPayload,
  encodeDocumentReadyEventPayload,
  encodeEngineHelloEventPayload,
  encodeEventPayload,
  encodeGenerationCompletedEventPayload,
  encodeGenerationPlannedEventPayload,
  encodeNeedDataEventPayload,
  encodeReadyEventPayload,
  encodeSessionClosedEventPayload,
  encodeSurfaceReadyEventPayload,
  encodeSurfaceReclaimedEventPayload,
  encodeWorkerStoppedEventPayload,
  type Correlation,
  type EnvelopeHeader,
  type Event,
  type EventEnvelope,
  type PayloadCodecResult,
  type ProtocolHello,
} from "../generated/engine-protocol.js";
import {
  BROWSER_CONTROL_HEADER_BYTES,
} from "../src/browser-command-boundary.js";
import {
  BrowserWorkerSupervisor,
  BrowserWorkerSupervisorError,
  createBrowserHostHello,
  type BrowserWorkerClock,
  type BrowserWorkerHandlers,
  type BrowserWorkerPort,
  type BrowserWorkerSupervisorConfiguration,
  type BrowserWorkerSupervisorErrorCode,
  type BrowserWorkerSupervisorLimits,
} from "../src/browser-worker-supervisor.js";

const SOURCE_ID = new Uint8Array(32).fill(0x51);
const SOURCE_VALIDATOR = new Uint8Array(32).fill(0x52);

const unwrap = <T>(result: PayloadCodecResult<T>): T => {
  if (result.ok) {
    return result.value;
  }
  throw new Error(result.error.code);
};

class VirtualClock implements BrowserWorkerClock {
  value = 0;

  now(): number {
    return this.value;
  }

  advance(milliseconds: number): void {
    this.value += milliseconds;
  }
}

interface SentMessage {
  readonly value: unknown[];
  readonly transfer: ArrayBuffer[];
}

class FakeWorkerPort implements BrowserWorkerPort {
  handlers: BrowserWorkerHandlers | undefined;
  readonly sent: SentMessage[] = [];
  terminateCount = 0;

  setHandlers(handlers: BrowserWorkerHandlers): void {
    this.handlers = handlers;
  }

  postMessage(value: unknown[], transfer: ArrayBuffer[]): void {
    this.sent.push(Object.freeze({ value, transfer }));
  }

  terminate(): void {
    this.terminateCount += 1;
  }

  emit(value: unknown): void {
    const handlers = this.handlers;
    if (handlers === undefined) {
      throw new Error("handlers not installed");
    }
    handlers.onMessage(value);
  }

  emitError(): void {
    const handlers = this.handlers;
    if (handlers === undefined) {
      throw new Error("handlers not installed");
    }
    handlers.onError();
  }
}

const defaultLimits = (
  overrides: Partial<BrowserWorkerSupervisorLimits> = {},
): BrowserWorkerSupervisorLimits => ({
  maxInboundEvents: 8,
  maxCriticalCommands: 4,
  maxOrdinaryCommands: 4,
  maxViewportCommands: 2,
  maxCriticalEvents: 8,
  maxProgressEvents: 4,
  admission: {
    maxSessions: 4,
    maxRequests: 8,
    maxSurfaces: 8,
  },
  ...overrides,
});

const fixture = (
  limits = defaultLimits(),
): Readonly<{
  supervisor: BrowserWorkerSupervisor;
  clock: VirtualClock;
  ports: FakeWorkerPort[];
}> => {
  const clock = new VirtualClock();
  const ports: FakeWorkerPort[] = [];
  const configuration: BrowserWorkerSupervisorConfiguration = {
    hostHello: createBrowserHostHello(
      EndpointCapability.TransferableArrayBuffer,
    ),
    limits,
    clock,
    startupTimeoutMs: 1_000,
    factory: () => {
      const port = new FakeWorkerPort();
      ports.push(port);
      return port;
    },
  };
  return Object.freeze({
    supervisor: new BrowserWorkerSupervisor(configuration),
    clock,
    ports,
  });
};

const engineHelloValue = (
  schemaHash = SCHEMA_HASH.slice(),
): ProtocolHello => ({
  major: PROTOCOL_MAJOR,
  minor: PROTOCOL_MINOR,
  schema_hash: schemaHash,
  endpoint_role: EndpointRole.Engine,
  capabilities: {
    supported: EndpointCapability.TransferableArrayBuffer,
    mandatory: 0n,
  },
  max_message_bytes: MAX_MESSAGE_BYTES,
  max_transfer_slots: MAX_TRANSFER_SLOTS,
});

const eventRecord = (event: Event): Uint8Array => {
  switch (event.type) {
    case "EngineHello":
      return unwrap(encodeEngineHelloEventPayload(event.payload));
    case "Ready":
      return unwrap(encodeReadyEventPayload(event.payload));
    case "NeedData":
      return unwrap(encodeNeedDataEventPayload(event.payload));
    case "DocumentReady":
      return unwrap(encodeDocumentReadyEventPayload(event.payload));
    case "CapabilityReported":
      return unwrap(
        encodeCapabilityReportedEventPayload(event.payload),
      );
    case "GenerationPlanned":
      return unwrap(encodeGenerationPlannedEventPayload(event.payload));
    case "GenerationCompleted":
      return unwrap(
        encodeGenerationCompletedEventPayload(event.payload),
      );
    case "SurfaceReady":
      return unwrap(encodeSurfaceReadyEventPayload(event.payload));
    case "SurfaceReclaimed":
      return unwrap(encodeSurfaceReclaimedEventPayload(event.payload));
    case "SessionClosed":
      return unwrap(encodeSessionClosedEventPayload(event.payload));
    case "WorkerStopped":
      return unwrap(encodeWorkerStoppedEventPayload(event.payload));
    default:
      throw new Error("unsupported test event");
  }
};

const eventFrame = (
  event: Event,
  correlation: Correlation,
  sequence: bigint,
  resources: readonly unknown[] = [],
): unknown[] => {
  const descriptor = MESSAGE_DESCRIPTORS.find(
    (candidate) =>
      candidate.kind === "event" && candidate.name === event.type,
  );
  if (descriptor === undefined) {
    throw new Error("missing event descriptor");
  }
  const encodedCorrelation = unwrap(
    encodeCorrelationPayload(correlation),
  );
  const encodedRecord = eventRecord(event);
  const payloadLength =
    encodedCorrelation.byteLength + encodedRecord.byteLength;
  const envelope: EventEnvelope = {
    header: {
      major: PROTOCOL_MAJOR,
      minor: PROTOCOL_MINOR,
      message_type: descriptor.id,
      flags: 0,
      payload_len: payloadLength,
      sequence,
    },
    correlation,
    event,
  };
  const encoded = unwrap(encodeEventPayload(envelope));
  const control = new ArrayBuffer(
    BROWSER_CONTROL_HEADER_BYTES + encoded.bytes.byteLength,
  );
  const header = new DataView(control);
  header.setUint16(0, PROTOCOL_MAJOR, true);
  header.setUint16(2, PROTOCOL_MINOR, true);
  header.setUint16(4, encoded.messageId, true);
  header.setUint16(6, 0, true);
  header.setUint32(8, encoded.bytes.byteLength, true);
  header.setBigUint64(12, sequence, true);
  new Uint8Array(control, BROWSER_CONTROL_HEADER_BYTES).set(
    encoded.bytes,
  );
  return [control, ...resources];
};

const engineHello = (
  worker: bigint,
  schemaHash = SCHEMA_HASH.slice(),
): unknown[] =>
  eventFrame(
    {
      type: "EngineHello",
      payload: {
        hello: engineHelloValue(schemaHash),
        execution_capabilities: { supported: 0n },
      },
    },
    { worker },
    1n,
  );

const ready = (
  worker: bigint,
  executionCapabilities = 0n,
): unknown[] =>
  eventFrame(
    {
      type: "Ready",
      payload: {
        worker,
        negotiated_minor: PROTOCOL_MINOR,
        schema_hash: SCHEMA_HASH.slice(),
        execution_capabilities: {
          supported: executionCapabilities,
        },
        capability_profiles: [CapabilityProfileId.BaselineNative],
        output_profiles: [OutputProfile.Srgb],
      },
    },
    { worker },
    2n,
  );

const handshake = (
  supervisor: BrowserWorkerSupervisor,
  port: FakeWorkerPort,
): void => {
  assert.equal(supervisor.state, "Starting");
  assert.equal(port.sent.length, 0);
  assert.equal(supervisor.drainOutboundTurn(), 1);
  assert.equal(messageType(port.sent[0]), "Hello");

  port.emit(engineHello(supervisor.worker));
  assert.equal(supervisor.state, "Starting");
  assert.equal(supervisor.connection, undefined);
  assert.equal(supervisor.queueDepths.inbound, 1);
  assert.equal(supervisor.processInboundTurn(), 1);
  assert.notEqual(supervisor.connection, undefined);
  assert.equal(port.sent.length, 1);
  assert.equal(supervisor.drainOutboundTurn(), 1);
  assert.equal(messageType(port.sent[1]), "HelloAccept");

  port.emit(ready(supervisor.worker));
  assert.equal(supervisor.state, "Starting");
  assert.equal(supervisor.processInboundTurn(), 1);
  assert.equal(supervisor.state, "Ready");
};

const sentControl = (
  sent: SentMessage | undefined,
): ArrayBuffer => {
  const control = sent?.value[0];
  assert.ok(control instanceof ArrayBuffer);
  return control;
};

const sentHeader = (
  sent: SentMessage | undefined,
): EnvelopeHeader => {
  const control = sentControl(sent);
  const view = new DataView(control);
  return {
    major: view.getUint16(0, true),
    minor: view.getUint16(2, true),
    message_type: view.getUint16(4, true),
    flags: view.getUint16(6, true),
    payload_len: view.getUint32(8, true),
    sequence: view.getBigUint64(12, true),
  };
};

const messageType = (
  sent: SentMessage | undefined,
): string | undefined =>
  MESSAGE_DESCRIPTORS.find(
    (descriptor) =>
      descriptor.kind === "command"
      && descriptor.id === sentHeader(sent).message_type,
  )?.name;

const decodedCommand = (sent: SentMessage | undefined) => {
  const control = sentControl(sent);
  const header = sentHeader(sent);
  const decoded = decodeCommandPayload(
    header,
    new Uint8Array(
      control,
      BROWSER_CONTROL_HEADER_BYTES,
      header.payload_len,
    ),
  );
  if (!decoded.ok) {
    throw new Error(decoded.error.code);
  }
  return decoded.value;
};

const openCommand = () => ({
  type: "Open" as const,
  payload: {
    source: {
      identity: {
        stable_id: SOURCE_ID.slice(),
        revision: 1n,
      },
      length: 4n,
      validator: SOURCE_VALIDATOR.slice(),
    },
  },
});

const openSession = (
  supervisor: BrowserWorkerSupervisor,
  port: FakeWorkerPort,
  request = 10n,
  session = 20n,
): void => {
  supervisor.submit(openCommand(), { request });
  assert.equal(supervisor.drainOutboundTurn(), 1);
  port.emit(
    eventFrame(
      {
        type: "NeedData",
        payload: {
          ticket: 30n,
          source: {
            stable_id: SOURCE_ID.slice(),
            revision: 1n,
          },
          ranges: [{ start: 0n, len: 4n }],
          priority: DataPriority.Metadata,
          checkpoint: 1n,
        },
      },
      { worker: supervisor.worker, session, request },
      3n,
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 1);
  port.emit(
    eventFrame(
      {
        type: "DocumentReady",
        payload: {
          session,
          document_revision: 1n,
          page_count: 1,
          profile: CapabilityProfileId.BaselineNative,
          policy_version: 1,
        },
      },
      { worker: supervisor.worker, session, request },
      4n,
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 1);
};

const viewportCommand = (generation: bigint) => ({
  type: "SetViewport" as const,
  payload: {
    viewport: {
      generation,
      document_revision: 1n,
      annotation_revision: 0n,
      zoom_numerator: 1,
      zoom_denominator: 1,
      visible_pages: [],
      quality: QualityPolicy.Preview,
      output_profile: OutputProfile.Srgb,
      device_scale_milli: 1_000,
      rotation: PageRotation.Degrees0,
      optional_content_id: 0n,
    },
  },
});

const capabilityReported = (): Event => ({
  type: "CapabilityReported",
  payload: {
    decision: {
      decision_schema_version: 1,
      status: SupportStatus.Unsupported,
      profile: CapabilityProfileId.BaselineNative,
      profile_version: 1,
      policy_version: 1,
      subject: {
        source: {
          stable_id: SOURCE_ID.slice(),
          revision: 1n,
        },
        document_revision: 1n,
        revision_startxref: 10n,
        page_index: 0,
        page_object_number: 1,
        page_object_generation: 0,
        scene_schema_major: 2,
        scene_schema_minor: 0,
        scene_hash: new Uint8Array(32).fill(2),
      },
      missing: [{
        id: 1,
        capability: 7,
        parameter: 0n,
        context: {
          code: 1,
          value: 0n,
        },
        dependencies: [],
        scope: {
          kind: CapabilityScopeKind.Page,
          page: 0,
        },
        contributor_ids: [],
        location: {
          page_index: 0,
        },
      }],
      missing_total: 1,
      missing_completeness: CollectionCompleteness.Complete,
      contributors: [],
      contributors_total: 0,
      contributors_completeness: CollectionCompleteness.Complete,
      locations_total: 1,
      locations_completeness: CollectionCompleteness.Complete,
      evaluated_requirements: 1,
      evaluated_dependencies: 0,
      evaluated_parameters: 1,
      evaluated_commands: 0,
      evaluated_resources: 0,
      scope: {
        kind: CapabilityScopeKind.Page,
        page: 0,
      },
      location: {
        page_index: 0,
      },
    },
    decision_hash: new Uint8Array(32).fill(4),
  },
});

const generationPlanned = (generation: bigint): Event => ({
  type: "GenerationPlanned",
  payload: {
    manifest: {
      plan_schema_version: 1,
      document_revision: 1n,
      render_config: new Uint8Array(32).fill(1),
      renderer_epoch: 1,
      plan_id: generation,
      generation,
      scene_hash: new Uint8Array(32).fill(2),
      decision_hash: new Uint8Array(32).fill(3),
      geometry_hash: new Uint8Array(32).fill(4),
      viewport_clip: {
        page_index: 0,
        x: 0,
        y: 0,
        width: 2,
        height: 2,
        coordinate_space:
          SurfaceCoordinateSpace.DevicePixelsTopLeft,
      },
      zoom_numerator: 1,
      zoom_denominator: 1,
      device_scale_milli: 1_000,
      rotation: PageRotation.Degrees0,
      optional_content: 0n,
      annotation_revision: 0n,
      backend: NativeBackend.FastCpu,
      output_profile: OutputProfile.Srgb,
      quality: QualityPolicy.Preview,
      regions: [{
        page_index: 0,
        x: 0,
        y: 0,
        width: 2,
        height: 2,
        coordinate_space:
          SurfaceCoordinateSpace.DevicePixelsTopLeft,
      }],
      tile_content_hashes: [
        new Uint8Array(32).fill(5),
      ],
    },
    plan_hash: new Uint8Array(32).fill(6),
  },
});

const generationCompleted = (
  status = GenerationCompletionStatus.Completed,
): Event => ({
  type: "GenerationCompleted",
  payload: {
    status,
    produced_regions: 0,
    ...(status === GenerationCompletionStatus.Failed
      ? {
        error: {
          code: EngineErrorCode.Internal,
          category: ErrorCategory.Internal,
          severity: ErrorSeverity.Fatal,
          recoverability: ErrorRecoverability.RestartWorker,
          diagnostic_id: 1n,
        },
      }
      : {}),
  },
});

const surfaceReady = (generation: bigint): Event => ({
  type: "SurfaceReady",
  payload: {
    metadata: {
      id: 90n,
      lease_token: 91n,
      owner: {
        worker: 1n,
        session: 20n,
      },
      generation,
      region: {
        page_index: 0,
        x: 0,
        y: 0,
        width: 2,
        height: 2,
        coordinate_space:
          SurfaceCoordinateSpace.DevicePixelsTopLeft,
      },
      width: 2,
      height: 2,
      stride: 8,
      format: PixelFormat.Rgba8,
      alpha: AlphaMode.Straight,
      byte_offset: 0n,
      byte_length: 16n,
      render_config: new Uint8Array(32).fill(1),
      renderer_epoch: 1,
      plan_id: 1n,
      plan_hash: new Uint8Array(32).fill(2),
      scene_hash: new Uint8Array(32).fill(3),
      decision_hash: new Uint8Array(32).fill(4),
      backend: NativeBackend.FastCpu,
    },
    transport: {
      kind: "BrowserArrayBuffer",
      slot: 0,
      buffer_length: 16n,
    },
  },
});

const surfaceReclaimed = (
  reason = SurfaceReclaimReason.MemoryPressure,
): Event => ({
  type: "SurfaceReclaimed",
  payload: {
    surface: 90n,
    lease_token: 91n,
    reason,
  },
});

const assertSupervisorError = (
  operation: () => unknown,
  code: BrowserWorkerSupervisorErrorCode,
): void => {
  assert.throws(
    operation,
    (error: unknown) =>
      error instanceof BrowserWorkerSupervisorError
      && error.code === code
      && error.message === code,
  );
};

const replacedGenerationFixture = () => {
  const result = fixture();
  result.supervisor.start();
  const port = result.ports[0];
  assert.ok(port !== undefined);
  handshake(result.supervisor, port);
  result.supervisor.takeEvents();
  openSession(result.supervisor, port);
  result.supervisor.takeEvents();
  result.supervisor.submit(
    viewportCommand(1n),
    { session: 20n, generation: 1n },
  );
  assert.equal(result.supervisor.drainOutboundTurn(), 1);
  result.supervisor.submit(
    viewportCommand(2n),
    { session: 20n, generation: 2n },
  );
  assert.equal(result.supervisor.drainOutboundTurn(), 1);
  return { ...result, port };
};

test("handshake and Worker callbacks advance only through explicit turns", () => {
  const { supervisor, ports } = fixture();
  supervisor.start();
  const port = ports[0];
  assert.ok(port !== undefined);

  handshake(supervisor, port);
  const events = supervisor.takeEvents();
  assert.equal(events.length, 1);
  assert.equal(events[0]?.type, "ProtocolEvent");
  assert.equal(supervisor.hasLiveWorker, true);
});

test("rejects EngineHello before Hello and Ready before HelloAccept", () => {
  const earlyHello = fixture();
  earlyHello.supervisor.start();
  const earlyHelloPort = earlyHello.ports[0];
  assert.ok(earlyHelloPort !== undefined);
  earlyHelloPort.emit(engineHello(1n));
  assert.equal(earlyHello.supervisor.processInboundTurn(), 0);
  assert.equal(earlyHello.supervisor.state, "Failed");

  const earlyReady = fixture();
  earlyReady.supervisor.start();
  const earlyReadyPort = earlyReady.ports[0];
  assert.ok(earlyReadyPort !== undefined);
  earlyReady.supervisor.drainOutboundTurn();
  earlyReadyPort.emit(engineHello(1n));
  earlyReadyPort.emit(ready(1n));
  assert.equal(earlyReady.supervisor.processInboundTurn(), 1);
  assert.equal(earlyReady.supervisor.state, "Failed");
  assert.equal(earlyReadyPort.sent.length, 1);
});

test("coalesces viewport generations while preserving wire sequence order", () => {
  const { supervisor, ports } = fixture(
    defaultLimits({ maxViewportCommands: 1 }),
  );
  supervisor.start();
  const port = ports[0];
  assert.ok(port !== undefined);
  handshake(supervisor, port);
  supervisor.takeEvents();
  openSession(supervisor, port);
  supervisor.takeEvents();

  for (const generation of [1n, 2n, 3n]) {
    supervisor.submit(
      viewportCommand(generation),
      { session: 20n, generation },
    );
  }
  assert.equal(supervisor.queueDepths.viewportCommands, 1);
  assert.equal(supervisor.drainOutboundTurn(), 1);
  const viewport = decodedCommand(port.sent.at(-1));
  assert.equal(viewport.command.type, "SetViewport");
  if (viewport.command.type === "SetViewport") {
    assert.equal(viewport.command.payload.viewport.generation, 3n);
  }
  assert.equal(viewport.header.sequence, 6n);

  supervisor.submit(
    viewportCommand(4n),
    { session: 20n, generation: 4n },
  );
  supervisor.submit(
    { type: "CloseSession", payload: {} },
    { session: 20n },
  );
  assert.equal(supervisor.queueDepths.viewportCommands, 0);
  assert.equal(supervisor.queueDepths.criticalCommands, 1);
  assert.equal(supervisor.drainOutboundTurn(), 1);
  assert.equal(messageType(port.sent.at(-1)), "CloseSession");
  assert.equal(sentHeader(port.sent.at(-1)).sequence, 8n);
});

test("unsent commands cannot authorize guessed Worker outcomes", () => {
  const { supervisor, ports } = fixture();
  supervisor.start();
  const port = ports[0];
  assert.ok(port !== undefined);
  handshake(supervisor, port);
  supervisor.takeEvents();

  supervisor.submit(openCommand(), { request: 10n });
  assert.equal(supervisor.queueDepths.ordinaryCommands, 1);
  port.emit(
    eventFrame(
      {
        type: "DocumentReady",
        payload: {
          session: 20n,
          document_revision: 1n,
          page_count: 1,
          profile: CapabilityProfileId.BaselineNative,
          policy_version: 1,
        },
      },
      { worker: 1n, session: 20n, request: 10n },
      3n,
    ),
  );

  assert.equal(supervisor.processInboundTurn(), 0);
  assert.equal(supervisor.state, "Failed");
  assert.deepEqual(supervisor.takeEvents(), [{
    type: "WorkerFault",
    worker: 1n,
    origin: "HostTransport",
    code: "ProtocolViolation",
  }]);
});

test("event delivery preserves retained wire sequence across coalescing classes", () => {
  const { supervisor, ports } = fixture();
  supervisor.start();
  const port = ports[0];
  assert.ok(port !== undefined);
  handshake(supervisor, port);
  supervisor.takeEvents();
  openSession(supervisor, port);
  supervisor.takeEvents();
  supervisor.submit(
    viewportCommand(1n),
    { session: 20n, generation: 1n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);

  port.emit(
    eventFrame(
      capabilityReported(),
      { worker: 1n, session: 20n, generation: 1n },
      5n,
    ),
  );
  port.emit(
    eventFrame(
      generationCompleted(),
      { worker: 1n, session: 20n, generation: 1n },
      6n,
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 2);
  assert.deepEqual(
    supervisor.takeEvents().map((event) =>
      event.type === "ProtocolEvent"
        ? event.value.envelope.header.sequence
        : undefined,
    ),
    [5n, 6n],
  );
});

test("progress replacement is ordered by the replacement sequence", () => {
  const { supervisor, ports } = fixture();
  supervisor.start();
  const port = ports[0];
  assert.ok(port !== undefined);
  handshake(supervisor, port);
  supervisor.takeEvents();
  openSession(supervisor, port);
  supervisor.takeEvents();
  supervisor.submit(
    viewportCommand(1n),
    { session: 20n, generation: 1n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);

  for (const [event, sequence] of [
    [capabilityReported(), 5n],
    [generationPlanned(1n), 6n],
    [capabilityReported(), 7n],
  ] as const) {
    port.emit(
      eventFrame(
        event,
        { worker: 1n, session: 20n, generation: 1n },
        sequence,
      ),
    );
  }
  assert.equal(supervisor.processInboundTurn(), 3);
  assert.deepEqual(
    supervisor.takeEvents().map((event) =>
      event.type === "ProtocolEvent"
        ? event.value.envelope.header.sequence
        : undefined,
    ),
    [6n, 7n],
  );
});

test("generation completion is terminal for every later publication", () => {
  const { supervisor, ports } = fixture();
  supervisor.start();
  const port = ports[0];
  assert.ok(port !== undefined);
  handshake(supervisor, port);
  supervisor.takeEvents();
  openSession(supervisor, port);
  supervisor.takeEvents();
  supervisor.submit(
    viewportCommand(1n),
    { session: 20n, generation: 1n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);

  port.emit(
    eventFrame(
      generationCompleted(),
      { worker: 1n, session: 20n, generation: 1n },
      5n,
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 1);
  supervisor.takeEvents();
  port.emit(
    eventFrame(
      generationCompleted(),
      { worker: 1n, session: 20n, generation: 1n },
      6n,
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 0);
  assert.equal(supervisor.state, "Failed");
});

test("Surface queued before replacement is rechecked and released at dequeue", () => {
  const { supervisor, ports } = fixture();
  supervisor.start();
  const port = ports[0];
  assert.ok(port !== undefined);
  handshake(supervisor, port);
  supervisor.takeEvents();
  openSession(supervisor, port);
  supervisor.takeEvents();

  supervisor.submit(
    viewportCommand(1n),
    { session: 20n, generation: 1n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);
  port.emit(
    eventFrame(
      surfaceReady(1n),
      { worker: 1n, session: 20n, generation: 1n },
      5n,
      [new ArrayBuffer(16)],
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 1);
  assert.equal(supervisor.queueDepths.criticalEvents, 1);

  supervisor.submit(
    viewportCommand(2n),
    { session: 20n, generation: 2n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);
  assert.deepEqual(supervisor.takeEvents(), []);
  assert.equal(supervisor.queueDepths.criticalEvents, 0);
  assert.equal(supervisor.queueDepths.criticalCommands, 1);

  assert.equal(supervisor.drainOutboundTurn(), 1);
  const release = decodedCommand(port.sent.at(-1));
  assert.equal(release.command.type, "ReleaseSurface");
  if (release.command.type === "ReleaseSurface") {
    assert.equal(release.command.payload.surface, 90n);
    assert.equal(release.command.payload.lease_token, 91n);
  }
  assert.deepEqual(release.correlation, {
    worker: 1n,
    session: 20n,
  });
});

test("queued Surface remains deliverable after its generation completes", () => {
  const { supervisor, ports } = fixture();
  supervisor.start();
  const port = ports[0];
  assert.ok(port !== undefined);
  handshake(supervisor, port);
  supervisor.takeEvents();
  openSession(supervisor, port);
  supervisor.takeEvents();

  supervisor.submit(
    viewportCommand(1n),
    { session: 20n, generation: 1n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);
  port.emit(
    eventFrame(
      surfaceReady(1n),
      { worker: 1n, session: 20n, generation: 1n },
      5n,
      [new ArrayBuffer(16)],
    ),
  );
  port.emit(
    eventFrame(
      generationCompleted(),
      { worker: 1n, session: 20n, generation: 1n },
      6n,
    ),
  );

  assert.equal(supervisor.processInboundTurn(), 2);
  assert.deepEqual(
    supervisor.takeEvents().map((event) =>
      event.type === "ProtocolEvent"
        ? event.value.envelope.event.type
        : event.type,
    ),
    ["SurfaceReady", "GenerationCompleted"],
  );
  assert.equal(supervisor.queueDepths.criticalCommands, 0);
});

test("queued Surface is released after its generation is cancelled or fails", () => {
  for (
    const status of [
      GenerationCompletionStatus.Cancelled,
      GenerationCompletionStatus.Failed,
    ]
  ) {
    const { supervisor, ports } = fixture();
    supervisor.start();
    const port = ports[0];
    assert.ok(port !== undefined);
    handshake(supervisor, port);
    supervisor.takeEvents();
    openSession(supervisor, port);
    supervisor.takeEvents();

    supervisor.submit(
      viewportCommand(1n),
      { session: 20n, generation: 1n },
    );
    assert.equal(supervisor.drainOutboundTurn(), 1);
    port.emit(
      eventFrame(
        surfaceReady(1n),
        { worker: 1n, session: 20n, generation: 1n },
        5n,
        [new ArrayBuffer(16)],
      ),
    );
    port.emit(
      eventFrame(
        generationCompleted(status),
        { worker: 1n, session: 20n, generation: 1n },
        6n,
      ),
    );

    assert.equal(supervisor.processInboundTurn(), 2);
    assert.deepEqual(
      supervisor.takeEvents().map((event) =>
        event.type === "ProtocolEvent"
          ? event.value.envelope.event.type
          : event.type,
      ),
      ["GenerationCompleted"],
    );
    assert.equal(supervisor.queueDepths.criticalCommands, 1);
    assert.equal(supervisor.drainOutboundTurn(), 1);
    assert.equal(
      decodedCommand(port.sent.at(-1)).command.type,
      "ReleaseSurface",
    );
  }
});

test("queued Surface is dropped after an active-generation reclaim", () => {
  const { supervisor, ports } = fixture();
  supervisor.start();
  const port = ports[0];
  assert.ok(port !== undefined);
  handshake(supervisor, port);
  supervisor.takeEvents();
  openSession(supervisor, port);
  supervisor.takeEvents();
  supervisor.submit(
    viewportCommand(1n),
    { session: 20n, generation: 1n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);
  port.emit(
    eventFrame(
      surfaceReady(1n),
      { worker: 1n, session: 20n, generation: 1n },
      5n,
      [new ArrayBuffer(16)],
    ),
  );
  port.emit(
    eventFrame(
      surfaceReclaimed(),
      { worker: 1n, session: 20n },
      6n,
    ),
  );

  assert.equal(supervisor.processInboundTurn(), 2);
  assert.deepEqual(
    supervisor.takeEvents().map((event) =>
      event.type === "ProtocolEvent"
        ? event.value.envelope.event.type
        : event.type,
    ),
    ["SurfaceReclaimed"],
  );
  assert.equal(supervisor.queueDepths.criticalCommands, 0);
  assert.equal(supervisor.state, "Ready");
});

test("reclaimed queued Surface stays dropped after WorkerStopped", () => {
  const { supervisor, ports } = fixture();
  supervisor.start();
  const port = ports[0];
  assert.ok(port !== undefined);
  handshake(supervisor, port);
  supervisor.takeEvents();
  openSession(supervisor, port);
  supervisor.takeEvents();
  supervisor.submit(
    viewportCommand(1n),
    { session: 20n, generation: 1n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);
  port.emit(
    eventFrame(
      surfaceReady(1n),
      { worker: 1n, session: 20n, generation: 1n },
      5n,
      [new ArrayBuffer(16)],
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 1);

  supervisor.submit(
    { type: "Shutdown", payload: { deadline_ms: 25 } },
    {},
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);
  port.emit(
    eventFrame(
      surfaceReclaimed(SurfaceReclaimReason.SessionClosed),
      { worker: 1n, session: 20n },
      6n,
    ),
  );
  port.emit(
    eventFrame(
      { type: "WorkerStopped", payload: { worker: 1n } },
      { worker: 1n },
      7n,
    ),
  );

  assert.equal(supervisor.processInboundTurn(), 2);
  assert.equal(supervisor.state, "Stopped");
  assert.deepEqual(
    supervisor.takeEvents().map((event) =>
      event.type === "ProtocolEvent"
        ? event.value.envelope.event.type
        : event.type,
    ),
    ["SurfaceReclaimed", "WorkerStopped"],
  );
  assert.equal(supervisor.state, "Stopped");
  assert.equal(supervisor.queueDepths.criticalCommands, 0);
});

test("accepts exactly one Superseded terminal from the prior generation", () => {
  const { supervisor, port } = replacedGenerationFixture();
  port.emit(
    eventFrame(
      generationCompleted(GenerationCompletionStatus.Superseded),
      { worker: 1n, session: 20n, generation: 1n },
      5n,
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 1);
  assert.equal(supervisor.state, "Ready");
  const event = supervisor.takeEvents()[0];
  assert.equal(event?.type, "ProtocolEvent");
  if (event?.type === "ProtocolEvent") {
    assert.equal(
      event.value.envelope.event.type,
      "GenerationCompleted",
    );
  }

  port.emit(
    eventFrame(
      generationCompleted(GenerationCompletionStatus.Superseded),
      { worker: 1n, session: 20n, generation: 1n },
      6n,
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 0);
  assert.equal(supervisor.state, "Failed");
});

test("accepts multiple displaced generation terminals out of order", () => {
  const { supervisor, port } = replacedGenerationFixture();
  supervisor.submit(
    viewportCommand(3n),
    { session: 20n, generation: 3n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);
  port.emit(
    eventFrame(
      generationCompleted(GenerationCompletionStatus.Superseded),
      { worker: 1n, session: 20n, generation: 2n },
      5n,
    ),
  );
  port.emit(
    eventFrame(
      generationCompleted(GenerationCompletionStatus.Superseded),
      { worker: 1n, session: 20n, generation: 1n },
      6n,
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 2);
  assert.equal(supervisor.state, "Ready");
  assert.deepEqual(
    supervisor.takeEvents().map((event) =>
      event.type === "ProtocolEvent"
        ? event.value.envelope.correlation.generation
        : undefined,
    ),
    [2n, 1n],
  );
});

test("drains displaced terminals before closing the session", () => {
  const { supervisor, port } = replacedGenerationFixture();
  supervisor.submit(
    { type: "CloseSession", payload: {} },
    { session: 20n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);
  port.emit(
    eventFrame(
      generationCompleted(GenerationCompletionStatus.Superseded),
      { worker: 1n, session: 20n, generation: 1n },
      5n,
    ),
  );
  port.emit(
    eventFrame(
      generationCompleted(GenerationCompletionStatus.Superseded),
      { worker: 1n, session: 20n, generation: 2n },
      6n,
    ),
  );
  port.emit(
    eventFrame(
      { type: "SessionClosed", payload: { session: 20n } },
      { worker: 1n, session: 20n },
      7n,
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 3);
  assert.equal(supervisor.state, "Ready");

  port.emit(
    eventFrame(
      generationCompleted(GenerationCompletionStatus.Superseded),
      { worker: 1n, session: 20n, generation: 1n },
      8n,
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 0);
  assert.equal(supervisor.state, "Failed");
});

test("drains displaced terminals before WorkerStopped on shutdown", () => {
  const { supervisor, port } = replacedGenerationFixture();
  supervisor.submit(
    { type: "Shutdown", payload: { deadline_ms: 25 } },
    {},
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);
  assert.equal(supervisor.state, "Draining");
  port.emit(
    eventFrame(
      generationCompleted(GenerationCompletionStatus.Superseded),
      { worker: 1n, session: 20n, generation: 1n },
      5n,
    ),
  );
  port.emit(
    eventFrame(
      generationCompleted(GenerationCompletionStatus.Superseded),
      { worker: 1n, session: 20n, generation: 2n },
      6n,
    ),
  );
  port.emit(
    eventFrame(
      { type: "WorkerStopped", payload: { worker: 1n } },
      { worker: 1n },
      7n,
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 3);
  assert.equal(supervisor.state, "Stopped");
  assert.equal(supervisor.hasLiveWorker, false);
});

test("backpressures viewport replacement before the terminal ledger fills", () => {
  const { supervisor, ports } = fixture(
    defaultLimits({ maxCriticalEvents: 2 }),
  );
  supervisor.start();
  const port = ports[0];
  assert.ok(port !== undefined);
  handshake(supervisor, port);
  supervisor.takeEvents();
  openSession(supervisor, port);
  supervisor.takeEvents();
  supervisor.submit(
    viewportCommand(1n),
    { session: 20n, generation: 1n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);
  supervisor.submit(
    viewportCommand(2n),
    { session: 20n, generation: 2n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);
  supervisor.submit(
    viewportCommand(3n),
    { session: 20n, generation: 3n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);

  const sentBeforeBackpressure = port.sent.length;
  assertSupervisorError(
    () =>
      supervisor.submit(
        viewportCommand(4n),
        { session: 20n, generation: 4n },
      ),
    "QueueFull",
  );
  assert.equal(supervisor.queueDepths.viewportCommands, 0);
  assert.equal(supervisor.drainOutboundTurn(), 0);
  assert.equal(port.sent.length, sentBeforeBackpressure);

  port.emit(
    eventFrame(
      generationCompleted(GenerationCompletionStatus.Superseded),
      { worker: 1n, session: 20n, generation: 1n },
      5n,
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 1);
  supervisor.takeEvents();
  supervisor.submit(
    viewportCommand(4n),
    { session: 20n, generation: 4n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);
});

test("rejects every non-terminal publication from a prior generation", () => {
  const cases = [
    {
      name: "CapabilityReported",
      event: capabilityReported(),
      resources: [],
    },
    {
      name: "GenerationPlanned",
      event: generationPlanned(1n),
      resources: [],
    },
    {
      name: "SurfaceReady",
      event: surfaceReady(1n),
      resources: [new ArrayBuffer(16)],
    },
    {
      name: "non-Superseded GenerationCompleted",
      event: generationCompleted(),
      resources: [],
    },
  ] as const;

  for (const stale of cases) {
    const { supervisor, port } = replacedGenerationFixture();
    port.emit(
      eventFrame(
        stale.event,
        { worker: 1n, session: 20n, generation: 1n },
        5n,
        stale.resources,
      ),
    );
    assert.equal(supervisor.processInboundTurn(), 0, stale.name);
    assert.equal(supervisor.state, "Failed", stale.name);
  }
});

test("replayable close on a closed session preserves the send sequencer", () => {
  const { supervisor, ports } = fixture();
  supervisor.start();
  const port = ports[0];
  assert.ok(port !== undefined);
  handshake(supervisor, port);
  supervisor.takeEvents();
  openSession(supervisor, port);
  supervisor.takeEvents();

  supervisor.submit(
    { type: "CloseSession", payload: {} },
    { session: 20n },
  );
  assert.equal(supervisor.drainOutboundTurn(), 1);
  port.emit(
    eventFrame(
      { type: "SessionClosed", payload: { session: 20n } },
      { worker: 1n, session: 20n },
      5n,
    ),
  );
  assert.equal(supervisor.processInboundTurn(), 1);
  supervisor.takeEvents();

  supervisor.submit(
    { type: "CloseSession", payload: {} },
    { session: 20n },
  );
  supervisor.submit(
    { type: "Shutdown", payload: { deadline_ms: 25 } },
    {},
  );
  assert.equal(supervisor.drainOutboundTurn(), 2);
  assert.deepEqual(
    port.sent.slice(-2).map((sent) => sentHeader(sent).sequence),
    [5n, 6n],
  );
  assert.deepEqual(
    port.sent.slice(-2).map((sent) => messageType(sent)),
    ["CloseSession", "Shutdown"],
  );
  assert.equal(supervisor.state, "Draining");
});

test("keeps ordinary capacity independent and leaves rejected work unqueued", () => {
  const { supervisor, ports } = fixture(
    defaultLimits({ maxOrdinaryCommands: 1 }),
  );
  supervisor.start();
  const port = ports[0];
  assert.ok(port !== undefined);
  handshake(supervisor, port);

  supervisor.submit(openCommand(), { request: 10n });
  assertSupervisorError(
    () => supervisor.submit(openCommand(), { request: 11n }),
    "QueueFull",
  );
  assert.equal(supervisor.queueDepths.ordinaryCommands, 1);
  assert.equal(supervisor.drainOutboundTurn(), 1);
  assert.equal(sentHeader(port.sent.at(-1)).sequence, 3n);
});

test("duplicate traffic fails one epoch and late old callbacks miss restart", () => {
  const { supervisor, ports } = fixture();
  supervisor.start();
  const first = ports[0];
  assert.ok(first !== undefined);
  handshake(supervisor, first);
  supervisor.takeEvents();

  first.emit(ready(1n));
  assert.equal(supervisor.processInboundTurn(), 0);
  assert.equal(supervisor.state, "Failed");
  assert.equal(first.terminateCount, 1);
  assert.deepEqual(
    supervisor.takeEvents(),
    [{
      type: "WorkerFault",
      worker: 1n,
      origin: "HostTransport",
      code: "ProtocolViolation",
    }],
  );

  supervisor.restart();
  const second = ports[1];
  assert.ok(second !== undefined);
  assert.equal(supervisor.worker, 2n);
  first.emit(engineHello(1n));
  assert.equal(supervisor.queueDepths.inbound, 0);
  assert.equal(supervisor.drainOutboundTurn(), 1);
  second.emit(engineHello(2n));
  assert.equal(supervisor.processInboundTurn(), 1);
});

test("schema mismatch, callback saturation, and port errors are bounded faults", () => {
  const mismatched = fixture();
  mismatched.supervisor.start();
  const mismatchPort = mismatched.ports[0];
  assert.ok(mismatchPort !== undefined);
  mismatched.supervisor.drainOutboundTurn();
  const badHash = SCHEMA_HASH.slice();
  badHash[0] = (badHash[0] ?? 0) ^ 0xff;
  mismatchPort.emit(engineHello(1n, badHash));
  assert.equal(mismatched.supervisor.processInboundTurn(), 0);
  assert.equal(mismatched.supervisor.state, "Failed");

  const saturated = fixture(
    defaultLimits({ maxInboundEvents: 1 }),
  );
  saturated.supervisor.start();
  const saturatedPort = saturated.ports[0];
  assert.ok(saturatedPort !== undefined);
  saturated.supervisor.drainOutboundTurn();
  saturatedPort.emit(engineHello(1n));
  saturatedPort.emit(engineHello(1n));
  assert.equal(saturated.supervisor.processInboundTurn(), 0);
  assert.deepEqual(
    saturated.supervisor.takeEvents(),
    [{
      type: "WorkerFault",
      worker: 1n,
      origin: "HostTransport",
      code: "InboundQueueOverflow",
    }],
  );

  const errored = fixture();
  errored.supervisor.start();
  const erroredPort = errored.ports[0];
  assert.ok(erroredPort !== undefined);
  erroredPort.emitError();
  assert.equal(errored.supervisor.state, "Starting");
  errored.supervisor.processInboundTurn();
  assert.equal(errored.supervisor.state, "Failed");
  assert.deepEqual(
    errored.supervisor.takeEvents(),
    [{
      type: "WorkerFault",
      worker: 1n,
      origin: "HostTransport",
      code: "WorkerError",
    }],
  );
});

test("Ready transcript mismatch is rejected before entering Ready", () => {
  const { supervisor, ports } = fixture();
  supervisor.start();
  const port = ports[0];
  assert.ok(port !== undefined);
  assert.equal(supervisor.drainOutboundTurn(), 1);
  port.emit(engineHello(1n));
  assert.equal(supervisor.processInboundTurn(), 1);
  assert.equal(supervisor.drainOutboundTurn(), 1);

  port.emit(ready(1n, 1n));
  assert.equal(supervisor.processInboundTurn(), 0);
  assert.equal(supervisor.state, "Failed");
});

test("virtual clock covers startup, shutdown, and graceful terminal cleanup", () => {
  const startup = fixture();
  startup.supervisor.start();
  const startupPort = startup.ports[0];
  assert.ok(startupPort !== undefined);
  startup.clock.advance(1_000);
  startup.supervisor.pollClock();
  assert.equal(startup.supervisor.state, "Failed");
  assert.equal(startupPort.terminateCount, 1);
  assert.equal(startup.supervisor.takeEvents()[0]?.type, "WorkerFault");

  const graceful = fixture();
  graceful.supervisor.start();
  const gracefulPort = graceful.ports[0];
  assert.ok(gracefulPort !== undefined);
  handshake(graceful.supervisor, gracefulPort);
  graceful.supervisor.takeEvents();
  graceful.supervisor.submit(
    { type: "Shutdown", payload: { deadline_ms: 25 } },
    {},
  );
  assert.equal(graceful.supervisor.state, "Ready");
  assert.equal(graceful.supervisor.drainOutboundTurn(), 1);
  assert.equal(graceful.supervisor.state, "Draining");
  gracefulPort.emit(
    eventFrame(
      { type: "WorkerStopped", payload: { worker: 1n } },
      { worker: 1n },
      3n,
    ),
  );
  assert.equal(graceful.supervisor.processInboundTurn(), 1);
  assert.equal(graceful.supervisor.state, "Stopped");
  assert.equal(graceful.supervisor.hasLiveWorker, false);
  assert.equal(gracefulPort.terminateCount, 1);
  assert.deepEqual(graceful.supervisor.queueDepths, {
    inbound: 0,
    criticalCommands: 0,
    ordinaryCommands: 0,
    viewportCommands: 0,
    criticalEvents: 1,
    progressEvents: 0,
  });

  const timedOut = fixture();
  timedOut.supervisor.start();
  const timedOutPort = timedOut.ports[0];
  assert.ok(timedOutPort !== undefined);
  handshake(timedOut.supervisor, timedOutPort);
  timedOut.supervisor.submit(
    { type: "Shutdown", payload: { deadline_ms: 25 } },
    {},
  );
  timedOut.clock.advance(25);
  timedOut.supervisor.pollClock();
  assert.equal(timedOut.supervisor.state, "Ready");
  assert.equal(timedOut.supervisor.drainOutboundTurn(), 1);
  timedOut.clock.advance(25);
  timedOut.supervisor.pollClock();
  assert.equal(timedOut.supervisor.state, "Failed");
  assert.deepEqual(
    timedOut.supervisor.takeEvents(),
    [{
      type: "WorkerFault",
      worker: 1n,
      origin: "HostTransport",
      code: "ShutdownTimeout",
    }],
  );
});
