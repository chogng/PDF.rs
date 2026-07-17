import type {
  NativeBackend,
  SurfaceMetadata,
  SurfaceReadyEvent,
  SurfaceReclaimedEvent,
  SurfaceRegion,
  SurfaceReleaseAcknowledgedEvent,
  SurfaceTransport,
} from "../generated/engine-protocol.js";
import {
  ENDPOINT_CAPABILITY_LOCAL_MEMORY,
  ENDPOINT_CAPABILITY_SHARED_ARRAY_BUFFER,
  ENDPOINT_CAPABILITY_SHARED_MEMORY,
  ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER,
  ENDPOINT_CAPABILITY_TRANSFERABLE_IMAGE_BITMAP,
  ENGINE_EXECUTION_CAPABILITY_OFFSCREEN_CANVAS_STAGING,
  PixelFormat,
  SurfaceReclaimReason,
  surfaceTransportRequiredCapability,
  validateAlphaMode,
  validateNativeBackend,
  validateOperationAckStatus,
  validateSurfaceRegion,
  validateSurfaceReadyEvent,
  validateSurfaceReclaimedEvent,
  validateSurfaceReleaseAcknowledgedEvent,
} from "../generated/engine-protocol.js";

const MAX_U64 = 0xffff_ffff_ffff_ffffn;
const KNOWN_ENDPOINT_CAPABILITIES =
  ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER
  | ENDPOINT_CAPABILITY_TRANSFERABLE_IMAGE_BITMAP
  | ENDPOINT_CAPABILITY_SHARED_ARRAY_BUFFER
  | ENDPOINT_CAPABILITY_SHARED_MEMORY
  | ENDPOINT_CAPABILITY_LOCAL_MEMORY;
const KNOWN_EXECUTION_CAPABILITIES =
  ENGINE_EXECUTION_CAPABILITY_OFFSCREEN_CANVAS_STAGING;
const MAX_HOST_LIMIT = 65_536;

/** Stable, content-free Surface bridge failures. */
export type BrowserSurfaceBridgeErrorCode =
  | "InvalidConfiguration"
  | "InvalidLifecycle"
  | "QueueFull"
  | "InvalidPublication"
  | "InvalidSurfaceOwner"
  | "InvalidWorkerEpoch"
  | "InvalidSession"
  | "InvalidGeneration"
  | "InvalidRendererEpoch"
  | "InvalidRenderConfig"
  | "InvalidPlanIdentity"
  | "InvalidSurfaceLayout"
  | "InvalidSurfaceSlot"
  | "InvalidResourceType"
  | "InvalidResourceExtent"
  | "MissingEndpointCapability"
  | "UnsupportedTransport"
  | "CrossOriginIsolationRequired"
  | "InvalidSharedFence"
  | "SharedPublicationChanged"
  | "StaleWorker"
  | "StaleSession"
  | "StaleGeneration"
  | "DuplicateSurface"
  | "SurfaceLimit"
  | "SessionLimit"
  | "LeaseLimit"
  | "PresentationFailure"
  | "ReleaseFailure"
  | "InvalidRelease"
  | "InvalidMemory"
  | "InvalidMemoryEpoch"
  | "InvalidMemoryRange"
  | "AdapterFailure";

/** A bounded error whose message never incorporates untrusted content. */
export class BrowserSurfaceBridgeError extends Error {
  readonly code: BrowserSurfaceBridgeErrorCode;

  constructor(code: BrowserSurfaceBridgeErrorCode) {
    super(code);
    this.name = "BrowserSurfaceBridgeError";
    this.code = code;
  }
}

/** Hard bounds applied before adopting a browser resource. */
export interface BrowserSurfaceLimits {
  readonly maxQueuedCallbacks: number;
  readonly maxLiveSurfaces: number;
  readonly maxTrackedLeases: number;
  readonly maxSessionsPerEpoch: number;
  readonly maxWorkerEpochs: number;
  readonly maxPlanRegions: number;
  readonly maxSurfaceDimension: number;
  readonly maxSurfaceStrideBytes: number;
  readonly maxSurfaceBytes: bigint;
}

/** Runtime object support, separate from negotiated protocol bits. */
export interface BrowserSurfaceRuntimeSupport {
  readonly imageBitmap: boolean;
  readonly arrayBuffer: boolean;
  readonly sharedArrayBuffer: boolean;
  readonly offscreenCanvasStaging: boolean;
}

/** One negotiated Worker process epoch. */
export interface BrowserSurfaceEpoch {
  readonly worker: bigint;
  readonly workerEpoch: bigint;
  readonly endpointCapabilities: bigint;
  readonly executionCapabilities: bigint;
  readonly crossOriginIsolated: boolean;
  readonly runtimeSupport: BrowserSurfaceRuntimeSupport;
}

/** Exact canonical render identity shared by a RenderPlan's regions. */
export interface BrowserSurfaceRenderIdentity {
  readonly renderConfig: Uint8Array;
  readonly rendererEpoch: number;
  readonly planId: bigint;
  readonly planHash: Uint8Array;
  readonly sceneHash: Uint8Array;
  readonly decisionHash: Uint8Array;
  readonly backend: NativeBackend;
  readonly format: PixelFormat;
  readonly alpha: number;
}

/** Trusted generation/RenderPlan binding supplied by the Native client. */
export interface BrowserSurfaceGeneration {
  readonly worker: bigint;
  readonly workerEpoch: bigint;
  readonly session: bigint;
  readonly generation: bigint;
  readonly identity: BrowserSurfaceRenderIdentity;
  readonly regions: readonly SurfaceRegion[];
}

/**
 * A decoded SurfaceReady plus its correlation and still-unadopted logical OOB
 * table. Worker epoch is host lifecycle state and is intentionally not inferred
 * from a resource or a public fault event.
 */
export interface BrowserSurfacePublication {
  readonly worker: bigint;
  readonly workerEpoch: bigint;
  readonly session: bigint;
  readonly generation: bigint;
  readonly surface: SurfaceReadyEvent;
  readonly resources: readonly unknown[];
}

export interface BrowserImageBitmapDescription {
  readonly width: number;
  readonly height: number;
  readonly open: boolean;
  readonly transferable: boolean;
  /** Stable underlying transferable identity, not a wrapper token. */
  readonly backingIdentity: object;
  readonly receiverOwned: boolean;
}

/** Injected ImageBitmap operations; tests need no browser global. */
export interface BrowserImageBitmapAdapter {
  isResource(value: unknown): boolean;
  describe(value: unknown): BrowserImageBitmapDescription | undefined;
  adopt(value: unknown): object;
  close(value: unknown): void;
}

export interface BrowserArrayBufferDescription {
  readonly byteLength: bigint;
  readonly fixedLength: boolean;
  readonly exclusive: boolean;
  /** Stable transferred backing-store identity. */
  readonly backingIdentity: object;
  readonly receiverOwned: boolean;
}

/** Injected fixed transferable ArrayBuffer operations. */
export interface BrowserArrayBufferAdapter {
  isResource(value: unknown): boolean;
  describe(value: unknown): BrowserArrayBufferDescription | undefined;
  adoptReadOnly(
    value: unknown,
    byteOffset: bigint,
    byteLength: bigint,
  ): object;
  release(value: unknown, presentation: object | undefined): void;
}

export interface BrowserSharedArrayBufferDescription {
  readonly byteLength: bigint;
  readonly maximumByteLength: bigint;
  readonly growable: boolean;
  /** Stable shared backing-store identity for lease reuse exclusion. */
  readonly backingIdentity: object;
}

/** Injected fenced SharedArrayBuffer operations. */
export interface BrowserSharedArrayBufferAdapter {
  isResource(value: unknown): boolean;
  describe(value: unknown): BrowserSharedArrayBufferDescription | undefined;
  /**
   * Must be an atomic acquire load of the aligned u32 publication fence.
   */
  loadPublicationEpoch(value: unknown, fenceByteOffset: bigint): number;
  adoptReadOnly(
    value: unknown,
    byteOffset: bigint,
    byteLength: bigint,
  ): object;
  release(value: unknown, presentation: object | undefined): void;
}

/** Detects a Worker-private Wasm memory so it can never pass as wire OOB. */
export interface BrowserWasmMemoryDetector {
  isMemory(value: unknown): boolean;
}

/** Browser-object adapters are injected and independently used at each side. */
export interface BrowserSurfaceAdapters {
  readonly imageBitmap: BrowserImageBitmapAdapter;
  readonly arrayBuffer: BrowserArrayBufferAdapter;
  readonly sharedArrayBuffer: BrowserSharedArrayBufferAdapter;
  readonly wasmMemory: BrowserWasmMemoryDetector;
}

/** Resource passed to the viewer only after complete bridge validation. */
export interface BrowserPresentedSurface {
  readonly metadata: SurfaceMetadata;
  readonly transport: SurfaceTransport["kind"];
  readonly resource: object;
}

/**
 * A presentation remains non-DOM staging until commit. This lets the bridge
 * perform the second SAB acquire before any visible mutation.
 */
export interface BrowserSurfacePresentationTransaction {
  commit(): void;
  abort(): void;
}

/** Injected fake/real Canvas presentation owner. */
export interface BrowserSurfacePresentationSink {
  stage(
    surface: BrowserPresentedSurface,
  ): BrowserSurfacePresentationTransaction;
  remove(surface: bigint): void;
}

export type BrowserSurfaceReleaseDisposition =
  | "Queued"
  | "AlreadyAcknowledged"
  | "WorkerTerminal";

export interface BrowserSurfaceReleaseRequest {
  readonly worker: bigint;
  readonly workerEpoch: bigint;
  readonly session: bigint;
  readonly surface: bigint;
  readonly leaseToken: bigint;
  readonly reason: SurfaceReclaimReason;
}

/** Critical ReleaseSurface delivery remains owned by the injected supervisor. */
export interface BrowserSurfaceReleaseSink {
  requestRelease(
    request: BrowserSurfaceReleaseRequest,
  ): BrowserSurfaceReleaseDisposition;
}

export interface BrowserSurfaceBridgeOptions {
  readonly limits: BrowserSurfaceLimits;
  readonly adapters: BrowserSurfaceAdapters;
  readonly presentation: BrowserSurfacePresentationSink;
  readonly releases: BrowserSurfaceReleaseSink;
  readonly epoch: BrowserSurfaceEpoch;
}

/** Correlated protocol lifecycle notification queued from a Worker callback. */
export interface BrowserSurfaceLifecycleNotification {
  readonly worker: bigint;
  readonly workerEpoch: bigint;
  readonly session: bigint;
  readonly event:
    | SurfaceReleaseAcknowledgedEvent
    | SurfaceReclaimedEvent;
}

export interface BrowserSurfaceDrainError {
  readonly code: BrowserSurfaceBridgeErrorCode;
}

/** Bounded, content-free evidence from one explicit actor drain. */
export interface BrowserSurfaceDrainResult {
  readonly processed: number;
  readonly presented: number;
  readonly released: number;
  readonly acknowledged: number;
  readonly reclaimed: number;
  readonly rejected: number;
  readonly errors: readonly BrowserSurfaceDrainError[];
}

export type BrowserSurfaceTransportKind = SurfaceTransport["kind"];

export interface BrowserSurfaceCapabilityEnvironment {
  readonly endpointCapabilities: bigint;
  readonly executionCapabilities: bigint;
  readonly crossOriginIsolated: boolean;
  readonly runtimeSupport: BrowserSurfaceRuntimeSupport;
}

/** Capability selection never re-evaluates or rewrites PDF support policy. */
export interface BrowserSurfaceCapabilityDecision<T> {
  readonly supportedTransports: readonly BrowserSurfaceTransportKind[];
  readonly preferredTransport: BrowserSurfaceTransportKind | undefined;
  readonly workerPrivateOffscreenCanvasStaging: boolean;
  readonly pdfCapabilityDecision: T;
}

const isSafePositive = (value: unknown, maximum: number): value is number =>
  typeof value === "number"
  && Number.isSafeInteger(value)
  && value > 0
  && value <= maximum;

const isU32 = (value: unknown): value is number =>
  typeof value === "number"
  && Number.isInteger(value)
  && value >= 0
  && value <= 0xffff_ffff;

