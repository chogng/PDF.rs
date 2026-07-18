import type {
  CapabilityDecision,
  EngineError,
  PageGeometry,
  PageViewport,
  SurfaceMetadata,
  ViewportRequest,
} from "../generated/engine-protocol.js";
import {
  EngineErrorCode,
  OutputProfile,
  PageRotation,
  QualityPolicy,
  SupportStatus,
  validateCapabilityDecision,
  validateEngineError,
  validatePageViewport,
  validateSurfaceMetadata,
  validateViewportRequest,
} from "../generated/engine-protocol.js";
import type {
  BrowserWorkerFaultCode,
} from "./browser-worker-supervisor.js";

const MAX_U64 = 0xffff_ffff_ffff_ffffn;
const MAX_VISIBLE_PAGES = 64;
const MAX_VIEWER_CAPACITY = 4_096;

/** Stable construction and lifecycle failures. */
export type BrowserViewerErrorCode =
  | "InvalidConfiguration"
  | "InvalidLifecycle"
  | "HostBindingFailure";

/** A content-free viewer API error. */
export class BrowserViewerError extends Error {
  readonly code: BrowserViewerErrorCode;

  constructor(code: BrowserViewerErrorCode) {
    super(code);
    this.name = "BrowserViewerError";
    this.code = code;
  }
}

/** Bounded host policy for coalescing, virtualization, and adopted surfaces. */
export interface BrowserViewerLimits {
  readonly maxVisiblePages: number;
  readonly maxCoalescedChanges: number;
  readonly maxAdoptedSurfaces: number;
}

/** Complete generation-independent interaction state. */
export interface BrowserViewerInitialState {
  readonly documentRevision: bigint;
  readonly annotationRevision: bigint;
  readonly zoomNumerator: number;
  readonly zoomDenominator: number;
  readonly visiblePages: readonly PageViewport[];
  readonly quality: QualityPolicy;
  readonly outputProfile: OutputProfile;
  readonly deviceScaleMilli: number;
  readonly rotation: PageRotation;
  readonly optionalContentId: bigint;
}

/** An opaque already-adopted presentation resource and its validated metadata. */
export interface BrowserViewerSurface {
  readonly metadata: SurfaceMetadata;
  readonly resource: object;
}

/** Engine callbacks installed only while the viewer is mounted. */
export interface BrowserViewerEngineHandlers {
  readonly onSurface: (surface: BrowserViewerSurface) => void;
  readonly onCapabilityDecision: (decision: CapabilityDecision) => void;
  readonly onEngineError: (error: EngineError) => void;
  readonly onWorkerFault: (code: BrowserWorkerFaultCode) => void;
}

/**
 * Session-scoped engine adapter injected into the thin viewer.
 *
 * The adapter owns protocol encoding, Worker lifecycle, and adopted-resource
 * destruction. The viewer owns only viewport intent and presentation policy.
 */
export interface BrowserViewerEngineClient {
  setHandlers(handlers: BrowserViewerEngineHandlers | undefined): void;
  setViewport(viewport: ViewportRequest): void;
  releaseSurface(surface: BrowserViewerSurface): void;
  close(): void;
}

/** Host observation callbacks for scroll, intersection, resize, and DPR. */
export interface BrowserViewerObservationHandlers {
  readonly onScroll: (visiblePages: readonly PageViewport[]) => void;
  readonly onIntersection: (visiblePages: readonly PageViewport[]) => void;
  readonly onResize: (visiblePages: readonly PageViewport[]) => void;
  readonly onDeviceScale: (deviceScaleMilli: number) => void;
}

/**
 * Owns browser observers and event listeners on behalf of the viewer.
 *
 * `disconnect` must synchronously remove every callback installed by
 * `connect`.
 */
export interface BrowserViewerHostObservations {
  connect(handlers: BrowserViewerObservationHandlers): void;
  disconnect(): void;
}

/** Injected frame scheduler used to coalesce replaceable host observations. */
export interface BrowserViewerFrameScheduler {
  request(callback: () => void): number;
  cancel(handle: number): void;
}

/** Opaque focus snapshot captured before presentation teardown. */
export interface BrowserViewerFocusSnapshot {
  readonly token: object;
}

