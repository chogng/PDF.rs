import assert from "node:assert/strict";
import test from "node:test";

import {
  AlphaMode,
  CapabilityContributorKind,
  CapabilityProfileId,
  CapabilityScopeKind,
  CollectionCompleteness,
  ENGINE_ERROR_DESCRIPTORS,
  EngineErrorCode,
  NativeBackend,
  OutputProfile,
  PageCoordinateSpace,
  PageRotation,
  PixelFormat,
  QualityPolicy,
  SupportStatus,
  SurfaceCoordinateSpace,
  validateCapabilityDecision,
  validateViewportRequest,
  type CapabilityDecision,
  type EngineError,
  type PageViewport,
  type SurfaceMetadata,
  type ViewportRequest,
} from "../generated/engine-protocol.js";
import {
  BrowserViewer,
  BrowserViewerError,
  type BrowserViewerConfiguration,
  type BrowserViewerEngineClient,
  type BrowserViewerEngineHandlers,
  type BrowserViewerFailure,
  type BrowserViewerFocus,
  type BrowserViewerFocusSnapshot,
  type BrowserViewerFrameScheduler,
  type BrowserViewerHostObservations,
  type BrowserViewerInitialState,
  type BrowserViewerLimits,
  type BrowserViewerObservationHandlers,
  type BrowserViewerPresentation,
  type BrowserViewerSurface,
} from "../src/browser-viewer.js";
import type {
  BrowserWorkerFaultCode,
} from "../src/browser-worker-supervisor.js";

const DEFAULT_LIMITS: BrowserViewerLimits = Object.freeze({
  maxVisiblePages: 64,
  maxCoalescedChanges: 8,
  maxAdoptedSurfaces: 8,
});

const page = (
  pageIndex: number,
  clipX = 0,
  clipY = 0,
  clipWidth = 612_000,
  clipHeight = 792_000,
): PageViewport => Object.freeze({
  page_index: pageIndex,
  coordinate_space: PageCoordinateSpace.PdfPointsBottomLeft,
  geometry: Object.freeze({
    identity: new Uint8Array(32).fill(pageIndex + 1),
    media_box_x_milli_points: 0,
    media_box_y_milli_points: 0,
    media_box_width_milli_points: 612_000,
    media_box_height_milli_points: 792_000,
    crop_box_x_milli_points: 0,
    crop_box_y_milli_points: 0,
    crop_box_width_milli_points: 612_000,
    crop_box_height_milli_points: 792_000,
    intrinsic_rotation: PageRotation.Degrees0,
  }),
  clip_x_milli_points: clipX,
  clip_y_milli_points: clipY,
  clip_width_milli_points: clipWidth,
  clip_height_milli_points: clipHeight,
});

const initialState = (
  visiblePages: readonly PageViewport[] = [page(0)],
): BrowserViewerInitialState => Object.freeze({
  documentRevision: 1n,
  annotationRevision: 0n,
  zoomNumerator: 1,
  zoomDenominator: 1,
  visiblePages,
  quality: QualityPolicy.Preview,
  outputProfile: OutputProfile.Srgb,
  deviceScaleMilli: 1_000,
  rotation: PageRotation.Degrees0,
  optionalContentId: 0n,
});

class FakeEngineClient implements BrowserViewerEngineClient {
  handlers: BrowserViewerEngineHandlers | undefined;
  readonly viewports: ViewportRequest[] = [];
  readonly released: BrowserViewerSurface[] = [];
  closeCount = 0;

  constructor(readonly log: string[] = []) {}

  setHandlers(
    handlers: BrowserViewerEngineHandlers | undefined,
  ): void {
    this.log.push(
      handlers === undefined
        ? "client:detach"
        : "client:attach",
    );
    this.handlers = handlers;
  }

  setViewport(viewport: ViewportRequest): void {
    this.log.push(`client:viewport:${viewport.generation}`);
    this.viewports.push(viewport);
  }

  releaseSurface(surface: BrowserViewerSurface): void {
    this.log.push(`client:release:${surface.metadata.id}`);
    this.released.push(surface);
  }

  close(): void {
    this.log.push("client:close");
    this.closeCount += 1;
  }

