import type {
  CommandEnvelope,
  MessageDescriptor,
} from "../generated/engine-protocol.js";
import {
  descriptorById,
  validateRequestId,
  validateSessionId,
  validateSurfaceId,
} from "../generated/engine-protocol.js";

/** Worker lifecycle states owned by the browser Worker supervisor. */
export type BrowserWorkerLifecycleState =
  | "NotStarted"
  | "Starting"
  | "Ready"
  | "Draining"
  | "Stopped"
  | "Failed";

/** Session lifecycle states relevant to command admission. */
export type BrowserSessionLifecycleState =
  | "Opening"
  | "Ready"
  | "Closing"
  | "Closed";

/** Request lifecycle states relevant to idempotent cancellation. */
export type BrowserRequestLifecycleState = "Active" | "Terminal";

/** Surface lifecycle states relevant to idempotent release. */
export type BrowserSurfaceLifecycleState = "Alive" | "Reclaimed";

/** Stable, content-free command-admission rejection categories. */
export type BrowserCommandAdmissionErrorCode =
  | "InvalidLifecycle"
  | "AdmissionCapacityExceeded"
  | "StaleSession"
  | "StaleRequest"
  | "StaleGeneration"
  | "StaleSurface";

/** Explicit state-table capacities supplied by the Worker supervisor. */
export interface BrowserCommandAdmissionLimits {
  readonly maxSessions: number;
  readonly maxRequests: number;
  readonly maxSurfaces: number;
}

/** Hard upper bound for session identities retained in one Worker epoch. */
export const MAX_BROWSER_ADMISSION_SESSIONS = 64 as const;

/** Hard upper bound for request identities retained in one Worker epoch. */
export const MAX_BROWSER_ADMISSION_REQUESTS = 4_096 as const;

/** Hard upper bound for Surface identities retained in one Worker epoch. */
export const MAX_BROWSER_ADMISSION_SURFACES = 4_096 as const;

/** Stable, content-free admission state-management rejection categories. */
export type BrowserCommandAdmissionStateErrorCode =
  | "InvalidAdmissionConfiguration"
  | "AdmissionCapacityExceeded"
  | "InvalidAdmissionTransition";

/** Reports invalid limits, state updates, or bounded-table exhaustion. */
export class BrowserCommandAdmissionStateError extends Error {
  readonly code: BrowserCommandAdmissionStateErrorCode;

  constructor(code: BrowserCommandAdmissionStateErrorCode) {
    super(code);
    this.name = "BrowserCommandAdmissionStateError";
    this.code = code;
  }
}

interface RequestAdmission {
  readonly state: BrowserRequestLifecycleState;
  readonly session: bigint | undefined;
}

interface SurfaceAdmission {
  readonly state: BrowserSurfaceLifecycleState;
  readonly session: bigint;
}

const WORKER_STATES: readonly BrowserWorkerLifecycleState[] = [
  "NotStarted",
  "Starting",
  "Ready",
  "Draining",
  "Stopped",
  "Failed",
];
const SESSION_STATES: readonly BrowserSessionLifecycleState[] = [
  "Opening",
  "Ready",
  "Closing",
  "Closed",
];
const REQUEST_STATES: readonly BrowserRequestLifecycleState[] = [
  "Active",
  "Terminal",
];
const SURFACE_STATES: readonly BrowserSurfaceLifecycleState[] = [
  "Alive",
  "Reclaimed",
];
const MAX_U64 = 0xffff_ffff_ffff_ffffn;
const INTRINSIC_REFLECT_APPLY = Reflect.apply;
const INTRINSIC_WEAK_SET_ADD = WeakSet.prototype.add;
const INTRINSIC_WEAK_SET_HAS = WeakSet.prototype.has;
const COMMAND_ADMISSIONS = new WeakSet<object>();

const addCommandAdmission = (value: object): void => {
  INTRINSIC_REFLECT_APPLY(
    INTRINSIC_WEAK_SET_ADD,
    COMMAND_ADMISSIONS,
    [value],
  );
};

const hasCommandAdmission = (value: object): boolean =>
  INTRINSIC_REFLECT_APPLY(
    INTRINSIC_WEAK_SET_HAS,
    COMMAND_ADMISSIONS,
    [value],
  ) as boolean;

const invalidAdmissionConfiguration = (): never => {
  throw new BrowserCommandAdmissionStateError(
    "InvalidAdmissionConfiguration",
  );
};

const admissionCapacityExceeded = (): never => {
  throw new BrowserCommandAdmissionStateError(
    "AdmissionCapacityExceeded",
  );
};

const invalidAdmissionTransition = (): never => {
  throw new BrowserCommandAdmissionStateError(
    "InvalidAdmissionTransition",
  );
};