const isNonzeroU64 = (value: unknown): value is bigint =>
  typeof value === "bigint" && value > 0n && value <= MAX_U64;

const isCapabilitySet = (value: unknown, known: bigint): value is bigint =>
  typeof value === "bigint" && value >= 0n && (value & ~known) === 0n;

const hasCapability = (capabilities: bigint, capability: bigint): boolean =>
  (capabilities & capability) === capability;

const bytesEqual = (left: Uint8Array, right: Uint8Array): boolean => {
  if (left.byteLength !== right.byteLength) {
    return false;
  }
  for (let index = 0; index < left.byteLength; index += 1) {
    if (left[index] !== right[index]) {
      return false;
    }
  }
  return true;
};

const exactDataRecord = (
  value: unknown,
  keys: readonly string[],
): Readonly<Record<string, unknown>> | undefined => {
  if (
    typeof value !== "object"
    || value === null
    || Object.getPrototypeOf(value) !== Object.prototype
  ) {
    return undefined;
  }
  try {
    const ownKeys = Reflect.ownKeys(value);
    if (
      ownKeys.length !== keys.length
      || ownKeys.some((key) => typeof key !== "string" || !keys.includes(key))
    ) {
      return undefined;
    }
    const snapshot: Record<string, unknown> = {};
    for (const key of keys) {
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (
        descriptor === undefined
        || !Object.prototype.hasOwnProperty.call(descriptor, "value")
        || descriptor.enumerable !== true
      ) {
        return undefined;
      }
      snapshot[key] = descriptor.value;
    }
    return snapshot;
  } catch {
    return undefined;
  }
};

const exactArraySnapshot = (value: unknown): readonly unknown[] | undefined => {
  if (!Array.isArray(value) || Object.getPrototypeOf(value) !== Array.prototype) {
    return undefined;
  }
  try {
    const keys = Reflect.ownKeys(value);
    if (keys.length !== value.length + 1 || !keys.includes("length")) {
      return undefined;
    }
    const snapshot: unknown[] = [];
    for (let index = 0; index < value.length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(value, String(index));
      if (
        descriptor === undefined
        || !Object.prototype.hasOwnProperty.call(descriptor, "value")
      ) {
        return undefined;
      }
      snapshot.push(descriptor.value);
    }
    return snapshot;
  } catch {
    return undefined;
  }
};

const cleanupResourceSnapshot = (
  value: unknown,
  maximum: number,
): readonly unknown[] | undefined => {
  try {
    if (
      !Array.isArray(value)
      || Object.getPrototypeOf(value) !== Array.prototype
    ) {
      return undefined;
    }
    const length = Object.getOwnPropertyDescriptor(value, "length");
    if (
      length === undefined
      || !Object.prototype.hasOwnProperty.call(length, "value")
      || typeof length.value !== "number"
      || !Number.isSafeInteger(length.value)
      || length.value < 0
      || length.value > maximum
    ) {
      return undefined;
    }
    const snapshot: unknown[] = [];
    const seen = new Set<unknown>();
    for (let index = 0; index < length.value; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(
        value,
        String(index),
      );
      if (
        descriptor !== undefined
        && Object.prototype.hasOwnProperty.call(descriptor, "value")
        && !seen.has(descriptor.value)
      ) {
        seen.add(descriptor.value);
        snapshot.push(descriptor.value);
      }
    }
    return snapshot;
  } catch {
    return undefined;
  }
};

interface CleanupResourceReservation {
  readonly charge: number;
  readonly resources: readonly unknown[];
}

const snapshotCleanupResourceReservation = (
  value: unknown,
  maximum: number,
): CleanupResourceReservation => {
  try {
    if (typeof value !== "object" || value === null) {
      return Object.freeze({
        charge: 1,
        resources: Object.freeze([]),
      });
    }
    const descriptor = Object.getOwnPropertyDescriptor(
      value,
      "resources",
    );
    if (
      descriptor === undefined
      || !Object.prototype.hasOwnProperty.call(descriptor, "value")
      || !Array.isArray(descriptor.value)
      || Object.getPrototypeOf(descriptor.value) !== Array.prototype
    ) {
      return Object.freeze({
        charge: 1,
        resources: Object.freeze([]),
      });
    }
    const length = Object.getOwnPropertyDescriptor(
      descriptor.value,
      "length",
    );
    if (
      length === undefined
      || !Object.prototype.hasOwnProperty.call(length, "value")
      || typeof length.value !== "number"
      || !Number.isSafeInteger(length.value)
      || length.value < 0
    ) {
      return Object.freeze({
        charge: 1,
        resources: Object.freeze([]),
      });
    }
    if (length.value > maximum) {
      throw new BrowserSurfaceBridgeError("LeaseLimit");
    }
    return Object.freeze({
      charge: Math.max(1, length.value),
      resources: Object.freeze([
        ...(cleanupResourceSnapshot(
          descriptor.value,
          maximum,
        ) ?? []),
      ]),
    });
  } catch (error: unknown) {
    if (error instanceof BrowserSurfaceBridgeError) {
      throw error;
    }
    return Object.freeze({
      charge: 1,
      resources: Object.freeze([]),
    });
  }
};

const regionKey = (region: SurfaceRegion): string =>
  [
    region.page_index,
    region.x,
    region.y,
    region.width,
    region.height,
    region.coordinate_space,
  ].join(":");

const ownedRegionKey = (
  workerEpoch: bigint,
  session: bigint,
  generation: bigint,
  region: SurfaceRegion,
): string =>
  `${workerEpoch}:${session}:${generation}:${regionKey(region)}`;

const leaseKey = (
  worker: bigint,
  workerEpoch: bigint,
  session: bigint,
  surface: bigint,
  leaseToken: bigint,
): string => `${worker}:${workerEpoch}:${session}:${surface}:${leaseToken}`;

const epochKey = (worker: bigint, workerEpoch: bigint): string =>
  `${worker}:${workerEpoch}`;

const sameRegion = (left: SurfaceRegion, right: SurfaceRegion): boolean =>
  left.page_index === right.page_index
  && left.x === right.x
  && left.y === right.y
  && left.width === right.width
  && left.height === right.height
  && left.coordinate_space === right.coordinate_space;

const validRegion = (region: SurfaceRegion): boolean =>
  isU32(region.page_index)
  && Number.isInteger(region.x)
  && region.x >= -0x8000_0000
  && region.x <= 0x7fff_ffff
  && Number.isInteger(region.y)
  && region.y >= -0x8000_0000
  && region.y <= 0x7fff_ffff
  && isU32(region.width)
  && region.width > 0
  && isU32(region.height)
  && region.height > 0;

const snapshotEpoch = (
  value: BrowserSurfaceEpoch,
): BrowserSurfaceEpoch => {
  if (
    !isNonzeroU64(value.worker)
    || !isNonzeroU64(value.workerEpoch)
    || !isCapabilitySet(
      value.endpointCapabilities,
      KNOWN_ENDPOINT_CAPABILITIES,
    )
    || !isCapabilitySet(
      value.executionCapabilities,
      KNOWN_EXECUTION_CAPABILITIES,
    )
    || typeof value.crossOriginIsolated !== "boolean"
    || typeof value.runtimeSupport.imageBitmap !== "boolean"
    || typeof value.runtimeSupport.arrayBuffer !== "boolean"
    || typeof value.runtimeSupport.sharedArrayBuffer !== "boolean"
    || typeof value.runtimeSupport.offscreenCanvasStaging !== "boolean"
  ) {
    throw new BrowserSurfaceBridgeError("InvalidConfiguration");
  }
  return Object.freeze({
    worker: value.worker,
    workerEpoch: value.workerEpoch,
    endpointCapabilities: value.endpointCapabilities,
    executionCapabilities: value.executionCapabilities,
    crossOriginIsolated: value.crossOriginIsolated,
    runtimeSupport: Object.freeze({
      imageBitmap: value.runtimeSupport.imageBitmap,
      arrayBuffer: value.runtimeSupport.arrayBuffer,
      sharedArrayBuffer: value.runtimeSupport.sharedArrayBuffer,
      offscreenCanvasStaging:
        value.runtimeSupport.offscreenCanvasStaging,
    }),
  });
};

const validateLimits = (limits: BrowserSurfaceLimits): void => {
  if (
    !isSafePositive(limits.maxQueuedCallbacks, MAX_HOST_LIMIT)
    || !isSafePositive(limits.maxLiveSurfaces, MAX_HOST_LIMIT)
    || !isSafePositive(limits.maxTrackedLeases, MAX_HOST_LIMIT)
    || limits.maxTrackedLeases < limits.maxLiveSurfaces
    || !isSafePositive(limits.maxSessionsPerEpoch, MAX_HOST_LIMIT)
    || !isSafePositive(limits.maxWorkerEpochs, MAX_HOST_LIMIT)
    || !isSafePositive(limits.maxPlanRegions, MAX_HOST_LIMIT)
    || !isSafePositive(limits.maxSurfaceDimension, 0xffff_ffff)
    || !isSafePositive(limits.maxSurfaceStrideBytes, 0xffff_ffff)
    || typeof limits.maxSurfaceBytes !== "bigint"
    || limits.maxSurfaceBytes <= 0n
    || limits.maxSurfaceBytes > MAX_U64
  ) {
    throw new BrowserSurfaceBridgeError("InvalidConfiguration");
  }
};

const snapshotLimits = (
  limits: BrowserSurfaceLimits,
): BrowserSurfaceLimits => {
  validateLimits(limits);
  return Object.freeze({
    maxQueuedCallbacks: limits.maxQueuedCallbacks,
    maxLiveSurfaces: limits.maxLiveSurfaces,
    maxTrackedLeases: limits.maxTrackedLeases,
    maxSessionsPerEpoch: limits.maxSessionsPerEpoch,
    maxWorkerEpochs: limits.maxWorkerEpochs,
    maxPlanRegions: limits.maxPlanRegions,
    maxSurfaceDimension: limits.maxSurfaceDimension,
    maxSurfaceStrideBytes: limits.maxSurfaceStrideBytes,
    maxSurfaceBytes: limits.maxSurfaceBytes,
  });
};

const validateAdapters = (adapters: BrowserSurfaceAdapters): void => {
  const methods: readonly [object, readonly string[]][] = [
    [
      adapters.imageBitmap,
      ["isResource", "describe", "adopt", "close"],
    ],
    [
      adapters.arrayBuffer,
      ["isResource", "describe", "adoptReadOnly", "release"],
    ],
    [
      adapters.sharedArrayBuffer,
      [
        "isResource",
        "describe",
        "loadPublicationEpoch",
        "adoptReadOnly",
        "release",
      ],
    ],
    [adapters.wasmMemory, ["isMemory"]],
  ];
  for (const [adapter, names] of methods) {
    if (
      typeof adapter !== "object"
      || adapter === null
      || names.some(
        (name) =>
          typeof (adapter as unknown as Record<string, unknown>)[name]
            !== "function",
      )
    ) {
      throw new BrowserSurfaceBridgeError("InvalidConfiguration");
    }
  }
};

