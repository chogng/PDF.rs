import type {
  AlphaMode,
  DocumentReadyEvent,
  GenerationCompletedEvent,
  PageMetricsEvent,
  PixelFormat,
  SourceDescriptor,
  ViewportRequest,
} from "../generated/engine-protocol.js";
import {
  GenerationCompletionStatus,
  MAX_TRANSFER_SLOTS,
  snapshotDocumentReadyEvent,
  snapshotGenerationCompletedEvent,
  snapshotSourceDescriptor,
  snapshotViewportRequest,
  validateAlphaMode,
  validatePixelFormat,
  validateSourceDescriptor,
  validateViewportRequest,
} from "../generated/engine-protocol.js";
import {
  BrowserSourceBridge,
  BrowserSourceBridgeError,
  type BrowserSourceAbortFactory,
  type BrowserSourceAcquirer,
  type BrowserSourceBridgeLimits,
  type BrowserSourceCommand,
  type BrowserSourceSinkReceipt,
} from "./browser-source-bridge.js";
import {
  BrowserSurfaceBridge,
  BrowserSurfaceBridgeError,
  type BrowserPresentedSurface,
  type BrowserSurfaceAdapters,
  type BrowserSurfaceEpoch,
  type BrowserSurfaceLimits,
  type BrowserSurfacePresentationSink,
  type BrowserSurfacePresentationTransaction,
  type BrowserSurfaceReleaseDisposition,
  type BrowserSurfaceReleaseRequest,
  type BrowserSurfaceRuntimeSupport,
} from "./browser-surface-bridge.js";
import type {
  BrowserViewerEngineClient,
  BrowserViewerEngineHandlers,
  BrowserViewerSurface,
} from "./browser-viewer.js";
import {
  BrowserWorkerSupervisor,
  BrowserWorkerSupervisorError,
  type BrowserSupervisorEvent,
  type BrowserWorkerClock,
  type BrowserWorkerFactory,
  type BrowserWorkerFaultCode,
  type BrowserWorkerHandlers,
  type BrowserWorkerPort,
  type BrowserWorkerSupervisorLimits,
} from "./browser-worker-supervisor.js";

const MAX_ACTOR_TURN = 4_096;
const MAX_RESTARTS = 16;
const MAX_SHUTDOWN_DEADLINE_MS = 600_000;

/** Stable reader construction and lifecycle failures. */
export type BrowserReaderClientErrorCode =
  | "InvalidConfiguration"
  | "InvalidLifecycle"
  | "InvalidViewport"
  | "SourceRejected"
  | "ActorFailure"
  | "RestartLimit";

/** A content-free reader API error. */
export class BrowserReaderClientError extends Error {
  readonly code: BrowserReaderClientErrorCode;

  constructor(code: BrowserReaderClientErrorCode) {
    super(code);
    this.name = "BrowserReaderClientError";
    this.code = code;
  }
}

/** Explicit actor scheduling; callbacks must run outside Worker callbacks. */
export interface BrowserReaderTurnScheduler {
  request(callback: () => void): number;
  cancel(handle: number): void;
}

/** Bounds for one reader, including automatic Worker recovery. */
export interface BrowserReaderClientLimits {
  readonly maxActorTurn: number;
  readonly maxAutomaticRestarts: number;
  readonly shutdownDeadlineMs: number;
  readonly source: BrowserSourceBridgeLimits;
  readonly surface: BrowserSurfaceLimits;
}

/** Fixed product Surface layout authenticated with every RenderPlan. */
export interface BrowserReaderSurfaceLayout {
  readonly format: PixelFormat;
  readonly alpha: AlphaMode;
}

/** Runtime-only facts used by the host Surface owner. */
export interface BrowserReaderSurfaceEnvironment {
  readonly adapters: BrowserSurfaceAdapters;
  readonly runtimeSupport: BrowserSurfaceRuntimeSupport;
  readonly crossOriginIsolated: boolean;
  readonly layout: BrowserReaderSurfaceLayout;
}

/** Host-owned immutable source, replayable after a Worker restart. */
export interface BrowserReaderSource {
  readonly descriptor: SourceDescriptor;
  readonly acquirer: BrowserSourceAcquirer;
}

/** Reader status that is intentionally outside the PDF-free viewer core. */
export interface BrowserReaderStatusHandlers {
  readonly onDocumentReady?: (ready: DocumentReadyEvent) => void;
  readonly onPageMetrics?: (metrics: PageMetricsEvent) => void;
  readonly onGenerationCompleted?: (
    generation: bigint,
    completion: GenerationCompletedEvent,
  ) => void;
  readonly onLifecycle?: (lifecycle: BrowserReaderLifecycle) => void;
}

/** Observable ownership lifecycle for one open document. */
export type BrowserReaderLifecycle =
  | "Created"
  | "Starting"
  | "Opening"
  | "Ready"
  | "Restarting"
  | "Closing"
  | "Closed"
  | "Failed";

/** Exact live resource counts used by teardown and fault-loop gates. */
export interface BrowserReaderResourceCounts {
  readonly ports: number;
  readonly scheduledTurns: number;
  readonly activeSourceTickets: number;
  readonly queuedSourceResults: number;
  readonly bufferedSourceBytes: number;
  readonly liveSurfaces: number;
  readonly deliveredSurfaces: number;
}

/** Construction data for the real BrowserViewer EngineClient. */
export interface BrowserReaderClientConfiguration {
  readonly hostHello:
    ConstructorParameters<typeof BrowserWorkerSupervisor>[0]["hostHello"];
  readonly workerLimits: BrowserWorkerSupervisorLimits;
  readonly workerFactory: BrowserWorkerFactory;
  readonly clock: BrowserWorkerClock;
  readonly startupTimeoutMs: number;
  readonly turns: BrowserReaderTurnScheduler;
  readonly source: BrowserReaderSource;
  readonly aborts: BrowserSourceAbortFactory;
  readonly surfaces: BrowserReaderSurfaceEnvironment;
  readonly limits: BrowserReaderClientLimits;
  readonly status?: BrowserReaderStatusHandlers;
}

