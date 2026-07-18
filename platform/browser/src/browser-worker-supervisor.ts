import type {
  Command,
  CommandEnvelope,
  CompatibleHandshake,
  Correlation,
  Event,
  EventEnvelope,
  PayloadCodecResult,
  ProtocolHello,
} from "../generated/engine-protocol.js";
import {
  EndpointRole,
  EnvelopeSequenceTracker,
  GenerationCompletionStatus,
  MAX_MESSAGE_BYTES,
  MAX_TRANSFER_SLOTS,
  MESSAGE_DESCRIPTORS,
  PROTOCOL_MAJOR,
  PROTOCOL_MINOR,
  SCHEMA_HASH_HEX,
  encodeCancelCommandPayload,
  encodeCloseSessionCommandPayload,
  encodeCommandPayload,
  encodeCorrelationPayload,
  encodeFailDataCommandPayload,
  encodeGetPageMetricsCommandPayload,
  encodeHelloAcceptCommandPayload,
  encodeHelloCommandPayload,
  encodeOpenCommandPayload,
  encodeProvideDataCommandPayload,
  encodeReleaseSurfaceCommandPayload,
  encodeSetViewportCommandPayload,
  encodeShutdownCommandPayload,
  snapshotProtocolHello,
  validateHandshakeTranscript,
  validateProtocolHello,
} from "../generated/engine-protocol.js";
import {
  BrowserCommandAdmission,
  MAX_BROWSER_ADMISSION_REQUESTS,
  MAX_BROWSER_ADMISSION_SESSIONS,
  MAX_BROWSER_ADMISSION_SURFACES,
  type BrowserCommandAdmissionLimits,
  type BrowserWorkerLifecycleState,
} from "./browser-command-admission.js";
import {
  BROWSER_CONTROL_HEADER_BYTES,
  BrowserCommandBoundary,
} from "./browser-command-boundary.js";
import {
  BrowserEventBoundary,
  decodeBrowserEngineHello,
  type BrowserEventBoundaryErrorCode,
  type ValidatedBrowserEvent,
} from "./browser-event-boundary.js";
import { negotiateBrowserHello } from "./browser-handshake.js";

const MAX_U64 = 0xffff_ffff_ffff_ffffn;
const MAX_SUPERVISOR_QUEUE_CAPACITY = 4_096;
const MAX_SUPERVISOR_TURN = 4_096;

const canonicalSchemaHash = (): Uint8Array =>
  Uint8Array.from(
    SCHEMA_HASH_HEX.match(/.{2}/gu)?.map((byte) =>
      Number.parseInt(byte, 16),
    ) ?? [],
  );

const hasCanonicalSchemaHash = (hello: ProtocolHello): boolean => {
  const canonical = canonicalSchemaHash();
  return hello.schema_hash.byteLength === canonical.byteLength
    && hello.schema_hash.every(
      (byte, index) => byte === canonical[index],
    );
};

/** Stable supervisor API failures. */
export type BrowserWorkerSupervisorErrorCode =
  | "InvalidConfiguration"
  | "InvalidState"
  | "InvalidCommand"
  | "QueueFull"
  | "SequenceExhausted";

/** An API error that never contains caller or Worker payload data. */
export class BrowserWorkerSupervisorError extends Error {
  readonly code: BrowserWorkerSupervisorErrorCode;

  constructor(code: BrowserWorkerSupervisorErrorCode) {
    super(code);
    this.name = "BrowserWorkerSupervisorError";
    this.code = code;
  }
}

/** Stable local fault categories produced without trusting Worker text. */
export type BrowserWorkerFaultCode =
  | "FactoryFailure"
  | "HandlerRegistrationFailure"
  | "WorkerError"
  | "WorkerMessageError"
  | "WorkerTerminated"
  | "StartupTimeout"
  | "ShutdownTimeout"
  | "InboundQueueOverflow"
  | "ProtocolViolation"
  | "OutboundTransportFailure"
  | "CriticalEventQueueOverflow";

/** Callbacks installed on exactly one live Worker adapter. */
export interface BrowserWorkerHandlers {
  readonly onMessage: (value: unknown) => void;
  readonly onMessageError: () => void;
  readonly onError: () => void;
  /**
   * Optional platform-observed unexpected exit. Web Dedicated Workers expose
   * no normal-exit event, so the browser production adapter never fabricates
   * this callback; clean WorkerStopped and active `terminate()` own that path.
   */
  readonly onTerminated: () => void;
}

/**
 * Minimal Dedicated Worker adapter.
 *
 * Implementations translate platform-observable callbacks into these handlers.
 * `onTerminated` is available to ports with an actual exit signal; the Web
 * Dedicated Worker adapter has none and never infers one. Protocol/error and
 * active teardown remain available; the embedding must add bounded
 * request/watchdog liveness before claiming silent-UA-termination coverage.
 * `postMessage` receives an ordinary physical resource table and the exact
 * ArrayBuffers whose ownership is transferred.
 */
export interface BrowserWorkerPort {
  setHandlers(handlers: BrowserWorkerHandlers): void;
  postMessage(value: unknown[], transfer: ArrayBuffer[]): void;
  terminate(): void;
}

/** Creates one port for one monotonically increasing Worker epoch. */
export type BrowserWorkerFactory = (
  worker: bigint,
) => BrowserWorkerPort;

/** Monotonic clock injected so lifecycle tests never depend on sleeps. */
export interface BrowserWorkerClock {
  now(): number;
}

/** Independent queue bounds prevent replaceable traffic from consuming reserve. */
export interface BrowserWorkerSupervisorLimits {
  readonly maxInboundEvents: number;
  readonly maxCriticalCommands: number;
  readonly maxOrdinaryCommands: number;
  readonly maxViewportCommands: number;
  readonly maxCriticalEvents: number;
  readonly maxProgressEvents: number;
  readonly admission: BrowserCommandAdmissionLimits;
}

/** Immutable construction data for a single-viewer Worker supervisor. */
export interface BrowserWorkerSupervisorConfiguration {
  readonly hostHello: ProtocolHello;
  readonly limits: BrowserWorkerSupervisorLimits;
  readonly factory: BrowserWorkerFactory;
  readonly clock: BrowserWorkerClock;
  readonly startupTimeoutMs: number;
}

/** Application commands; handshake messages remain supervisor-owned. */
export type BrowserApplicationCommand = Exclude<
  Command,
  | { type: "Hello"; payload: unknown }
  | { type: "HelloAccept"; payload: unknown }
>;

/** Host correlation with the current Worker epoch injected by the supervisor. */
export type BrowserApplicationCorrelation = Omit<Correlation, "worker">;

/** Decoded protocol traffic or one bounded local transport fault. */
export type BrowserSupervisorEvent =
  | Readonly<{
    readonly type: "ProtocolEvent";
    readonly value: ValidatedBrowserEvent;
  }>
  | Readonly<{
    readonly type: "WorkerFault";
    readonly worker: bigint;
    readonly origin: "HostTransport";
    readonly code: BrowserWorkerFaultCode;
  }>
  | Readonly<{
    readonly type: "WorkerFault";
    readonly worker: bigint;
    readonly origin: "EngineProtocol";
    readonly code: "WorkerFault" | "ProtocolFault";
    readonly value: ValidatedBrowserEvent;
  }>;