const snapshotGeneration = (
  value: BrowserSurfaceGeneration,
  limits: BrowserSurfaceLimits,
): GenerationState => {
  const { identity } = value;
  if (
    !isNonzeroU64(value.worker)
    || !isNonzeroU64(value.workerEpoch)
    || !isNonzeroU64(value.session)
    || !isNonzeroU64(value.generation)
    || !(identity.renderConfig instanceof Uint8Array)
    || identity.renderConfig.byteLength !== 32
    || !identity.renderConfig.some((byte) => byte !== 0)
    || !isU32(identity.rendererEpoch)
    || identity.rendererEpoch === 0
    || !isNonzeroU64(identity.planId)
    || !(identity.planHash instanceof Uint8Array)
    || identity.planHash.byteLength !== 32
    || !identity.planHash.some((byte) => byte !== 0)
    || !(identity.sceneHash instanceof Uint8Array)
    || identity.sceneHash.byteLength !== 32
    || !identity.sceneHash.some((byte) => byte !== 0)
    || !(identity.decisionHash instanceof Uint8Array)
    || identity.decisionHash.byteLength !== 32
    || !identity.decisionHash.some((byte) => byte !== 0)
    || !validateNativeBackend(identity.backend)
    || identity.format !== PixelFormat.Rgba8
    || !validateAlphaMode(identity.alpha)
    || !Array.isArray(value.regions)
    || value.regions.length === 0
    || value.regions.length > limits.maxPlanRegions
  ) {
    throw new BrowserSurfaceBridgeError("InvalidConfiguration");
  }
  const regions = new Map<string, SurfaceRegion>();
  for (const region of value.regions) {
    if (!validRegion(region) || !validateSurfaceRegion(region)) {
      throw new BrowserSurfaceBridgeError("InvalidConfiguration");
    }
    const key = regionKey(region);
    if (regions.has(key)) {
      throw new BrowserSurfaceBridgeError("InvalidConfiguration");
    }
    regions.set(key, Object.freeze({ ...region }));
  }
  return {
    worker: value.worker,
    workerEpoch: value.workerEpoch,
    session: value.session,
    generation: value.generation,
    identity: Object.freeze({
      renderConfig: new Uint8Array(identity.renderConfig),
      rendererEpoch: identity.rendererEpoch,
      planId: identity.planId,
      planHash: new Uint8Array(identity.planHash),
      sceneHash: new Uint8Array(identity.sceneHash),
      decisionHash: new Uint8Array(identity.decisionHash),
      backend: identity.backend,
      format: identity.format,
      alpha: identity.alpha,
    }),
    regions,
    closed: false,
  };
};

/** Computes browser transport support without changing the PDF decision. */
export const evaluateBrowserSurfaceCapabilities = <T>(
  environment: BrowserSurfaceCapabilityEnvironment,
  pdfCapabilityDecision: T,
): BrowserSurfaceCapabilityDecision<T> => {
  const epoch = snapshotEpoch({
    worker: 1n,
    workerEpoch: 1n,
    ...environment,
  });
  const supported: BrowserSurfaceTransportKind[] = [];
  if (
    epoch.runtimeSupport.imageBitmap
    && hasCapability(
      epoch.endpointCapabilities,
      ENDPOINT_CAPABILITY_TRANSFERABLE_IMAGE_BITMAP,
    )
  ) {
    supported.push("BrowserImageBitmap");
  }
  if (
    epoch.runtimeSupport.sharedArrayBuffer
    && epoch.crossOriginIsolated
    && hasCapability(
      epoch.endpointCapabilities,
      ENDPOINT_CAPABILITY_SHARED_ARRAY_BUFFER,
    )
  ) {
    supported.push("BrowserSharedArrayBuffer");
  }
  if (
    epoch.runtimeSupport.arrayBuffer
    && hasCapability(
      epoch.endpointCapabilities,
      ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER,
    )
  ) {
    supported.push("BrowserArrayBuffer");
  }
  return Object.freeze({
    supportedTransports: Object.freeze(supported),
    preferredTransport: supported[0],
    workerPrivateOffscreenCanvasStaging:
      epoch.runtimeSupport.offscreenCanvasStaging
      && hasCapability(
        epoch.executionCapabilities,
        ENGINE_EXECUTION_CAPABILITY_OFFSCREEN_CANVAS_STAGING,
      ),
    pdfCapabilityDecision,
  });
};

interface ParsedPublication {
  readonly worker: bigint;
  readonly workerEpoch: bigint;
  readonly session: bigint;
  readonly generation: bigint;
  readonly surface: SurfaceReadyEvent;
  readonly resources: readonly unknown[];
  readonly resource: unknown;
}

interface ValidatedPublication extends ParsedPublication {
  readonly regionKey: string;
  readonly backingIdentity: object;
}

interface ReleaseIdentity {
  readonly worker: bigint;
  readonly workerEpoch: bigint;
  readonly session: bigint;
  readonly metadata: SurfaceMetadata;
}

const releaseIdentityFor = (
  publication: Pick<
    ParsedPublication,
    "worker" | "workerEpoch" | "session" | "surface"
  >,
): ReleaseIdentity => ({
  worker: publication.worker,
  workerEpoch: publication.workerEpoch,
  session: publication.session,
  metadata: publication.surface.metadata,
});

interface GenerationState {
  readonly worker: bigint;
  readonly workerEpoch: bigint;
  readonly session: bigint;
  readonly generation: bigint;
  readonly identity: BrowserSurfaceRenderIdentity;
  readonly regions: ReadonlyMap<string, SurfaceRegion>;
  closed: boolean;
}

const sameGeneration = (
  left: GenerationState,
  right: GenerationState,
): boolean => {
  const leftIdentity = left.identity;
  const rightIdentity = right.identity;
  if (
    left.worker !== right.worker
    || left.workerEpoch !== right.workerEpoch
    || left.session !== right.session
    || left.generation !== right.generation
    || leftIdentity.rendererEpoch !== rightIdentity.rendererEpoch
    || leftIdentity.planId !== rightIdentity.planId
    || leftIdentity.backend !== rightIdentity.backend
    || leftIdentity.format !== rightIdentity.format
    || leftIdentity.alpha !== rightIdentity.alpha
    || !bytesEqual(leftIdentity.renderConfig, rightIdentity.renderConfig)
    || !bytesEqual(leftIdentity.planHash, rightIdentity.planHash)
    || !bytesEqual(leftIdentity.sceneHash, rightIdentity.sceneHash)
    || !bytesEqual(leftIdentity.decisionHash, rightIdentity.decisionHash)
    || left.regions.size !== right.regions.size
  ) {
    return false;
  }
  for (const [key, region] of left.regions) {
    const other = right.regions.get(key);
    if (other === undefined || !sameRegion(region, other)) {
      return false;
    }
  }
  return true;
};

const parsePublication = (value: unknown): ParsedPublication => {
  const record = exactDataRecord(
    value,
    [
      "worker",
      "workerEpoch",
      "session",
      "generation",
      "surface",
      "resources",
    ],
  );
  if (
    record === undefined
    || !isNonzeroU64(record.worker)
    || !isNonzeroU64(record.workerEpoch)
    || !isNonzeroU64(record.session)
    || !isNonzeroU64(record.generation)
    || !validateSurfaceReadyEvent(record.surface)
  ) {
    throw new BrowserSurfaceBridgeError("InvalidPublication");
  }
  const resources = exactArraySnapshot(record.resources);
  if (resources === undefined || resources.length !== 1) {
    throw new BrowserSurfaceBridgeError("InvalidSurfaceSlot");
  }
  const surface = record.surface;
  const slot = surface.transport.kind === "BrowserSharedArrayBuffer"
    ? surface.transport.attachment_slot
    : surface.transport.slot;
  if (slot !== 0) {
    throw new BrowserSurfaceBridgeError("InvalidSurfaceSlot");
  }
  return {
    worker: record.worker,
    workerEpoch: record.workerEpoch,
    session: record.session,
    generation: record.generation,
    surface,
    resources,
    resource: resources[0],
  };
};

const validateResource = (
  parsed: ParsedPublication,
  epoch: BrowserSurfaceEpoch,
  limits: BrowserSurfaceLimits,
  adapters: BrowserSurfaceAdapters,
): object => {
  const { metadata, transport } = parsed.surface;
  const rangeEnd = metadata.byte_offset + metadata.byte_length;
  if (
    (typeof parsed.resource !== "object" || parsed.resource === null)
    && typeof parsed.resource !== "function"
  ) {
    throw new BrowserSurfaceBridgeError("InvalidResourceType");
  }
  if (
    metadata.format !== PixelFormat.Rgba8
    || metadata.width > limits.maxSurfaceDimension
    || metadata.height > limits.maxSurfaceDimension
    || metadata.stride > limits.maxSurfaceStrideBytes
    || metadata.byte_length > limits.maxSurfaceBytes
    || metadata.byte_offset > MAX_U64 - metadata.byte_length
    || rangeEnd > limits.maxSurfaceBytes
  ) {
    throw new BrowserSurfaceBridgeError("InvalidSurfaceLayout");
  }
  const requiredCapability = surfaceTransportRequiredCapability(transport);
  if (!hasCapability(epoch.endpointCapabilities, requiredCapability)) {
    throw new BrowserSurfaceBridgeError("MissingEndpointCapability");
  }
  try {
    if (adapters.wasmMemory.isMemory(parsed.resource)) {
      throw new BrowserSurfaceBridgeError("InvalidResourceType");
    }
    switch (transport.kind) {
      case "BrowserImageBitmap": {
        if (!epoch.runtimeSupport.imageBitmap) {
          throw new BrowserSurfaceBridgeError("UnsupportedTransport");
        }
        if (!adapters.imageBitmap.isResource(parsed.resource)) {
          throw new BrowserSurfaceBridgeError("InvalidResourceType");
        }
        const description = adapters.imageBitmap.describe(parsed.resource);
        if (
          description === undefined
          || !description.open
          || !description.transferable
          || !description.receiverOwned
          || typeof description.backingIdentity !== "object"
          || description.backingIdentity === null
          || description.width !== transport.width
          || description.height !== transport.height
          || description.width !== metadata.width
          || description.height !== metadata.height
        ) {
          throw new BrowserSurfaceBridgeError("InvalidResourceExtent");
        }
        return description.backingIdentity;
      }
      case "BrowserArrayBuffer": {
        if (!epoch.runtimeSupport.arrayBuffer) {
          throw new BrowserSurfaceBridgeError("UnsupportedTransport");
        }
        if (!adapters.arrayBuffer.isResource(parsed.resource)) {
          throw new BrowserSurfaceBridgeError("InvalidResourceType");
        }
        const description = adapters.arrayBuffer.describe(parsed.resource);
        if (
          description === undefined
          || !description.fixedLength
          || !description.exclusive
          || !description.receiverOwned
          || typeof description.backingIdentity !== "object"
          || description.backingIdentity === null
          || description.byteLength > limits.maxSurfaceBytes
          || description.byteLength !== transport.buffer_length
          || rangeEnd > description.byteLength
        ) {
          throw new BrowserSurfaceBridgeError("InvalidResourceExtent");
        }
        return description.backingIdentity;
      }
      case "BrowserSharedArrayBuffer": {
        if (!epoch.crossOriginIsolated) {
          throw new BrowserSurfaceBridgeError(
            "CrossOriginIsolationRequired",
          );
        }
        if (!epoch.runtimeSupport.sharedArrayBuffer) {
          throw new BrowserSurfaceBridgeError("UnsupportedTransport");
        }
        if (!adapters.sharedArrayBuffer.isResource(parsed.resource)) {
          throw new BrowserSurfaceBridgeError("InvalidResourceType");
        }
        const description =
          adapters.sharedArrayBuffer.describe(parsed.resource);
        if (
          description === undefined
          || description.growable
          || typeof description.backingIdentity !== "object"
          || description.backingIdentity === null
          || description.byteLength > limits.maxSurfaceBytes
          || description.maximumByteLength > limits.maxSurfaceBytes
          || description.byteLength !== transport.buffer_length
          || description.maximumByteLength !== transport.buffer_length
          || rangeEnd > description.byteLength
        ) {
          throw new BrowserSurfaceBridgeError("InvalidResourceExtent");
        }
        const acquired = adapters.sharedArrayBuffer.loadPublicationEpoch(
          parsed.resource,
          transport.fence_byte_offset,
        );
        if (acquired !== transport.publication_epoch) {
          throw new BrowserSurfaceBridgeError("InvalidSharedFence");
        }
        return description.backingIdentity;
      }
    }
  } catch (error: unknown) {
    if (error instanceof BrowserSurfaceBridgeError) {
      throw error;
    }
    throw new BrowserSurfaceBridgeError("AdapterFailure");
  }
};