  emitSurface(surface: BrowserViewerSurface): void {
    this.handlers?.onSurface(surface);
  }

  emitCapabilityDecision(decision: CapabilityDecision): void {
    this.handlers?.onCapabilityDecision(decision);
  }

  emitEngineError(error: EngineError): void {
    this.handlers?.onEngineError(error);
  }

  emitWorkerFault(code: BrowserWorkerFaultCode): void {
    this.handlers?.onWorkerFault(code);
  }
}

class FakeObservations implements BrowserViewerHostObservations {
  handlers: BrowserViewerObservationHandlers | undefined;
  connectCount = 0;
  disconnectCount = 0;

  constructor(readonly log: string[] = []) {}

  connect(handlers: BrowserViewerObservationHandlers): void {
    this.log.push("observations:connect");
    this.handlers = handlers;
    this.connectCount += 1;
  }

  disconnect(): void {
    this.log.push("observations:disconnect");
    this.handlers = undefined;
    this.disconnectCount += 1;
  }

  scroll(pages: readonly PageViewport[]): void {
    this.handlers?.onScroll(pages);
  }

  intersect(pages: readonly PageViewport[]): void {
    this.handlers?.onIntersection(pages);
  }

  resize(pages: readonly PageViewport[]): void {
    this.handlers?.onResize(pages);
  }

  deviceScale(scale: number): void {
    this.handlers?.onDeviceScale(scale);
  }
}

class FakeFrames implements BrowserViewerFrameScheduler {
  readonly callbacks = new Map<number, () => void>();
  readonly cancelled: number[] = [];
  #nextHandle = 1;

  constructor(readonly log: string[] = []) {}

  request(callback: () => void): number {
    const handle = this.#nextHandle;
    this.#nextHandle += 1;
    this.log.push(`frames:request:${handle}`);
    this.callbacks.set(handle, callback);
    return handle;
  }

  cancel(handle: number): void {
    this.log.push(`frames:cancel:${handle}`);
    this.callbacks.delete(handle);
    this.cancelled.push(handle);
  }

  runNext(): void {
    const entry = this.callbacks.entries().next().value as
      | [number, () => void]
      | undefined;
    assert.notEqual(entry, undefined, "one frame must be scheduled");
    if (entry === undefined) {
      return;
    }
    this.callbacks.delete(entry[0]);
    entry[1]();
  }
}

class FakeFocus implements BrowserViewerFocus {
  readonly snapshot: BrowserViewerFocusSnapshot = Object.freeze({
    token: Object.freeze({ id: "focus-token" }),
  });
  restored: BrowserViewerFocusSnapshot[] = [];

  constructor(readonly log: string[] = []) {}

  captureBeforeUnmount(): BrowserViewerFocusSnapshot {
    this.log.push("focus:capture");
    return this.snapshot;
  }

  restoreAfterUnmount(snapshot: BrowserViewerFocusSnapshot): void {
    this.log.push("focus:restore");
    this.restored.push(snapshot);
  }
}

class FakePresentation implements BrowserViewerPresentation {
  readonly presented: BrowserViewerSurface[] = [];
  readonly current = new Map<string, BrowserViewerSurface>();
  readonly failures: BrowserViewerFailure[] = [];
  clearCount = 0;
  clearFailureCount = 0;

  constructor(readonly log: string[] = []) {}

  present(surface: BrowserViewerSurface): void {
    this.log.push(`presentation:present:${surface.metadata.id}`);
    this.presented.push(surface);
    this.current.set(surfaceRegionKey(surface.metadata), surface);
  }

  clear(): void {
    this.log.push("presentation:clear");
    this.current.clear();
    this.clearCount += 1;
  }

  showFailure(failure: BrowserViewerFailure): void {
    this.log.push(`presentation:failure:${failure.source}:${failure.code}`);
    this.failures.push(failure);
  }

  clearFailure(): void {
    this.log.push("presentation:clear-failure");
    this.clearFailureCount += 1;
  }
}

interface Fixture {
  readonly viewer: BrowserViewer;
  readonly client: FakeEngineClient;
  readonly observations: FakeObservations;
  readonly frames: FakeFrames;
  readonly focus: FakeFocus;
  readonly presentation: FakePresentation;
}

