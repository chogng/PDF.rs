import assert from "node:assert/strict";
import test from "node:test";

import {
  AlphaMode,
  CapabilityProfileId,
  CapabilityScopeKind,
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
  decodeCommandPayload,
  encodeCapabilityReportedEventPayload,
  encodeCorrelationPayload,
  encodeDocumentReadyEventPayload,
  encodeEngineHelloEventPayload,
  encodeEventPayload,
  encodeGenerationCompletedEventPayload,
  encodeGenerationPlannedEventPayload,
  encodeNeedDataEventPayload,
  encodeReadyEventPayload,
  encodeRequestFailedEventPayload,
  encodeSurfaceReadyEventPayload,
  encodeWorkerStoppedEventPayload,
  type Correlation,
  type EnvelopeHeader,
  type Event,
  type EventEnvelope,
  type PayloadCodecResult,
  type SourceDescriptor,
  type SourceIdentity,
  type ViewportRequest,
} from "../generated/engine-protocol.js";
import {
  BROWSER_CONTROL_HEADER_BYTES,
} from "../src/browser-command-boundary.js";
import {
  BrowserReaderClient,
  type BrowserReaderClientConfiguration,
  type BrowserReaderLifecycle,
  type BrowserReaderTurnScheduler,
} from "../src/browser-reader-client.js";
import {
  deriveBrowserHttpValidatorBinding,
  type BrowserHttpHeaders,
  type BrowserHttpRequest,
  type BrowserHttpResponse,
  type BrowserLocalReadRequest,
  type BrowserSourceAbortFactory,
  type BrowserSourceAbortHandle,
} from "../src/browser-source-bridge.js";
import type {
  BrowserArrayBufferAdapter,
  BrowserImageBitmapAdapter,
  BrowserSharedArrayBufferAdapter,
  BrowserSurfaceAdapters,
  BrowserWasmMemoryDetector,
} from "../src/browser-surface-bridge.js";
import type {
  BrowserViewerEngineHandlers,
  BrowserViewerFailure,
  BrowserViewerFocusSnapshot,
  BrowserViewerFrameScheduler,
  BrowserViewerHostObservations,
  BrowserViewerObservationHandlers,
  BrowserViewerPresentation,
  BrowserViewerSurface,
} from "../src/browser-viewer.js";
import {
  BrowserViewer,
} from "../src/browser-viewer.js";
import {
  createBrowserHostHello,
  type BrowserWorkerClock,
  type BrowserWorkerHandlers,
  type BrowserWorkerPort,
} from "../src/browser-worker-supervisor.js";

const SOURCE_ID = new Uint8Array(32).fill(0x51);
const SOURCE_VALIDATOR = new Uint8Array(32).fill(0x52);
const HTTP_ETAG = "\"pdf-rs-reader-v1\"";
const REGION = Object.freeze({
  page_index: 0,
  x: 0,
  y: 0,
  width: 2,
  height: 2,
  coordinate_space: SurfaceCoordinateSpace.DevicePixelsTopLeft,
});

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
}

class FakeTurns implements BrowserReaderTurnScheduler {
  #next = 1;
  readonly #callbacks = new Map<number, () => void>();

  get pending(): number {
    return this.#callbacks.size;
  }

  request(callback: () => void): number {
    const handle = this.#next;
    this.#next += 1;
    this.#callbacks.set(handle, callback);
    return handle;
  }

  cancel(handle: number): void {
    this.#callbacks.delete(handle);
  }

  flush(maximum = 100): number {
    let completed = 0;
    while (completed < maximum) {
      const entry = this.#callbacks.entries().next().value as
        | [number, () => void]
        | undefined;
      if (entry === undefined) {
        return completed;
      }
      this.#callbacks.delete(entry[0]);
      entry[1]();
      completed += 1;
    }
    throw new Error("turn scheduler did not quiesce");
  }
}

class FakeViewerFrames implements BrowserViewerFrameScheduler {
  #next = 1;
  readonly #callbacks = new Map<number, () => void>();

  request(callback: () => void): number {
    const handle = this.#next;
    this.#next += 1;
    this.#callbacks.set(handle, callback);
    return handle;
  }

  cancel(handle: number): void {
    this.#callbacks.delete(handle);
  }

  runNext(): void {
    const entry = this.#callbacks.entries().next().value as
      | [number, () => void]
      | undefined;
    assert.notEqual(entry, undefined);
    if (entry === undefined) {
      return;
    }
    this.#callbacks.delete(entry[0]);
    entry[1]();
  }
}

class FakeViewerPresentation implements BrowserViewerPresentation {
  readonly current = new Map<bigint, BrowserViewerSurface>();
  readonly failures: BrowserViewerFailure[] = [];