/** Injected focus adapter used to preserve focus across unmount. */
export interface BrowserViewerFocus {
  captureBeforeUnmount(): BrowserViewerFocusSnapshot | undefined;
  restoreAfterUnmount(snapshot: BrowserViewerFocusSnapshot): void;
}

/** Presentation-only page DOM and Canvas adapter. */
export interface BrowserViewerPresentation {
  present(surface: BrowserViewerSurface): void;
  clear(): void;
  showFailure(failure: BrowserViewerFailure): void;
  clearFailure(): void;
}

/** Immutable dependencies and initial viewport state. */
export interface BrowserViewerConfiguration {
  readonly client: BrowserViewerEngineClient;
  readonly observations: BrowserViewerHostObservations;
  readonly frames: BrowserViewerFrameScheduler;
  readonly focus: BrowserViewerFocus;
  readonly presentation: BrowserViewerPresentation;
  readonly limits: BrowserViewerLimits;
  readonly initialState: BrowserViewerInitialState;
}

/** Stable user-visible failure categories. */
export type BrowserViewerFailureKind =
  | "Unsupported"
  | "Rejected"
  | "Budget"
  | "SourceIntegrity"
  | "Cancelled"
  | "Transport"
  | "Worker"
  | "Document"
  | "InvalidInput";

/** Stable viewer-owned failure codes. */
export type BrowserViewerLocalFailureCode =
  | "InvalidViewport"
  | "CoalescingLimit"
  | "GenerationExhausted"
  | "SchedulerFault"
  | "ClientFault"
  | "InvalidCapabilityDecision"
  | "InvalidEngineError"
  | "InvalidWorkerFault"
  | "InvalidSurface"
  | "SurfaceLimit"
  | "PresentationFault";

/** Structured failure state; diagnostic strings are deliberately absent. */
export type BrowserViewerFailure =
  | Readonly<{
    readonly source: "CapabilityDecision";
    readonly kind: "Unsupported" | "Rejected";
    readonly code: SupportStatus.Unsupported | SupportStatus.Rejected;
    readonly decision: CapabilityDecision;
  }>
  | Readonly<{
    readonly source: "EngineError";
    readonly kind: BrowserViewerFailureKind;
    readonly code: EngineErrorCode;
    readonly diagnosticId: bigint;
  }>
  | Readonly<{
    readonly source: "WorkerFault";
    readonly kind: "Transport" | "Worker";
    readonly code: BrowserWorkerFaultCode;
  }>
  | Readonly<{
    readonly source: "Viewer";
    readonly kind: "Budget" | "Transport" | "InvalidInput";
    readonly code: BrowserViewerLocalFailureCode;
  }>;

/** Result of one interaction or host-observation update. */
export type BrowserViewerUpdateResult =
  | "Scheduled"
  | "Coalesced"
  | "Rejected";

/** Observable viewer lifecycle. */
export type BrowserViewerLifecycle = "Created" | "Mounted" | "Unmounted";

interface ViewerState {
  readonly documentRevision: bigint;
  readonly annotationRevision: bigint;
  readonly zoomNumerator: number;
  readonly zoomDenominator: number;
  readonly visiblePages: PageViewport[];
  readonly quality: QualityPolicy;
  readonly outputProfile: OutputProfile;
  readonly deviceScaleMilli: number;
  readonly rotation: PageRotation;
  readonly optionalContentId: bigint;
}

interface SnapshotLimits {
  readonly maxVisiblePages: number;
  readonly maxCoalescedChanges: number;
  readonly maxAdoptedSurfaces: number;
}

const isCapacity = (value: unknown, maximum: number): value is number =>
  typeof value === "number"
  && Number.isSafeInteger(value)
  && value > 0
  && value <= maximum;

const snapshotLimits = (
  limits: BrowserViewerLimits,
): SnapshotLimits | undefined => {
  try {
    if (
      !isCapacity(limits.maxVisiblePages, MAX_VISIBLE_PAGES)
      || !isCapacity(
        limits.maxCoalescedChanges,
        MAX_VIEWER_CAPACITY,
      )
      || !isCapacity(
        limits.maxAdoptedSurfaces,
        MAX_VIEWER_CAPACITY,
      )
    ) {
      return undefined;
    }
    return Object.freeze({
      maxVisiblePages: limits.maxVisiblePages,
      maxCoalescedChanges: limits.maxCoalescedChanges,
      maxAdoptedSurfaces: limits.maxAdoptedSurfaces,
    });
  } catch {
    return undefined;
  }
};