const fixture = (
  limits: BrowserViewerLimits = DEFAULT_LIMITS,
  state: BrowserViewerInitialState = initialState(),
  log: string[] = [],
): Fixture => {
  const client = new FakeEngineClient(log);
  const observations = new FakeObservations(log);
  const frames = new FakeFrames(log);
  const focus = new FakeFocus(log);
  const presentation = new FakePresentation(log);
  const configuration: BrowserViewerConfiguration = {
    client,
    observations,
    frames,
    focus,
    presentation,
    limits,
    initialState: state,
  };
  return {
    viewer: new BrowserViewer(configuration),
    client,
    observations,
    frames,
    focus,
    presentation,
  };
};

const surfaceRegionKey = (metadata: SurfaceMetadata): string =>
  [
    metadata.region.page_index,
    metadata.region.x,
    metadata.region.y,
    metadata.region.width,
    metadata.region.height,
  ].join(":");

const surface = (
  id: bigint,
  generation: bigint,
  regionX = 0,
): BrowserViewerSurface => Object.freeze({
  metadata: Object.freeze({
    id,
    lease_token: id + 100n,
    owner: Object.freeze({
      worker: 1n,
      session: 1n,
    }),
    generation,
    region: Object.freeze({
      page_index: 0,
      x: regionX,
      y: 0,
      width: 2,
      height: 2,
      coordinate_space: SurfaceCoordinateSpace.DevicePixelsTopLeft,
    }),
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
    plan_hash: new Uint8Array(32).fill(2),
    scene_hash: new Uint8Array(32).fill(3),
    decision_hash: new Uint8Array(32).fill(4),
    backend: NativeBackend.FastCpu,
  }),
  resource: Object.freeze({ id }),
});

const engineError = (
  code: EngineErrorCode,
  diagnosticId = 1n,
): EngineError => {
  const descriptor = ENGINE_ERROR_DESCRIPTORS.find(
    (candidate) => candidate.code === code,
  );
  assert.notEqual(descriptor, undefined);
  if (descriptor === undefined) {
    throw new Error("test descriptor missing");
  }
  return Object.freeze({
    code,
    category: descriptor.category,
    severity: descriptor.severity,
    recoverability: descriptor.recoverability,
    diagnostic_id: diagnosticId,
  });
};

const capabilitySubject = Object.freeze({
  source: Object.freeze({
    stable_id: new Uint8Array(32).fill(1),
    revision: 1n,
  }),
  document_revision: 1n,
  revision_startxref: 10n,
  page_index: 0,
  page_object_number: 1,
  page_object_generation: 0,
  scene_schema_major: 2,
  scene_schema_minor: 0,
  scene_hash: new Uint8Array(32).fill(2),
});

const unsupportedDecision = (): CapabilityDecision => {
  const decision: CapabilityDecision = {
    decision_schema_version: 1,
    status: SupportStatus.Unsupported,
    profile: CapabilityProfileId.BaselineNative,
    profile_version: 1,
    policy_version: 1,
    subject: capabilitySubject,
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
  };
  assert.equal(validateCapabilityDecision(decision), true);
  return decision;
};

const rejectedDecision = (): CapabilityDecision => {
  const location = Object.freeze({ page_index: 0 });
  const decision: CapabilityDecision = {
    decision_schema_version: 1,
    status: SupportStatus.Rejected,
    profile: CapabilityProfileId.BaselineNative,
    profile_version: 1,
    policy_version: 1,
    subject: capabilitySubject,
    missing: [],
    missing_total: 0,
    missing_completeness: CollectionCompleteness.Complete,
    contributors: [{
      id: 1,
      kind: CapabilityContributorKind.Policy,
      code: 1,
      location,
    }],
    contributors_total: 1,
    contributors_completeness: CollectionCompleteness.Complete,
    locations_total: 1,
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
    location,
    rejection_code: 1,
  };
  assert.equal(validateCapabilityDecision(decision), true);
  return decision;
};