interface SnapshotLimits {
  readonly maxActorTurn: number;
  readonly maxAutomaticRestarts: number;
  readonly shutdownDeadlineMs: number;
  readonly source: BrowserSourceBridgeLimits;
  readonly surface: BrowserSurfaceLimits;
}

interface LivePort {
  readonly worker: bigint;
  readonly inner: BrowserWorkerPort;
  readonly token: object;
  handlers: BrowserWorkerHandlers | undefined;
  active: boolean;
}

interface DeliveredSurface {
  readonly worker: bigint;
  readonly value: BrowserViewerSurface;
}

const isRecord = (
  value: unknown,
): value is Readonly<Record<string, unknown>> =>
  typeof value === "object" && value !== null;

const hasMethods = (
  value: unknown,
  names: readonly string[],
): boolean => {
  if (!isRecord(value)) {
    return false;
  }
  try {
    return names.every(
      (name) => typeof Reflect.get(value, name) === "function",
    );
  } catch {
    return false;
  }
};

const isPositiveCapacity = (
  value: unknown,
  maximum: number,
): value is number =>
  typeof value === "number"
  && Number.isSafeInteger(value)
  && value > 0
  && value <= maximum;

const snapshotLimits = (
  limits: BrowserReaderClientLimits,
): SnapshotLimits | undefined => {
  try {
    if (
      !isPositiveCapacity(limits.maxActorTurn, MAX_ACTOR_TURN)
      || !isPositiveCapacity(
        limits.maxAutomaticRestarts,
        MAX_RESTARTS,
      )
      || !isPositiveCapacity(
        limits.shutdownDeadlineMs,
        MAX_SHUTDOWN_DEADLINE_MS,
      )
    ) {
      return undefined;
    }
    return Object.freeze({
      maxActorTurn: limits.maxActorTurn,
      maxAutomaticRestarts: limits.maxAutomaticRestarts,
      shutdownDeadlineMs: limits.shutdownDeadlineMs,
      source: Object.freeze({ ...limits.source }),
      surface: Object.freeze({ ...limits.surface }),
    });
  } catch {
    return undefined;
  }
};

const instrumentSource = (
  source: BrowserSourceAcquirer,
  notify: () => void,
): BrowserSourceAcquirer => {
  try {
    if (source.kind === "Local") {
      if (!hasMethods(source.reader, ["read"])) {
        throw new BrowserReaderClientError("InvalidConfiguration");
      }
      return Object.freeze({
        kind: "Local" as const,
        reader: Object.freeze({
          read: (request: Parameters<typeof source.reader.read>[0]) => {
            let result: ReturnType<typeof source.reader.read>;
            try {
              result = source.reader.read(request);
            } catch (error: unknown) {
              notify();
              throw error;
            }
            return Promise.resolve(result).then(
              (value) => {
                notify();
                return value;
              },
              (error: unknown) => {
                notify();
                throw error;
              },
            );
          },
        }),
      });
    }
    if (
      source.kind !== "Http"
      || !hasMethods(source.fetcher, ["fetch"])
    ) {
      throw new BrowserReaderClientError("InvalidConfiguration");
    }
    return Object.freeze({
      kind: "Http" as const,
      url: source.url,
      validator: Object.freeze({ ...source.validator }),
      fetcher: Object.freeze({
        fetch: (request: Parameters<typeof source.fetcher.fetch>[0]) => {
          let result: ReturnType<typeof source.fetcher.fetch>;
          try {
            result = source.fetcher.fetch(request);
          } catch (error: unknown) {
            notify();
            throw error;
          }
          return Promise.resolve(result).then(
            (value) => {
              notify();
              return value;
            },
            (error: unknown) => {
              notify();
              throw error;
            },
          );
        },
      }),
    });
  } catch (error: unknown) {
    if (error instanceof BrowserReaderClientError) {
      throw error;
    }
    throw new BrowserReaderClientError("InvalidConfiguration");
  }
};

const snapshotRuntimeSupport = (
  support: BrowserSurfaceRuntimeSupport,
): BrowserSurfaceRuntimeSupport => {
  try {
    if (
      typeof support.imageBitmap !== "boolean"
      || typeof support.arrayBuffer !== "boolean"
      || typeof support.sharedArrayBuffer !== "boolean"
      || typeof support.offscreenCanvasStaging !== "boolean"
    ) {
      throw new BrowserReaderClientError("InvalidConfiguration");
    }
    return Object.freeze({ ...support });
  } catch (error: unknown) {
    if (error instanceof BrowserReaderClientError) {
      throw error;
    }
    throw new BrowserReaderClientError("InvalidConfiguration");
  }
};

/**
 * Session controller injected into BrowserViewer.
 *
 * It owns the immutable source, one supervised Worker epoch at a time,
 * protocol actor turns, Surface leases, and restart/reopen replay. It does not
 * interpret PDF, scene, graphics, font, color, or raster semantics.
 */