const validateContext = (
  parsed: ParsedPublication,
  epoch: BrowserSurfaceEpoch,
  generation: GenerationState | undefined,
): string => {
  const { metadata } = parsed.surface;
  if (
    parsed.worker !== metadata.owner.worker
    || parsed.session !== metadata.owner.session
    || parsed.generation !== metadata.generation
  ) {
    throw new BrowserSurfaceBridgeError("InvalidSurfaceOwner");
  }
  if (
    parsed.worker !== epoch.worker
    || parsed.workerEpoch !== epoch.workerEpoch
  ) {
    throw new BrowserSurfaceBridgeError("StaleWorker");
  }
  if (generation === undefined || generation.closed) {
    throw new BrowserSurfaceBridgeError("StaleSession");
  }
  if (
    parsed.session !== generation.session
    || parsed.worker !== generation.worker
    || parsed.workerEpoch !== generation.workerEpoch
  ) {
    throw new BrowserSurfaceBridgeError("InvalidSession");
  }
  if (parsed.generation !== generation.generation) {
    throw new BrowserSurfaceBridgeError("StaleGeneration");
  }
  const expectedRegion = generation.regions.get(regionKey(metadata.region));
  if (
    expectedRegion === undefined
    || !sameRegion(expectedRegion, metadata.region)
  ) {
    throw new BrowserSurfaceBridgeError("InvalidPlanIdentity");
  }
  const identity = generation.identity;
  if (metadata.renderer_epoch !== identity.rendererEpoch) {
    throw new BrowserSurfaceBridgeError("InvalidRendererEpoch");
  }
  if (!bytesEqual(metadata.render_config, identity.renderConfig)) {
    throw new BrowserSurfaceBridgeError("InvalidRenderConfig");
  }
  if (
    metadata.plan_id !== identity.planId
    || !bytesEqual(metadata.plan_hash, identity.planHash)
    || !bytesEqual(metadata.scene_hash, identity.sceneHash)
    || !bytesEqual(metadata.decision_hash, identity.decisionHash)
    || metadata.backend !== identity.backend
    || metadata.format !== identity.format
    || metadata.alpha !== identity.alpha
  ) {
    throw new BrowserSurfaceBridgeError("InvalidPlanIdentity");
  }
  return ownedRegionKey(
    parsed.workerEpoch,
    parsed.session,
    parsed.generation,
    metadata.region,
  );
};

/**
 * Full producer/consumer validator. Each side constructs its own instance and
 * calls `validate`; no producer-authenticated marker can skip consumer checks.
 */
export class BrowserSurfacePublicationValidator {
  readonly #limits: BrowserSurfaceLimits;
  readonly #adapters: BrowserSurfaceAdapters;

  constructor(
    limits: BrowserSurfaceLimits,
    adapters: BrowserSurfaceAdapters,
  ) {
    const trustedLimits = snapshotLimits(limits);
    validateAdapters(adapters);
    this.#limits = trustedLimits;
    this.#adapters = adapters;
  }