const isGeneration = (value: bigint): boolean =>
  value > 0n && value <= MAX_U64;

const isCapacity = (value: number, hardMaximum: number): boolean =>
  Number.isSafeInteger(value) && value >= 0 && value <= hardMaximum;

const snapshotCapacity = (
  value: unknown,
  hardMaximum: number,
): number => {
  if (typeof value !== "number") {
    return invalidAdmissionConfiguration();
  }
  if (!isCapacity(value, hardMaximum)) {
    return invalidAdmissionConfiguration();
  }
  return value;
};

const snapshotLimits = (
  limits: BrowserCommandAdmissionLimits,
): Readonly<BrowserCommandAdmissionLimits> => {
  let maxSessions: unknown;
  let maxRequests: unknown;
  let maxSurfaces: unknown;
  try {
    if (
      typeof limits !== "object"
      || limits === null
      || Object.getPrototypeOf(limits) !== Object.prototype
      || Reflect.ownKeys(limits).length !== 3
    ) {
      return invalidAdmissionConfiguration();
    }
    const sessionsDescriptor = Object.getOwnPropertyDescriptor(
      limits,
      "maxSessions",
    );
    const requestsDescriptor = Object.getOwnPropertyDescriptor(
      limits,
      "maxRequests",
    );
    const surfacesDescriptor = Object.getOwnPropertyDescriptor(
      limits,
      "maxSurfaces",
    );
    if (
      sessionsDescriptor === undefined
      || !Object.prototype.hasOwnProperty.call(sessionsDescriptor, "value")
      || requestsDescriptor === undefined
      || !Object.prototype.hasOwnProperty.call(requestsDescriptor, "value")
      || surfacesDescriptor === undefined
      || !Object.prototype.hasOwnProperty.call(surfacesDescriptor, "value")
    ) {
      return invalidAdmissionConfiguration();
    }
    maxSessions = sessionsDescriptor.value;
    maxRequests = requestsDescriptor.value;
    maxSurfaces = surfacesDescriptor.value;
  } catch {
    return invalidAdmissionConfiguration();
  }
  return Object.freeze({
    maxSessions: snapshotCapacity(
      maxSessions,
      MAX_BROWSER_ADMISSION_SESSIONS,
    ),
    maxRequests: snapshotCapacity(
      maxRequests,
      MAX_BROWSER_ADMISSION_REQUESTS,
    ),
    maxSurfaces: snapshotCapacity(
      maxSurfaces,
      MAX_BROWSER_ADMISSION_SURFACES,
    ),
  });
};

const isWorkerTransition = (
  current: BrowserWorkerLifecycleState,
  next: BrowserWorkerLifecycleState,
): boolean => {
  if (current === next) {
    return true;
  }
  if (next === "Failed") {
    return current !== "Stopped" && current !== "Failed";
  }
  switch (current) {
    case "NotStarted":
      return next === "Starting";
    case "Starting":
      return next === "Ready";
    case "Ready":
      return next === "Draining";
    case "Draining":
      return next === "Stopped";
    case "Stopped":
    case "Failed":
      return false;
  }
};

const isSessionTransition = (
  current: BrowserSessionLifecycleState,
  next: BrowserSessionLifecycleState,
): boolean => {
  if (current === next) {
    return true;
  }
  switch (current) {
    case "Opening":
      return next === "Ready" || next === "Closing" || next === "Closed";
    case "Ready":
      return next === "Closing" || next === "Closed";
    case "Closing":
      return next === "Closed";
    case "Closed":
      return false;
  }
};

const isRequestTransition = (
  current: BrowserRequestLifecycleState,
  next: BrowserRequestLifecycleState,
): boolean =>
  current === next || (current === "Active" && next === "Terminal");

const isSurfaceTransition = (
  current: BrowserSurfaceLifecycleState,
  next: BrowserSurfaceLifecycleState,
): boolean =>
  current === next || (current === "Alive" && next === "Reclaimed");

/**
 * Supervisor-owned state consulted by the binary command ingress boundary.
 *
 * State changes are explicit so the future Worker supervisor can update this
 * object only after its own lifecycle transitions become authoritative. New
 * Open and GetPageMetrics request IDs, and a newly accepted SetViewport
 * generation, must be recorded by the caller immediately after `decode`
 * succeeds. Failed decoding never mutates this state.
 */
export class BrowserCommandAdmission {
  #workerState: BrowserWorkerLifecycleState;
  readonly #limits: Readonly<BrowserCommandAdmissionLimits>;
  readonly #sessions = new Map<bigint, BrowserSessionLifecycleState>();
  readonly #requests = new Map<bigint, RequestAdmission>();
  readonly #generationHighWatermarks = new Map<bigint, bigint>();
  readonly #surfaces = new Map<bigint, SurfaceAdmission>();