const hasMethods = (
  value: unknown,
  names: readonly string[],
): value is Record<string, (...values: never[]) => unknown> => {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  try {
    return names.every(
      (name) =>
        typeof Reflect.get(value, name) === "function",
    );
  } catch {
    return false;
  }
};

const snapshotGeometry = (
  geometry: PageGeometry,
): PageGeometry | undefined => {
  try {
    const snapshot = Object.freeze({
      identity: geometry.identity.slice(),
      media_box_x_milli_points: geometry.media_box_x_milli_points,
      media_box_y_milli_points: geometry.media_box_y_milli_points,
      media_box_width_milli_points: geometry.media_box_width_milli_points,
      media_box_height_milli_points: geometry.media_box_height_milli_points,
      crop_box_x_milli_points: geometry.crop_box_x_milli_points,
      crop_box_y_milli_points: geometry.crop_box_y_milli_points,
      crop_box_width_milli_points: geometry.crop_box_width_milli_points,
      crop_box_height_milli_points: geometry.crop_box_height_milli_points,
      intrinsic_rotation: geometry.intrinsic_rotation,
    }) as PageGeometry;
    return snapshot;
  } catch {
    return undefined;
  }
};

const snapshotVisiblePages = (
  pages: readonly PageViewport[],
  maximum: number,
): PageViewport[] | undefined => {
  if (!Array.isArray(pages) || pages.length > maximum) {
    return undefined;
  }
  const snapshots: PageViewport[] = [];
  try {
    for (const page of pages) {
      const geometry = snapshotGeometry(page.geometry);
      if (geometry === undefined) {
        return undefined;
      }
      const snapshot = Object.freeze({
        page_index: page.page_index,
        coordinate_space: page.coordinate_space,
        geometry,
        clip_x_milli_points: page.clip_x_milli_points,
        clip_y_milli_points: page.clip_y_milli_points,
        clip_width_milli_points: page.clip_width_milli_points,
        clip_height_milli_points: page.clip_height_milli_points,
      }) as PageViewport;
      if (!validatePageViewport(snapshot)) {
        return undefined;
      }
      snapshots.push(snapshot);
    }
  } catch {
    return undefined;
  }
  return Object.freeze(snapshots) as unknown as PageViewport[];
};

const requestForState = (
  state: ViewerState,
  generation: bigint,
): ViewportRequest | undefined => {
  const request = Object.freeze({
    generation,
    document_revision: state.documentRevision,
    annotation_revision: state.annotationRevision,
    zoom_numerator: state.zoomNumerator,
    zoom_denominator: state.zoomDenominator,
    visible_pages: state.visiblePages,
    quality: state.quality,
    output_profile: state.outputProfile,
    device_scale_milli: state.deviceScaleMilli,
    rotation: state.rotation,
    optional_content_id: state.optionalContentId,
  }) as ViewportRequest;
  return validateViewportRequest(request) ? request : undefined;
};

const snapshotInitialState = (
  state: BrowserViewerInitialState,
  limits: SnapshotLimits,
): ViewerState | undefined => {
  try {
    const visiblePages = snapshotVisiblePages(
      state.visiblePages,
      limits.maxVisiblePages,
    );
    if (visiblePages === undefined) {
      return undefined;
    }
    const snapshot: ViewerState = Object.freeze({
      documentRevision: state.documentRevision,
      annotationRevision: state.annotationRevision,
      zoomNumerator: state.zoomNumerator,
      zoomDenominator: state.zoomDenominator,
      visiblePages,
      quality: state.quality,
      outputProfile: state.outputProfile,
      deviceScaleMilli: state.deviceScaleMilli,
      rotation: state.rotation,
      optionalContentId: state.optionalContentId,
    });
    return requestForState(snapshot, 1n) === undefined
      ? undefined
      : snapshot;
  } catch {
    return undefined;
  }
};