  present(surface: BrowserViewerSurface): void {
    this.current.set(surface.metadata.id, surface);
  }

  clear(): void {
    this.current.clear();
  }

  showFailure(failure: BrowserViewerFailure): void {
    this.failures.push(failure);
  }

  clearFailure(): void {
    this.failures.length = 0;
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

class FakeAbort implements BrowserSourceAbortHandle {
  readonly signal = {};
  aborted = false;

  abort(): void {
    this.aborted = true;
  }
}

class FakeAborts implements BrowserSourceAbortFactory {
  readonly handles: FakeAbort[] = [];

  create(): BrowserSourceAbortHandle {
    const handle = new FakeAbort();
    this.handles.push(handle);
    return handle;
  }
}

class FakeHeaders implements BrowserHttpHeaders {
  readonly #values: Readonly<Record<string, string>>;

  constructor(values: Readonly<Record<string, string>>) {
    this.#values = values;
  }

  get(name: string): string | null {
    const wanted = name.toLowerCase();
    const entry = Object.entries(this.#values).find(
      ([candidate]) => candidate.toLowerCase() === wanted,
    );
    return entry?.[1] ?? null;
  }
}

class SurfaceAdapters {
  releases = 0;

  readonly adapters: BrowserSurfaceAdapters;

  constructor(readonly throwOnRelease = false) {
    const imageBitmap: BrowserImageBitmapAdapter = {
      isResource: (): boolean => false,
      describe: () => undefined,
      adopt: (): object => {
        throw new Error("unused");
      },
      close: (): void => undefined,
    };
    const arrayBuffer: BrowserArrayBufferAdapter = {
      isResource: (value: unknown): boolean =>
        value instanceof ArrayBuffer,
      describe: (value: unknown) => value instanceof ArrayBuffer
        ? Object.freeze({
            byteLength: BigInt(value.byteLength),
            fixedLength: true,
            exclusive: true,
            backingIdentity: value,
            receiverOwned: true,
          })
        : undefined,
      adoptReadOnly: (
        value: unknown,
        byteOffset: bigint,
        byteLength: bigint,
      ): object => Object.freeze({ value, byteOffset, byteLength }),
      release: (): void => {
        this.releases += 1;
        if (this.throwOnRelease) {
          throw new Error("injected release failure");
        }
      },
    };
    const sharedArrayBuffer: BrowserSharedArrayBufferAdapter = {
      isResource: (): boolean => false,
      describe: () => undefined,
      loadPublicationEpoch: (): number => 0,
      adoptReadOnly: (): object => {
        throw new Error("unused");
      },
      release: (): void => undefined,
    };
    const wasmMemory: BrowserWasmMemoryDetector = {
      isMemory: (): boolean => false,
    };
    this.adapters = Object.freeze({
      imageBitmap,
      arrayBuffer,
      sharedArrayBuffer,
      wasmMemory,
    });
  }
}

const sourceIdentity = (): SourceIdentity => Object.freeze({
  stable_id: SOURCE_ID.slice(),
  revision: 1n,
});

const localDescriptor = (): SourceDescriptor => Object.freeze({
  identity: sourceIdentity(),
  length: 16n,
  validator: SOURCE_VALIDATOR.slice(),
});

const viewport = (
  generation: bigint,
  quality: QualityPolicy,
): ViewportRequest => Object.freeze({
  generation,
  document_revision: 1n,
  annotation_revision: 0n,
  zoom_numerator: 1,
  zoom_denominator: 1,
  visible_pages: [],
  quality,
  output_profile: OutputProfile.Srgb,
  device_scale_milli: 1_000,
  rotation: PageRotation.Degrees0,
  optional_content_id: 0n,
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
    case "RequestFailed":
      return unwrap(encodeRequestFailedEventPayload(event.payload));
    case "GenerationPlanned":
      return unwrap(
        encodeGenerationPlannedEventPayload(event.payload),
      );
    case "SurfaceReady":
      return unwrap(encodeSurfaceReadyEventPayload(event.payload));
    case "GenerationCompleted":
      return unwrap(
        encodeGenerationCompletedEventPayload(event.payload),
      );
    case "WorkerStopped":
      return unwrap(encodeWorkerStoppedEventPayload(event.payload));
    default:
      throw new Error("unsupported reader test event");
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

const engineHello = (worker: bigint): unknown[] =>
  eventFrame(
    {
      type: "EngineHello",
      payload: {
        hello: {
          major: PROTOCOL_MAJOR,
          minor: PROTOCOL_MINOR,
          schema_hash: SCHEMA_HASH.slice(),
          endpoint_role: EndpointRole.Engine,
          capabilities: {
            supported: EndpointCapability.TransferableArrayBuffer,
            mandatory: 0n,
          },
          max_message_bytes: MAX_MESSAGE_BYTES,
          max_transfer_slots: MAX_TRANSFER_SLOTS,
        },
        execution_capabilities: { supported: 0n },
      },
    },
    { worker },
    1n,
  );

const ready = (worker: bigint): unknown[] =>
  eventFrame(
    {
      type: "Ready",
      payload: {
        worker,
        negotiated_minor: PROTOCOL_MINOR,
        schema_hash: SCHEMA_HASH.slice(),
        execution_capabilities: { supported: 0n },
        capability_profiles: [CapabilityProfileId.BaselineNative],
        output_profiles: [OutputProfile.Srgb],
      },
    },
    { worker },
    2n,
  );

const needData = (
  worker: bigint,
  session: bigint,
  request: bigint,
  sequence: bigint,
  start = 0n,
  len = 4n,
): unknown[] =>
  eventFrame(
    {
      type: "NeedData",
      payload: {
        ticket: sequence,
        source: sourceIdentity(),
        ranges: [{ start, len }],
        priority: DataPriority.VisiblePage,
        checkpoint: sequence,
      },
    },
    { worker, session, request },
    sequence,
  );

const documentReady = (
  worker: bigint,
  session: bigint,
  request: bigint,
  sequence: bigint,
): unknown[] =>
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
    { worker, session, request },
    sequence,
  );

const capabilityReported = (
  worker: bigint,
  session: bigint,
  generation: bigint,
  sequence: bigint,
): unknown[] =>
  eventFrame(
    {
      type: "CapabilityReported",
      payload: {
        decision: {
          decision_schema_version: 1,
          status: SupportStatus.Supported,
          profile: CapabilityProfileId.BaselineNative,
          profile_version: 1,
          policy_version: 1,
          subject: {
            source: sourceIdentity(),
            document_revision: 1n,
            revision_startxref: 10n,
            page_index: 0,
            page_object_number: 1,
            page_object_generation: 0,
            scene_schema_major: 2,
            scene_schema_minor: 0,
            scene_hash: new Uint8Array(32).fill(2),
          },
          missing: [],
          missing_total: 0,
          missing_completeness: CollectionCompleteness.Complete,
          contributors: [],
          contributors_total: 0,
          contributors_completeness:
            CollectionCompleteness.Complete,
          locations_total: 0,
          locations_completeness: CollectionCompleteness.Complete,
          evaluated_requirements: 0,
          evaluated_dependencies: 0,
          evaluated_parameters: 0,
          evaluated_commands: 0,
          evaluated_resources: 0,
          scope: {
            kind: CapabilityScopeKind.Page,
            page: 0,
          },
        },
        decision_hash: new Uint8Array(32).fill(3),
      },
    },
    { worker, session, generation },
    sequence,
  );

const requestFailed = (
  worker: bigint,
  session: bigint,
  request: bigint,
  sequence: bigint,
): unknown[] =>
  eventFrame(
    {
      type: "RequestFailed",
      payload: {
        error: {
          code: EngineErrorCode.SourceUnavailable,
          category: ErrorCategory.Source,
          severity: ErrorSeverity.Recoverable,
          recoverability: ErrorRecoverability.RetryRequest,
          diagnostic_id: 7n,
        },
      },
    },
    { worker, session, request },
    sequence,
  );

const generationPlanned = (
  worker: bigint,
  session: bigint,
  generation: bigint,
  sequence: bigint,
  quality: QualityPolicy,
): unknown[] =>
  eventFrame(
    {
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
          viewport_clip: { ...REGION },
          zoom_numerator: 1,
          zoom_denominator: 1,
          device_scale_milli: 1_000,
          rotation: PageRotation.Degrees0,
          optional_content: 0n,
          annotation_revision: 0n,
          backend: NativeBackend.FastCpu,
          output_profile: OutputProfile.Srgb,
          quality,
          regions: [{ ...REGION }],
          tile_content_hashes: [
            new Uint8Array(32).fill(5),
          ],
        },
        plan_hash: new Uint8Array(32).fill(6),
      },
    },
    { worker, session, generation },
    sequence,
  );

const surfaceReady = (
  worker: bigint,
  session: bigint,
  generation: bigint,
  sequence: bigint,
  id: bigint,
): unknown[] =>
  eventFrame(
    {
      type: "SurfaceReady",
      payload: {
        metadata: {
          id,
          lease_token: id + 100n,
          owner: { worker, session },
          generation,
          region: { ...REGION },
          width: 2,
          height: 2,
          stride: 8,
          format: PixelFormat.Rgba8,
          alpha: AlphaMode.Straight,
          byte_offset: 0n,
          byte_length: 16n,
          render_config: new Uint8Array(32).fill(1),
          renderer_epoch: 1,
          plan_id: generation,
          plan_hash: new Uint8Array(32).fill(6),
          scene_hash: new Uint8Array(32).fill(2),
          decision_hash: new Uint8Array(32).fill(3),
          backend: NativeBackend.FastCpu,
        },
        transport: {
          kind: "BrowserArrayBuffer",
          slot: 0,
          buffer_length: 16n,
        },
      },
    },
    { worker, session, generation },
    sequence,
    [new ArrayBuffer(16)],
  );

const generationCompleted = (
  worker: bigint,
  session: bigint,
  generation: bigint,
  sequence: bigint,
): unknown[] =>
  eventFrame(
    {
      type: "GenerationCompleted",
      payload: {
        status: GenerationCompletionStatus.Completed,
        produced_regions: 1,
      },
    },
    { worker, session, generation },
    sequence,
  );

const workerStopped = (
  worker: bigint,
  sequence: bigint,
): unknown[] =>
  eventFrame(
    {
      type: "WorkerStopped",
      payload: { worker },
    },
    { worker },
    sequence,
  );

const sentControl = (
  sent: SentMessage | undefined,
): ArrayBuffer => {
  const value = sent?.value[0];
  assert.ok(value instanceof ArrayBuffer);
  return value;
};

const sentHeader = (
  sent: SentMessage | undefined,
): EnvelopeHeader => {
  const view = new DataView(sentControl(sent));
  return {
    major: view.getUint16(0, true),
    minor: view.getUint16(2, true),
    message_type: view.getUint16(4, true),
    flags: view.getUint16(6, true),
    payload_len: view.getUint32(8, true),
    sequence: view.getBigUint64(12, true),
  };
};

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

const commands = (
  port: FakeWorkerPort,
  type: string,
) => port.sent
  .map((sent) => decodedCommand(sent))
  .filter((envelope) => envelope.command.type === type);

interface ReaderFixture {
  readonly client: BrowserReaderClient;
  readonly turns: FakeTurns;
  readonly ports: FakeWorkerPort[];
  readonly aborts: FakeAborts;
  readonly adapters: SurfaceAdapters;
  readonly surfaces: BrowserViewerSurface[];
  readonly faults: string[];
  readonly decisions: number[];
  readonly errors: EngineErrorCode[];
  readonly lifecycles: BrowserReaderLifecycle[];
  readonly readyPages: number[];
}

const configuration = (
  options: Pick<
    BrowserReaderClientConfiguration,
    "source"
  >,
  fixture: Omit<ReaderFixture, "client">,
): BrowserReaderClientConfiguration => ({
  hostHello: createBrowserHostHello(
    EndpointCapability.TransferableArrayBuffer,
  ),
  workerLimits: {
    maxInboundEvents: 16,
    maxCriticalCommands: 16,
    maxOrdinaryCommands: 16,
    maxViewportCommands: 4,
    maxCriticalEvents: 16,
    maxProgressEvents: 16,
    admission: {
      maxSessions: 4,
      maxRequests: 16,
      maxSurfaces: 16,
    },
  },
  workerFactory: () => {
    const port = new FakeWorkerPort();
    fixture.ports.push(port);
    return port;
  },
  clock: new VirtualClock(),
  startupTimeoutMs: 1_000,
  turns: fixture.turns,
  source: options.source,
  aborts: fixture.aborts,
  surfaces: {
    adapters: fixture.adapters.adapters,
    runtimeSupport: {
      imageBitmap: false,
      arrayBuffer: true,
      sharedArrayBuffer: false,
      offscreenCanvasStaging: false,
    },
    crossOriginIsolated: false,
    layout: {
      format: PixelFormat.Rgba8,
      alpha: AlphaMode.Straight,
    },
  },
  limits: {
    maxActorTurn: 64,
    maxAutomaticRestarts: 2,
    shutdownDeadlineMs: 1_000,
    source: {
      maxActiveTickets: 4,
      maxQueuedResults: 8,
      maxTrackedTickets: 16,
      maxBufferedBytes: 1_024,
      maxWholeSourceBytes: 1_024,
      maxDrainTurn: 8,
    },
    surface: {
      maxQueuedCallbacks: 16,
      maxLiveSurfaces: 8,
      maxTrackedLeases: 16,
      maxSessionsPerEpoch: 2,
      maxWorkerEpochs: 4,
      maxPlanRegions: 16,
      maxSurfaceDimension: 1_024,
      maxSurfaceStrideBytes: 4_096,
      maxSurfaceBytes: 1_048_576n,
    },
  },
  status: {
    onDocumentReady: (readyEvent): void => {
      fixture.readyPages.push(readyEvent.page_count);
    },
    onLifecycle: (lifecycle): void => {
      fixture.lifecycles.push(lifecycle);
    },
  },
});

const readerFixture = (
  source: BrowserReaderClientConfiguration["source"],
  attachHandlers = true,
  adapters = new SurfaceAdapters(),
): ReaderFixture => {
  const partial = {
    turns: new FakeTurns(),
    ports: [] as FakeWorkerPort[],
    aborts: new FakeAborts(),
    adapters,
    surfaces: [] as BrowserViewerSurface[],
    faults: [] as string[],
    decisions: [] as number[],
    errors: [] as EngineErrorCode[],
    lifecycles: [] as BrowserReaderLifecycle[],
    readyPages: [] as number[],
  };
  const client = new BrowserReaderClient(
    configuration({ source }, partial),
  );
  const handlers: BrowserViewerEngineHandlers = {
    onSurface: (surface): void => {
      partial.surfaces.push(surface);
    },
    onCapabilityDecision: (decision): void => {
      partial.decisions.push(decision.status);
    },
    onEngineError: (error): void => {
      partial.errors.push(error.code);
    },
    onWorkerFault: (code): void => {
      partial.faults.push(code);
    },
  };
  if (attachHandlers) {
    client.setHandlers(handlers);
  }
  return Object.freeze({ client, ...partial });
};

const completeHandshake = (
  fixture: ReaderFixture,
  port: FakeWorkerPort,
): void => {
  fixture.turns.flush();
  assert.equal(commands(port, "Hello").length, 1);
  port.emit(engineHello(fixture.client.worker));
  fixture.turns.flush();
  assert.equal(commands(port, "HelloAccept").length, 1);
  port.emit(ready(fixture.client.worker));
  assert.equal(fixture.client.pump(1), 1);
  assert.equal(fixture.client.lifecycle, "Starting");
  assert.equal(commands(port, "Open").length, 0);
  assert.equal(fixture.client.pump(1), 1);
  assert.equal(fixture.client.lifecycle, "Opening");
  assert.equal(commands(port, "Open").length, 0);
  fixture.turns.flush();
  assert.equal(commands(port, "Open").length, 1);
};

const settleSource = async (
  fixture: ReaderFixture,
): Promise<void> => {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
  fixture.turns.flush();
};

test("local reader reaches ready, preview/full display, and structured states", async () => {
  const reads: BrowserLocalReadRequest[] = [];
  const fixture = readerFixture({
    descriptor: localDescriptor(),
    acquirer: {
      kind: "Local",
      reader: {
        read: async (request) => {
          reads.push(request);
          return {
            type: "Bytes",
            source: sourceIdentity(),
            range: request.range,
            totalLength: 16n,
            bytes: new ArrayBuffer(Number(request.range.len)),
          };
        },
      },
    },
  });
  const port = fixture.ports[0]!;
  completeHandshake(fixture, port);
  fixture.client.setViewport(viewport(1n, QualityPolicy.Preview));

  port.emit(needData(1n, 20n, 1n, 3n));
  fixture.turns.flush();
  assert.equal(reads.length, 1);
  await settleSource(fixture);
  assert.equal(commands(port, "ProvideData").length, 1);

  port.emit(documentReady(1n, 20n, 1n, 4n));
  fixture.turns.flush();
  assert.equal(fixture.client.lifecycle, "Ready");
  assert.deepEqual(fixture.readyPages, [1]);
  assert.equal(commands(port, "GetPageMetrics").length, 1);
  assert.equal(commands(port, "SetViewport").length, 1);

  port.emit(capabilityReported(1n, 20n, 1n, 5n));
  port.emit(requestFailed(1n, 20n, 2n, 6n));
  port.emit(
    generationPlanned(
      1n,
      20n,
      1n,
      7n,
      QualityPolicy.Preview,
    ),
  );
  port.emit(surfaceReady(1n, 20n, 1n, 8n, 90n));
  fixture.turns.flush();
  assert.deepEqual(fixture.decisions, [SupportStatus.Supported]);
  assert.deepEqual(fixture.errors, [EngineErrorCode.SourceUnavailable]);
  assert.equal(fixture.surfaces.length, 1);
  assert.equal(fixture.surfaces[0]?.metadata.generation, 1n);

  fixture.client.setViewport(viewport(2n, QualityPolicy.Full));
  // This callback was already queued by epoch 1, but Host state has advanced.
  port.emit(surfaceReady(1n, 20n, 1n, 9n, 91n));
  port.emit(generationCompleted(1n, 20n, 1n, 10n));
  fixture.turns.flush();
  assert.equal(fixture.surfaces.length, 1);
  assert.equal(commands(port, "ReleaseSurface").length, 1);
  assert.equal(commands(port, "SetViewport").length, 2);

  port.emit(
    generationPlanned(
      1n,
      20n,
      2n,
      11n,
      QualityPolicy.Full,
    ),
  );
  port.emit(surfaceReady(1n, 20n, 2n, 12n, 92n));
  port.emit(generationCompleted(1n, 20n, 2n, 13n));
  fixture.turns.flush();
  assert.equal(fixture.surfaces.length, 2);
  assert.equal(fixture.surfaces[1]?.metadata.generation, 2n);
  assert.equal(fixture.client.resources.deliveredSurfaces, 1);

  fixture.client.setViewport(viewport(3n, QualityPolicy.Preview));
  fixture.turns.flush();
  port.emit(
    generationPlanned(
      1n,
      20n,
      3n,
      14n,
      QualityPolicy.Preview,
    ),
  );
  port.emit(surfaceReady(1n, 20n, 3n, 15n, 93n));
  fixture.turns.flush();
  assert.equal(fixture.surfaces.length, 3);
  assert.equal(fixture.client.resources.deliveredSurfaces, 1);
  const current = fixture.surfaces[2]!;
  fixture.client.releaseSurface(current);
  fixture.client.releaseSurface(current);
  assert.equal(fixture.client.resources.deliveredSurfaces, 0);
  assert.equal(fixture.client.resources.liveSurfaces, 0);

  fixture.client.close();
  fixture.client.close();
  assert.deepEqual(fixture.client.resources, {
    ports: 0,
    scheduledTurns: 0,
    activeSourceTickets: 0,
    queuedSourceResults: 0,
    bufferedSourceBytes: 0,
    liveSurfaces: 0,
    deliveredSurfaces: 0,
  });
  assert.equal(fixture.turns.pending, 0);
  assert.equal(port.terminateCount, 1);
});

test("immutable URL reader binds Range and If-Range before ProvideData", async () => {
  const validator = Object.freeze({
    kind: "StrongEtag" as const,
    value: HTTP_ETAG,
  });
  const identity = sourceIdentity();
  const descriptor: SourceDescriptor = {
    identity,
    length: 16n,
    validator: deriveBrowserHttpValidatorBinding(identity, validator),
  };
  const requests: BrowserHttpRequest[] = [];
  const fixture = readerFixture({
    descriptor,
    acquirer: {
      kind: "Http",
      url: "https://example.test/immutable.pdf",
      validator,
      fetcher: {
        fetch: async (request): Promise<BrowserHttpResponse> => {
          requests.push(request);
          const body = new ArrayBuffer(4);
          return {
            type: "Response",
            status: 206,
            headers: new FakeHeaders({
              etag: HTTP_ETAG,
              "content-range": "bytes 4-7/16",
              "content-length": "4",
            }),
            source: identity,
            validator: deriveBrowserHttpValidatorBinding(
              identity,
              validator,
            ),
            body,
          };
        },
      },
    },
  });
  const port = fixture.ports[0]!;
  completeHandshake(fixture, port);
  port.emit(needData(1n, 20n, 1n, 3n, 4n, 4n));
  fixture.turns.flush();
  assert.equal(requests.length, 1);
  assert.deepEqual(requests[0]?.headers, {
    Range: "bytes=4-7",
    "If-Range": HTTP_ETAG,
  });
  assert.equal(
    requests[0]?.url,
    "https://example.test/immutable.pdf",
  );
  await settleSource(fixture);
  assert.equal(commands(port, "ProvideData").length, 1);
  fixture.client.close();
  assert.equal(fixture.client.resources.ports, 0);
});

test("Worker crash releases old epoch before fault and reopens latest Host state", async () => {
  const reads: BrowserLocalReadRequest[] = [];
  const fixture = readerFixture({
    descriptor: localDescriptor(),
    acquirer: {
      kind: "Local",
      reader: {
        read: async (request) => {
          reads.push(request);
          return {
            type: "Bytes",
            source: sourceIdentity(),
            range: request.range,
            totalLength: 16n,
            bytes: new ArrayBuffer(Number(request.range.len)),
          };
        },
      },
    },
  });
  const first = fixture.ports[0]!;
  completeHandshake(fixture, first);
  fixture.client.setViewport(viewport(3n, QualityPolicy.Full));
  first.emit(needData(1n, 20n, 1n, 3n));
  fixture.turns.flush();
  await settleSource(fixture);
  first.emit(documentReady(1n, 20n, 1n, 4n));
  fixture.turns.flush();
  first.emit(
    generationPlanned(
      1n,
      20n,
      3n,
      5n,
      QualityPolicy.Full,
    ),
  );
  first.emit(surfaceReady(1n, 20n, 3n, 6n, 90n));
  fixture.turns.flush();
  assert.equal(fixture.client.resources.deliveredSurfaces, 1);

  first.emitError();
  fixture.turns.flush();
  assert.deepEqual(fixture.faults, ["WorkerError"]);
  assert.equal(fixture.client.restartCount, 1);
  assert.equal(fixture.client.worker, 2n);
  assert.equal(fixture.client.resources.deliveredSurfaces, 0);
  assert.equal(first.terminateCount, 1);
  const second = fixture.ports[1]!;
  assert.equal(commands(second, "Hello").length, 1);

  // A retained callback closure from epoch 1 cannot enqueue into epoch 2.
  const pendingBefore = fixture.turns.pending;
  const releasesBefore = fixture.adapters.releases;
  first.emit(surfaceReady(1n, 20n, 3n, 7n, 91n));
  assert.equal(fixture.turns.pending, pendingBefore);
  assert.equal(fixture.surfaces.length, 1);
  assert.equal(fixture.adapters.releases, releasesBefore + 1);
  const hugeSparseCallback: unknown[] = [];
  hugeSparseCallback.length = 0xffff_ffff;
  hugeSparseCallback[1] = new ArrayBuffer(1);
  first.emit(hugeSparseCallback);
  assert.equal(fixture.turns.pending, pendingBefore);
  assert.equal(fixture.surfaces.length, 1);
  assert.equal(fixture.adapters.releases, releasesBefore + 2);

  second.emit(engineHello(2n));
  fixture.turns.flush();
  second.emit(ready(2n));
  fixture.turns.flush();
  assert.equal(commands(second, "Open").length, 1);
  second.emit(needData(2n, 30n, 1n, 3n));
  fixture.turns.flush();
  await settleSource(fixture);
  second.emit(documentReady(2n, 30n, 1n, 4n));
  fixture.turns.flush();
  const replay = commands(second, "SetViewport").at(-1);
  assert.equal(replay?.command.type, "SetViewport");
  if (replay?.command.type === "SetViewport") {
    assert.equal(replay.command.payload.viewport.generation, 3n);
    assert.equal(
      replay.command.payload.viewport.quality,
      QualityPolicy.Full,
    );
  }
  assert.deepEqual(fixture.readyPages, [1, 1]);
  assert.equal(reads.length, 2);

  fixture.client.close();
  assert.deepEqual(fixture.client.resources, {
    ports: 0,
    scheduledTurns: 0,
    activeSourceTickets: 0,
    queuedSourceResults: 0,
    bufferedSourceBytes: 0,
    liveSurfaces: 0,
    deliveredSurfaces: 0,
  });
  assert.equal(second.terminateCount, 1);
});

test("WorkerStopped independently clears ownership when bridge cleanup throws", async () => {
  const adapters = new SurfaceAdapters(true);
  const fixture = readerFixture({
    descriptor: localDescriptor(),
    acquirer: {
      kind: "Local",
      reader: {
        read: async (request) => ({
          type: "Bytes",
          source: sourceIdentity(),
          range: request.range,
          totalLength: 16n,
          bytes: new ArrayBuffer(Number(request.range.len)),
        }),
      },
    },
  }, true, adapters);
  const port = fixture.ports[0]!;
  completeHandshake(fixture, port);
  fixture.client.setViewport(viewport(1n, QualityPolicy.Preview));
  port.emit(needData(1n, 20n, 1n, 3n));
  fixture.turns.flush();
  await settleSource(fixture);
  port.emit(documentReady(1n, 20n, 1n, 4n));
  fixture.turns.flush();
  port.emit(
    generationPlanned(
      1n,
      20n,
      1n,
      5n,
      QualityPolicy.Preview,
    ),
  );
  port.emit(surfaceReady(1n, 20n, 1n, 6n, 90n));
  fixture.turns.flush();
  assert.equal(fixture.client.resources.deliveredSurfaces, 1);
  assert.equal(fixture.client.resources.liveSurfaces, 1);

  fixture.client.beginShutdown();
  fixture.turns.flush();
  assert.equal(commands(port, "Shutdown").length, 1);
  port.emit(workerStopped(1n, 7n));
  fixture.turns.flush();

  assert.ok(adapters.releases >= 1);
  assert.equal(fixture.client.lifecycle, "Closed");
  assert.deepEqual(fixture.faults, []);
  assert.deepEqual(fixture.client.resources, {
    ports: 0,
    scheduledTurns: 0,
    activeSourceTickets: 0,
    queuedSourceResults: 0,
    bufferedSourceBytes: 0,
    liveSurfaces: 0,
    deliveredSurfaces: 0,
  });
  assert.equal(fixture.turns.pending, 0);
  assert.equal(port.terminateCount, 1);
});

test("terminal fallback cancels an existing turn before releasing the port", () => {
  const fixture = readerFixture({
    descriptor: localDescriptor(),
    acquirer: {
      kind: "Local",
      reader: {
        read: async (request) => ({
          type: "Bytes",
          source: sourceIdentity(),
          range: request.range,
          totalLength: 16n,
          bytes: new ArrayBuffer(Number(request.range.len)),
        }),
      },
    },
  }, false);
  const terminalFaults: string[] = [];
  fixture.client.setHandlers({
    onSurface: (): void => undefined,
    onCapabilityDecision: (): void => {
      throw new Error("injected observer failure");
    },
    onEngineError: (): void => undefined,
    onWorkerFault: (code): void => {
      terminalFaults.push(code);
    },
  });
  const port = fixture.ports[0]!;
  completeHandshake(fixture, port);
  fixture.client.setViewport(viewport(1n, QualityPolicy.Preview));
  port.emit(documentReady(1n, 20n, 1n, 3n));
  fixture.turns.flush();
  port.emit(capabilityReported(1n, 20n, 1n, 4n));
  assert.equal(fixture.turns.pending, 1);

  fixture.client.pump();
  assert.equal(fixture.client.lifecycle, "Failed");
  assert.deepEqual(terminalFaults, ["WorkerTerminated"]);
  assert.deepEqual(fixture.client.resources, {
    ports: 0,
    scheduledTurns: 0,
    activeSourceTickets: 0,
    queuedSourceResults: 0,
    bufferedSourceBytes: 0,
    liveSurfaces: 0,
    deliveredSurfaces: 0,
  });
  assert.equal(fixture.turns.pending, 0);
  assert.equal(port.terminateCount, 1);
});

test("real Viewer and Reader compose fault cleanup without a stale ClientFault", async () => {
  const fixture = readerFixture({
    descriptor: localDescriptor(),
    acquirer: {
      kind: "Local",
      reader: {
        read: async (request) => ({
          type: "Bytes",
          source: sourceIdentity(),
          range: request.range,
          totalLength: 16n,
          bytes: new ArrayBuffer(Number(request.range.len)),
        }),
      },
    },
  }, false);
  const viewerFrames = new FakeViewerFrames();
  const presentation = new FakeViewerPresentation();
  let observationHandlers: BrowserViewerObservationHandlers | undefined;
  const observations: BrowserViewerHostObservations = {
    connect: (handlers): void => {
      observationHandlers = handlers;
    },
    disconnect: (): void => {
      observationHandlers = undefined;
    },
  };
  const viewer = new BrowserViewer({
    client: fixture.client,
    observations,
    frames: viewerFrames,
    focus: {
      captureBeforeUnmount: ():
        | BrowserViewerFocusSnapshot
        | undefined => undefined,
      restoreAfterUnmount: (): void => undefined,
    },
    presentation,
    limits: {
      maxVisiblePages: 8,
      maxCoalescedChanges: 8,
      maxAdoptedSurfaces: 8,
    },
    initialState: {
      documentRevision: 1n,
      annotationRevision: 0n,
      zoomNumerator: 1,
      zoomDenominator: 1,
      visiblePages: [],
      quality: QualityPolicy.Preview,
      outputProfile: OutputProfile.Srgb,
      deviceScaleMilli: 1_000,
      rotation: PageRotation.Degrees0,
      optionalContentId: 0n,
    },
  });

  viewer.mount();
  assert.notEqual(observationHandlers, undefined);
  viewerFrames.runNext();
  const first = fixture.ports[0]!;
  completeHandshake(fixture, first);
  first.emit(needData(1n, 20n, 1n, 3n));
  fixture.turns.flush();
  await settleSource(fixture);
  first.emit(documentReady(1n, 20n, 1n, 4n));
  fixture.turns.flush();
  first.emit(
    generationPlanned(
      1n,
      20n,
      1n,
      5n,
      QualityPolicy.Preview,
    ),
  );
  first.emit(surfaceReady(1n, 20n, 1n, 6n, 90n));
  fixture.turns.flush();
  assert.equal(viewer.adoptedSurfaceCount, 1);
  assert.equal(presentation.current.size, 1);

  first.emitError();
  fixture.turns.flush();
  assert.equal(viewer.adoptedSurfaceCount, 0);
  assert.equal(presentation.current.size, 0);
  assert.deepEqual(viewer.failure, {
    source: "WorkerFault",
    kind: "Worker",
    code: "WorkerError",
  });
  assert.equal(fixture.client.resources.deliveredSurfaces, 0);
  assert.equal(fixture.client.resources.liveSurfaces, 0);
  assert.equal(fixture.client.worker, 2n);

  viewer.unmount();
  viewer.unmount();
  assert.equal(observationHandlers, undefined);
  assert.deepEqual(fixture.client.resources, {
    ports: 0,
    scheduledTurns: 0,
    activeSourceTickets: 0,
    queuedSourceResults: 0,
    bufferedSourceBytes: 0,
    liveSurfaces: 0,
    deliveredSurfaces: 0,
  });
});