export class BrowserReaderClient implements BrowserViewerEngineClient {
  readonly #turns: BrowserReaderTurnScheduler;
  readonly #sourceDescriptor: SourceDescriptor;
  readonly #source: BrowserSourceAcquirer;
  readonly #aborts: BrowserSourceAbortFactory;
  readonly #surfaceAdapters: BrowserSurfaceAdapters;
  readonly #surfaceRuntimeSupport: BrowserSurfaceRuntimeSupport;
  readonly #surfaceCrossOriginIsolated: boolean;
  readonly #surfaceLayout: BrowserReaderSurfaceLayout;
  readonly #limits: SnapshotLimits;
  readonly #status: BrowserReaderStatusHandlers | undefined;
  readonly #supervisor: BrowserWorkerSupervisor;
  #lifecycle: BrowserReaderLifecycle = "Created";
  #handlers: BrowserViewerEngineHandlers | undefined;
  #scheduledTicket: object | undefined;
  #scheduledHandle: number | undefined;
  #activePort: LivePort | undefined;
  #sourceBridge: BrowserSourceBridge | undefined;
  #surfaceBridge: BrowserSurfaceBridge | undefined;
  #session: bigint | undefined;
  #openRequest = 1n;
  #nextRequest = 2n;
  #latestViewport: ViewportRequest | undefined;
  #document: DocumentReadyEvent | undefined;
  #restartCount = 0;
  #gracefulShutdown = false;
  readonly #delivered = new Map<BrowserViewerSurface, DeliveredSurface>();
  readonly #deliveredById = new Map<bigint, DeliveredSurface>();
  readonly #terminalDelivered = new WeakSet<BrowserViewerSurface>();