const isSurface = (value: BrowserViewerSurface): boolean => {
  try {
    const { metadata, resource } = value;
    return (
      typeof value === "object"
      && value !== null
      && typeof resource === "object"
      && resource !== null
      && validateSurfaceMetadata(metadata)
      && metadata.id !== 0n
      && metadata.lease_token !== 0n
      && metadata.generation !== 0n
      && metadata.width !== 0
      && metadata.height !== 0
      && metadata.region.width !== 0
      && metadata.region.height !== 0
    );
  } catch {
    return false;
  }
};

const regionKey = (metadata: SurfaceMetadata): string =>
  [
    metadata.region.page_index,
    metadata.region.x,
    metadata.region.y,
    metadata.region.width,
    metadata.region.height,
    metadata.region.coordinate_space,
  ].join(":");

const kindForEngineError = (
  code: EngineErrorCode,
): BrowserViewerFailureKind => {
  switch (code) {
    case EngineErrorCode.UnsupportedFeature:
      return "Unsupported";
    case EngineErrorCode.ResourceLimit:
      return "Budget";
    case EngineErrorCode.SourceChanged:
    case EngineErrorCode.SourceUnavailable:
      return "SourceIntegrity";
    case EngineErrorCode.Cancelled:
    case EngineErrorCode.StaleGeneration:
      return "Cancelled";
    case EngineErrorCode.SurfaceImportFailed:
    case EngineErrorCode.ProtocolViolation:
      return "Transport";
    case EngineErrorCode.Internal:
      return "Worker";
    case EngineErrorCode.InvalidDocument:
    case EngineErrorCode.InvalidPassword:
      return "Document";
  }
};

const kindForWorkerFault = (
  code: BrowserWorkerFaultCode,
): "Transport" | "Worker" => {
  switch (code) {
    case "FactoryFailure":
    case "HandlerRegistrationFailure":
    case "WorkerError":
    case "WorkerMessageError":
    case "WorkerTerminated":
    case "StartupTimeout":
    case "ShutdownTimeout":
      return "Worker";
    case "InboundQueueOverflow":
    case "ProtocolViolation":
    case "OutboundTransportFailure":
    case "CriticalEventQueueOverflow":
      return "Transport";
  }
};

const isWorkerFaultCode = (
  value: unknown,
): value is BrowserWorkerFaultCode => {
  switch (value) {
    case "FactoryFailure":
    case "HandlerRegistrationFailure":
    case "WorkerError":
    case "WorkerMessageError":
    case "WorkerTerminated":
    case "StartupTimeout":
    case "ShutdownTimeout":
    case "InboundQueueOverflow":
    case "ProtocolViolation":
    case "OutboundTransportFailure":
    case "CriticalEventQueueOverflow":
      return true;
    default:
      return false;
  }
};

const kindForLocalFailure = (
  code: BrowserViewerLocalFailureCode,
): "Budget" | "Transport" | "InvalidInput" => {
  switch (code) {
    case "CoalescingLimit":
    case "GenerationExhausted":
    case "SurfaceLimit":
      return "Budget";
    case "SchedulerFault":
    case "ClientFault":
    case "PresentationFault":
      return "Transport";
    case "InvalidViewport":
    case "InvalidCapabilityDecision":
    case "InvalidEngineError":
    case "InvalidWorkerFault":
    case "InvalidSurface":
      return "InvalidInput";
  }
};

/**
 * Thin, single-session browser viewer.
 *
 * This class owns only complete viewport intent, bounded coalescing,
 * presentation-resource replacement, structured failure display, and host
 * cleanup. It contains no PDF, graphics, font, color, or raster semantics.
 */
export class BrowserViewer {
  readonly #client: BrowserViewerEngineClient;
  readonly #observations: BrowserViewerHostObservations;
  readonly #frames: BrowserViewerFrameScheduler;
  readonly #focus: BrowserViewerFocus;
  readonly #presentation: BrowserViewerPresentation;
  readonly #limits: SnapshotLimits;
  #state: ViewerState;
  #lifecycle: BrowserViewerLifecycle = "Created";
  #currentGeneration = 0n;
  #currentViewport: ViewportRequest | undefined;
  #failure: BrowserViewerFailure | undefined;
  #scheduledTicket: object | undefined;
  #frameHandle: number | undefined;
  #pendingChanges = 0;
  readonly #surfacesById = new Map<bigint, BrowserViewerSurface>();
  readonly #surfacesByRegion = new Map<string, BrowserViewerSurface>();