/** Observable bounded queue state for deterministic tests and host scheduling. */
export interface BrowserWorkerSupervisorQueueDepths {
  readonly inbound: number;
  readonly criticalCommands: number;
  readonly ordinaryCommands: number;
  readonly viewportCommands: number;
  readonly criticalEvents: number;
  readonly progressEvents: number;
}

interface SnapshotLimits {
  readonly maxInboundEvents: number;
  readonly maxCriticalCommands: number;
  readonly maxOrdinaryCommands: number;
  readonly maxViewportCommands: number;
  readonly maxCriticalEvents: number;
  readonly maxProgressEvents: number;
  readonly admission: Readonly<BrowserCommandAdmissionLimits>;
}

interface QueuedCommand {
  readonly sequence: bigint;
  readonly type: Command["type"];
  readonly envelope: CommandEnvelope;
  readonly frame: unknown[];
  readonly transfer: ArrayBuffer[];
}

interface RequestState {
  readonly kind: "Open" | "Session";
  readonly admissionSession: bigint | undefined;
  state: "Active" | "Terminal";
  session: bigint | undefined;
}

interface SurfaceState {
  readonly session: bigint;
  readonly generation: bigint;
  readonly leaseToken: bigint;
  state: "Alive" | "Reclaimed";
}

interface GenerationState {
  readonly generation: bigint;
  state: "Active" | "Terminal";
  terminalStatus: GenerationCompletionStatus | undefined;
}

const unwrapBytes = (
  result: PayloadCodecResult<Uint8Array>,
): Uint8Array | undefined => result.ok ? result.value : undefined;

const encodeCommandRecord = (
  command: Command,
): Uint8Array | undefined => {
  switch (command.type) {
    case "Hello":
      return unwrapBytes(encodeHelloCommandPayload(command.payload));
    case "HelloAccept":
      return unwrapBytes(
        encodeHelloAcceptCommandPayload(command.payload),
      );
    case "Open":
      return unwrapBytes(encodeOpenCommandPayload(command.payload));
    case "ProvideData":
      return unwrapBytes(
        encodeProvideDataCommandPayload(command.payload),
      );
    case "SetViewport":
      return unwrapBytes(
        encodeSetViewportCommandPayload(command.payload),
      );
    case "Cancel":
      return unwrapBytes(encodeCancelCommandPayload(command.payload));
    case "ReleaseSurface":
      return unwrapBytes(
        encodeReleaseSurfaceCommandPayload(command.payload),
      );
    case "CloseSession":
      return unwrapBytes(
        encodeCloseSessionCommandPayload(command.payload),
      );
    case "Shutdown":
      return unwrapBytes(encodeShutdownCommandPayload(command.payload));
    case "FailData":
      return unwrapBytes(encodeFailDataCommandPayload(command.payload));
    case "GetPageMetrics":
      return unwrapBytes(
        encodeGetPageMetricsCommandPayload(command.payload),
      );
  }
};

const commandMessageId = (command: Command): number | undefined =>
  MESSAGE_DESCRIPTORS.find(
    (descriptor) =>
      descriptor.kind === "command"
      && descriptor.name === command.type,
  )?.id;

const buildCommandFrame = (
  command: Command,
  correlation: Correlation,
  sequence: bigint,
  resources: readonly ArrayBuffer[],
): Readonly<{
  envelope: CommandEnvelope;
  queued: QueuedCommand;
}> | undefined => {
  const messageId = commandMessageId(command);
  const encodedCorrelation = unwrapBytes(
    encodeCorrelationPayload(correlation),
  );
  const encodedRecord = encodeCommandRecord(command);
  if (
    messageId === undefined
    || encodedCorrelation === undefined
    || encodedRecord === undefined
  ) {
    return undefined;
  }
  const payloadLength =
    encodedCorrelation.byteLength + encodedRecord.byteLength;
  const envelope: CommandEnvelope = {
    header: {
      major: PROTOCOL_MAJOR,
      minor: PROTOCOL_MINOR,
      message_type: messageId,
      flags: 0,
      payload_len: payloadLength,
      sequence,
    },
    correlation,
    command,
  };
  const encoded = encodeCommandPayload(envelope);
  if (
    !encoded.ok
    || encoded.value.messageId !== messageId
    || encoded.value.bytes.byteLength !== payloadLength
  ) {
    return undefined;
  }

  const control = new ArrayBuffer(
    BROWSER_CONTROL_HEADER_BYTES + payloadLength,
  );
  const header = new DataView(control);
  header.setUint16(0, PROTOCOL_MAJOR, true);
  header.setUint16(2, PROTOCOL_MINOR, true);
  header.setUint16(4, messageId, true);
  header.setUint16(6, 0, true);
  header.setUint32(8, payloadLength, true);
  header.setBigUint64(12, sequence, true);
  new Uint8Array(control, BROWSER_CONTROL_HEADER_BYTES).set(
    encoded.value.bytes,
  );
  const frame: unknown[] = [control, ...resources];
  const transfer: ArrayBuffer[] = [control, ...resources];
  return Object.freeze({
    envelope,
    queued: Object.freeze({
      sequence,
      type: command.type,
      envelope,
      frame,
      transfer,
    }),
  });
};

const snapshotCapacity = (value: unknown): number | undefined =>
  typeof value === "number"
  && Number.isSafeInteger(value)
  && value > 0
  && value <= MAX_SUPERVISOR_QUEUE_CAPACITY
    ? value
    : undefined;

const snapshotAdmissionLimits = (
  value: unknown,
): Readonly<BrowserCommandAdmissionLimits> | undefined => {
  if (typeof value !== "object" || value === null) {
    return undefined;
  }
  const input = value as Partial<BrowserCommandAdmissionLimits>;
  if (
    typeof input.maxSessions !== "number"
    || typeof input.maxRequests !== "number"
    || typeof input.maxSurfaces !== "number"
    || !Number.isSafeInteger(input.maxSessions)
    || input.maxSessions < 0
    || input.maxSessions > MAX_BROWSER_ADMISSION_SESSIONS
    || !Number.isSafeInteger(input.maxRequests)
    || input.maxRequests < 0
    || input.maxRequests > MAX_BROWSER_ADMISSION_REQUESTS
    || !Number.isSafeInteger(input.maxSurfaces)
    || input.maxSurfaces < 0
    || input.maxSurfaces > MAX_BROWSER_ADMISSION_SURFACES
  ) {
    return undefined;
  }
  return Object.freeze({
    maxSessions: input.maxSessions,
    maxRequests: input.maxRequests,
    maxSurfaces: input.maxSurfaces,
  });
};

const snapshotLimits = (
  value: BrowserWorkerSupervisorLimits,
): SnapshotLimits | undefined => {
  const maxInboundEvents = snapshotCapacity(value.maxInboundEvents);
  const maxCriticalCommands = snapshotCapacity(
    value.maxCriticalCommands,
  );
  const maxOrdinaryCommands = snapshotCapacity(
    value.maxOrdinaryCommands,
  );
  const maxViewportCommands = snapshotCapacity(
    value.maxViewportCommands,
  );
  const maxCriticalEvents = snapshotCapacity(value.maxCriticalEvents);
  const maxProgressEvents = snapshotCapacity(value.maxProgressEvents);
  const admission = snapshotAdmissionLimits(value.admission);
  if (
    maxInboundEvents === undefined
    || maxCriticalCommands === undefined
    || maxOrdinaryCommands === undefined
    || maxViewportCommands === undefined
    || maxCriticalEvents === undefined
    || maxProgressEvents === undefined
    || admission === undefined
  ) {
    return undefined;
  }
  return Object.freeze({
    maxInboundEvents,
    maxCriticalCommands,
    maxOrdinaryCommands,
    maxViewportCommands,
    maxCriticalEvents,
    maxProgressEvents,
    admission,
  });
};