test("all interaction dimensions submit complete monotonic validated generations", () => {
  const {
    viewer,
    client,
    observations,
    frames,
  } = fixture();
  viewer.mount();
  frames.runNext();

  observations.scroll([page(0, 10)]);
  frames.runNext();
  assert.equal(viewer.setZoom(5, 2), "Scheduled");
  frames.runNext();
  assert.equal(
    viewer.setRotation(PageRotation.Degrees90),
    "Scheduled",
  );
  frames.runNext();
  observations.deviceScale(1_500);
  frames.runNext();
  observations.resize([page(0, 20, 30, 500_000, 700_000)]);
  frames.runNext();
  observations.intersect([page(1)]);
  frames.runNext();
  assert.equal(viewer.setOptionalContentId(17n), "Scheduled");
  frames.runNext();
  assert.equal(viewer.setDocumentRevision(2n), "Scheduled");
  frames.runNext();
  assert.equal(viewer.setAnnotationRevision(3n), "Scheduled");
  frames.runNext();
  assert.equal(viewer.setQuality(QualityPolicy.Full), "Scheduled");
  frames.runNext();
  assert.equal(
    viewer.setOutputProfile(OutputProfile.Srgb),
    "Scheduled",
  );
  frames.runNext();

  assert.deepEqual(
    client.viewports.map((viewport) => viewport.generation),
    Array.from({ length: 12 }, (_, index) => BigInt(index + 1)),
  );
  for (const viewport of client.viewports) {
    assert.equal(validateViewportRequest(viewport), true);
    assert.ok(Object.isFrozen(viewport));
    assert.ok(Object.isFrozen(viewport.visible_pages));
  }
  const current = client.viewports.at(-1);
  assert.notEqual(current, undefined);
  assert.equal(current?.document_revision, 2n);
  assert.equal(current?.annotation_revision, 3n);
  assert.equal(current?.zoom_numerator, 5);
  assert.equal(current?.zoom_denominator, 2);
  assert.equal(current?.visible_pages[0]?.page_index, 1);
  assert.equal(current?.quality, QualityPolicy.Full);
  assert.equal(current?.output_profile, OutputProfile.Srgb);
  assert.equal(current?.device_scale_milli, 1_500);
  assert.equal(current?.rotation, PageRotation.Degrees90);
  assert.equal(current?.optional_content_id, 17n);
  assert.equal(viewer.currentGeneration, 12n);
  assert.equal(viewer.currentViewport, current);
});

test("bounded coalescing, viewport validation, and surface capacity fail structurally", () => {
  const {
    viewer,
    client,
    observations,
    frames,
    presentation,
  } = fixture(Object.freeze({
    maxVisiblePages: 64,
    maxCoalescedChanges: 2,
    maxAdoptedSurfaces: 1,
  }));
  viewer.mount();
  observations.scroll([page(0, 10)]);
  observations.resize([page(0, 20)]);
  assert.equal(viewer.pendingChanges, 2);
  assert.deepEqual(viewer.failure, {
    source: "Viewer",
    kind: "Budget",
    code: "CoalescingLimit",
  });
  frames.runNext();
  assert.equal(
    client.viewports[0]?.visible_pages[0]?.clip_x_milli_points,
    10,
    "the over-limit resize must not replace the accepted scroll state",
  );

  const tooManyPages = Array.from(
    { length: 65 },
    (_, index) => page(index),
  );
  assert.equal(viewer.setVisiblePages(tooManyPages), "Rejected");
  assert.deepEqual(viewer.failure, {
    source: "Viewer",
    kind: "InvalidInput",
    code: "InvalidViewport",
  });
  assert.equal(viewer.setZoom(2, 2), "Rejected");
  assert.equal(frames.callbacks.size, 0);

  const first = surface(1n, viewer.currentGeneration, 0);
  const overLimit = surface(2n, viewer.currentGeneration, 2);
  client.emitSurface(first);
  client.emitSurface(overLimit);
  assert.deepEqual(
    presentation.presented.map((candidate) => candidate.metadata.id),
    [1n],
  );
  assert.deepEqual(
    client.released.map((candidate) => candidate.metadata.id),
    [2n],
  );
  assert.deepEqual(viewer.failure, {
    source: "Viewer",
    kind: "Budget",
    code: "SurfaceLimit",
  });
  assert.equal(viewer.adoptedSurfaceCount, 1);
});