  constructor(configuration: BrowserViewerConfiguration) {
    const limits = snapshotLimits(configuration.limits);
    if (
      limits === undefined
      || !hasMethods(
        configuration.client,
        ["setHandlers", "setViewport", "releaseSurface", "close"],
      )
      || !hasMethods(
        configuration.observations,
        ["connect", "disconnect"],
      )
      || !hasMethods(configuration.frames, ["request", "cancel"])
      || !hasMethods(
        configuration.focus,
        ["captureBeforeUnmount", "restoreAfterUnmount"],
      )
      || !hasMethods(
        configuration.presentation,
        ["present", "clear", "showFailure", "clearFailure"],
      )
    ) {
      throw new BrowserViewerError("InvalidConfiguration");
    }
    const state = snapshotInitialState(
      configuration.initialState,
      limits,
    );
    if (state === undefined) {
      throw new BrowserViewerError("InvalidConfiguration");
    }
    this.#client = configuration.client;
    this.#observations = configuration.observations;
    this.#frames = configuration.frames;
    this.#focus = configuration.focus;
    this.#presentation = configuration.presentation;
    this.#limits = limits;
    this.#state = state;
  }

  /** Current mount lifecycle. */
  get lifecycle(): BrowserViewerLifecycle {
    return this.#lifecycle;
  }

  /** Latest generation accepted by the injected EngineClient. */
  get currentGeneration(): bigint {
    return this.#currentGeneration;
  }

  /** Latest complete viewport accepted by the injected EngineClient. */
  get currentViewport(): ViewportRequest | undefined {
    return this.#currentViewport;
  }

  /** Current structured failure state. */
  get failure(): BrowserViewerFailure | undefined {
    return this.#failure;
  }

  /** Number of complete current-generation presentation surfaces retained. */
  get adoptedSurfaceCount(): number {
    return this.#surfacesById.size;
  }

  /** Number of replaceable changes currently coalesced into one frame. */
  get pendingChanges(): number {
    return this.#pendingChanges;
  }

  /** Installs host and engine callbacks and schedules the first viewport. */
  mount(): void {
    if (this.#lifecycle !== "Created") {
      throw new BrowserViewerError("InvalidLifecycle");
    }
    this.#lifecycle = "Mounted";
    const engineHandlers: BrowserViewerEngineHandlers = Object.freeze({
      onSurface: (surface: BrowserViewerSurface): void => {
        this.#acceptSurface(surface);
      },
      onCapabilityDecision: (decision: CapabilityDecision): void => {
        this.#acceptCapabilityDecision(decision);
      },
      onEngineError: (error: EngineError): void => {
        this.#acceptEngineError(error);
      },
      onWorkerFault: (code: BrowserWorkerFaultCode): void => {
        this.#acceptWorkerFault(code);
      },
    });
    const observationHandlers: BrowserViewerObservationHandlers =
      Object.freeze({
        onScroll: (pages: readonly PageViewport[]): void => {
          this.setVisiblePages(pages);
        },
        onIntersection: (pages: readonly PageViewport[]): void => {
          this.setVisiblePages(pages);
        },
        onResize: (pages: readonly PageViewport[]): void => {
          this.setVisiblePages(pages);
        },
        onDeviceScale: (scale: number): void => {
          this.setDeviceScaleMilli(scale);
        },
      });
    try {
      this.#client.setHandlers(engineHandlers);
      this.#observations.connect(observationHandlers);
    } catch {
      this.#lifecycle = "Unmounted";
      try {
        this.#client.setHandlers(undefined);
      } catch {
        // Cleanup continues; no untrusted error text is observed.
      }
      try {
        this.#observations.disconnect();
      } catch {
        // Cleanup continues; no untrusted error text is observed.
      }
      try {
        this.#client.close();
      } catch {
        // Cleanup continues; no untrusted error text is observed.
      }
      throw new BrowserViewerError("HostBindingFailure");
    }
    this.#schedule();
  }