const isTurnBudget = (value: number): boolean =>
  Number.isSafeInteger(value)
  && value > 0
  && value <= MAX_SUPERVISOR_TURN;

const isCriticalCommand = (command: Command): boolean =>
  command.type === "Hello"
  || command.type === "HelloAccept"
  || command.type === "Cancel"
  || command.type === "ReleaseSurface"
  || command.type === "CloseSession"
  || command.type === "Shutdown"
  || command.type === "FailData";

const isProgressEvent = (event: Event): boolean =>
  event.type === "CapabilityReported"
  || event.type === "GenerationPlanned";

/**
 * Owns one live Dedicated Worker epoch and exposes only explicitly drained work.
 *
 * Worker callbacks enqueue opaque data or a stable fault marker. They never
 * decode protocol messages, transition Native state, invoke engine work, or
 * call application code on a browser completion stack.
 */
export class BrowserWorkerSupervisor {
  readonly #hostHello: ProtocolHello;
  readonly #limits: SnapshotLimits;
  readonly #factory: BrowserWorkerFactory;
  readonly #clock: BrowserWorkerClock;
  readonly #startupTimeoutMs: number;

  #state: BrowserWorkerLifecycleState = "NotStarted";
  #worker = 0n;
  #port: BrowserWorkerPort | undefined;
  #epochToken: object = {};
  #pendingCallbackFault: BrowserWorkerFaultCode | undefined;
  #lifecycleDeadline: number | undefined;

  #connection: CompatibleHandshake | undefined;
  #admission: BrowserCommandAdmission | undefined;
  #commandBoundary: BrowserCommandBoundary | undefined;
  #eventBoundary: BrowserEventBoundary | undefined;
  #receiveSequence: EnvelopeSequenceTracker | undefined;
  #nextSendSequence = 1n;

  #hostHelloCommand: Command | undefined;
  #engineHelloEvent: Event | undefined;
  #hostAcceptCommand: Command | undefined;
  #helloSent = false;
  #helloAcceptSent = false;

  readonly #inbound: unknown[] = [];
  readonly #criticalCommands: QueuedCommand[] = [];
  readonly #ordinaryCommands: QueuedCommand[] = [];
  readonly #viewportCommands = new Map<bigint, QueuedCommand>();
  readonly #criticalEvents: BrowserSupervisorEvent[] = [];
  readonly #progressEvents = new Map<string, BrowserSupervisorEvent>();

  readonly #sessions =
    new Map<bigint, "Opening" | "Ready" | "Closing" | "Closed">();
  readonly #requests = new Map<bigint, RequestState>();
  readonly #generations = new Map<bigint, GenerationState>();
  /**
   * Successfully sent generations displaced before their terminal arrives.
   * Each per-session set is capped by the critical-event capacity, and the
   * session admission bound caps the number of sets.
   */
  readonly #displacedGenerationTerminals =
    new Map<bigint, Set<bigint>>();
  readonly #surfaces = new Map<bigint, SurfaceState>();

  constructor(configuration: BrowserWorkerSupervisorConfiguration) {
    const limits = snapshotLimits(configuration.limits);
    if (
      new.target !== BrowserWorkerSupervisor
      || limits === undefined
      || typeof configuration.factory !== "function"
      || typeof configuration.clock !== "object"
      || configuration.clock === null
      || typeof configuration.clock.now !== "function"
      || !Number.isSafeInteger(configuration.startupTimeoutMs)
      || configuration.startupTimeoutMs <= 0
      || configuration.startupTimeoutMs > 600_000
      || !validateProtocolHello(configuration.hostHello)
      || configuration.hostHello.endpoint_role !== EndpointRole.Host
      || !hasCanonicalSchemaHash(configuration.hostHello)
    ) {
      throw new BrowserWorkerSupervisorError("InvalidConfiguration");
    }
    this.#hostHello = snapshotProtocolHello(configuration.hostHello);
    this.#limits = limits;
    this.#factory = configuration.factory;
    this.#clock = configuration.clock;
    this.#startupTimeoutMs = configuration.startupTimeoutMs;
  }

  /** Current terminal or live lifecycle state. */
  get state(): BrowserWorkerLifecycleState {
    return this.#state;
  }

  /** Current Worker epoch, or zero before first start. */
  get worker(): bigint {
    return this.#worker;
  }

  /** Negotiated connection after EngineHello, if handshake succeeded. */
  get connection(): CompatibleHandshake | undefined {
    return this.#connection;
  }

  /** True only while this supervisor owns one live port. */
  get hasLiveWorker(): boolean {
    return this.#port !== undefined;
  }

  /** Exact bounded depths; no queue is hidden behind a callback. */
  get queueDepths(): BrowserWorkerSupervisorQueueDepths {
    return Object.freeze({
      inbound: this.#inbound.length,
      criticalCommands: this.#criticalCommands.length,
      ordinaryCommands: this.#ordinaryCommands.length,
      viewportCommands: this.#viewportCommands.size,
      criticalEvents: this.#criticalEvents.length,
      progressEvents: this.#progressEvents.size,
    });
  }

  /** Starts the first epoch and queues Host Hello. */
  start(): void {
    if (this.#state !== "NotStarted") {
      throw new BrowserWorkerSupervisorError("InvalidState");
    }
    this.#startEpoch();
  }

  /** Starts a fresh epoch only after the previous port is terminal. */
  restart(): void {
    if (
      (this.#state !== "Failed" && this.#state !== "Stopped")
      || this.#port !== undefined
    ) {
      throw new BrowserWorkerSupervisorError("InvalidState");
    }
    this.#startEpoch();
  }

  /**
   * Validates and queues an application command for the current epoch.
   *
   * Critical and ordinary capacity checks occur before sequence admission.
   * SetViewport replaces the pending command for the same session.
   */
  submit(
    command: BrowserApplicationCommand,
    correlation: BrowserApplicationCorrelation,
    resources: readonly ArrayBuffer[] = [],
  ): void {
    if (
      this.#port === undefined
      || this.#commandBoundary === undefined
      || this.#admission === undefined
    ) {
      throw new BrowserWorkerSupervisorError("InvalidState");
    }
    const fullCorrelation: Correlation = {
      worker: this.#worker,
      ...correlation,
    };
    this.#admitAndQueue(
      command,
      fullCorrelation,
      resources,
    );
  }