test("three rapid generations release out-of-order stale surfaces and present only current", () => {
  const {
    viewer,
    client,
    observations,
    frames,
    presentation,
  } = fixture();
  viewer.mount();
  frames.runNext();
  viewer.setZoom(3, 2);
  frames.runNext();
  viewer.setRotation(PageRotation.Degrees180);
  frames.runNext();
  observations.deviceScale(2_000);
  frames.runNext();
  assert.equal(viewer.currentGeneration, 4n);

  const generationTwo = surface(2n, 2n);
  const generationOne = surface(1n, 1n);
  const generationFour = surface(4n, 4n);
  const generationThree = surface(3n, 3n);
  client.emitSurface(generationTwo);
  client.emitSurface(generationOne);
  client.emitSurface(generationFour);
  client.emitSurface(generationThree);

  assert.deepEqual(
    presentation.presented.map((candidate) => candidate.metadata.id),
    [4n],
  );
  assert.deepEqual(
    client.released.map((candidate) => candidate.metadata.id),
    [2n, 1n, 3n],
  );
  assert.equal(viewer.adoptedSurfaceCount, 1);

  const replacement = surface(5n, 4n);
  client.emitSurface(replacement);
  assert.deepEqual(
    presentation.presented.map((candidate) => candidate.metadata.id),
    [4n, 5n],
  );
  assert.deepEqual(
    client.released.map((candidate) => candidate.metadata.id),
    [2n, 1n, 3n, 4n],
  );
  assert.equal(viewer.adoptedSurfaceCount, 1);

  viewer.setZoom(2, 1);
  frames.runNext();
  assert.equal(viewer.currentGeneration, 5n);
  assert.equal(viewer.adoptedSurfaceCount, 0);
  assert.deepEqual(
    client.released.map((candidate) => candidate.metadata.id),
    [2n, 1n, 3n, 4n, 5n],
  );
  assert.equal(presentation.current.size, 0);
});

test("capability, engine, transport, and Worker failures map only by stable codes", () => {
  const log: string[] = [];
  const {
    viewer,
    client,
    frames,
    presentation,
  } = fixture(DEFAULT_LIMITS, initialState(), log);
  viewer.mount();
  frames.runNext();

  const unsupported = unsupportedDecision();
  client.emitCapabilityDecision(unsupported);
  assert.equal(viewer.failure?.source, "CapabilityDecision");
  assert.equal(viewer.failure?.kind, "Unsupported");
  assert.equal(viewer.failure?.code, SupportStatus.Unsupported);
  if (viewer.failure?.source === "CapabilityDecision") {
    assert.equal(viewer.failure.decision, unsupported);
  }

  const rejected = rejectedDecision();
  client.emitCapabilityDecision(rejected);
  assert.equal(viewer.failure?.source, "CapabilityDecision");
  assert.equal(viewer.failure?.kind, "Rejected");
  assert.equal(viewer.failure?.code, SupportStatus.Rejected);

  for (const [code, expectedKind] of [
    [EngineErrorCode.ResourceLimit, "Budget"],
    [EngineErrorCode.SourceChanged, "SourceIntegrity"],
    [EngineErrorCode.Cancelled, "Cancelled"],
    [EngineErrorCode.ProtocolViolation, "Transport"],
    [EngineErrorCode.Internal, "Worker"],
    [EngineErrorCode.InvalidDocument, "Document"],
  ] as const) {
    client.emitEngineError(engineError(code, BigInt(code)));
    const failure = viewer.failure as BrowserViewerFailure | undefined;
    assert.equal(failure?.source, "EngineError");
    assert.equal(failure?.kind, expectedKind);
    assert.equal(failure?.code, code);
  }

  client.emitSurface(surface(77n, viewer.currentGeneration));
  assert.equal(viewer.adoptedSurfaceCount, 1);
  log.length = 0;
  client.emitWorkerFault("WorkerMessageError");
  assert.deepEqual(log, [
    "client:release:77",
    "presentation:clear",
    "presentation:failure:WorkerFault:WorkerMessageError",
  ]);
  assert.equal(viewer.adoptedSurfaceCount, 0);
  assert.equal(presentation.current.size, 0);
  assert.deepEqual(viewer.failure, {
    source: "WorkerFault",
    kind: "Worker",
    code: "WorkerMessageError",
  });
  client.emitWorkerFault("ProtocolViolation");
  assert.deepEqual(viewer.failure, {
    source: "WorkerFault",
    kind: "Transport",
    code: "ProtocolViolation",
  });
  client.emitWorkerFault(
    "ForeignWorkerFault" as BrowserWorkerFaultCode,
  );
  assert.deepEqual(viewer.failure, {
    source: "Viewer",
    kind: "InvalidInput",
    code: "InvalidWorkerFault",
  });

  client.emitEngineError({
    ...engineError(EngineErrorCode.ResourceLimit),
    code: EngineErrorCode.SourceChanged,
  });
  assert.deepEqual(viewer.failure, {
    source: "Viewer",
    kind: "InvalidInput",
    code: "InvalidEngineError",
  });
});