  /** Replaces visible pages after scroll, intersection, or resize. */
  setVisiblePages(
    pages: readonly PageViewport[],
  ): BrowserViewerUpdateResult {
    if (!this.#canAcceptChange()) {
      return "Rejected";
    }
    const visiblePages = snapshotVisiblePages(
      pages,
      this.#limits.maxVisiblePages,
    );
    if (visiblePages === undefined) {
      return this.#rejectLocal("InvalidViewport");
    }
    return this.#replaceState(Object.freeze({
      ...this.#state,
      visiblePages,
    }));
  }

  /** Replaces the canonical reduced zoom ratio. */
  setZoom(
    numerator: number,
    denominator: number,
  ): BrowserViewerUpdateResult {
    return this.#replacePrimitiveState({
      zoomNumerator: numerator,
      zoomDenominator: denominator,
    });
  }

  /** Replaces the user-requested canonical rotation. */
  setRotation(rotation: PageRotation): BrowserViewerUpdateResult {
    return this.#replacePrimitiveState({ rotation });
  }

  /** Replaces the observed integer milli-scale DPR. */
  setDeviceScaleMilli(
    deviceScaleMilli: number,
  ): BrowserViewerUpdateResult {
    return this.#replacePrimitiveState({ deviceScaleMilli });
  }

  /** Replaces the optional-content configuration identity. */
  setOptionalContentId(
    optionalContentId: bigint,
  ): BrowserViewerUpdateResult {
    return this.#replacePrimitiveState({ optionalContentId });
  }

  /** Replaces the immutable product document revision. */
  setDocumentRevision(
    documentRevision: bigint,
  ): BrowserViewerUpdateResult {
    return this.#replacePrimitiveState({ documentRevision });
  }

  /** Replaces the annotation revision. */
  setAnnotationRevision(
    annotationRevision: bigint,
  ): BrowserViewerUpdateResult {
    return this.#replacePrimitiveState({ annotationRevision });
  }

  /** Replaces the product quality policy. */
  setQuality(quality: QualityPolicy): BrowserViewerUpdateResult {
    return this.#replacePrimitiveState({ quality });
  }

  /** Replaces the registered output profile. */
  setOutputProfile(
    outputProfile: OutputProfile,
  ): BrowserViewerUpdateResult {
    return this.#replacePrimitiveState({ outputProfile });
  }

  /**
   * Disconnects all host work, releases surfaces, closes client ownership,
   * and restores the pre-teardown focus snapshot.
   */
  unmount(): void {
    if (this.#lifecycle === "Unmounted") {
      return;
    }
    let focusSnapshot: BrowserViewerFocusSnapshot | undefined;
    try {
      focusSnapshot = this.#focus.captureBeforeUnmount();
    } catch {
      focusSnapshot = undefined;
      this.#setLocalFailureWithoutPresentation("PresentationFault");
    }
    this.#lifecycle = "Unmounted";
    try {
      this.#client.setHandlers(undefined);
    } catch {
      this.#setLocalFailureWithoutPresentation("ClientFault");
    }
    try {
      this.#observations.disconnect();
    } catch {
      this.#setLocalFailureWithoutPresentation("ClientFault");
    }
    const frameHandle = this.#frameHandle;
    this.#scheduledTicket = undefined;
    this.#frameHandle = undefined;
    this.#pendingChanges = 0;
    if (frameHandle !== undefined) {
      try {
        this.#frames.cancel(frameHandle);
      } catch {
        this.#setLocalFailureWithoutPresentation("SchedulerFault");
      }
    }
    this.#releaseAllSurfaces();
    this.#clearPresentation();
    try {
      this.#client.close();
    } catch {
      this.#setLocalFailureWithoutPresentation("ClientFault");
    }
    if (focusSnapshot !== undefined) {
      try {
        this.#focus.restoreAfterUnmount(focusSnapshot);
      } catch {
        this.#setLocalFailureWithoutPresentation("PresentationFault");
      }
    }
  }

  #replacePrimitiveState(
    change: Partial<Omit<ViewerState, "visiblePages">>,
  ): BrowserViewerUpdateResult {
    if (!this.#canAcceptChange()) {
      return "Rejected";
    }
    return this.#replaceState(Object.freeze({
      ...this.#state,
      ...change,
    }));
  }

  #canAcceptChange(): boolean {
    if (this.#lifecycle !== "Mounted") {
      this.#publishLocalFailure("InvalidViewport");
      return false;
    }
    if (
      this.#scheduledTicket !== undefined
      && this.#pendingChanges >= this.#limits.maxCoalescedChanges
    ) {
      this.#publishLocalFailure("CoalescingLimit");
      return false;
    }
    return true;
  }

  #replaceState(state: ViewerState): BrowserViewerUpdateResult {
    if (!this.#canAcceptChange()) {
      return "Rejected";
    }
    const nextGeneration = this.#currentGeneration + 1n;
    if (
      nextGeneration > MAX_U64
      || requestForState(state, nextGeneration) === undefined
    ) {
      return this.#rejectLocal(
        nextGeneration > MAX_U64
          ? "GenerationExhausted"
          : "InvalidViewport",
      );
    }
    this.#state = state;
    if (this.#scheduledTicket !== undefined) {
      this.#pendingChanges += 1;
      return "Coalesced";
    }
    return this.#schedule() ? "Scheduled" : "Rejected";
  }

  #schedule(): boolean {
    if (this.#lifecycle !== "Mounted") {
      return false;
    }
    if (this.#scheduledTicket !== undefined) {
      return true;
    }
    const ticket = {};
    this.#scheduledTicket = ticket;
    this.#pendingChanges = 1;
    let handle: number;
    try {
      handle = this.#frames.request((): void => {
        this.#runScheduledFrame(ticket);
      });
    } catch {
      this.#scheduledTicket = undefined;
      this.#pendingChanges = 0;
      this.#publishLocalFailure("SchedulerFault");
      return false;
    }
    if (this.#scheduledTicket === ticket) {
      if (
        !Number.isSafeInteger(handle)
        || handle < 0
      ) {
        this.#scheduledTicket = undefined;
        this.#pendingChanges = 0;
        this.#publishLocalFailure("SchedulerFault");
        return false;
      }
      this.#frameHandle = handle;
    }
    return true;
  }

  #runScheduledFrame(ticket: object): void {
    if (
      this.#scheduledTicket !== ticket
      || this.#lifecycle !== "Mounted"
    ) {
      return;
    }
    this.#scheduledTicket = undefined;
    this.#frameHandle = undefined;
    this.#pendingChanges = 0;
    const generation = this.#currentGeneration + 1n;
    if (generation > MAX_U64) {
      this.#publishLocalFailure("GenerationExhausted");
      return;
    }
    const request = requestForState(this.#state, generation);
    if (request === undefined) {
      this.#publishLocalFailure("InvalidViewport");
      return;
    }
    try {
      this.#client.setViewport(request);
    } catch {
      this.#publishLocalFailure("ClientFault");
      return;
    }
    this.#currentGeneration = generation;
    this.#currentViewport = request;
    this.#clearFailure();
    this.#releaseAllSurfaces();
    this.#clearPresentation();
  }

  #acceptSurface(surface: BrowserViewerSurface): void {
    if (!isSurface(surface)) {
      this.#publishLocalFailure("InvalidSurface");
      return;
    }
    if (
      this.#lifecycle !== "Mounted"
      || surface.metadata.generation !== this.#currentGeneration
    ) {
      this.#releaseSurface(surface);
      return;
    }
    if (this.#surfacesById.has(surface.metadata.id)) {
      this.#releaseAllSurfaces();
      this.#releaseSurface(surface);
      this.#publishLocalFailure("InvalidSurface");
      return;
    }
    const key = regionKey(surface.metadata);
    const replaced = this.#surfacesByRegion.get(key);
    if (
      replaced === undefined
      && this.#surfacesById.size
        >= this.#limits.maxAdoptedSurfaces
    ) {
      this.#releaseSurface(surface);
      this.#publishLocalFailure("SurfaceLimit");
      return;
    }
    this.#clearFailure();
    try {
      this.#presentation.present(surface);
    } catch {
      this.#releaseSurface(surface);
      this.#publishLocalFailure("PresentationFault");
      return;
    }
    this.#surfacesById.set(surface.metadata.id, surface);
    this.#surfacesByRegion.set(key, surface);
    if (replaced !== undefined) {
      this.#surfacesById.delete(replaced.metadata.id);
      this.#releaseSurface(replaced);
    }
  }

  #acceptCapabilityDecision(decision: CapabilityDecision): void {
    if (!validateCapabilityDecision(decision)) {
      this.#publishLocalFailure("InvalidCapabilityDecision");
      return;
    }
    switch (decision.status) {
      case SupportStatus.Supported:
        this.#clearFailure();
        break;
      case SupportStatus.Unsupported:
      case SupportStatus.Rejected:
        this.#publishFailure(Object.freeze({
          source: "CapabilityDecision",
          kind: decision.status === SupportStatus.Unsupported
            ? "Unsupported"
            : "Rejected",
          code: decision.status,
          decision,
        }));
        break;
    }
  }

  #acceptEngineError(error: EngineError): void {
    if (!validateEngineError(error)) {
      this.#publishLocalFailure("InvalidEngineError");
      return;
    }
    this.#publishFailure(Object.freeze({
      source: "EngineError",
      kind: kindForEngineError(error.code),
      code: error.code,
      diagnosticId: error.diagnostic_id,
    }));
  }

  #acceptWorkerFault(code: unknown): void {
    if (!isWorkerFaultCode(code)) {
      this.#publishLocalFailure("InvalidWorkerFault");
      return;
    }
    // The client has already made the old Worker epoch terminal. Release every
    // adopted handle before exposing the fault so no stale page remains visible
    // during reopen and one release failure cannot block the remaining cleanup.
    this.#releaseAllSurfaces();
    this.#clearPresentation();
    this.#publishFailure(Object.freeze({
      source: "WorkerFault",
      kind: kindForWorkerFault(code),
      code,
    }));
  }

  #releaseAllSurfaces(): void {
    const surfaces = Array.from(this.#surfacesById.values());
    this.#surfacesById.clear();
    this.#surfacesByRegion.clear();
    for (const surface of surfaces) {
      this.#releaseSurface(surface);
    }
  }

  #releaseSurface(surface: BrowserViewerSurface): void {
    try {
      this.#client.releaseSurface(surface);
    } catch {
      if (this.#lifecycle === "Mounted") {
        this.#publishLocalFailure("ClientFault");
      } else {
        this.#setLocalFailureWithoutPresentation("ClientFault");
      }
    }
  }

  #clearPresentation(): void {
    try {
      this.#presentation.clear();
    } catch {
      if (this.#lifecycle === "Mounted") {
        this.#publishLocalFailure("PresentationFault");
      } else {
        this.#setLocalFailureWithoutPresentation("PresentationFault");
      }
    }
  }

  #rejectLocal(
    code: BrowserViewerLocalFailureCode,
  ): "Rejected" {
    this.#publishLocalFailure(code);
    return "Rejected";
  }

  #publishLocalFailure(code: BrowserViewerLocalFailureCode): void {
    this.#publishFailure(Object.freeze({
      source: "Viewer",
      kind: kindForLocalFailure(code),
      code,
    }));
  }

  #setLocalFailureWithoutPresentation(
    code: BrowserViewerLocalFailureCode,
  ): void {
    this.#failure = Object.freeze({
      source: "Viewer",
      kind: kindForLocalFailure(code),
      code,
    });
  }

  #publishFailure(failure: BrowserViewerFailure): void {
    this.#failure = failure;
    if (this.#lifecycle !== "Mounted") {
      return;
    }
    try {
      this.#presentation.showFailure(failure);
    } catch {
      this.#setLocalFailureWithoutPresentation("PresentationFault");
    }
  }

  #clearFailure(): void {
    if (this.#failure === undefined) {
      return;
    }
    this.#failure = undefined;
    try {
      this.#presentation.clearFailure();
    } catch {
      this.#setLocalFailureWithoutPresentation("PresentationFault");
    }
  }
}