  constructor(
    workerState: BrowserWorkerLifecycleState,
    limits: BrowserCommandAdmissionLimits,
  ) {
    if (
      new.target !== BrowserCommandAdmission
      || !WORKER_STATES.includes(workerState)
    ) {
      invalidAdmissionConfiguration();
    }
    this.#limits = snapshotLimits(limits);
    this.#workerState = workerState;
    addCommandAdmission(this);
    Object.freeze(this);
  }

  /** Advances the authoritative Worker lifecycle without reviving an epoch. */
  setWorkerState(workerState: BrowserWorkerLifecycleState): void {
    if (!WORKER_STATES.includes(workerState)) {
      invalidAdmissionConfiguration();
    }
    if (!isWorkerTransition(this.#workerState, workerState)) {
      invalidAdmissionTransition();
    }
    this.#workerState = workerState;
  }

  /** Records or irreversibly advances one known session. */
  setSessionState(
    session: bigint,
    state: BrowserSessionLifecycleState,
  ): void {
    if (
      !validateSessionId(session)
      || session === 0n
      || !SESSION_STATES.includes(state)
    ) {
      invalidAdmissionConfiguration();
    }
    const current = this.#sessions.get(session);
    if (current !== undefined) {
      if (!isSessionTransition(current, state)) {
        invalidAdmissionTransition();
      }
      this.#sessions.set(session, state);
      return;
    }
    if (
      this.#sessions.size >= this.#limits.maxSessions
    ) {
      admissionCapacityExceeded();
    }
    this.#sessions.set(session, state);
  }

  /** Records or terminates a request without changing its original owner. */
  setRequestState(
    request: bigint,
    state: BrowserRequestLifecycleState,
    session?: bigint,
  ): void {
    if (
      !validateRequestId(request)
      || request === 0n
      || !REQUEST_STATES.includes(state)
      || (
        session !== undefined
        && (
          !validateSessionId(session)
          || session === 0n
          || !this.#sessions.has(session)
        )
      )
    ) {
      invalidAdmissionConfiguration();
    }
    const current = this.#requests.get(request);
    if (current !== undefined) {
      if (
        current.session !== session
        || !isRequestTransition(current.state, state)
      ) {
        invalidAdmissionTransition();
      }
      this.#requests.set(request, Object.freeze({ state, session }));
      return;
    }
    if (
      this.#requests.size >= this.#limits.maxRequests
    ) {
      admissionCapacityExceeded();
    }
    this.#requests.set(request, Object.freeze({ state, session }));
  }

  /** Advances one session's generation high-watermark; equality is idempotent. */
  setActiveGeneration(session: bigint, generation: bigint): void {
    if (
      !validateSessionId(session)
      || session === 0n
      || !isGeneration(generation)
      || !this.#sessions.has(session)
    ) {
      invalidAdmissionConfiguration();
    }
    const current = this.#generationHighWatermarks.get(session);
    if (current !== undefined) {
      if (generation < current) {
        invalidAdmissionTransition();
      }
      if (generation === current) {
        return;
      }
    }
    this.#generationHighWatermarks.set(session, generation);
  }

  /** Records or reclaims a Surface without changing its original owner. */
  setSurfaceState(
    surface: bigint,
    session: bigint,
    state: BrowserSurfaceLifecycleState,
  ): void {
    if (
      !validateSurfaceId(surface)
      || surface === 0n
      || !validateSessionId(session)
      || session === 0n
      || !SURFACE_STATES.includes(state)
      || !this.#sessions.has(session)
    ) {
      invalidAdmissionConfiguration();
    }
    const current = this.#surfaces.get(surface);
    if (current !== undefined) {
      if (
        current.session !== session
        || !isSurfaceTransition(current.state, state)
      ) {
        invalidAdmissionTransition();
      }
      this.#surfaces.set(surface, Object.freeze({ state, session }));
      return;
    }
    if (
      this.#surfaces.size >= this.#limits.maxSurfaces
    ) {
      admissionCapacityExceeded();
    }
    this.#surfaces.set(surface, Object.freeze({ state, session }));
  }

  /**
   * Checks generated descriptor lifecycle and active correlation state.
   *
   * The method is read-only: the boundary can call it before validating OOB
   * resources and committing the receive-direction sequence.
   */
  validate(
    envelope: CommandEnvelope,
  ): BrowserCommandAdmissionErrorCode | undefined {
    const descriptor = descriptorById(envelope.header.message_type);
    if (
      descriptor === undefined
      || descriptor.kind !== "command"
      || descriptor.name !== envelope.command.type
    ) {
      return "InvalidLifecycle";
    }

    const lifecycleError = this.#validateLifecycle(envelope, descriptor);
    if (lifecycleError !== undefined) {
      return lifecycleError;
    }
    return this.#validateCorrelation(envelope, descriptor);
  }

  #validateLifecycle(
    envelope: CommandEnvelope,
    descriptor: MessageDescriptor,
  ): BrowserCommandAdmissionErrorCode | undefined {
    const session = envelope.correlation.session;
    switch (descriptor.state_precondition) {
      case "Starting":
        return this.#workerState === "Starting"
          ? undefined
          : "InvalidLifecycle";
      case "Ready":
        if (this.#workerState !== "Ready") {
          return "InvalidLifecycle";
        }
        return session === undefined
          || this.#sessions.get(session) === "Ready"
          ? undefined
          : "StaleSession";
      case "OpeningOrReady": {
        if (this.#workerState !== "Ready") {
          return "InvalidLifecycle";
        }
        const state =
          session === undefined ? undefined : this.#sessions.get(session);
        return state === "Opening" || state === "Ready"
          ? undefined
          : "StaleSession";
      }
      case "ActiveOrTerminalRequest":
        return this.#validateRequestReference(envelope);
      case "SurfaceAliveOrReclaimed":
        return this.#validateSurfaceReference(envelope);
      case "NonClosedOrClosed":
        return session !== undefined && this.#sessions.has(session)
          ? undefined
          : "StaleSession";
      case "ReadyOrDrainingOrStopped":
        return (
          this.#workerState === "Ready"
          || this.#workerState === "Draining"
          || this.#workerState === "Stopped"
        )
          ? undefined
          : "InvalidLifecycle";
      default:
        return "InvalidLifecycle";
    }
  }