  validate(
    value: unknown,
    epochValue: BrowserSurfaceEpoch,
    generationValue: BrowserSurfaceGeneration | undefined,
  ): BrowserSurfacePublication {
    const epoch = snapshotEpoch(epochValue);
    const generation = generationValue === undefined
      ? undefined
      : snapshotGeneration(generationValue, this.#limits);
    const parsed = parsePublication(value);
    validateResource(parsed, epoch, this.#limits, this.#adapters);
    validateContext(parsed, epoch, generation);
    return Object.freeze({
      worker: parsed.worker,
      workerEpoch: parsed.workerEpoch,
      session: parsed.session,
      generation: parsed.generation,
      surface: parsed.surface,
      resources: parsed.resources,
    });
  }
}

interface LiveSurface {
  readonly worker: bigint;
  readonly workerEpoch: bigint;
  readonly session: bigint;
  readonly generation: bigint;
  readonly metadata: SurfaceMetadata;
  readonly transport: SurfaceTransport["kind"];
  readonly rawResource: unknown;
  readonly presentation: object;
  readonly regionKey: string;
  readonly leaseKey: string;
  readonly slotId: string;
  readonly backingIdentity: object;
  displayed: boolean;
  retired: boolean;
}

type ReleaseState = "Sending" | "Pending" | "Failed" | "Terminal";

interface ReleaseRecord {
  readonly key: string;
  readonly slotId: string;
  readonly request: BrowserSurfaceReleaseRequest;
  readonly backingIdentity: object | undefined;
  state: ReleaseState;
}

interface BackingLeaseRecord {
  readonly leaseKey: string;
  readonly session: bigint;
  readonly surface: bigint;
  readonly resource: object;
  readonly ordinal: number;
  terminal: boolean;
  cleanupDone: boolean;
}

type ActualResourceKind =
  | "BrowserImageBitmap"
  | "BrowserArrayBuffer"
  | "BrowserSharedArrayBuffer";

interface CleanupRecord {
  readonly key: string;
  readonly slotId: string;
  readonly leaseKey: string | undefined;
  readonly resource: object;
  readonly presentation: object | undefined;
  readonly backingIdentity: object | undefined;
  actualKind: ActualResourceKind | undefined;
  state: "Blocked" | "Failed" | "Done";
}

type CallbackWork =
  | {
      readonly kind: "Surface";
      readonly value: unknown;
      readonly slotId: string;
      readonly cleanupResources: readonly unknown[];
    }
  | { readonly kind: "Lifecycle"; readonly value: unknown };

interface MaintenanceSlot {
  readonly id: string;
  readonly charge: number;
  queued: boolean;
}

interface MutableDrain {
  processed: number;
  presented: number;
  released: number;
  acknowledged: number;
  reclaimed: number;
  rejected: number;
  errors: BrowserSurfaceDrainError[];
}

const lifecycleNotification = (
  value: unknown,
): BrowserSurfaceLifecycleNotification => {
  const record = exactDataRecord(
    value,
    ["worker", "workerEpoch", "session", "event"],
  );
  if (
    record === undefined
    || !isNonzeroU64(record.worker)
    || !isNonzeroU64(record.workerEpoch)
    || !isNonzeroU64(record.session)
    || (
      !validateSurfaceReleaseAcknowledgedEvent(record.event)
      && !validateSurfaceReclaimedEvent(record.event)
    )
  ) {
    throw new BrowserSurfaceBridgeError("InvalidRelease");
  }
  return {
    worker: record.worker,
    workerEpoch: record.workerEpoch,
    session: record.session,
    event: record.event,
  };
};

/**
 * Host-mediated browser Surface owner. Message callbacks only enqueue opaque
 * work; all validation, adoption, Canvas staging, and release happens in
 * `drain`.
 */
export class BrowserSurfaceBridge {
  readonly #limits: BrowserSurfaceLimits;
  readonly #adapters: BrowserSurfaceAdapters;
  readonly #presentation: BrowserSurfacePresentationSink;
  readonly #releases: BrowserSurfaceReleaseSink;
  readonly #queue: CallbackWork[] = [];
  readonly #generations = new Map<bigint, GenerationState>();
  readonly #seenSessions = new Set<bigint>();
  readonly #liveById = new Map<bigint, LiveSurface>();
  readonly #liveByRegion = new Map<string, LiveSurface>();
  readonly #releaseRecords = new Map<string, ReleaseRecord>();
  readonly #backingLeases =
    new Map<object, Map<string, BackingLeaseRecord>>();
  readonly #cleanupRecords = new Map<string, CleanupRecord>();
  readonly #failedRemovals = new Map<
    string,
    {
      readonly surface: bigint;
      readonly slotId: string;
    }
  >();
  readonly #failedAborts = new Map<
    string,
    BrowserSurfacePresentationTransaction
  >();
  readonly #maintenanceSlots = new Map<string, MaintenanceSlot>();
  readonly #usedEpochs = new Set<string>();
  readonly #terminalEpochs = new Set<string>();
  #epoch: BrowserSurfaceEpoch | undefined;
  #draining = false;
  #cleanupSequence = 0;
  #slotSequence = 0;
  #maintenanceCharge = 0;
  #backingSequence = 0;
  #lastFault:
    | { readonly worker: bigint; readonly workerEpoch: bigint }
    | undefined;

  constructor(options: BrowserSurfaceBridgeOptions) {
    const limits = snapshotLimits(options.limits);
    validateAdapters(options.adapters);
    if (
      typeof options.presentation.stage !== "function"
      || typeof options.presentation.remove !== "function"
      || typeof options.releases.requestRelease !== "function"
    ) {
      throw new BrowserSurfaceBridgeError("InvalidConfiguration");
    }
    this.#limits = limits;
    this.#adapters = options.adapters;
    this.#presentation = options.presentation;
    this.#releases = options.releases;
    this.#epoch = snapshotEpoch(options.epoch);
    this.#usedEpochs.add(
      epochKey(this.#epoch.worker, this.#epoch.workerEpoch),
    );
  }

  get queuedCallbacks(): number {
    return this.#queue.length;
  }

  get liveSurfaces(): number {
    return this.#liveById.size;
  }

  /** True only for negotiated Worker-private staging, never a wire transport. */
  get workerPrivateOffscreenCanvasStaging(): boolean {
    const epoch = this.#epoch;
    return epoch !== undefined
      && epoch.runtimeSupport.offscreenCanvasStaging
      && hasCapability(
        epoch.executionCapabilities,
        ENGINE_EXECUTION_CAPABILITY_OFFSCREEN_CANVAS_STAGING,
      );
  }

  /** Adds a Surface callback without inspecting or adopting its resource. */
  enqueueSurfaceReady(value: unknown): void {
    if (this.#queue.length >= this.#limits.maxQueuedCallbacks) {
      throw new BrowserSurfaceBridgeError("QueueFull");
    }
    const reservation = snapshotCleanupResourceReservation(
      value,
      this.#limits.maxTrackedLeases,
    );
    const slot = this.#allocateMaintenanceSlot(
      reservation.charge,
    );
    this.#queue.push({
      kind: "Surface",
      value,
      slotId: slot.id,
      cleanupResources: reservation.resources,
    });
  }

  /** Adds an ack/reclaim callback without applying lifecycle inline. */
  enqueueLifecycle(value: unknown): void {
    this.#enqueue({ kind: "Lifecycle", value });
  }

  #enqueue(work: CallbackWork): void {
    if (this.#queue.length >= this.#limits.maxQueuedCallbacks) {
      throw new BrowserSurfaceBridgeError("QueueFull");
    }
    this.#queue.push(work);
  }

  /**
   * Installs one exact accepted Native RenderPlan generation. Advancing a
   * session releases every prior-generation Surface before the new binding is
   * eligible to display.
   */
  activateGeneration(value: BrowserSurfaceGeneration): void {
    const epoch = this.#requireEpoch();
    const next = snapshotGeneration(value, this.#limits);
    if (
      next.worker !== epoch.worker
      || next.workerEpoch !== epoch.workerEpoch
    ) {
      throw new BrowserSurfaceBridgeError("InvalidWorkerEpoch");
    }
    const current = this.#generations.get(next.session);
    if (current === undefined) {
      if (this.#seenSessions.has(next.session)) {
        throw new BrowserSurfaceBridgeError("InvalidSession");
      }
      if (
        this.#seenSessions.size >= this.#limits.maxSessionsPerEpoch
        || this.#generations.size >= this.#limits.maxSessionsPerEpoch
      ) {
        throw new BrowserSurfaceBridgeError("SessionLimit");
      }
      this.#seenSessions.add(next.session);
    } else {
      if (!current.closed && sameGeneration(current, next)) {
        return;
      }
      if (current.closed || next.generation <= current.generation) {
        throw new BrowserSurfaceBridgeError("InvalidGeneration");
      }
      const errors = this.#releaseSessionSurfaces(
        next.session,
        SurfaceReclaimReason.GenerationReplaced,
      );
      this.#generations.set(next.session, next);
      if (errors.length !== 0) {
        throw new BrowserSurfaceBridgeError(errors[0]!);
      }
      return;
    }
    this.#generations.set(next.session, next);
  }

  /**
   * Explicit actor turn. No adapter, sink, DOM, or release callback is invoked
   * by enqueue methods before this drain.
   */
  drain(maximum: number = this.#limits.maxQueuedCallbacks): BrowserSurfaceDrainResult {
    if (!isSafePositive(maximum, this.#limits.maxQueuedCallbacks)) {
      throw new BrowserSurfaceBridgeError("InvalidConfiguration");
    }
    if (this.#draining) {
      throw new BrowserSurfaceBridgeError("InvalidLifecycle");
    }
    this.#draining = true;
    const result: MutableDrain = {
      processed: 0,
      presented: 0,
      released: 0,
      acknowledged: 0,
      reclaimed: 0,
      rejected: 0,
      errors: [],
    };
    try {
      this.#retryMaintenance(result);
      while (result.processed < maximum) {
        const work = this.#queue.shift();
        if (work === undefined) {
          break;
        }
        result.processed += 1;
        try {
          if (work.kind === "Surface") {
            this.#consumeSurface(work.value, work.slotId, result);
            const slot = this.#maintenanceSlots.get(work.slotId);
            if (slot !== undefined) {
              slot.queued = false;
            }
            this.#settleCompletedCleanups(work.slotId);
          } else {
            this.#consumeLifecycle(work.value, result);
          }
        } catch (error: unknown) {
          const code = error instanceof BrowserSurfaceBridgeError
            ? error.code
            : "AdapterFailure";
          result.rejected += 1;
          result.errors.push(Object.freeze({ code }));
          if (work.kind === "Surface") {
            if (this.#maintenanceSlots.has(work.slotId)) {
              this.#rejectSurface(
                work.value,
                work.slotId,
                work.cleanupResources,
                result,
              );
            }
            const slot = this.#maintenanceSlots.get(work.slotId);
            if (slot !== undefined) {
              slot.queued = false;
            }
            this.#settleCompletedCleanups(work.slotId);
          }
        }
      }
      return Object.freeze({
        ...result,
        errors: Object.freeze(result.errors),
      });
    } finally {
      this.#draining = false;
    }
  }

  /** Idempotently requests release of a viewer-owned live lease. */
  releaseSurface(surface: bigint, leaseToken: bigint): void {
    if (!isNonzeroU64(surface) || !isNonzeroU64(leaseToken)) {
      throw new BrowserSurfaceBridgeError("InvalidRelease");
    }
    const live = this.#liveById.get(surface);
    if (live?.metadata.lease_token === leaseToken) {
      const errors = this.#retireLive(
        live,
        SurfaceReclaimReason.ReleasedByHost,
        true,
      );
      if (errors.length !== 0) {
        throw new BrowserSurfaceBridgeError(errors[0]!);
      }
      return;
    }
    const epoch = this.#epoch;
    const record = epoch === undefined
      ? undefined
      : this.#findReleaseRecord(
          epoch.worker,
          epoch.workerEpoch,
          surface,
          leaseToken,
        );
    if (record !== undefined) {
      if (record.state === "Failed") {
        this.#sendRelease(record);
      }
      return;
    }
    throw new BrowserSurfaceBridgeError("InvalidRelease");
  }

  /** Session close is idempotent and releases every adopted presentation. */
  closeSession(session: bigint): void {
    if (!isNonzeroU64(session)) {
      throw new BrowserSurfaceBridgeError("InvalidSession");
    }
    const generation = this.#generations.get(session);
    if (generation !== undefined) {
      generation.closed = true;
    }
    const errors = this.#releaseSessionSurfaces(
      session,
      SurfaceReclaimReason.SessionClosed,
    );
    errors.push(...this.#releaseQueuedSession(session));
    this.#generations.delete(session);
    if (errors.length !== 0) {
      throw new BrowserSurfaceBridgeError(errors[0]!);
    }
  }

  /**
   * Worker terminal cleanup is idempotent. It is deliberately driven by
   * explicit epoch values, not a supervisor fault enum or error string.
   */
  workerFault(worker: bigint, workerEpoch: bigint): void {
    if (!isNonzeroU64(worker) || !isNonzeroU64(workerEpoch)) {
      throw new BrowserSurfaceBridgeError("InvalidWorkerEpoch");
    }
    const epoch = this.#epoch;
    if (epoch === undefined) {
      if (
        this.#lastFault?.worker === worker
        && this.#lastFault.workerEpoch === workerEpoch
      ) {
        return;
      }
      throw new BrowserSurfaceBridgeError("InvalidWorkerEpoch");
    }
    if (epoch.worker !== worker || epoch.workerEpoch !== workerEpoch) {
      throw new BrowserSurfaceBridgeError("InvalidWorkerEpoch");
    }
    const terminalErrors: BrowserSurfaceBridgeErrorCode[] = [];
    for (const live of Array.from(this.#liveById.values())) {
      terminalErrors.push(...this.#retireLive(
        live,
        SurfaceReclaimReason.RendererRestarted,
        true,
      ));
    }
    const pending = this.#queue.splice(0);
    const ignored: MutableDrain = {
      processed: 0,
      presented: 0,
      released: 0,
      acknowledged: 0,
      reclaimed: 0,
      rejected: 0,
      errors: [],
    };
    for (const work of pending) {
      if (work.kind === "Surface") {
        this.#rejectSurface(
          work.value,
          work.slotId,
          work.cleanupResources,
          ignored,
          SurfaceReclaimReason.RendererRestarted,
        );
        const slot = this.#maintenanceSlots.get(work.slotId);
        if (slot !== undefined) {
          slot.queued = false;
        }
        this.#settleCompletedCleanups(work.slotId);
      }
    }
    this.#generations.clear();
    this.#seenSessions.clear();
    this.#terminalEpochs.add(epochKey(worker, workerEpoch));
    this.#markEpochTerminal(worker, workerEpoch);
    this.#epoch = undefined;
    this.#lastFault = Object.freeze({ worker, workerEpoch });
    terminalErrors.push(
      ...ignored.errors.map((error) => error.code),
    );
    if (terminalErrors.length !== 0) {
      throw new BrowserSurfaceBridgeError(terminalErrors[0]!);
    }
  }

  /** Starts a fresh epoch only after the prior epoch is terminal. */
  startEpoch(value: BrowserSurfaceEpoch): void {
    if (this.#epoch !== undefined) {
      throw new BrowserSurfaceBridgeError("InvalidLifecycle");
    }
    const next = snapshotEpoch(value);
    const key = epochKey(next.worker, next.workerEpoch);
    if (this.#usedEpochs.has(key)) {
      throw new BrowserSurfaceBridgeError("InvalidWorkerEpoch");
    }
    if (this.#usedEpochs.size >= this.#limits.maxWorkerEpochs) {
      throw new BrowserSurfaceBridgeError("SurfaceLimit");
    }
    this.#usedEpochs.add(key);
    this.#epoch = next;
    this.#generations.clear();
    this.#seenSessions.clear();
    this.#lastFault = undefined;
  }

  #requireEpoch(): BrowserSurfaceEpoch {
    if (this.#epoch === undefined) {
      throw new BrowserSurfaceBridgeError("InvalidLifecycle");
    }
    return this.#epoch;
  }

  #consumeSurface(
    value: unknown,
    slotId: string,
    result: MutableDrain,
  ): void {
    const epoch = this.#requireEpoch();
    const parsed = parsePublication(value);
    const generation = this.#generations.get(parsed.session);
    const backingIdentity = validateResource(
      parsed,
      epoch,
      this.#limits,
      this.#adapters,
    );
    const region = validateContext(parsed, epoch, generation);
    const validated: ValidatedPublication = {
      ...parsed,
      regionKey: region,
      backingIdentity,
    };
    this.#present(validated, slotId, result);
  }

  #present(
    publication: ValidatedPublication,
    slotId: string,
    result: MutableDrain,
  ): void {
    const { metadata, transport } = publication.surface;
    const key = leaseKey(
      publication.worker,
      publication.workerEpoch,
      publication.session,
      metadata.id,
      metadata.lease_token,
    );
    const backing = this.#backingLeases
      .get(publication.backingIdentity);
    if (
      backing !== undefined
      && Array.from(backing.keys()).some(
        (candidate) => candidate !== key,
      )
    ) {
      this.#scheduleCleanup(
        publication.resource,
        undefined,
        this.#nextCleanupKey(key),
        slotId,
        publication.backingIdentity,
      );
      try {
        this.#requestRelease(
          releaseIdentityFor(publication),
          SurfaceReclaimReason.ReleasedByHost,
          slotId,
          publication.backingIdentity,
          publication.resource,
        );
        result.released += 1;
      } catch (error: unknown) {
        this.#appendError(result, error, "ReleaseFailure");
      }
      throw new BrowserSurfaceBridgeError("DuplicateSurface");
    }
    if (
      Array.from(this.#failedRemovals.values()).some(
        (removal) => removal.surface === metadata.id,
      )
    ) {
      this.#scheduleCleanup(
        publication.resource,
        undefined,
        this.#nextCleanupKey(key),
        slotId,
        publication.backingIdentity,
      );
      throw new BrowserSurfaceBridgeError("DuplicateSurface");
    }
    const duplicate = this.#liveById.get(metadata.id);
    if (duplicate !== undefined) {
      this.#appendCodes(result, this.#retireLive(
        duplicate,
        SurfaceReclaimReason.ReleasedByHost,
        true,
      ));
      this.#scheduleCleanup(
        publication.resource,
        undefined,
        this.#nextCleanupKey(key),
        slotId,
        publication.backingIdentity,
      );
      throw new BrowserSurfaceBridgeError("DuplicateSurface");
    }
    if (this.#releaseRecords.has(key)) {
      this.#scheduleCleanup(
        publication.resource,
        undefined,
        this.#nextCleanupKey(key),
        slotId,
        publication.backingIdentity,
      );
      throw new BrowserSurfaceBridgeError("DuplicateSurface");
    }
    const replaced = this.#liveByRegion.get(publication.regionKey);
    if (
      replaced === undefined
      && this.#liveById.size >= this.#limits.maxLiveSurfaces
    ) {
      const cleanupCode = this.#scheduleCleanup(
        publication.resource,
        undefined,
        key,
        slotId,
        publication.backingIdentity,
      );
      if (cleanupCode !== undefined) {
        result.errors.push(Object.freeze({ code: cleanupCode }));
      }
      try {
        this.#requestRelease(
          releaseIdentityFor(publication),
          SurfaceReclaimReason.ReleasedByHost,
          slotId,
          publication.backingIdentity,
          publication.resource,
        );
        result.released += 1;
      } catch (error: unknown) {
        this.#appendError(result, error, "ReleaseFailure");
      }
      throw new BrowserSurfaceBridgeError("SurfaceLimit");
    }

    this.#reserveBacking(
      publication.backingIdentity,
      key,
      publication.session,
      metadata.id,
      publication.resource as object,
      false,
    );
    let presentation: object | undefined;
    let transaction: BrowserSurfacePresentationTransaction | undefined;
    try {
      if (transport.kind === "BrowserSharedArrayBuffer") {
        const before =
          this.#adapters.sharedArrayBuffer.loadPublicationEpoch(
            publication.resource,
            transport.fence_byte_offset,
          );
        if (before !== transport.publication_epoch) {
          throw new BrowserSurfaceBridgeError(
            "SharedPublicationChanged",
          );
        }
      }
      presentation = this.#adopt(publication);
      transaction = this.#presentation.stage(Object.freeze({
        metadata,
        transport: transport.kind,
        resource: presentation,
      }));
      if (transport.kind === "BrowserSharedArrayBuffer") {
        const after =
          this.#adapters.sharedArrayBuffer.loadPublicationEpoch(
            publication.resource,
            transport.fence_byte_offset,
          );
        if (after !== transport.publication_epoch) {
          throw new BrowserSurfaceBridgeError(
            "SharedPublicationChanged",
          );
        }
      }
      this.#assertPublicationCurrent(publication);
      transaction.commit();
    } catch (error: unknown) {
      try {
        transaction?.abort();
      } catch {
        if (transaction !== undefined) {
          this.#failedAborts.set(slotId, transaction);
        }
        result.errors.push(Object.freeze({
          code: "PresentationFailure",
        }));
      }
      const cleanupCode = this.#scheduleCleanup(
        publication.resource,
        presentation,
        key,
        slotId,
        publication.backingIdentity,
      );
      if (cleanupCode !== undefined) {
        result.errors.push(Object.freeze({ code: cleanupCode }));
      }
      try {
        this.#requestRelease(
          releaseIdentityFor(publication),
          SurfaceReclaimReason.ReleasedByHost,
          slotId,
          publication.backingIdentity,
          publication.resource,
        );
        result.released += 1;
      } catch (releaseError: unknown) {
        this.#appendError(result, releaseError, "ReleaseFailure");
      }
      if (error instanceof BrowserSurfaceBridgeError) {
        throw error;
      }
      throw new BrowserSurfaceBridgeError("PresentationFailure");
    }

    const live: LiveSurface = {
      worker: publication.worker,
      workerEpoch: publication.workerEpoch,
      session: publication.session,
      generation: publication.generation,
      metadata,
      transport: transport.kind,
      rawResource: publication.resource,
      presentation,
      regionKey: publication.regionKey,
      leaseKey: key,
      slotId,
      backingIdentity: publication.backingIdentity,
      displayed: true,
      retired: false,
    };
    this.#liveById.set(metadata.id, live);
    this.#liveByRegion.set(publication.regionKey, live);
    result.presented += 1;
    if (replaced !== undefined) {
      this.#appendCodes(result, this.#retireLive(
        replaced,
        SurfaceReclaimReason.ReleasedByHost,
        false,
      ));
      result.released += 1;
    }
  }

  #adopt(publication: ValidatedPublication): object {
    const { metadata, transport } = publication.surface;
    try {
      switch (transport.kind) {
        case "BrowserImageBitmap":
          return this.#adapters.imageBitmap.adopt(publication.resource);
        case "BrowserArrayBuffer":
          return this.#adapters.arrayBuffer.adoptReadOnly(
            publication.resource,
            metadata.byte_offset,
            metadata.byte_length,
          );
        case "BrowserSharedArrayBuffer":
          return this.#adapters.sharedArrayBuffer.adoptReadOnly(
            publication.resource,
            metadata.byte_offset,
            metadata.byte_length,
          );
      }
    } catch {
      throw new BrowserSurfaceBridgeError("AdapterFailure");
    }
  }

  #consumeLifecycle(value: unknown, result: MutableDrain): void {
    const notification = lifecycleNotification(value);
    const epoch = this.#epoch;
    if (
      epoch === undefined
      || notification.worker !== epoch.worker
      || notification.workerEpoch !== epoch.workerEpoch
    ) {
      return;
    }
    const event = notification.event;
    const key = leaseKey(
      notification.worker,
      notification.workerEpoch,
      notification.session,
      event.surface,
      event.lease_token,
    );
    const record = this.#releaseRecords.get(key);
    const live = this.#liveById.get(event.surface);
    const liveMatchesEvent =
      live !== undefined
      && live.worker === notification.worker
      && live.workerEpoch === notification.workerEpoch
      && live.session === notification.session
      && live.metadata.id === event.surface
      && live.metadata.lease_token === event.lease_token;
    if (
      record === undefined
      && live !== undefined
      && !liveMatchesEvent
    ) {
      throw new BrowserSurfaceBridgeError("InvalidRelease");
    }
    const matchingLive = liveMatchesEvent ? live : undefined;
    if ("status" in event) {
      if (
        !validateOperationAckStatus(event.status)
        || record === undefined
        || record.request.session !== notification.session
      ) {
        throw new BrowserSurfaceBridgeError("InvalidRelease");
      }
      if (record.state === "Terminal") {
        return;
      }
      if (matchingLive !== undefined) {
        this.#appendCodes(
          result,
          this.#retireLocal(matchingLive, true),
        );
      }
      this.#terminalizeRelease(record);
      result.acknowledged += 1;
      return;
    }
    if (record?.state === "Terminal") {
      return;
    }
    if (matchingLive === undefined && record === undefined) {
      throw new BrowserSurfaceBridgeError("InvalidRelease");
    }
    if (record === undefined) {
      this.#ensureReleaseCapacity();
    }
    if (matchingLive !== undefined) {
      this.#appendCodes(
        result,
        this.#retireLocal(matchingLive, true),
      );
    }
    if (record === undefined) {
      this.#rememberReclaimed(
        notification,
        event,
        matchingLive,
      );
    } else {
      this.#terminalizeRelease(record);
    }
    result.reclaimed += 1;
  }

  #rejectSurface(
    value: unknown,
    slotId: string,
    cleanupResources: readonly unknown[],
    result: MutableDrain,
    reason: SurfaceReclaimReason = SurfaceReclaimReason.ReleasedByHost,
  ): void {
    let parsed: ParsedPublication;
    try {
      parsed = parsePublication(value);
    } catch {
      this.#cleanupMalformedResources(
        cleanupResources,
        slotId,
        result,
      );
      return;
    }
    const actual = this.#describeActualResource(parsed.resource);
    const key = leaseKey(
      parsed.worker,
      parsed.workerEpoch,
      parsed.session,
      parsed.surface.metadata.id,
      parsed.surface.metadata.lease_token,
    );
    const cleanupCode = this.#scheduleCleanup(
      parsed.resource,
      undefined,
      key,
      slotId,
      actual?.backingIdentity,
      actual?.kind,
    );
    if (cleanupCode !== undefined) {
      result.errors.push(Object.freeze({ code: cleanupCode }));
    }
    try {
      if (this.#requestRelease(
        releaseIdentityFor(parsed),
        reason,
        slotId,
        actual?.backingIdentity,
        parsed.resource,
      )) {
        result.released += 1;
      }
    } catch (error: unknown) {
      result.errors.push(Object.freeze({
        code: error instanceof BrowserSurfaceBridgeError
          ? error.code
          : "ReleaseFailure",
      }));
    }
    this.#settleCompletedCleanups(slotId);
  }

  #appendCodes(
    result: MutableDrain,
    codes: readonly BrowserSurfaceBridgeErrorCode[],
  ): void {
    for (const code of codes) {
      result.errors.push(Object.freeze({ code }));
    }
  }

  #appendError(
    result: MutableDrain,
    error: unknown,
    fallback: BrowserSurfaceBridgeErrorCode,
  ): void {
    result.errors.push(Object.freeze({
      code: error instanceof BrowserSurfaceBridgeError
        ? error.code
        : fallback,
    }));
  }

  #assertPublicationCurrent(publication: ValidatedPublication): void {
    const epoch = this.#requireEpoch();
    const generation = this.#generations.get(publication.session);
    const currentRegion = validateContext(
      publication,
      epoch,
      generation,
    );
    const key = leaseKey(
      publication.worker,
      publication.workerEpoch,
      publication.session,
      publication.surface.metadata.id,
      publication.surface.metadata.lease_token,
    );
    const backing = this.#backingLeases
      .get(publication.backingIdentity)
      ?.get(key);
    if (
      currentRegion !== publication.regionKey
      || backing?.leaseKey !== key
      || backing.terminal
    ) {
      throw new BrowserSurfaceBridgeError("StaleGeneration");
    }
  }

  #nextCleanupKey(base: string): string {
    if (this.#cleanupSequence >= Number.MAX_SAFE_INTEGER) {
      throw new BrowserSurfaceBridgeError("SurfaceLimit");
    }
    this.#cleanupSequence += 1;
    return `${base}:cleanup:${this.#cleanupSequence}`;
  }

  #allocateMaintenanceSlot(charge: number): MaintenanceSlot {
    if (
      !isSafePositive(charge, this.#limits.maxTrackedLeases)
      || this.#maintenanceCharge
        > this.#limits.maxTrackedLeases - charge
    ) {
      throw new BrowserSurfaceBridgeError("LeaseLimit");
    }
    if (this.#slotSequence >= Number.MAX_SAFE_INTEGER) {
      throw new BrowserSurfaceBridgeError("LeaseLimit");
    }
    this.#slotSequence += 1;
    const slot: MaintenanceSlot = {
      id: `slot:${this.#slotSequence}`,
      charge,
      queued: true,
    };
    this.#maintenanceSlots.set(slot.id, slot);
    this.#maintenanceCharge += charge;
    return slot;
  }

  #maybeFreeMaintenanceSlot(slotId: string): void {
    const slot = this.#maintenanceSlots.get(slotId);
    if (slot === undefined || slot.queued) {
      return;
    }
    if (
      Array.from(this.#liveById.values()).some(
        (live) => live.slotId === slotId,
      )
      || Array.from(this.#cleanupRecords.values()).some(
        (cleanup) => cleanup.slotId === slotId,
      )
      || this.#failedAborts.has(slotId)
      || Array.from(this.#failedRemovals.values()).some(
        (removal) => removal.slotId === slotId,
      )
      || Array.from(this.#releaseRecords.values()).some(
        (release) =>
          release.slotId === slotId
          && release.state !== "Terminal",
      )
    ) {
      return;
    }
    if (this.#maintenanceSlots.delete(slotId)) {
      this.#maintenanceCharge -= slot.charge;
    }
  }

  #cleanupDoneForLease(key: string, resource: object): boolean {
    return Array.from(this.#cleanupRecords.values()).some(
      (cleanup) =>
        cleanup.leaseKey === key
        && cleanup.resource === resource
        && cleanup.state === "Done",
    );
  }

  #settleCompletedCleanups(slotId: string): void {
    if (this.#maintenanceSlots.get(slotId)?.queued === true) {
      return;
    }
    for (const [key, cleanup] of this.#cleanupRecords) {
      if (cleanup.slotId !== slotId || cleanup.state !== "Done") {
        continue;
      }
      const backing = cleanup.backingIdentity === undefined
        || cleanup.leaseKey === undefined
        ? undefined
        : this.#backingLeases
            .get(cleanup.backingIdentity)
            ?.get(cleanup.leaseKey);
      if (backing === undefined) {
        this.#cleanupRecords.delete(key);
      }
    }
    this.#maybeFreeMaintenanceSlot(slotId);
  }

  #describeActualResource(
    resource: unknown,
  ): {
    readonly kind: ActualResourceKind;
    readonly backingIdentity: object;
  } | undefined {
    if (
      (typeof resource !== "object" || resource === null)
      && typeof resource !== "function"
    ) {
      return undefined;
    }
    const object = resource as object;
    try {
      const kind = this.#actualResourceKind(resource);
      if (kind === undefined) {
        return undefined;
      }
      let identity: object | undefined;
      switch (kind) {
        case "BrowserImageBitmap":
          identity =
            this.#adapters.imageBitmap.describe(resource)?.backingIdentity;
          break;
        case "BrowserArrayBuffer":
          identity =
            this.#adapters.arrayBuffer.describe(resource)?.backingIdentity;
          break;
        case "BrowserSharedArrayBuffer":
          identity =
            this.#adapters.sharedArrayBuffer
              .describe(resource)?.backingIdentity;
          break;
      }
      return {
        kind,
        backingIdentity:
          typeof identity === "object" && identity !== null
            ? identity
            : object,
      };
    } catch {
      return undefined;
    }
  }

  #actualResourceKind(
    resource: unknown,
  ): ActualResourceKind | undefined {
    const matches: ActualResourceKind[] = [];
    if (this.#adapters.imageBitmap.isResource(resource)) {
      matches.push("BrowserImageBitmap");
    }
    if (this.#adapters.arrayBuffer.isResource(resource)) {
      matches.push("BrowserArrayBuffer");
    }
    if (this.#adapters.sharedArrayBuffer.isResource(resource)) {
      matches.push("BrowserSharedArrayBuffer");
    }
    if (matches.length > 1) {
      throw new BrowserSurfaceBridgeError("InvalidResourceType");
    }
    return matches[0];
  }

  #scheduleCleanup(
    resource: unknown,
    presentation: object | undefined,
    requestedKey: string,
    slotId: string,
    backingIdentity: object | undefined,
    actualKind?: ActualResourceKind,
  ): BrowserSurfaceBridgeErrorCode | undefined {
    if (!this.#maintenanceSlots.has(slotId)) {
      throw new BrowserSurfaceBridgeError("InvalidLifecycle");
    }
    if (
      (typeof resource !== "object" || resource === null)
      && typeof resource !== "function"
    ) {
      return undefined;
    }
    const lease = requestedKey.startsWith("untrusted:")
      ? undefined
      : requestedKey.split(":cleanup:", 1)[0];
    for (const existing of this.#cleanupRecords.values()) {
      if (
        existing.resource === resource
        && existing.leaseKey === lease
      ) {
        return this.#attemptCleanup(existing);
      }
    }
    let key = requestedKey;
    const prior = this.#cleanupRecords.get(key);
    if (prior !== undefined && prior.resource !== resource) {
      key = this.#nextCleanupKey(requestedKey);
    } else if (prior?.state === "Done") {
      return undefined;
    }
    let record = this.#cleanupRecords.get(key);
    if (record === undefined) {
      record = {
        key,
        slotId,
        leaseKey: lease,
        resource: resource as object,
        presentation,
        backingIdentity,
        actualKind,
        state: "Failed",
      };
      this.#cleanupRecords.set(key, record);
    }
    return this.#attemptCleanup(record);
  }

  #attemptCleanup(
    record: CleanupRecord,
  ): BrowserSurfaceBridgeErrorCode | undefined {
    if (record.state === "Done") {
      return undefined;
    }
    const backingGroup = record.backingIdentity === undefined
      ? undefined
      : this.#backingLeases.get(record.backingIdentity);
    const backing = record.leaseKey === undefined
      ? undefined
      : backingGroup?.get(record.leaseKey);
    const ownsCanonicalResource =
      backing !== undefined && backing.resource === record.resource;
    const hasEarlierLease =
      backing !== undefined
      && backingGroup !== undefined
      && Array.from(backingGroup.values()).some(
        (candidate) => candidate.ordinal < backing.ordinal,
      );
    if (
      hasEarlierLease
      || (
        !ownsCanonicalResource
        && (
        (
          backingGroup !== undefined
          && Array.from(backingGroup.keys()).some(
            (candidate) => candidate !== record.leaseKey,
          )
        )
        || (
          backing !== undefined
          && !backing.terminal
          && backing.resource !== record.resource
        )
        )
      )
    ) {
      record.state = "Blocked";
      return undefined;
    }
    try {
      let kind = record.actualKind;
      if (kind === undefined) {
        kind = this.#actualResourceKind(record.resource);
        record.actualKind = kind;
      }
      switch (kind) {
        case "BrowserImageBitmap":
          this.#adapters.imageBitmap.close(record.resource);
          break;
        case "BrowserArrayBuffer":
          this.#adapters.arrayBuffer.release(
            record.resource,
            record.presentation,
          );
          break;
        case "BrowserSharedArrayBuffer":
          this.#adapters.sharedArrayBuffer.release(
            record.resource,
            record.presentation,
          );
          break;
        case undefined:
          /*
           * Unknown and Wasm Memory objects are never adopted here. There is
           * no registered browser-resource destructor to invoke.
           */
          break;
      }
      record.state = "Done";
      if (record.backingIdentity !== undefined) {
        const current = record.leaseKey === undefined
          ? undefined
          : this.#backingLeases
              .get(record.backingIdentity)
              ?.get(record.leaseKey);
        if (
          current !== undefined
          && current.leaseKey === record.leaseKey
          && current.resource === record.resource
        ) {
          current.cleanupDone = true;
          this.#maybeReleaseBacking(record.backingIdentity, current);
        }
      }
      if (record.leaseKey === undefined) {
        this.#cleanupRecords.delete(record.key);
      }
      this.#settleCompletedCleanups(record.slotId);
      return undefined;
    } catch (error: unknown) {
      record.state = "Failed";
      return error instanceof BrowserSurfaceBridgeError
        ? error.code
        : "AdapterFailure";
    }
  }

  #cleanupMalformedResources(
    resources: readonly unknown[],
    slotId: string,
    result: MutableDrain,
  ): void {
    for (const resource of resources) {
      const actual = this.#describeActualResource(resource);
      const key = `untrusted:${this.#nextCleanupKey("resource")}`;
      const code = this.#scheduleCleanup(
        resource,
        undefined,
        key,
        slotId,
        actual?.backingIdentity,
        actual?.kind,
      );
      if (code !== undefined) {
        result.errors.push(Object.freeze({ code }));
      }
    }
  }

  #retryMaintenance(result: MutableDrain): void {
    for (const [slotId, transaction] of this.#failedAborts) {
      try {
        transaction.abort();
        this.#failedAborts.delete(slotId);
        this.#maybeFreeMaintenanceSlot(slotId);
      } catch {
        result.errors.push(Object.freeze({
          code: "PresentationFailure",
        }));
      }
    }
    for (const [key, removal] of this.#failedRemovals) {
      try {
        this.#presentation.remove(removal.surface);
        this.#failedRemovals.delete(key);
        this.#maybeFreeMaintenanceSlot(removal.slotId);
      } catch {
        result.errors.push(Object.freeze({
          code: "PresentationFailure",
        }));
      }
    }
    for (const cleanup of this.#cleanupRecords.values()) {
      if (cleanup.state !== "Done") {
        const code = this.#attemptCleanup(cleanup);
        if (code !== undefined) {
          result.errors.push(Object.freeze({ code }));
        }
      }
    }
    for (const release of this.#releaseRecords.values()) {
      if (release.state === "Failed") {
        try {
          this.#sendRelease(release);
        } catch (error: unknown) {
          this.#appendError(result, error, "ReleaseFailure");
        }
      }
    }
  }

  #requestRelease(
    publication: ReleaseIdentity,
    reason: SurfaceReclaimReason,
    slotId: string,
    backingIdentity: object | undefined,
    resource?: unknown,
  ): boolean {
    if (!this.#maintenanceSlots.has(slotId)) {
      throw new BrowserSurfaceBridgeError("InvalidLifecycle");
    }
    const metadata = publication.metadata;
    const key = leaseKey(
      publication.worker,
      publication.workerEpoch,
      publication.session,
      metadata.id,
      metadata.lease_token,
    );
    const existing = this.#releaseRecords.get(key);
    if (existing !== undefined) {
      if (existing.request.session !== publication.session) {
        throw new BrowserSurfaceBridgeError("InvalidRelease");
      }
      if (existing.state === "Failed") {
        this.#sendRelease(existing);
      }
      return false;
    }
    this.#ensureReleaseCapacity();
    const request = Object.freeze({
      worker: publication.worker,
      workerEpoch: publication.workerEpoch,
      session: publication.session,
      surface: metadata.id,
      leaseToken: metadata.lease_token,
      reason,
    });
    const record: ReleaseRecord = {
      key,
      slotId,
      request,
      backingIdentity,
      state: "Sending",
    };
    this.#releaseRecords.set(key, record);
    if (
      backingIdentity !== undefined
      && typeof resource === "object"
      && resource !== null
    ) {
      this.#reserveBacking(
        backingIdentity,
        key,
        publication.session,
        metadata.id,
        resource,
        this.#cleanupDoneForLease(key, resource),
      );
    }
    this.#sendRelease(record);
    return true;
  }

  #sendRelease(record: ReleaseRecord): void {
    let disposition: BrowserSurfaceReleaseDisposition;
    record.state = "Sending";
    try {
      disposition = this.#releases.requestRelease(record.request);
    } catch {
      if (this.#releaseRecords.get(record.key)?.state === "Terminal") {
        return;
      }
      record.state = "Failed";
      throw new BrowserSurfaceBridgeError("ReleaseFailure");
    }
    if (this.#releaseRecords.get(record.key)?.state === "Terminal") {
      return;
    }
    if (
      disposition === "Queued"
      && !this.#terminalEpochs.has(epochKey(
        record.request.worker,
        record.request.workerEpoch,
      ))
    ) {
      record.state = "Pending";
      return;
    }
    if (
      disposition !== "Queued"
      && disposition !== "AlreadyAcknowledged"
      && disposition !== "WorkerTerminal"
    ) {
      record.state = "Failed";
      throw new BrowserSurfaceBridgeError("ReleaseFailure");
    }
    this.#terminalizeRelease(record);
  }

  #ensureReleaseCapacity(): void {
    if (this.#releaseRecords.size < this.#limits.maxTrackedLeases) {
      return;
    }
    for (const [key, record] of this.#releaseRecords) {
      if (record.state === "Terminal") {
        this.#releaseRecords.delete(key);
        if (this.#releaseRecords.size < this.#limits.maxTrackedLeases) {
          return;
        }
      }
    }
    throw new BrowserSurfaceBridgeError("LeaseLimit");
  }

  #reserveBacking(
    identity: object,
    key: string,
    session: bigint,
    surface: bigint,
    resource: object,
    cleanupDone: boolean,
  ): BackingLeaseRecord {
    let leases = this.#backingLeases.get(identity);
    if (leases === undefined) {
      leases = new Map();
      this.#backingLeases.set(identity, leases);
    }
    const existing = leases.get(key);
    if (existing !== undefined) {
      return existing;
    }
    if (this.#backingSequence >= Number.MAX_SAFE_INTEGER) {
      throw new BrowserSurfaceBridgeError("SurfaceLimit");
    }
    this.#backingSequence += 1;
    const record: BackingLeaseRecord = {
      leaseKey: key,
      session,
      surface,
      resource,
      ordinal: this.#backingSequence,
      terminal: false,
      cleanupDone,
    };
    leases.set(key, record);
    return record;
  }

  #terminalizeRelease(record: ReleaseRecord): void {
    record.state = "Terminal";
    const identity = record.backingIdentity;
    if (identity === undefined) {
      this.#maybeFreeMaintenanceSlot(record.slotId);
      return;
    }
    const backing = this.#backingLeases
      .get(identity)
      ?.get(record.key);
    if (backing !== undefined) {
      backing.terminal = true;
      this.#maybeReleaseBacking(identity, backing);
    }
    this.#maybeFreeMaintenanceSlot(record.slotId);
  }

  #maybeReleaseBacking(
    identity: object,
    backing: BackingLeaseRecord,
  ): void {
    if (!backing.terminal || !backing.cleanupDone) {
      return;
    }
    const leases = this.#backingLeases.get(identity);
    leases?.delete(backing.leaseKey);
    if (leases?.size === 0) {
      this.#backingLeases.delete(identity);
    }
    for (const [key, cleanup] of this.#cleanupRecords) {
      if (
        cleanup.leaseKey === backing.leaseKey
        && cleanup.state === "Done"
      ) {
        this.#cleanupRecords.delete(key);
        this.#maybeFreeMaintenanceSlot(cleanup.slotId);
      }
    }
  }

  #findReleaseRecord(
    worker: bigint,
    workerEpoch: bigint,
    surface: bigint,
    leaseToken: bigint,
  ): ReleaseRecord | undefined {
    let found: ReleaseRecord | undefined;
    for (const record of this.#releaseRecords.values()) {
      if (
        record.request.worker === worker
        && record.request.workerEpoch === workerEpoch
        && record.request.surface === surface
        && record.request.leaseToken === leaseToken
      ) {
        if (found !== undefined) {
          throw new BrowserSurfaceBridgeError("InvalidRelease");
        }
        found = record;
      }
    }
    return found;
  }

  #rememberReclaimed(
    notification: BrowserSurfaceLifecycleNotification,
    event: SurfaceReclaimedEvent,
    live: LiveSurface | undefined,
  ): void {
    if (live === undefined) {
      throw new BrowserSurfaceBridgeError("InvalidRelease");
    }
    const key = leaseKey(
      notification.worker,
      notification.workerEpoch,
      notification.session,
      event.surface,
      event.lease_token,
    );
    const record: ReleaseRecord = {
      key,
      slotId: live.slotId,
      request: Object.freeze({
        worker: notification.worker,
        workerEpoch: notification.workerEpoch,
        session: notification.session,
        surface: event.surface,
        leaseToken: event.lease_token,
        reason: event.reason,
      }),
      backingIdentity: live.backingIdentity,
      state: "Terminal",
    };
    this.#releaseRecords.set(key, record);
    this.#terminalizeRelease(record);
  }

  #markEpochTerminal(worker: bigint, workerEpoch: bigint): void {
    for (const record of this.#releaseRecords.values()) {
      if (
        record.request.worker === worker
        && record.request.workerEpoch === workerEpoch
      ) {
        this.#terminalizeRelease(record);
      }
    }
  }

  #retireLocal(
    live: LiveSurface,
    removePresentation: boolean,
  ): BrowserSurfaceBridgeErrorCode[] {
    if (live.retired) {
      return [];
    }
    live.retired = true;
    const errors: BrowserSurfaceBridgeErrorCode[] = [];
    if (this.#liveById.get(live.metadata.id) === live) {
      this.#liveById.delete(live.metadata.id);
    }
    if (this.#liveByRegion.get(live.regionKey) === live) {
      this.#liveByRegion.delete(live.regionKey);
    }
    if (removePresentation && live.displayed) {
      live.displayed = false;
      try {
        this.#presentation.remove(live.metadata.id);
      } catch {
        this.#failedRemovals.set(live.leaseKey, {
          surface: live.metadata.id,
          slotId: live.slotId,
        });
        errors.push("PresentationFailure");
      }
    }
    const cleanupCode = this.#scheduleCleanup(
      live.rawResource,
      live.presentation,
      live.leaseKey,
      live.slotId,
      live.backingIdentity,
      live.transport,
    );
    if (cleanupCode !== undefined) {
      errors.push(cleanupCode);
    }
    return errors;
  }

  #retireLive(
    live: LiveSurface,
    reason: SurfaceReclaimReason,
    removePresentation: boolean,
  ): BrowserSurfaceBridgeErrorCode[] {
    const errors = this.#retireLocal(live, removePresentation);
    try {
      this.#requestRelease(
        {
          worker: live.worker,
          workerEpoch: live.workerEpoch,
          session: live.session,
          metadata: live.metadata,
        },
        reason,
        live.slotId,
        live.backingIdentity,
        live.rawResource,
      );
    } catch (error: unknown) {
      errors.push(
        error instanceof BrowserSurfaceBridgeError
          ? error.code
          : "ReleaseFailure",
      );
    }
    return errors;
  }

  #releaseSessionSurfaces(
    session: bigint,
    reason: SurfaceReclaimReason,
  ): BrowserSurfaceBridgeErrorCode[] {
    const errors: BrowserSurfaceBridgeErrorCode[] = [];
    for (const live of Array.from(this.#liveById.values())) {
      if (live.session === session) {
        errors.push(...this.#retireLive(live, reason, true));
      }
    }
    return errors;
  }

  #releaseQueuedSession(
    session: bigint,
  ): BrowserSurfaceBridgeErrorCode[] {
    const retained: CallbackWork[] = [];
    const result: MutableDrain = {
      processed: 0,
      presented: 0,
      released: 0,
      acknowledged: 0,
      reclaimed: 0,
      rejected: 0,
      errors: [],
    };
    for (const work of this.#queue.splice(0)) {
      if (
        work.kind === "Surface"
        && this.#publicationSession(work.value) === session
      ) {
        this.#rejectSurface(
          work.value,
          work.slotId,
          work.cleanupResources,
          result,
          SurfaceReclaimReason.SessionClosed,
        );
        const slot = this.#maintenanceSlots.get(work.slotId);
        if (slot !== undefined) {
          slot.queued = false;
        }
        this.#settleCompletedCleanups(work.slotId);
      } else {
        retained.push(work);
      }
    }
    this.#queue.push(...retained);
    return result.errors.map((error) => error.code);
  }

  #publicationSession(value: unknown): bigint | undefined {
    if (typeof value !== "object" || value === null) {
      return undefined;
    }
    try {
      const descriptor = Object.getOwnPropertyDescriptor(value, "session");
      return descriptor !== undefined
        && Object.prototype.hasOwnProperty.call(descriptor, "value")
        && isNonzeroU64(descriptor.value)
        ? descriptor.value
        : undefined;
    } catch {
      return undefined;
    }
  }
}