  /** Sends at most `maximum` queued frames in accepted sequence order. */
  drainOutboundTurn(maximum = MAX_SUPERVISOR_TURN): number {
    if (!isTurnBudget(maximum)) {
      throw new BrowserWorkerSupervisorError("InvalidConfiguration");
    }
    let sent = 0;
    while (sent < maximum && this.#port !== undefined) {
      const queued = this.#takeNextCommand();
      if (queued === undefined) {
        break;
      }
      try {
        this.#port.postMessage(queued.frame, queued.transfer);
      } catch {
        this.#enterFailed("OutboundTransportFailure");
        break;
      }
      try {
        this.#commitSentCommand(queued.envelope);
      } catch {
        this.#enterFailed("ProtocolViolation");
        break;
      }
      if (queued.type === "Hello") {
        this.#helloSent = true;
      } else if (queued.type === "HelloAccept") {
        this.#helloAcceptSent = true;
      }
      sent += 1;
    }
    return sent;
  }

  /** Decodes and applies at most `maximum` callback-enqueued messages. */
  processInboundTurn(maximum = MAX_SUPERVISOR_TURN): number {
    if (!isTurnBudget(maximum)) {
      throw new BrowserWorkerSupervisorError("InvalidConfiguration");
    }
    if (this.#pendingCallbackFault !== undefined) {
      const fault = this.#pendingCallbackFault;
      this.#pendingCallbackFault = undefined;
      this.#enterFailed(fault);
      return 0;
    }

    let processed = 0;
    while (
      processed < maximum
      && this.#inbound.length > 0
      && this.#port !== undefined
    ) {
      const value = this.#inbound.shift();
      try {
        if (this.#connection === undefined) {
          this.#acceptEngineHello(value);
        } else {
          this.#acceptNegotiatedEvent(value);
        }
      } catch {
        this.#enterFailed("ProtocolViolation");
        break;
      }
      processed += 1;
      if (this.#pendingCallbackFault !== undefined) {
        const fault = this.#pendingCallbackFault;
        this.#pendingCallbackFault = undefined;
        this.#enterFailed(fault);
        break;
      }
    }
    return processed;
  }

  /** Applies startup and graceful-shutdown deadlines using the injected clock. */
  pollClock(): void {
    const deadline = this.#lifecycleDeadline;
    if (deadline === undefined) {
      return;
    }
    const now = this.#readNow();
    if (now < deadline) {
      return;
    }
    if (this.#state === "Starting") {
      this.#enterFailed("StartupTimeout");
    } else if (this.#state === "Draining") {
      this.#enterFailed("ShutdownTimeout");
    } else {
      this.#lifecycleDeadline = undefined;
    }
  }

  /** Returns queued decoded traffic without invoking host callbacks inline. */
  takeEvents(maximum = MAX_SUPERVISOR_TURN): BrowserSupervisorEvent[] {
    if (!isTurnBudget(maximum)) {
      throw new BrowserWorkerSupervisorError("InvalidConfiguration");
    }
    const events: BrowserSupervisorEvent[] = [];
    while (events.length < maximum) {
      const event = this.#takeNextEvent();
      if (event === undefined) {
        break;
      }
      if (this.#releaseIfSurfaceBecameStale(event)) {
        continue;
      }
      events.push(event);
    }
    return events;
  }