  #validateCorrelation(
    envelope: CommandEnvelope,
    descriptor: MessageDescriptor,
  ): BrowserCommandAdmissionErrorCode | undefined {
    const { session, request, generation } = envelope.correlation;
    switch (descriptor.correlation) {
      case "Worker":
      case "Session":
        return undefined;
      case "OpenRequest":
      case "SessionRequest":
        if (request === undefined || this.#requests.has(request)) {
          return "StaleRequest";
        }
        return this.#requests.size >= this.#limits.maxRequests
          ? "AdmissionCapacityExceeded"
          : undefined;
      case "Request":
        return this.#validateRequestReference(envelope);
      case "Generation": {
        if (session === undefined || generation === undefined) {
          return "StaleGeneration";
        }
        const active = this.#generationHighWatermarks.get(session);
        return active === undefined || generation > active
          ? undefined
          : "StaleGeneration";
      }
      default:
        return "InvalidLifecycle";
    }
  }

  #validateRequestReference(
    envelope: CommandEnvelope,
  ): BrowserCommandAdmissionErrorCode | undefined {
    const { request, session } = envelope.correlation;
    if (request === undefined) {
      return "StaleRequest";
    }
    const admission = this.#requests.get(request);
    if (admission === undefined) {
      return "StaleRequest";
    }
    return session === undefined
      || admission.session === session
      ? undefined
      : "StaleRequest";
  }

  #validateSurfaceReference(
    envelope: CommandEnvelope,
  ): BrowserCommandAdmissionErrorCode | undefined {
    if (envelope.command.type !== "ReleaseSurface") {
      return "InvalidLifecycle";
    }
    const session = envelope.correlation.session;
    const admission = this.#surfaces.get(envelope.command.payload.surface);
    return (
      session !== undefined
      && admission !== undefined
      && admission.session === session
    )
      ? undefined
      : "StaleSurface";
  }
}

/** Returns true only for an admission object created by this module. */
export function isBrowserCommandAdmission(
  value: unknown,
): value is BrowserCommandAdmission {
  return (
    typeof value === "object"
    && value !== null
    && hasCommandAdmission(value)
  );
}

const ORIGINAL_VALIDATE_BROWSER_COMMAND_ADMISSION =
  BrowserCommandAdmission.prototype.validate;
Object.freeze(BrowserCommandAdmission.prototype);

/**
 * Validates with the module-captured implementation, never caller dispatch.
 *
 * The authenticity check keeps direct callers from invoking private state on a
 * prototype forgery. BrowserCommandBoundary performs the same check at
 * construction and then uses this entry point for every command.
 */
export function validateBrowserCommandAdmission(
  admission: BrowserCommandAdmission,
  envelope: CommandEnvelope,
): BrowserCommandAdmissionErrorCode | undefined {
  if (!isBrowserCommandAdmission(admission)) {
    return "InvalidLifecycle";
  }
  return ORIGINAL_VALIDATE_BROWSER_COMMAND_ADMISSION.call(
    admission,
    envelope,
  );
}