/** Worker-private Wasm memory description. */
export interface BrowserWasmLocalMemoryDescription {
  readonly worker: bigint;
  readonly workerEpoch: bigint;
  readonly memoryEpoch: number;
  readonly byteLength: bigint;
  readonly sameRealm: boolean;
}

/** Injected same-Worker Wasm glue; it never serializes a pointer or Memory. */
export interface BrowserWasmLocalMemoryAdapter {
  describe(memory: unknown): BrowserWasmLocalMemoryDescription | undefined;
  createReadOnlyView(
    memory: unknown,
    byteOffset: bigint,
    byteLength: bigint,
  ): object;
  release(view: object): void;
}

export interface BrowserWasmLocalViewRequest {
  readonly worker: bigint;
  readonly workerEpoch: bigint;
  readonly memory: unknown;
  readonly memoryEpoch: number;
  readonly byteOffset: bigint;
  readonly byteLength: bigint;
}

interface LocalView {
  readonly memory: unknown;
  readonly memoryEpoch: number;
  readonly byteOffset: bigint;
  readonly byteLength: bigint;
  readonly view: object;
}

/**
 * Same-Worker local view cache. This class belongs in Worker glue only; its API
 * exposes no numeric pointer and accepts no cross-realm Memory.
 */
export class BrowserWasmLocalViewBridge {
  readonly #worker: bigint;
  readonly #workerEpoch: bigint;
  readonly #adapter: BrowserWasmLocalMemoryAdapter;
  readonly #maximumViews: number;
  readonly #views = new Map<unknown, Map<string, LocalView>>();
  readonly #orphanedViews = new Set<object>();
  #viewCount = 0;
  #closed = false;