test("unmount cancels host work, releases surfaces, closes ownership, and restores focus", () => {
  const log: string[] = [];
  const {
    viewer,
    client,
    observations,
    frames,
    focus,
  } = fixture(DEFAULT_LIMITS, initialState(), log);
  viewer.mount();
  frames.runNext();
  client.emitSurface(surface(1n, 1n));
  viewer.setZoom(3, 2);
  assert.equal(frames.callbacks.size, 1);
  log.length = 0;

  viewer.unmount();
  assert.deepEqual(log, [
    "focus:capture",
    "client:detach",
    "observations:disconnect",
    "frames:cancel:2",
    "client:release:1",
    "presentation:clear",
    "client:close",
    "focus:restore",
  ]);
  assert.equal(viewer.lifecycle, "Unmounted");
  assert.equal(viewer.pendingChanges, 0);
  assert.equal(viewer.adoptedSurfaceCount, 0);
  assert.equal(client.handlers, undefined);
  assert.equal(client.closeCount, 1);
  assert.equal(observations.disconnectCount, 1);
  assert.deepEqual(frames.cancelled, [2]);
  assert.deepEqual(focus.restored, [focus.snapshot]);

  viewer.unmount();
  assert.equal(client.closeCount, 1);
  assert.equal(observations.disconnectCount, 1);
  assert.deepEqual(focus.restored, [focus.snapshot]);
});

test("construction rejects invalid bounds and an invalid complete initial viewport", () => {
  const valid = fixture();
  const base: BrowserViewerConfiguration = {
    client: valid.client,
    observations: valid.observations,
    frames: valid.frames,
    focus: valid.focus,
    presentation: valid.presentation,
    limits: DEFAULT_LIMITS,
    initialState: initialState(),
  };
  for (const limits of [
    {
      ...DEFAULT_LIMITS,
      maxVisiblePages: 0,
    },
    {
      ...DEFAULT_LIMITS,
      maxCoalescedChanges: 4_097,
    },
    {
      ...DEFAULT_LIMITS,
      maxAdoptedSurfaces: Number.POSITIVE_INFINITY,
    },
  ]) {
    assert.throws(
      () => new BrowserViewer({ ...base, limits }),
      (error: unknown) =>
        error instanceof BrowserViewerError
        && error.code === "InvalidConfiguration"
        && error.message === "InvalidConfiguration",
    );
  }
  assert.throws(
    () =>
      new BrowserViewer({
        ...base,
        initialState: initialState(
          Array.from({ length: 65 }, (_, index) => page(index)),
        ),
      }),
    (error: unknown) =>
      error instanceof BrowserViewerError
      && error.code === "InvalidConfiguration",
  );
  assert.throws(
    () =>
      new BrowserViewer({
        ...base,
        initialState: {
          ...initialState(),
          zoomNumerator: 2,
          zoomDenominator: 2,
        },
      }),
    (error: unknown) =>
      error instanceof BrowserViewerError
      && error.code === "InvalidConfiguration",
  );
});