  constructor(configuration: BrowserReaderClientConfiguration) {
    const limits = snapshotLimits(configuration.limits);
    try {
      if (
        new.target !== BrowserReaderClient
        || limits === undefined
        || !validateSourceDescriptor(configuration.source.descriptor)
        || !hasMethods(configuration.aborts, ["create"])
        || !hasMethods(configuration.turns, ["request", "cancel"])
        || typeof configuration.workerFactory !== "function"
        || !hasMethods(configuration.clock, ["now"])
        || !hasMethods(
          configuration.surfaces.adapters.imageBitmap,
          ["isResource", "describe", "adopt", "close"],
        )
        || !hasMethods(
          configuration.surfaces.adapters.arrayBuffer,
          ["isResource", "describe", "adoptReadOnly", "release"],
        )
        || !hasMethods(
          configuration.surfaces.adapters.sharedArrayBuffer,
          [
            "isResource",
            "describe",
            "loadPublicationEpoch",
            "adoptReadOnly",
            "release",
          ],
        )
        || !hasMethods(
          configuration.surfaces.adapters.wasmMemory,
          ["isMemory"],
        )
        || !validatePixelFormat(configuration.surfaces.layout.format)
        || !validateAlphaMode(configuration.surfaces.layout.alpha)
        || typeof configuration.surfaces.crossOriginIsolated !== "boolean"
      ) {
        throw new BrowserReaderClientError("InvalidConfiguration");
      }
      this.#limits = limits;
      this.#turns = configuration.turns;
      this.#sourceDescriptor = snapshotSourceDescriptor(
        configuration.source.descriptor,
      );
      this.#aborts = configuration.aborts;
      this.#surfaceAdapters = configuration.surfaces.adapters;
      this.#surfaceRuntimeSupport = snapshotRuntimeSupport(
        configuration.surfaces.runtimeSupport,
      );
      this.#surfaceCrossOriginIsolated =
        configuration.surfaces.crossOriginIsolated;
      this.#surfaceLayout = Object.freeze({
        format: configuration.surfaces.layout.format,
        alpha: configuration.surfaces.layout.alpha,
      });
      this.#status = configuration.status;
      this.#source = instrumentSource(
        configuration.source.acquirer,
        (): void => {
          this.#scheduleTurn();
        },
      );
      this.#supervisor = new BrowserWorkerSupervisor({
        hostHello: configuration.hostHello,
        limits: configuration.workerLimits,
        clock: configuration.clock,
        startupTimeoutMs: configuration.startupTimeoutMs,
        factory: (worker: bigint): BrowserWorkerPort =>
          this.#createPort(configuration.workerFactory, worker),
      });
    } catch (error: unknown) {
      if (error instanceof BrowserReaderClientError) {
        throw error;
      }
      throw new BrowserReaderClientError("InvalidConfiguration");
    }
  }

  get lifecycle(): BrowserReaderLifecycle {
    return this.#lifecycle;
  }

  get worker(): bigint {
    return this.#supervisor.worker;
  }

  get session(): bigint | undefined {
    return this.#session;
  }

  get document(): DocumentReadyEvent | undefined {
    return this.#document === undefined
      ? undefined
      : snapshotDocumentReadyEvent(this.#document);
  }

  get restartCount(): number {
    return this.#restartCount;
  }

  get latestViewport(): ViewportRequest | undefined {
    return this.#latestViewport === undefined
      ? undefined
      : snapshotViewportRequest(this.#latestViewport);
  }

  get resources(): BrowserReaderResourceCounts {
    return Object.freeze({
      ports: this.#activePort?.active === true ? 1 : 0,
      scheduledTurns: this.#scheduledTicket === undefined ? 0 : 1,
      activeSourceTickets: this.#sourceBridge?.activeTickets ?? 0,
      queuedSourceResults: this.#sourceBridge?.queuedResults ?? 0,
      bufferedSourceBytes: this.#sourceBridge?.bufferedBytes ?? 0,
      liveSurfaces: this.#surfaceBridge?.liveSurfaces ?? 0,
      deliveredSurfaces: this.#delivered.size,
    });
  }

  setHandlers(handlers: BrowserViewerEngineHandlers | undefined): void {
    if (handlers === undefined) {
      this.#handlers = undefined;
      return;
    }
    if (
      this.#lifecycle !== "Created"
      || !hasMethods(handlers, [
        "onSurface",
        "onCapabilityDecision",
        "onEngineError",
        "onWorkerFault",
      ])
    ) {
      throw new BrowserReaderClientError("InvalidLifecycle");
    }
    this.#handlers = handlers;
    this.#setLifecycle("Starting");
    try {
      this.#supervisor.start();
    } catch {
      this.#failTerminal("FactoryFailure");
      throw new BrowserReaderClientError("ActorFailure");
    }
    this.#scheduleTurn();
  }

  setViewport(viewport: ViewportRequest): void {
    if (
      this.#lifecycle === "Closing"
      || this.#lifecycle === "Closed"
      || this.#lifecycle === "Failed"
      || !validateViewportRequest(viewport)
      || (
        this.#latestViewport !== undefined
        && viewport.generation <= this.#latestViewport.generation
      )
    ) {
      throw new BrowserReaderClientError("InvalidViewport");
    }
    const snapshot = snapshotViewportRequest(viewport);
    this.#latestViewport = snapshot;
    if (
      this.#lifecycle === "Ready"
      && this.#session !== undefined
    ) {
      this.#submitViewport(snapshot);
    }
  }

  releaseSurface(surface: BrowserViewerSurface): void {
    const delivered = this.#delivered.get(surface);
    if (delivered === undefined) {
      if (
        this.#lifecycle === "Closed"
        || (
          typeof surface === "object"
          && surface !== null
          && this.#terminalDelivered.has(surface)
        )
      ) {
        return;
      }
      throw new BrowserReaderClientError("InvalidLifecycle");
    }
    this.#forgetDelivered(delivered);
    if (
      delivered.worker !== this.#supervisor.worker
      || this.#surfaceBridge === undefined
      || this.#supervisor.state === "Failed"
      || this.#supervisor.state === "Stopped"
    ) {
      return;
    }
    try {
      this.#surfaceBridge.releaseSurface(
        delivered.value.metadata.id,
        delivered.value.metadata.lease_token,
      );
      this.#scheduleTurn();
    } catch {
      // The exact handle is already terminal at the viewer boundary. Faulting
      // the epoch is the only safe fallback for a bridge cleanup failure.
      this.#signalTerminalPort();
    }
  }

  /**
   * Begins a protocol-visible graceful Worker shutdown.
   *
   * BrowserViewer normally uses synchronous `close`; this actor-driven path is
   * available to hosts that keep pumping until WorkerStopped is observed.
   */
  beginShutdown(): void {
    if (
      this.#lifecycle !== "Ready"
      || this.#supervisor.state !== "Ready"
    ) {
      throw new BrowserReaderClientError("InvalidLifecycle");
    }
    try {
      this.#sourceBridge?.close();
      this.#supervisor.submit(
        Object.freeze({
          type: "Shutdown",
          payload: Object.freeze({
            deadline_ms: this.#limits.shutdownDeadlineMs,
          }),
        }),
        Object.freeze({}),
      );
    } catch {
      this.#failTerminal("WorkerTerminated");
      return;
    }
    this.#gracefulShutdown = true;
    this.#setLifecycle("Closing");
    this.#scheduleTurn();
  }

  /**
   * Executes one bounded Host actor turn. Tests and non-continuous embedders
   * may call this directly; the injected scheduler uses the same path.
   */
  pump(maximum = this.#limits.maxActorTurn): number {
    if (
      !Number.isSafeInteger(maximum)
      || maximum <= 0
      || maximum > this.#limits.maxActorTurn
    ) {
      throw new BrowserReaderClientError("InvalidConfiguration");
    }
    if (
      this.#lifecycle === "Closed"
      || this.#lifecycle === "Failed"
    ) {
      return 0;
    }
    let work = 0;
    try {
      this.#supervisor.pollClock();
      if (work < maximum) {
        work += this.#supervisor.processInboundTurn(maximum - work);
      }
      const events = work < maximum
        ? this.#supervisor.takeEvents(maximum - work)
        : [];
      for (const event of events) {
        this.#handleEvent(event);
        work += 1;
        if (
          work >= maximum
          || this.#isTerminal()
        ) {
          break;
        }
      }
      if (work < maximum && this.#sourceBridge !== undefined) {
        work += this.#sourceBridge.drain(Math.min(
          maximum - work,
          this.#limits.source.maxDrainTurn,
        ));
      }
      if (work < maximum && this.#surfaceBridge !== undefined) {
        const result = this.#surfaceBridge.drain(Math.min(
          maximum - work,
          this.#limits.surface.maxQueuedCallbacks,
        ));
        work += result.processed;
        if (result.rejected !== 0 || result.errors.length !== 0) {
          this.#signalTerminalPort();
        }
      }
      if (work < maximum) {
        work += this.#supervisor.drainOutboundTurn(maximum - work);
      }
    } catch (error: unknown) {
      if (
        error instanceof BrowserSourceBridgeError
        || error instanceof BrowserSurfaceBridgeError
        || error instanceof BrowserWorkerSupervisorError
        || error instanceof BrowserReaderClientError
      ) {
        this.#signalTerminalPort();
      } else {
        this.#failTerminal("WorkerTerminated");
      }
    }
    this.#scheduleIfPending();
    return work;
  }

  close(): void {
    if (this.#lifecycle === "Closed") {
      return;
    }
    this.#setLifecycle("Closing");
    this.#cancelScheduledTurn();
    this.#handlers = undefined;
    try {
      this.#sourceBridge?.close();
    } catch {
      // Teardown continues through every independently owned resource.
    }
    const session = this.#session;
    if (
      session !== undefined
      && this.#supervisor.state === "Ready"
    ) {
      try {
        this.#surfaceBridge?.closeSession(session);
      } catch {
        // Teardown continues; the epoch terminal path owns the fallback.
      }
      try {
        this.#supervisor.submit(
          Object.freeze({
            type: "CloseSession",
            payload: Object.freeze({}),
          }),
          Object.freeze({ session }),
        );
      } catch {
        // A terminal port does not require a close acknowledgement.
      }
    }
    if (this.#supervisor.state === "Ready") {
      try {
        this.#supervisor.submit(
          Object.freeze({
            type: "Shutdown",
            payload: Object.freeze({
              deadline_ms: this.#limits.shutdownDeadlineMs,
            }),
          }),
          Object.freeze({}),
        );
        this.#supervisor.drainOutboundTurn(this.#limits.maxActorTurn);
      } catch {
        // Immediate termination below is the bounded shutdown fallback.
      }
    }
    try {
      this.#surfaceBridge?.workerFault(
        this.#supervisor.worker,
        this.#supervisor.worker,
      );
    } catch {
      // Every delivered handle is independently made terminal below.
    }
    for (const delivered of Array.from(this.#delivered.values())) {
      this.#forgetDelivered(delivered);
    }
    this.#terminateActivePort();
    this.#session = undefined;
    this.#document = undefined;
    this.#gracefulShutdown = false;
    this.#setLifecycle("Closed");
  }

  #createPort(
    factory: BrowserWorkerFactory,
    worker: bigint,
  ): BrowserWorkerPort {
    const inner = factory(worker);
    const live: LivePort = {
      worker,
      inner,
      token: {},
      handlers: undefined,
      active: true,
    };
    this.#activePort = live;
    return Object.freeze({
      setHandlers: (handlers: BrowserWorkerHandlers): void => {
        live.handlers = handlers;
        inner.setHandlers(Object.freeze({
          onMessage: (value: unknown): void => {
            if (!this.#isCurrentPort(live)) {
              this.#discardLateWorkerValue(value);
              return;
            }
            handlers.onMessage(value);
            this.#scheduleTurn();
          },
          onMessageError: (): void => {
            if (!this.#isCurrentPort(live)) {
              return;
            }
            handlers.onMessageError();
            this.#scheduleTurn();
          },
          onError: (): void => {
            if (!this.#isCurrentPort(live)) {
              return;
            }
            handlers.onError();
            this.#scheduleTurn();
          },
          onTerminated: (): void => {
            if (!this.#isCurrentPort(live)) {
              return;
            }
            handlers.onTerminated();
            this.#scheduleTurn();
          },
        }));
      },
      postMessage: (
        value: unknown[],
        transfer: ArrayBuffer[],
      ): void => {
        if (!this.#isCurrentPort(live)) {
          throw new BrowserReaderClientError("InvalidLifecycle");
        }
        inner.postMessage(value, transfer);
      },
      terminate: (): void => {
        if (!live.active) {
          return;
        }
        live.active = false;
        if (this.#activePort === live) {
          this.#activePort = undefined;
        }
        inner.terminate();
      },
    });
  }

  #isCurrentPort(port: LivePort): boolean {
    return this.#lifecycle !== "Closed"
      && port.active
      && this.#activePort === port
      && this.#activePort.token === port.token
      && this.#supervisor.worker === port.worker;
  }

  #isTerminal(): boolean {
    return this.#lifecycle === "Closed"
      || this.#lifecycle === "Failed";
  }

  #handleEvent(event: BrowserSupervisorEvent): void {
    if (event.type === "WorkerFault") {
      this.#handleWorkerFault(event);
      return;
    }
    const { correlation, event: protocolEvent } = event.value.envelope;
    switch (protocolEvent.type) {
      case "Ready": {
        const connection = this.#supervisor.connection;
        if (connection === undefined) {
          this.#signalTerminalPort();
          return;
        }
        const epoch: BrowserSurfaceEpoch = Object.freeze({
          worker: this.#supervisor.worker,
          workerEpoch: this.#supervisor.worker,
          endpointCapabilities: connection.capabilities,
          executionCapabilities:
            protocolEvent.payload.execution_capabilities.supported,
          crossOriginIsolated: this.#surfaceCrossOriginIsolated,
          runtimeSupport: this.#surfaceRuntimeSupport,
        });
        const presentation = this.#surfacePresentation();
        const releases = Object.freeze({
          requestRelease: (
            request: BrowserSurfaceReleaseRequest,
          ): BrowserSurfaceReleaseDisposition =>
            this.#requestSurfaceRelease(request),
        });
        if (this.#surfaceBridge === undefined) {
          this.#surfaceBridge = new BrowserSurfaceBridge({
            limits: this.#limits.surface,
            adapters: this.#surfaceAdapters,
            presentation,
            releases,
            epoch,
          });
        } else {
          this.#surfaceBridge.startEpoch(epoch);
        }
        this.#setLifecycle("Opening");
        this.#supervisor.submit(
          Object.freeze({
            type: "Open",
            payload: Object.freeze({
              source: snapshotSourceDescriptor(this.#sourceDescriptor),
            }),
          }),
          Object.freeze({ request: this.#openRequest }),
        );
        break;
      }
      case "NeedData": {
        const session = correlation.session;
        const request = correlation.request;
        if (session === undefined || request === undefined) {
          this.#signalTerminalPort();
          return;
        }
        const bridge = this.#ensureSourceBridge(session);
        const result = bridge.request(Object.freeze({
          need: protocolEvent.payload,
          owner: Object.freeze({
            worker: this.#supervisor.worker,
            session,
            request,
          }),
        }));
        if (result !== "Accepted") {
          this.#signalTerminalPort();
        }
        break;
      }
      case "DocumentReady": {
        const session = correlation.session;
        if (
          session === undefined
          || session !== protocolEvent.payload.session
          || (
            this.#session !== undefined
            && this.#session !== session
          )
        ) {
          this.#signalTerminalPort();
          return;
        }
        this.#session = session;
        this.#document = snapshotDocumentReadyEvent(
          protocolEvent.payload,
        );
        this.#setLifecycle("Ready");
        this.#notifyStatus("onDocumentReady", this.#document);
        if (protocolEvent.payload.page_count > 0) {
          this.#supervisor.submit(
            Object.freeze({
              type: "GetPageMetrics",
              payload: Object.freeze({
                document_revision:
                  protocolEvent.payload.document_revision,
                start_index: 0,
                max_count: Math.min(
                  protocolEvent.payload.page_count,
                  64,
                ),
              }),
            }),
            Object.freeze({
              session,
              request: this.#nextRequest,
            }),
          );
          this.#nextRequest += 1n;
        }
        if (this.#latestViewport !== undefined) {
          this.#submitViewport(this.#latestViewport);
        }
        break;
      }
      case "PageMetrics":
        this.#notifyStatus("onPageMetrics", protocolEvent.payload);
        break;
      case "CapabilityReported":
        this.#handlers?.onCapabilityDecision(
          protocolEvent.payload.decision,
        );
        break;
      case "GenerationPlanned": {
        const session = correlation.session;
        const generation = correlation.generation;
        const latest = this.#latestViewport;
        if (
          session === undefined
          || generation === undefined
          || session !== this.#session
          || generation !== protocolEvent.payload.manifest.generation
          || generation !== latest?.generation
          || this.#surfaceBridge === undefined
        ) {
          break;
        }
        const manifest = protocolEvent.payload.manifest;
        this.#surfaceBridge.activateGeneration(Object.freeze({
          worker: this.#supervisor.worker,
          workerEpoch: this.#supervisor.worker,
          session,
          generation,
          identity: Object.freeze({
            renderConfig: manifest.render_config.slice(),
            rendererEpoch: manifest.renderer_epoch,
            planId: manifest.plan_id,
            planHash: protocolEvent.payload.plan_hash.slice(),
            sceneHash: manifest.scene_hash.slice(),
            decisionHash: manifest.decision_hash.slice(),
            backend: manifest.backend,
            format: this.#surfaceLayout.format,
            alpha: this.#surfaceLayout.alpha,
          }),
          regions: Object.freeze(
            manifest.regions.map((region) =>
              Object.freeze({ ...region }),
            ),
          ),
        }));
        break;
      }
      case "SurfaceReady": {
        const session = correlation.session;
        const generation = correlation.generation;
        if (session === undefined || generation === undefined) {
          this.#signalTerminalPort();
          return;
        }
        if (
          generation !== this.#latestViewport?.generation
          || session !== this.#session
        ) {
          this.#supervisor.submit(
            Object.freeze({
              type: "ReleaseSurface",
              payload: Object.freeze({
                surface: protocolEvent.payload.metadata.id,
                lease_token:
                  protocolEvent.payload.metadata.lease_token,
              }),
            }),
            Object.freeze({ session }),
          );
          break;
        }
        this.#surfaceBridge?.enqueueSurfaceReady(Object.freeze({
          worker: this.#supervisor.worker,
          workerEpoch: this.#supervisor.worker,
          session,
          generation,
          surface: protocolEvent.payload,
          resources: event.value.resources,
        }));
        break;
      }
      case "SurfaceReclaimed":
      case "SurfaceReleaseAcknowledged": {
        const session = correlation.session;
        if (session !== undefined) {
          this.#surfaceBridge?.enqueueLifecycle(Object.freeze({
            worker: this.#supervisor.worker,
            workerEpoch: this.#supervisor.worker,
            session,
            event: protocolEvent.payload,
          }));
        }
        break;
      }
      case "GenerationCompleted": {
        const generation = correlation.generation;
        if (generation !== undefined) {
          const completion = snapshotGenerationCompletedEvent(
            protocolEvent.payload,
          );
          this.#notifyGenerationCompleted(generation, completion);
          if (
            completion.status === GenerationCompletionStatus.Failed
            && completion.error !== undefined
          ) {
            this.#handlers?.onEngineError(completion.error);
          }
        }
        break;
      }
      case "RequestFailed":
      case "DataFailed":
        this.#handlers?.onEngineError(protocolEvent.payload.error);
        break;
      case "SessionClosed":
        if (correlation.session !== undefined) {
          this.#surfaceBridge?.closeSession(correlation.session);
        }
        break;
      case "WorkerStopped":
        if (
          this.#lifecycle === "Closing"
          && this.#gracefulShutdown
        ) {
          this.#completeGracefulShutdown();
        } else {
          this.#handleStoppedWorker();
        }
        break;
      case "EngineHello":
      case "WorkerFault":
      case "ProtocolFault":
        this.#signalTerminalPort();
        break;
      case "RequestCancelled":
      case "CancelAcknowledged":
      case "CloseSessionAcknowledged":
      case "ShutdownAcknowledged":
        break;
    }
  }

  #handleWorkerFault(
    event: Extract<BrowserSupervisorEvent, { type: "WorkerFault" }>,
  ): void {
    const oldWorker = event.worker;
    try {
      this.#sourceBridge?.fault();
    } catch {
      // Surface and viewer cleanup must still run.
    }
    try {
      this.#surfaceBridge?.workerFault(oldWorker, oldWorker);
    } catch {
      // Every delivered handle is made terminal independently below.
    }
    for (const delivered of Array.from(this.#delivered.values())) {
      if (delivered.worker === oldWorker) {
        this.#forgetDelivered(delivered);
      }
    }
    this.#session = undefined;
    this.#document = undefined;

    if (event.origin === "EngineProtocol") {
      const fault = event.value.envelope.event;
      if (fault.type === "WorkerFault" || fault.type === "ProtocolFault") {
        this.#handlers?.onEngineError(fault.payload.error);
      }
      this.#handlers?.onWorkerFault("ProtocolViolation");
    } else {
      this.#handlers?.onWorkerFault(event.code);
    }
    if (
      this.#lifecycle === "Closing"
      || this.#lifecycle === "Closed"
    ) {
      return;
    }
    if (this.#restartCount >= this.#limits.maxAutomaticRestarts) {
      this.#setLifecycle("Failed");
      return;
    }
    this.#restartCount += 1;
    this.#openRequest = 1n;
    this.#nextRequest = 2n;
    this.#setLifecycle("Restarting");
    try {
      this.#supervisor.restart();
    } catch {
      this.#setLifecycle("Failed");
      return;
    }
    this.#setLifecycle("Starting");
    this.#scheduleTurn();
  }

  #handleStoppedWorker(): void {
    this.#failTerminal("WorkerTerminated");
  }

  #completeGracefulShutdown(): void {
    this.#cancelScheduledTurn();
    try {
      this.#sourceBridge?.close();
    } catch {
      // Surface and port cleanup remain independent.
    }
    const worker = this.#supervisor.worker;
    try {
      this.#surfaceBridge?.workerFault(worker, worker);
    } catch {
      // Every delivered handle is independently forgotten below.
    }
    for (const delivered of Array.from(this.#delivered.values())) {
      this.#forgetDelivered(delivered);
    }
    this.#terminateActivePort();
    this.#session = undefined;
    this.#document = undefined;
    this.#gracefulShutdown = false;
    this.#lifecycle = "Closed";
    try {
      this.#status?.onLifecycle?.("Closed");
    } catch {
      // Reader-status observers do not own engine or resource lifecycle.
    }
  }

  #ensureSourceBridge(session: bigint): BrowserSourceBridge {
    if (
      this.#session !== undefined
      && this.#session !== session
    ) {
      throw new BrowserReaderClientError("SourceRejected");
    }
    this.#session = session;
    if (this.#sourceBridge === undefined) {
      this.#sourceBridge = new BrowserSourceBridge({
        worker: this.#supervisor.worker,
        session,
        descriptor: this.#sourceDescriptor,
        source: this.#source,
        aborts: this.#aborts,
        sink: Object.freeze({
          submit: (
            command: BrowserSourceCommand,
            correlation: {
              readonly worker: bigint;
              readonly session: bigint;
            },
            resources: readonly ArrayBuffer[],
          ): BrowserSourceSinkReceipt => {
            if (
              correlation.worker !== this.#supervisor.worker
              || correlation.session !== this.#session
            ) {
              throw new BrowserReaderClientError("InvalidLifecycle");
            }
            this.#supervisor.submit(
              command,
              Object.freeze({ session: correlation.session }),
              resources,
            );
            this.#scheduleTurn();
            return Object.freeze({
              ticket: command.payload.ticket,
              ownership: "AdoptedOwnership" as const,
            });
          },
        }),
        limits: this.#limits.source,
      });
      return this.#sourceBridge;
    }
    if (this.#sourceBridge.lifecycle === "Faulted") {
      this.#sourceBridge.restart(this.#supervisor.worker, session);
    }
    if (
      this.#sourceBridge.lifecycle !== "Active"
      || this.#sourceBridge.worker !== this.#supervisor.worker
      || this.#sourceBridge.session !== session
    ) {
      throw new BrowserReaderClientError("SourceRejected");
    }
    return this.#sourceBridge;
  }

  #submitViewport(viewport: ViewportRequest): void {
    const session = this.#session;
    if (session === undefined || this.#supervisor.state !== "Ready") {
      return;
    }
    this.#supervisor.submit(
      Object.freeze({
        type: "SetViewport",
        payload: Object.freeze({
          viewport: snapshotViewportRequest(viewport),
        }),
      }),
      Object.freeze({
        session,
        generation: viewport.generation,
      }),
    );
    this.#scheduleTurn();
  }

  #surfacePresentation(): BrowserSurfacePresentationSink {
    return Object.freeze({
      stage: (
        surface: BrowserPresentedSurface,
      ): BrowserSurfacePresentationTransaction => {
        const handlers = this.#handlers;
        if (handlers === undefined) {
          throw new BrowserReaderClientError("InvalidLifecycle");
        }
        const presented: BrowserViewerSurface = Object.freeze({
          metadata: surface.metadata,
          resource: surface.resource,
        });
        let terminal = false;
        return Object.freeze({
          commit: (): void => {
            if (terminal) {
              throw new BrowserReaderClientError("InvalidLifecycle");
            }
            terminal = true;
            const delivered: DeliveredSurface = {
              worker: this.#supervisor.worker,
              value: presented,
            };
            this.#delivered.set(presented, delivered);
            this.#deliveredById.set(surface.metadata.id, delivered);
            handlers.onSurface(presented);
          },
          abort: (): void => {
            if (terminal) {
              return;
            }
            terminal = true;
          },
        });
      },
      remove: (surface: bigint): void => {
        const delivered = this.#deliveredById.get(surface);
        if (delivered !== undefined) {
          this.#forgetDelivered(delivered);
        }
      },
    });
  }

  #requestSurfaceRelease(
    request: BrowserSurfaceReleaseRequest,
  ): BrowserSurfaceReleaseDisposition {
    if (
      request.worker !== this.#supervisor.worker
      || request.workerEpoch !== this.#supervisor.worker
      || (
        this.#supervisor.state !== "Ready"
        && this.#supervisor.state !== "Draining"
      )
    ) {
      return "WorkerTerminal";
    }
    this.#supervisor.submit(
      Object.freeze({
        type: "ReleaseSurface",
        payload: Object.freeze({
          surface: request.surface,
          lease_token: request.leaseToken,
        }),
      }),
      Object.freeze({ session: request.session }),
    );
    this.#scheduleTurn();
    return "Queued";
  }

  #forgetDelivered(delivered: DeliveredSurface): void {
    this.#delivered.delete(delivered.value);
    if (
      this.#deliveredById.get(delivered.value.metadata.id)
        === delivered
    ) {
      this.#deliveredById.delete(delivered.value.metadata.id);
    }
    this.#terminalDelivered.add(delivered.value);
  }

  #notifyStatus<K extends keyof BrowserReaderStatusHandlers>(
    key: K,
    value: NonNullable<BrowserReaderStatusHandlers[K]> extends
      (input: infer T) => void
      ? T
      : never,
  ): void {
    const callback = this.#status?.[key];
    if (typeof callback !== "function") {
      return;
    }
    try {
      (callback as (input: typeof value) => void)(value);
    } catch {
      // Reader-status observers do not own engine or resource lifecycle.
    }
  }

  #notifyGenerationCompleted(
    generation: bigint,
    completion: GenerationCompletedEvent,
  ): void {
    try {
      this.#status?.onGenerationCompleted?.(
        generation,
        completion,
      );
    } catch {
      // Reader-status observers do not own engine or resource lifecycle.
    }
  }

  #setLifecycle(lifecycle: BrowserReaderLifecycle): void {
    this.#lifecycle = lifecycle;
    try {
      this.#status?.onLifecycle?.(lifecycle);
    } catch {
      // Reader-status observers do not own engine or resource lifecycle.
    }
  }

  #signalTerminalPort(): void {
    const handlers = this.#activePort?.handlers;
    if (
      handlers === undefined
      || this.#lifecycle === "Closing"
      || this.#lifecycle === "Closed"
      || this.#supervisor.state === "Failed"
      || this.#supervisor.state === "Stopped"
    ) {
      return;
    }
    handlers.onTerminated();
    this.#scheduleTurn();
  }

  #scheduleIfPending(): void {
    const queues = this.#supervisor.queueDepths;
    if (
      queues.inbound !== 0
      || queues.criticalCommands !== 0
      || queues.ordinaryCommands !== 0
      || queues.viewportCommands !== 0
      || queues.criticalEvents !== 0
      || queues.progressEvents !== 0
      || (this.#sourceBridge?.queuedResults ?? 0) !== 0
      || (this.#surfaceBridge?.queuedCallbacks ?? 0) !== 0
    ) {
      this.#scheduleTurn();
    }
  }

  #scheduleTurn(): void {
    if (
      this.#scheduledTicket !== undefined
      || this.#lifecycle === "Created"
      || (
        this.#lifecycle === "Closing"
        && !this.#gracefulShutdown
      )
      || this.#lifecycle === "Closed"
      || this.#lifecycle === "Failed"
    ) {
      return;
    }
    const ticket = {};
    this.#scheduledTicket = ticket;
    let handle: number;
    try {
      handle = this.#turns.request((): void => {
        if (this.#scheduledTicket !== ticket) {
          return;
        }
        this.#scheduledTicket = undefined;
        this.#scheduledHandle = undefined;
        try {
          this.pump();
        } catch {
          this.#failTerminal("WorkerTerminated");
        }
      });
    } catch {
      this.#scheduledTicket = undefined;
      this.#failTerminal("WorkerTerminated");
      return;
    }
    if (
      this.#scheduledTicket === ticket
      && (
        !Number.isSafeInteger(handle)
        || handle < 0
      )
    ) {
      this.#scheduledHandle = handle;
      this.#failTerminal("WorkerTerminated");
      return;
    }
    if (this.#scheduledTicket === ticket) {
      this.#scheduledHandle = handle;
    }
  }

  #cancelScheduledTurn(): void {
    const handle = this.#scheduledHandle;
    this.#scheduledTicket = undefined;
    this.#scheduledHandle = undefined;
    if (handle === undefined) {
      return;
    }
    try {
      this.#turns.cancel(handle);
    } catch {
      // The terminal client no longer observes scheduler failures.
    }
  }

  #terminateActivePort(): void {
    const port = this.#activePort;
    this.#activePort = undefined;
    if (port === undefined || !port.active) {
      return;
    }
    port.active = false;
    try {
      port.inner.terminate();
    } catch {
      // The Host no longer retains the port after terminal close.
    }
  }

  #discardLateWorkerValue(value: unknown): void {
    if (!Array.isArray(value)) {
      return;
    }
    const seen = new Set<unknown>();
    const maximum = Math.min(
      value.length - 1,
      MAX_TRANSFER_SLOTS,
    );
    for (let index = 1; index <= maximum; index += 1) {
      let resource: unknown;
      try {
        const descriptor = Object.getOwnPropertyDescriptor(
          value,
          String(index),
        );
        if (
          descriptor === undefined
          || !Object.prototype.hasOwnProperty.call(
            descriptor,
            "value",
          )
        ) {
          continue;
        }
        resource = descriptor.value;
      } catch {
        continue;
      }
      if (seen.has(resource)) {
        continue;
      }
      seen.add(resource);
      try {
        if (this.#surfaceAdapters.imageBitmap.isResource(resource)) {
          this.#surfaceAdapters.imageBitmap.close(resource);
        } else if (
          this.#surfaceAdapters.arrayBuffer.isResource(resource)
        ) {
          this.#surfaceAdapters.arrayBuffer.release(
            resource,
            undefined,
          );
        } else if (
          this.#surfaceAdapters.sharedArrayBuffer.isResource(resource)
        ) {
          this.#surfaceAdapters.sharedArrayBuffer.release(
            resource,
            undefined,
          );
        }
      } catch {
        // Late callbacks cannot regain an epoch even if cleanup fails.
      }
    }
  }

  #failTerminal(code: BrowserWorkerFaultCode): void {
    if (
      this.#lifecycle === "Closed"
      || (
        this.#lifecycle === "Closing"
        && !this.#gracefulShutdown
      )
    ) {
      return;
    }
    this.#cancelScheduledTurn();
    try {
      this.#sourceBridge?.fault();
    } catch {
      // Surface and port cleanup remain independent.
    }
    const worker = this.#supervisor.worker;
    if (worker !== 0n) {
      try {
        this.#surfaceBridge?.workerFault(worker, worker);
      } catch {
        // Every delivered handle is made terminal below.
      }
    }
    for (const delivered of Array.from(this.#delivered.values())) {
      this.#forgetDelivered(delivered);
    }
    this.#terminateActivePort();
    this.#session = undefined;
    this.#document = undefined;
    this.#gracefulShutdown = false;
    try {
      this.#handlers?.onWorkerFault(code);
    } catch {
      // A failing viewer callback cannot retain Host resources.
    }
    this.#lifecycle = "Failed";
    try {
      this.#status?.onLifecycle?.("Failed");
    } catch {
      // Reader-status observers do not own engine or resource lifecycle.
    }
  }
}

Object.freeze(BrowserReaderClient.prototype);