  constructor(
    worker: bigint,
    workerEpoch: bigint,
    adapter: BrowserWasmLocalMemoryAdapter,
    maximumViews = 64,
  ) {
    if (
      !isNonzeroU64(worker)
      || !isNonzeroU64(workerEpoch)
      || typeof adapter.describe !== "function"
      || typeof adapter.createReadOnlyView !== "function"
      || typeof adapter.release !== "function"
      || !isSafePositive(maximumViews, MAX_HOST_LIMIT)
    ) {
      throw new BrowserSurfaceBridgeError("InvalidConfiguration");
    }
    this.#worker = worker;
    this.#workerEpoch = workerEpoch;
    this.#adapter = adapter;
    this.#maximumViews = maximumViews;
  }

  acquire(request: BrowserWasmLocalViewRequest): object {
    if (this.#closed) {
      throw new BrowserSurfaceBridgeError("InvalidLifecycle");
    }
    this.#retryOrphanedViews();
    let description: BrowserWasmLocalMemoryDescription | undefined;
    try {
      description = this.#adapter.describe(request.memory);
    } catch {
      throw new BrowserSurfaceBridgeError("AdapterFailure");
    }
    if (
      description === undefined
      || !description.sameRealm
      || (
        (typeof request.memory !== "object" || request.memory === null)
        && typeof request.memory !== "function"
      )
    ) {
      throw new BrowserSurfaceBridgeError("InvalidMemory");
    }
    if (
      request.worker !== this.#worker
      || request.workerEpoch !== this.#workerEpoch
      || description.worker !== this.#worker
      || description.workerEpoch !== this.#workerEpoch
    ) {
      throw new BrowserSurfaceBridgeError("InvalidWorkerEpoch");
    }
    if (
      !isU32(request.memoryEpoch)
      || request.memoryEpoch === 0
      || !isU32(description.memoryEpoch)
      || description.memoryEpoch === 0
    ) {
      throw new BrowserSurfaceBridgeError("InvalidMemoryEpoch");
    }
    this.#dropOlderMemoryViews(
      request.memory,
      description.memoryEpoch,
    );
    if (request.memoryEpoch !== description.memoryEpoch) {
      throw new BrowserSurfaceBridgeError("InvalidMemoryEpoch");
    }
    if (
      typeof request.byteOffset !== "bigint"
      || request.byteOffset < 0n
      || typeof request.byteLength !== "bigint"
      || request.byteLength <= 0n
      || request.byteOffset > MAX_U64 - request.byteLength
      || description.byteLength < 0n
      || description.byteLength > MAX_U64
      || request.byteOffset + request.byteLength > description.byteLength
    ) {
      throw new BrowserSurfaceBridgeError("InvalidMemoryRange");
    }
    const key = [
      request.memoryEpoch,
      request.byteOffset,
      request.byteLength,
    ].join(":");
    let memoryViews = this.#views.get(request.memory);
    const cached = memoryViews?.get(key);
    if (cached !== undefined) {
      return cached.view;
    }
    if (
      this.#viewCount + this.#orphanedViews.size
      >= this.#maximumViews
    ) {
      throw new BrowserSurfaceBridgeError("SurfaceLimit");
    }
    let view: object;
    try {
      view = this.#adapter.createReadOnlyView(
        request.memory,
        request.byteOffset,
        request.byteLength,
      );
    } catch {
      throw new BrowserSurfaceBridgeError("AdapterFailure");
    }
    let after: BrowserWasmLocalMemoryDescription | undefined;
    try {
      after = this.#adapter.describe(request.memory);
    } catch {
      this.#releaseOrRemember(view);
      throw new BrowserSurfaceBridgeError("AdapterFailure");
    }
    if (
      after === undefined
      || !after.sameRealm
      || after.worker !== this.#worker
      || after.workerEpoch !== this.#workerEpoch
    ) {
      this.#releaseOrRemember(view);
      throw new BrowserSurfaceBridgeError("InvalidWorkerEpoch");
    }
    if (
      after.memoryEpoch !== request.memoryEpoch
      || !isU32(after.memoryEpoch)
      || after.memoryEpoch === 0
    ) {
      this.#releaseOrRemember(view);
      this.#dropOlderMemoryViews(request.memory, after.memoryEpoch);
      throw new BrowserSurfaceBridgeError("InvalidMemoryEpoch");
    }
    if (
      after.byteLength < 0n
      || after.byteLength > MAX_U64
      || request.byteOffset + request.byteLength > after.byteLength
    ) {
      this.#releaseOrRemember(view);
      throw new BrowserSurfaceBridgeError("InvalidMemoryRange");
    }
    if (memoryViews === undefined) {
      memoryViews = new Map();
      this.#views.set(request.memory, memoryViews);
    }
    memoryViews.set(key, {
      memory: request.memory,
      memoryEpoch: request.memoryEpoch,
      byteOffset: request.byteOffset,
      byteLength: request.byteLength,
      view,
    });
    this.#viewCount += 1;
    return view;
  }

  /** Invalidates old views after grow; a later acquire must name the new epoch. */
  memoryGrew(memory: unknown): void {
    if (this.#closed) {
      return;
    }
    let description: BrowserWasmLocalMemoryDescription | undefined;
    try {
      description = this.#adapter.describe(memory);
    } catch {
      throw new BrowserSurfaceBridgeError("AdapterFailure");
    }
    if (
      description === undefined
      || !description.sameRealm
      || description.worker !== this.#worker
      || description.workerEpoch !== this.#workerEpoch
      || !isU32(description.memoryEpoch)
      || description.memoryEpoch === 0
    ) {
      throw new BrowserSurfaceBridgeError("InvalidMemory");
    }
    this.#dropOlderMemoryViews(memory, description.memoryEpoch);
  }

  close(): void {
    if (!this.#closed) {
      this.#closed = true;
      for (const memoryViews of this.#views.values()) {
        for (const local of memoryViews.values()) {
          this.#releaseOrRemember(local.view);
        }
      }
      this.#views.clear();
      this.#viewCount = 0;
    }
    this.#retryOrphanedViews();
  }

  #dropOlderMemoryViews(memory: unknown, memoryEpoch: number): void {
    const memoryViews = this.#views.get(memory);
    if (memoryViews === undefined) {
      return;
    }
    for (const [key, local] of memoryViews) {
      if (local.memoryEpoch !== memoryEpoch) {
        memoryViews.delete(key);
        this.#viewCount -= 1;
        this.#releaseOrRemember(local.view);
      }
    }
    if (memoryViews.size === 0) {
      this.#views.delete(memory);
    }
  }

  #releaseOrRemember(view: object): void {
    try {
      this.#adapter.release(view);
      this.#orphanedViews.delete(view);
    } catch {
      this.#orphanedViews.add(view);
    }
  }

  #retryOrphanedViews(): void {
    for (const view of Array.from(this.#orphanedViews)) {
      this.#releaseOrRemember(view);
    }
  }
}