  #releaseIfSurfaceBecameStale(
    event: BrowserSupervisorEvent,
  ): boolean {
    if (
      event.type !== "ProtocolEvent"
      || event.value.envelope.event.type !== "SurfaceReady"
    ) {
      return false;
    }
    const { correlation } = event.value.envelope;
    const session = correlation.session;
    const generation = correlation.generation;
    const generationState = session === undefined
      ? undefined
      : this.#generations.get(session);
    const metadata = event.value.envelope.event.payload.metadata;
    const surface = this.#surfaces.get(metadata.id);
    const exactSurface = surface !== undefined
      && surface.session === session
      && surface.generation === generation
      && surface.leaseToken === metadata.lease_token;
    if (!exactSurface || surface.state === "Reclaimed") {
      if (surface !== undefined && surface.state === "Alive") {
        this.#enterFailed("ProtocolViolation");
      }
      return true;
    }
    if (
      session !== undefined
      && generation !== undefined
      && generationState?.generation === generation
      && (
        generationState.state === "Active"
        || (
          generationState.state === "Terminal"
          && generationState.terminalStatus
            === GenerationCompletionStatus.Completed
        )
      )
      && this.#sessions.get(session) === "Ready"
      && this.#state === "Ready"
    ) {
      return false;
    }
    if (session === undefined) {
      this.#enterFailed("ProtocolViolation");
      return true;
    }
    if (this.#state !== "Ready" && this.#state !== "Draining") {
      return true;
    }
    try {
      this.#admitAndQueue(
        Object.freeze({
          type: "ReleaseSurface",
          payload: Object.freeze({
            surface: metadata.id,
            lease_token: metadata.lease_token,
          }),
        }),
        Object.freeze({
          worker: this.#worker,
          session,
        }),
        [],
      );
    } catch {
      // Terminating the epoch is the only safe fallback when the reserved
      // lifecycle path cannot accept the release.
      this.#enterFailed("ProtocolViolation");
    }
    return true;
  }

  #startEpoch(): void {
    if (this.#worker >= MAX_U64) {
      throw new BrowserWorkerSupervisorError("SequenceExhausted");
    }
    const startupDeadline = this.#deadlineAfter(
      this.#startupTimeoutMs,
    );
    this.#resetEpochState();
    this.#worker += 1n;
    this.#state = "Starting";
    this.#lifecycleDeadline = startupDeadline;
    this.#admission = new BrowserCommandAdmission(
      "Starting",
      this.#limits.admission,
    );
    this.#receiveSequence = new EnvelopeSequenceTracker();

    let port: BrowserWorkerPort;
    try {
      port = this.#factory(this.#worker);
      if (
        typeof port !== "object"
        || port === null
        || typeof port.setHandlers !== "function"
        || typeof port.postMessage !== "function"
        || typeof port.terminate !== "function"
      ) {
        throw new Error("invalid port");
      }
    } catch {
      this.#enterFailed("FactoryFailure");
      return;
    }

    const token = {};
    this.#epochToken = token;
    this.#port = port;
    try {
      port.setHandlers(Object.freeze({
        onMessage: (value: unknown): void => {
          if (this.#epochToken !== token || this.#port !== port) {
            return;
          }
          if (this.#inbound.length >= this.#limits.maxInboundEvents) {
            this.#pendingCallbackFault ??= "InboundQueueOverflow";
            return;
          }
          this.#inbound.push(value);
        },
        onMessageError: (): void => {
          this.#queueCallbackFault(token, port, "WorkerMessageError");
        },
        onError: (): void => {
          this.#queueCallbackFault(token, port, "WorkerError");
        },
        onTerminated: (): void => {
          this.#queueCallbackFault(token, port, "WorkerTerminated");
        },
      }));
    } catch {
      this.#enterFailed("HandlerRegistrationFailure");
      return;
    }

    const hello: Command = Object.freeze({
      type: "Hello",
      payload: Object.freeze({ hello: this.#hostHello }),
    });
    const correlation: Correlation = Object.freeze({
      worker: this.#worker,
    });
    const built = buildCommandFrame(
      hello,
      correlation,
      this.#nextSendSequence,
      [],
    );
    if (built === undefined) {
      this.#enterFailed("ProtocolViolation");
      return;
    }
    this.#hostHelloCommand = hello;
    this.#criticalCommands.push(built.queued);
    this.#nextSendSequence += 1n;
  }

  #resetEpochState(): void {
    this.#connection = undefined;
    this.#admission = undefined;
    this.#commandBoundary = undefined;
    this.#eventBoundary = undefined;
    this.#receiveSequence = undefined;
    this.#nextSendSequence = 1n;
    this.#hostHelloCommand = undefined;
    this.#engineHelloEvent = undefined;
    this.#hostAcceptCommand = undefined;
    this.#helloSent = false;
    this.#helloAcceptSent = false;
    this.#pendingCallbackFault = undefined;
    this.#lifecycleDeadline = undefined;
    this.#inbound.length = 0;
    this.#criticalCommands.length = 0;
    this.#ordinaryCommands.length = 0;
    this.#viewportCommands.clear();
    this.#criticalEvents.length = 0;
    this.#progressEvents.clear();
    this.#sessions.clear();
    this.#requests.clear();
    this.#generations.clear();
    this.#displacedGenerationTerminals.clear();
    this.#surfaces.clear();
  }

  #queueCallbackFault(
    token: object,
    port: BrowserWorkerPort,
    code: BrowserWorkerFaultCode,
  ): void {
    if (this.#epochToken === token && this.#port === port) {
      this.#pendingCallbackFault ??= code;
    }
  }

  #acceptEngineHello(value: unknown): void {
    const receiveSequence = this.#receiveSequence;
    const admission = this.#admission;
    const hostHelloCommand = this.#hostHelloCommand;
    if (
      this.#state !== "Starting"
      || !this.#helloSent
      || receiveSequence === undefined
      || admission === undefined
      || hostHelloCommand?.type !== "Hello"
    ) {
      throw new BrowserWorkerSupervisorError("InvalidState");
    }
    let negotiatedConnection: CompatibleHandshake | undefined;
    const envelope = decodeBrowserEngineHello(
      value,
      this.#worker,
      receiveSequence,
      (candidate: EventEnvelope): boolean => {
        if (candidate.event.type !== "EngineHello") {
          return false;
        }
        try {
          negotiatedConnection = negotiateBrowserHello(
            this.#hostHello,
            candidate.event.payload.hello,
          );
          return true;
        } catch {
          return false;
        }
      },
    );
    if (
      envelope.event.type !== "EngineHello"
      || negotiatedConnection === undefined
    ) {
      throw new BrowserWorkerSupervisorError("InvalidCommand");
    }
    const connection = negotiatedConnection;
    this.#connection = connection;
    this.#engineHelloEvent = envelope.event;
    this.#commandBoundary = new BrowserCommandBoundary(
      this.#worker,
      connection,
      admission,
    );
    this.#eventBoundary = new BrowserEventBoundary(
      this.#worker,
      connection,
      receiveSequence,
      (candidate: EventEnvelope): BrowserEventBoundaryErrorCode | undefined =>
        this.#validateInboundLifecycle(candidate),
    );

    const accept: Command = Object.freeze({
      type: "HelloAccept",
      payload: Object.freeze({
        negotiated_minor: connection.minor,
        schema_hash: this.#hostHello.schema_hash.slice(),
      }),
    });
    this.#hostAcceptCommand = accept;
    this.#admitAndQueue(
      accept,
      Object.freeze({ worker: this.#worker }),
      [],
    );
  }

  #acceptNegotiatedEvent(value: unknown): void {
    const boundary = this.#eventBoundary;
    if (boundary === undefined) {
      throw new BrowserWorkerSupervisorError("InvalidState");
    }
    const accepted = boundary.decode(value);
    const event = accepted.envelope.event;
    const wrapped: BrowserSupervisorEvent = Object.freeze({
      type: "ProtocolEvent",
      value: accepted,
    });
    if (event.type === "WorkerFault" || event.type === "ProtocolFault") {
      this.#enterFailed(
        "ProtocolViolation",
        Object.freeze({
          type: "WorkerFault",
          worker: this.#worker,
          origin: "EngineProtocol",
          code: event.type,
          value: accepted,
        }),
      );
      return;
    }
    this.#applyInboundEvent(accepted.envelope);
    this.#publishEvent(wrapped);
  }

  #admitAndQueue(
    command: Command,
    correlation: Correlation,
    resources: readonly ArrayBuffer[],
  ): void {
    const boundary = this.#commandBoundary;
    if (boundary === undefined) {
      throw new BrowserWorkerSupervisorError("InvalidState");
    }
    let queue: "Critical" | "Ordinary" | "Viewport";
    let viewportSession: bigint | undefined;
    if (command.type === "SetViewport") {
      queue = "Viewport";
      viewportSession = correlation.session;
      if (viewportSession === undefined) {
        throw new BrowserWorkerSupervisorError("InvalidCommand");
      }
      if (
        !this.#viewportCommands.has(viewportSession)
        && this.#viewportCommands.size
          >= this.#limits.maxViewportCommands
      ) {
        throw new BrowserWorkerSupervisorError("QueueFull");
      }
      if (
        !this.#viewportCommands.has(viewportSession)
        && this.#generations.get(viewportSession)?.state === "Active"
        && (
          this.#displacedGenerationTerminals.get(viewportSession)?.size
            ?? 0
        ) >= this.#limits.maxCriticalEvents
      ) {
        throw new BrowserWorkerSupervisorError("QueueFull");
      }
    } else if (isCriticalCommand(command)) {
      queue = "Critical";
      if (
        this.#criticalCommands.length
        >= this.#limits.maxCriticalCommands
      ) {
        throw new BrowserWorkerSupervisorError("QueueFull");
      }
    } else {
      queue = "Ordinary";
      if (
        this.#ordinaryCommands.length
        >= this.#limits.maxOrdinaryCommands
      ) {
        throw new BrowserWorkerSupervisorError("QueueFull");
      }
    }
    if (this.#nextSendSequence > MAX_U64) {
      throw new BrowserWorkerSupervisorError("SequenceExhausted");
    }
    const built = buildCommandFrame(
      command,
      correlation,
      this.#nextSendSequence,
      resources,
    );
    if (built === undefined) {
      throw new BrowserWorkerSupervisorError("InvalidCommand");
    }
    let accepted: CommandEnvelope;
    try {
      accepted = boundary.decode(built.queued.frame).envelope;
    } catch {
      throw new BrowserWorkerSupervisorError("InvalidCommand");
    }
    try {
      this.#reserveOutboundCommand(accepted);
    } catch {
      this.#enterFailed("ProtocolViolation");
      throw new BrowserWorkerSupervisorError("InvalidCommand");
    }
    this.#nextSendSequence += 1n;
    switch (queue) {
      case "Critical":
        this.#criticalCommands.push(built.queued);
        break;
      case "Ordinary":
        this.#ordinaryCommands.push(built.queued);
        break;
      case "Viewport": {
        if (viewportSession === undefined) {
          throw new BrowserWorkerSupervisorError("InvalidCommand");
        }
        this.#viewportCommands.set(viewportSession, built.queued);
        break;
      }
    }
  }

  #reserveOutboundCommand(envelope: CommandEnvelope): void {
    const admission = this.#admission;
    if (admission === undefined) {
      throw new BrowserWorkerSupervisorError("InvalidState");
    }
    const { command, correlation } = envelope;
    switch (command.type) {
      case "Open": {
        const request = correlation.request;
        if (request === undefined) {
          throw new BrowserWorkerSupervisorError("InvalidCommand");
        }
        admission.setRequestState(request, "Active");
        break;
      }
      case "GetPageMetrics": {
        const { request, session } = correlation;
        if (request === undefined || session === undefined) {
          throw new BrowserWorkerSupervisorError("InvalidCommand");
        }
        admission.setRequestState(request, "Active", session);
        break;
      }
      case "SetViewport": {
        const { session, generation } = correlation;
        if (session === undefined || generation === undefined) {
          throw new BrowserWorkerSupervisorError("InvalidCommand");
        }
        admission.setActiveGeneration(session, generation);
        break;
      }
      case "CloseSession": {
        const session = correlation.session;
        if (session === undefined) {
          throw new BrowserWorkerSupervisorError("InvalidCommand");
        }
        if (this.#sessions.get(session) !== "Closed") {
          admission.setSessionState(session, "Closing");
        }
        this.#viewportCommands.delete(session);
        break;
      }
      case "Shutdown":
        admission.setWorkerState("Draining");
        this.#ordinaryCommands.length = 0;
        this.#viewportCommands.clear();
        break;
      case "Hello":
      case "HelloAccept":
      case "ProvideData":
      case "Cancel":
      case "ReleaseSurface":
      case "FailData":
        break;
    }
  }

  #commitSentCommand(envelope: CommandEnvelope): void {
    const { command, correlation } = envelope;
    switch (command.type) {
      case "Open": {
        const request = correlation.request;
        if (request === undefined || this.#requests.has(request)) {
          throw new BrowserWorkerSupervisorError("InvalidCommand");
        }
        this.#requests.set(request, {
          kind: "Open",
          admissionSession: undefined,
          state: "Active",
          session: undefined,
        });
        break;
      }
      case "GetPageMetrics": {
        const { request, session } = correlation;
        if (
          request === undefined
          || session === undefined
          || this.#requests.has(request)
        ) {
          throw new BrowserWorkerSupervisorError("InvalidCommand");
        }
        this.#requests.set(request, {
          kind: "Session",
          admissionSession: session,
          state: "Active",
          session,
        });
        break;
      }
      case "SetViewport": {
        const { session, generation } = correlation;
        if (session === undefined || generation === undefined) {
          throw new BrowserWorkerSupervisorError("InvalidCommand");
        }
        const current = this.#generations.get(session);
        if (current?.state === "Active") {
          this.#recordDisplacedGeneration(
            session,
            current.generation,
          );
        }
        this.#generations.set(session, {
          generation,
          state: "Active",
          terminalStatus: undefined,
        });
        break;
      }
      case "CloseSession": {
        const session = correlation.session;
        if (session === undefined) {
          throw new BrowserWorkerSupervisorError("InvalidCommand");
        }
        if (this.#sessions.get(session) !== "Closed") {
          this.#sessions.set(session, "Closing");
        }
        break;
      }
      case "Shutdown":
        if (this.#state === "Ready") {
          this.#state = "Draining";
        }
        if (this.#state !== "Draining") {
          throw new BrowserWorkerSupervisorError("InvalidCommand");
        }
        this.#lifecycleDeadline = this.#deadlineAfter(
          command.payload.deadline_ms,
        );
        break;
      case "Hello":
      case "HelloAccept":
      case "ProvideData":
      case "Cancel":
      case "ReleaseSurface":
      case "FailData":
        break;
    }
  }

  #takeNextCommand(): QueuedCommand | undefined {
    const critical = this.#criticalCommands[0];
    const ordinary = this.#ordinaryCommands[0];
    let viewport:
      | Readonly<{ session: bigint; command: QueuedCommand }>
      | undefined;
    for (const [session, command] of this.#viewportCommands) {
      if (
        viewport === undefined
        || command.sequence < viewport.command.sequence
      ) {
        viewport = Object.freeze({ session, command });
      }
    }
    const candidates = [
      critical === undefined
        ? undefined
        : Object.freeze({
          kind: "Critical" as const,
          command: critical,
        }),
      ordinary === undefined
        ? undefined
        : Object.freeze({
          kind: "Ordinary" as const,
          command: ordinary,
        }),
      viewport === undefined
        ? undefined
        : Object.freeze({
          kind: "Viewport" as const,
          command: viewport.command,
          session: viewport.session,
        }),
    ].filter((candidate) => candidate !== undefined);
    let selected = candidates[0];
    if (selected === undefined) {
      return undefined;
    }
    for (const candidate of candidates.slice(1)) {
      if (candidate.command.sequence < selected.command.sequence) {
        selected = candidate;
      }
    }
    switch (selected.kind) {
      case "Critical":
        this.#criticalCommands.shift();
        break;
      case "Ordinary":
        this.#ordinaryCommands.shift();
        break;
      case "Viewport":
        this.#viewportCommands.delete(selected.session);
        break;
    }
    return selected.command;
  }

  #validateInboundLifecycle(
    envelope: EventEnvelope,
  ): BrowserEventBoundaryErrorCode | undefined {
    const { correlation, event } = envelope;
    const session = correlation.session;
    const request = correlation.request;
    const generation = correlation.generation;
    switch (event.type) {
      case "Ready": {
        if (
          this.#state !== "Starting"
          || !this.#helloAcceptSent
          || this.#hostHelloCommand === undefined
          || this.#engineHelloEvent === undefined
          || this.#hostAcceptCommand === undefined
        ) {
          return "InvalidLifecycle";
        }
        try {
          return validateHandshakeTranscript(
            this.#hostHelloCommand,
            this.#engineHelloEvent,
            this.#hostAcceptCommand,
            event,
          ) === undefined
            ? "InvalidLifecycle"
            : undefined;
        } catch {
          return "InvalidLifecycle";
        }
      }
      case "NeedData":
      case "DocumentReady": {
        if (
          this.#state !== "Ready"
          || session === undefined
          || request === undefined
        ) {
          return "InvalidLifecycle";
        }
        const requestState = this.#requests.get(request);
        const sessionState = this.#sessions.get(session);
        return (
          requestState?.kind === "Open"
          && requestState.state === "Active"
          && (
            requestState.session === session
            || (
              requestState.session === undefined
              && sessionState === undefined
              && this.#sessions.size
                < this.#limits.admission.maxSessions
            )
          )
          && (
            sessionState === undefined
            || sessionState === "Opening"
            || (
              event.type === "NeedData"
              && sessionState === "Ready"
            )
          )
        )
          ? undefined
          : "InvalidLifecycle";
      }
      case "CapabilityReported":
      case "SurfaceReady":
      case "GenerationPlanned":
      case "GenerationCompleted":
        if (
          session === undefined
          || generation === undefined
        ) {
          return "InvalidLifecycle";
        }
        {
          const sessionState = this.#sessions.get(session);
          const generationState = this.#generations.get(session);
          const isCurrentActive = (
            generationState?.generation === generation
            && generationState.state === "Active"
          );
          if (
            this.#state === "Ready"
            && sessionState === "Ready"
            && isCurrentActive
            && (
              event.type !== "SurfaceReady"
              || (
                !this.#surfaces.has(event.payload.metadata.id)
                && this.#surfaces.size
                  < this.#limits.admission.maxSurfaces
              )
            )
          ) {
            return undefined;
          }
          return (
            event.type === "GenerationCompleted"
            && event.payload.status
              === GenerationCompletionStatus.Superseded
            && (
              isCurrentActive
              || this.#isDisplacedGeneration(session, generation)
            )
            && (
              (
                this.#state === "Ready"
                && (
                  sessionState === "Ready"
                  || sessionState === "Closing"
                )
              )
              || (
                this.#state === "Draining"
                && (
                  sessionState === "Ready"
                  || sessionState === "Closing"
                )
              )
            )
          )
            ? undefined
            : "InvalidLifecycle";
        }
      case "RequestCancelled":
      case "RequestFailed": {
        if (request === undefined) {
          return "InvalidLifecycle";
        }
        const state = this.#requests.get(request);
        return (
          state?.state === "Active"
          && (session === undefined || state.session === session)
        )
          ? undefined
          : "InvalidLifecycle";
      }
      case "SessionClosed":
      case "CloseSessionAcknowledged":
        return (
          session !== undefined
          && this.#sessions.get(session) === "Closing"
        )
          ? undefined
          : "InvalidLifecycle";
      case "WorkerStopped":
      case "ShutdownAcknowledged":
        return this.#state === "Draining"
          ? undefined
          : "InvalidLifecycle";
      case "WorkerFault":
      case "ProtocolFault":
        return (
          this.#state !== "Stopped" && this.#state !== "Failed"
        )
          ? undefined
          : "InvalidLifecycle";
      case "SurfaceReclaimed":
      case "SurfaceReleaseAcknowledged": {
        if (session === undefined) {
          return "InvalidLifecycle";
        }
        const surface = this.#surfaces.get(event.payload.surface);
        return surface !== undefined
          && surface.session === session
          && surface.leaseToken === event.payload.lease_token
          && surface.state === "Alive"
          ? undefined
          : "InvalidLifecycle";
      }
      case "DataFailed":
        return (
          session !== undefined
          && (
            this.#sessions.get(session) === "Opening"
            || this.#sessions.get(session) === "Ready"
          )
        )
          ? undefined
          : "InvalidLifecycle";
      case "PageMetrics": {
        if (session === undefined || request === undefined) {
          return "InvalidLifecycle";
        }
        const state = this.#requests.get(request);
        return (
          this.#state === "Ready"
          && this.#sessions.get(session) === "Ready"
          && state?.kind === "Session"
          && state.state === "Active"
          && state.session === session
        )
          ? undefined
          : "InvalidLifecycle";
      }
      case "CancelAcknowledged": {
        if (request === undefined) {
          return "InvalidLifecycle";
        }
        const state = this.#requests.get(request);
        return (
          state !== undefined
          && (session === undefined || state.session === session)
        )
          ? undefined
          : "InvalidLifecycle";
      }
      case "EngineHello":
        return "InvalidLifecycle";
    }
  }

  #applyInboundEvent(envelope: EventEnvelope): void {
    const admission = this.#admission;
    const { correlation, event } = envelope;
    if (admission === undefined) {
      throw new BrowserWorkerSupervisorError("InvalidState");
    }
    switch (event.type) {
      case "Ready": {
        admission.setWorkerState("Ready");
        this.#state = "Ready";
        this.#lifecycleDeadline = undefined;
        break;
      }
      case "NeedData":
        this.#bindOpenSession(
          correlation.request,
          correlation.session,
        );
        break;
      case "DocumentReady": {
        const session = this.#bindOpenSession(
          correlation.request,
          correlation.session,
        );
        admission.setSessionState(session, "Ready");
        this.#sessions.set(session, "Ready");
        this.#terminateRequest(correlation.request);
        break;
      }
      case "SurfaceReady": {
        const session = correlation.session;
        if (session === undefined) {
          throw new BrowserWorkerSupervisorError("InvalidCommand");
        }
        admission.setSurfaceState(
          event.payload.metadata.id,
          session,
          "Alive",
        );
        this.#surfaces.set(event.payload.metadata.id, {
          session,
          generation: event.payload.metadata.generation,
          leaseToken: event.payload.metadata.lease_token,
          state: "Alive",
        });
        break;
      }
      case "GenerationCompleted": {
        const session = correlation.session;
        const generation = correlation.generation;
        if (session === undefined || generation === undefined) {
          throw new BrowserWorkerSupervisorError("InvalidCommand");
        }
        const state = this.#generations.get(session);
        if (
          state?.generation === generation
          && state.state === "Active"
        ) {
          state.state = "Terminal";
          state.terminalStatus = event.payload.status;
          break;
        }
        if (
          event.payload.status
            === GenerationCompletionStatus.Superseded
          && this.#consumeDisplacedGeneration(session, generation)
        ) {
          break;
        }
        throw new BrowserWorkerSupervisorError("InvalidCommand");
      }
      case "RequestCancelled":
      case "RequestFailed":
      case "PageMetrics":
        this.#terminateRequest(correlation.request);
        break;
      case "SessionClosed": {
        const session = correlation.session;
        if (session === undefined) {
          throw new BrowserWorkerSupervisorError("InvalidCommand");
        }
        admission.setSessionState(session, "Closed");
        this.#sessions.set(session, "Closed");
        this.#generations.delete(session);
        this.#displacedGenerationTerminals.delete(session);
        break;
      }
      case "WorkerStopped":
        admission.setWorkerState("Stopped");
        this.#state = "Stopped";
        this.#lifecycleDeadline = undefined;
        this.#terminatePort();
        this.#clearPendingWork();
        this.#generations.clear();
        this.#displacedGenerationTerminals.clear();
        break;
      case "SurfaceReclaimed":
      case "SurfaceReleaseAcknowledged":
        this.#reclaimSurface(
          event.payload.surface,
          event.payload.lease_token,
        );
        break;
      case "EngineHello":
      case "WorkerFault":
      case "ProtocolFault":
        throw new BrowserWorkerSupervisorError("InvalidCommand");
      case "CapabilityReported":
      case "DataFailed":
      case "GenerationPlanned":
      case "CancelAcknowledged":
      case "CloseSessionAcknowledged":
      case "ShutdownAcknowledged":
        break;
    }
  }

  #bindOpenSession(
    requestId: bigint | undefined,
    sessionId: bigint | undefined,
  ): bigint {
    if (requestId === undefined || sessionId === undefined) {
      throw new BrowserWorkerSupervisorError("InvalidCommand");
    }
    const request = this.#requests.get(requestId);
    const admission = this.#admission;
    if (
      request === undefined
      || request.kind !== "Open"
      || request.state !== "Active"
      || admission === undefined
    ) {
      throw new BrowserWorkerSupervisorError("InvalidCommand");
    }
    if (request.session === undefined) {
      admission.setSessionState(sessionId, "Opening");
      this.#sessions.set(sessionId, "Opening");
      request.session = sessionId;
    } else if (request.session !== sessionId) {
      throw new BrowserWorkerSupervisorError("InvalidCommand");
    }
    return sessionId;
  }

  #terminateRequest(requestId: bigint | undefined): void {
    if (requestId === undefined) {
      throw new BrowserWorkerSupervisorError("InvalidCommand");
    }
    const request = this.#requests.get(requestId);
    const admission = this.#admission;
    if (
      request === undefined
      || request.state !== "Active"
      || admission === undefined
    ) {
      throw new BrowserWorkerSupervisorError("InvalidCommand");
    }
    admission.setRequestState(
      requestId,
      "Terminal",
      request.admissionSession,
    );
    request.state = "Terminal";
  }

  #recordDisplacedGeneration(
    session: bigint,
    generation: bigint,
  ): void {
    let generations = this.#displacedGenerationTerminals.get(session);
    if (generations?.has(generation) === true) {
      return;
    }
    if (
      (generations?.size ?? 0) >= this.#limits.maxCriticalEvents
    ) {
      throw new BrowserWorkerSupervisorError("InvalidCommand");
    }
    if (generations === undefined) {
      generations = new Set<bigint>();
      this.#displacedGenerationTerminals.set(session, generations);
    }
    generations.add(generation);
  }

  #isDisplacedGeneration(
    session: bigint,
    generation: bigint,
  ): boolean {
    return this.#displacedGenerationTerminals.get(session)?.has(generation)
      === true;
  }

  #consumeDisplacedGeneration(
    session: bigint,
    generation: bigint,
  ): boolean {
    const generations = this.#displacedGenerationTerminals.get(session);
    if (generations?.delete(generation) !== true) {
      return false;
    }
    if (generations.size === 0) {
      this.#displacedGenerationTerminals.delete(session);
    }
    return true;
  }

  #reclaimSurface(surfaceId: bigint, leaseToken: bigint): void {
    const surface = this.#surfaces.get(surfaceId);
    const admission = this.#admission;
    if (
      surface === undefined
      || surface.leaseToken !== leaseToken
      || admission === undefined
    ) {
      throw new BrowserWorkerSupervisorError("InvalidCommand");
    }
    admission.setSurfaceState(
      surfaceId,
      surface.session,
      "Reclaimed",
    );
    surface.state = "Reclaimed";
  }

  #publishEvent(event: BrowserSupervisorEvent): void {
    if (
      event.type === "ProtocolEvent"
      && isProgressEvent(event.value.envelope.event)
    ) {
      const correlation = event.value.envelope.correlation;
      const key = [
        event.value.envelope.event.type,
        correlation.session?.toString() ?? "-",
        correlation.generation?.toString() ?? "-",
      ].join(":");
      if (
        !this.#progressEvents.has(key)
        && this.#progressEvents.size >= this.#limits.maxProgressEvents
      ) {
        const oldest = this.#oldestProgressEvent();
        if (oldest !== undefined) {
          this.#progressEvents.delete(oldest[0]);
        }
      }
      this.#progressEvents.set(key, event);
      return;
    }
    if (
      this.#criticalEvents.length >= this.#limits.maxCriticalEvents
    ) {
      this.#enterFailed("CriticalEventQueueOverflow");
      return;
    }
    this.#criticalEvents.push(event);
  }

  #oldestProgressEvent():
    | [string, BrowserSupervisorEvent]
    | undefined {
    let oldest: [string, BrowserSupervisorEvent] | undefined;
    for (const entry of this.#progressEvents) {
      const sequence = this.#eventSequence(entry[1]);
      if (sequence === undefined) {
        continue;
      }
      const oldestSequence = oldest === undefined
        ? undefined
        : this.#eventSequence(oldest[1]);
      if (oldestSequence === undefined || sequence < oldestSequence) {
        oldest = entry;
      }
    }
    return oldest;
  }

  #eventSequence(event: BrowserSupervisorEvent): bigint | undefined {
    if (event.type === "ProtocolEvent") {
      return event.value.envelope.header.sequence;
    }
    return event.origin === "EngineProtocol"
      ? event.value.envelope.header.sequence
      : undefined;
  }

  #takeNextEvent(): BrowserSupervisorEvent | undefined {
    const critical = this.#criticalEvents[0];
    const progress = this.#oldestProgressEvent();
    if (critical === undefined && progress === undefined) {
      return undefined;
    }
    if (critical === undefined && progress !== undefined) {
      this.#progressEvents.delete(progress[0]);
      return progress[1];
    }
    if (critical === undefined) {
      return undefined;
    }
    if (progress === undefined) {
      this.#criticalEvents.shift();
      return critical;
    }
    const criticalSequence = this.#eventSequence(critical);
    const progressSequence = this.#eventSequence(progress[1]);
    if (
      criticalSequence === undefined
      || progressSequence === undefined
      || criticalSequence <= progressSequence
    ) {
      this.#criticalEvents.shift();
      return critical;
    }
    this.#progressEvents.delete(progress[0]);
    return progress[1];
  }

  #enterFailed(
    code: BrowserWorkerFaultCode,
    protocolFault?: BrowserSupervisorEvent,
  ): void {
    this.#terminatePort();
    try {
      if (
        this.#admission !== undefined
        && this.#state !== "Failed"
        && this.#state !== "Stopped"
      ) {
        this.#admission.setWorkerState("Failed");
      }
    } catch {
      // The epoch is still irreversibly terminal at the host boundary.
    }
    this.#state = "Failed";
    this.#lifecycleDeadline = undefined;
    this.#pendingCallbackFault = undefined;
    this.#clearPendingWork();
    this.#generations.clear();
    this.#displacedGenerationTerminals.clear();
    this.#criticalEvents.length = 0;
    this.#progressEvents.clear();
    this.#criticalEvents.push(
      protocolFault
      ?? Object.freeze({
        type: "WorkerFault",
        worker: this.#worker,
        origin: "HostTransport",
        code,
      }),
    );
  }

  #terminatePort(): void {
    const port = this.#port;
    this.#port = undefined;
    this.#epochToken = {};
    if (port !== undefined) {
      try {
        port.terminate();
      } catch {
        // Termination is best-effort after ownership is already invalidated.
      }
    }
  }

  #clearPendingWork(): void {
    this.#inbound.length = 0;
    this.#criticalCommands.length = 0;
    this.#ordinaryCommands.length = 0;
    this.#viewportCommands.clear();
  }

  #readNow(): number {
    let now: number;
    try {
      now = this.#clock.now();
    } catch {
      throw new BrowserWorkerSupervisorError("InvalidConfiguration");
    }
    if (!Number.isFinite(now) || now < 0) {
      throw new BrowserWorkerSupervisorError("InvalidConfiguration");
    }
    return now;
  }

  #deadlineAfter(milliseconds: number): number {
    const deadline = this.#readNow() + milliseconds;
    if (!Number.isFinite(deadline)) {
      throw new BrowserWorkerSupervisorError("InvalidConfiguration");
    }
    return deadline;
  }
}

/**
 * Canonical bounded Host Hello helper for browser adapters.
 *
 * Callers supply only capability policy; protocol identity and hard bounds stay
 * generated. The returned value remains subject to supervisor validation.
 */
export function createBrowserHostHello(
  supportedCapabilities: bigint,
  mandatoryCapabilities = 0n,
): ProtocolHello {
  const hello: ProtocolHello = {
    major: PROTOCOL_MAJOR,
    minor: PROTOCOL_MINOR,
    schema_hash: canonicalSchemaHash(),
    endpoint_role: EndpointRole.Host,
    capabilities: {
      supported: supportedCapabilities,
      mandatory: mandatoryCapabilities,
    },
    max_message_bytes: MAX_MESSAGE_BYTES,
    max_transfer_slots: MAX_TRANSFER_SLOTS,
  };
  if (!validateProtocolHello(hello)) {
    throw new BrowserWorkerSupervisorError("InvalidConfiguration");
  }
  return snapshotProtocolHello(hello);
}
