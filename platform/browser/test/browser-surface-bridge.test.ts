import assert from "node:assert/strict";
import test from "node:test";

import {
  AlphaMode,
  EndpointCapability,
  EngineExecutionCapability,
  NativeBackend,
  OperationAckStatus,
  PixelFormat,
  SurfaceCoordinateSpace,
  SurfaceReclaimReason,
  type SurfaceMetadata,
  type SurfaceReadyEvent,
  type SurfaceRegion,
} from "../generated/engine-protocol.js";
import {
  BrowserSurfaceBridge,
  BrowserSurfaceBridgeError,
  BrowserSurfacePublicationValidator,
  BrowserWasmLocalViewBridge,
  evaluateBrowserSurfaceCapabilities,
  type BrowserArrayBufferDescription,
  type BrowserImageBitmapDescription,
  type BrowserSharedArrayBufferDescription,
  type BrowserSurfaceAdapters,
  type BrowserSurfaceBridgeErrorCode,
  type BrowserSurfaceCapabilityEnvironment,
  type BrowserSurfaceEpoch,
  type BrowserSurfaceGeneration,
  type BrowserSurfaceLimits,
  type BrowserSurfacePresentationSink,
  type BrowserSurfaceReleaseDisposition,
  type BrowserSurfaceReleaseRequest,
  type BrowserSurfaceReleaseSink,
  type BrowserSurfaceRuntimeSupport,
  type BrowserWasmLocalMemoryAdapter,
  type BrowserWasmLocalMemoryDescription,
  type BrowserWasmLocalViewRequest,
} from "../src/browser-surface-bridge.js";

const LIMITS: BrowserSurfaceLimits = Object.freeze({
  maxQueuedCallbacks: 16,
  maxLiveSurfaces: 4,
  maxTrackedLeases: 16,
  maxSessionsPerEpoch: 4,
  maxWorkerEpochs: 8,
  maxPlanRegions: 8,
  maxSurfaceDimension: 4_096,
  maxSurfaceStrideBytes: 1_048_576,
  maxSurfaceBytes: 16_777_216n,
});

const REGION: SurfaceRegion = Object.freeze({
  page_index: 0,
  x: 0,
  y: 0,
  width: 2,
  height: 1,
  coordinate_space: SurfaceCoordinateSpace.DevicePixelsTopLeft,
});

const hash = (byte: number): Uint8Array =>
  new Uint8Array(32).fill(byte);

const generation = (
  generationId: bigint = 3n,
  session: bigint = 2n,
  worker: bigint = 1n,
  workerEpoch: bigint = 11n,
): BrowserSurfaceGeneration => ({
  worker,
  workerEpoch,
  session,
  generation: generationId,
  identity: {
    renderConfig: hash(1),
    rendererEpoch: 9,
    planId: 21n,
    planHash: hash(2),
    sceneHash: hash(3),
    decisionHash: hash(4),
    backend: NativeBackend.ReferenceCpu,
    format: PixelFormat.Rgba8,
    alpha: AlphaMode.Straight,
  },
  regions: [REGION],
});

const runtimeSupport = (
  mask: number = 0b111,
  offscreenCanvasStaging = false,
): BrowserSurfaceRuntimeSupport => ({
  imageBitmap: (mask & 0b001) !== 0,
  arrayBuffer: (mask & 0b010) !== 0,
  sharedArrayBuffer: (mask & 0b100) !== 0,
  offscreenCanvasStaging,
});

const capabilitiesForMask = (mask: number): bigint =>
  ((mask & 0b001) !== 0
    ? EndpointCapability.TransferableImageBitmap
    : 0n)
  | ((mask & 0b010) !== 0
    ? EndpointCapability.TransferableArrayBuffer
    : 0n)
  | ((mask & 0b100) !== 0
    ? EndpointCapability.SharedArrayBuffer
    : 0n);

const epoch = (
  endpointCapabilities: bigint =
    EndpointCapability.TransferableImageBitmap
    | EndpointCapability.TransferableArrayBuffer
    | EndpointCapability.SharedArrayBuffer,
  crossOriginIsolated = true,
  support: BrowserSurfaceRuntimeSupport = runtimeSupport(),
): BrowserSurfaceEpoch => ({
  worker: 1n,
  workerEpoch: 11n,
  endpointCapabilities,
  executionCapabilities:
    EngineExecutionCapability.OffscreenCanvasStaging,
  crossOriginIsolated,
  runtimeSupport: support,
});

type FakeResourceKind =
  | "bitmap"
  | "array"
  | "shared"
  | "memory"
  | "offscreen";

interface FakeResource {
  readonly kind: FakeResourceKind;
  width?: number;
  height?: number;
  open?: boolean;
  transferable?: boolean;
  byteLength?: bigint;
  maximumByteLength?: bigint;
  growable?: boolean;
  exclusive?: boolean;
  publicationEpoch?: number;
  memoryEpoch?: number;
  worker?: bigint;
  workerEpoch?: bigint;
  sameRealm?: boolean;
  backingIdentity?: object;
  receiverOwned?: boolean;
  cleanupFailures?: number;
  closes: number;
  releases: number;
}

const bitmap = (width = 2, height = 1): FakeResource => ({
  kind: "bitmap",
  width,
  height,
  open: true,
  transferable: true,
  closes: 0,
  releases: 0,
});

const arrayBuffer = (byteLength = 12n): FakeResource => ({
  kind: "array",
  byteLength,
  maximumByteLength: byteLength,
  growable: false,
  exclusive: true,
  closes: 0,
  releases: 0,
});

const sharedBuffer = (
  byteLength = 16n,
  publicationEpoch = 7,
): FakeResource => ({
  kind: "shared",
  byteLength,
  maximumByteLength: byteLength,
  growable: false,
  publicationEpoch,
  closes: 0,
  releases: 0,
});

interface FakeAdapterBundle {
  readonly adapters: BrowserSurfaceAdapters;
  readonly log: string[];
}

const fakeAdapters = (
  name: string,
  sharedLoads: number[] = [],
): FakeAdapterBundle => {
  const log: string[] = [];
  const adapters: BrowserSurfaceAdapters = {
    imageBitmap: {
      isResource: (value: unknown): boolean =>
        (value as Partial<FakeResource> | null)?.kind === "bitmap",
      describe: (value: unknown): BrowserImageBitmapDescription | undefined => {
        log.push(`${name}:bitmap:describe`);
        const resource = value as FakeResource;
        return resource.kind === "bitmap"
          ? {
              width: resource.width ?? 0,
              height: resource.height ?? 0,
              open: resource.open === true,
              transferable: resource.transferable === true,
              backingIdentity: resource.backingIdentity ?? resource,
              receiverOwned: resource.receiverOwned !== false,
            }
          : undefined;
      },
      adopt: (value: unknown): object => {
        log.push(`${name}:bitmap:adopt`);
        return { source: value, mode: "bitmap" };
      },
      close: (value: unknown): void => {
        log.push(`${name}:bitmap:close`);
        const resource = value as FakeResource;
        if ((resource.cleanupFailures ?? 0) > 0) {
          resource.cleanupFailures = (resource.cleanupFailures ?? 0) - 1;
          throw new Error("injected cleanup failure");
        }
        resource.closes += 1;
      },
    },
    arrayBuffer: {
      isResource: (value: unknown): boolean =>
        (value as Partial<FakeResource> | null)?.kind === "array",
      describe: (value: unknown): BrowserArrayBufferDescription | undefined => {
        log.push(`${name}:array:describe`);
        const resource = value as FakeResource;
        return resource.kind === "array"
          ? {
              byteLength: resource.byteLength ?? 0n,
              fixedLength: resource.growable === false,
              exclusive: resource.exclusive === true,
              backingIdentity: resource.backingIdentity ?? resource,
              receiverOwned: resource.receiverOwned !== false,
            }
          : undefined;
      },
      adoptReadOnly: (
        value: unknown,
        byteOffset: bigint,
        byteLength: bigint,
      ): object => {
        log.push(`${name}:array:adopt:${byteOffset}:${byteLength}`);
        return { source: value, byteOffset, byteLength, readOnly: true };
      },
      release: (value: unknown): void => {
        log.push(`${name}:array:release`);
        const resource = value as FakeResource;
        if ((resource.cleanupFailures ?? 0) > 0) {
          resource.cleanupFailures = (resource.cleanupFailures ?? 0) - 1;
          throw new Error("injected cleanup failure");
        }
        resource.releases += 1;
      },
    },
    sharedArrayBuffer: {
      isResource: (value: unknown): boolean =>
        (value as Partial<FakeResource> | null)?.kind === "shared",
      describe: (
        value: unknown,
      ): BrowserSharedArrayBufferDescription | undefined => {
        log.push(`${name}:shared:describe`);
        const resource = value as FakeResource;
        return resource.kind === "shared"
          ? {
              byteLength: resource.byteLength ?? 0n,
              maximumByteLength: resource.maximumByteLength ?? 0n,
              growable: resource.growable === true,
              backingIdentity: resource.backingIdentity ?? resource,
            }
          : undefined;
      },
      loadPublicationEpoch: (value: unknown): number => {
        const resource = value as FakeResource;
        const loaded = sharedLoads.length === 0
          ? resource.publicationEpoch ?? 0
          : sharedLoads.shift() ?? 0;
        log.push(`${name}:shared:load:${loaded}`);
        return loaded;
      },
      adoptReadOnly: (
        value: unknown,
        byteOffset: bigint,
        byteLength: bigint,
      ): object => {
        log.push(`${name}:shared:adopt:${byteOffset}:${byteLength}`);
        return { source: value, byteOffset, byteLength, readOnly: true };
      },
      release: (value: unknown): void => {
        log.push(`${name}:shared:release`);
        const resource = value as FakeResource;
        if ((resource.cleanupFailures ?? 0) > 0) {
          resource.cleanupFailures = (resource.cleanupFailures ?? 0) - 1;
          throw new Error("injected cleanup failure");
        }
        resource.releases += 1;
      },
    },
    wasmMemory: {
      isMemory: (value: unknown): boolean =>
        (value as Partial<FakeResource> | null)?.kind === "memory",
    },
  };
  return { adapters, log };
};

class FakePresentation implements BrowserSurfacePresentationSink {
  readonly log: string[] = [];
  readonly committed: bigint[] = [];
  readonly removed: bigint[] = [];
  readonly aborted: bigint[] = [];
  readonly removeFailures = new Map<bigint, number>();
  onStage: (() => void) | undefined;

  stage(surface: { readonly metadata: SurfaceMetadata }): {
    commit(): void;
    abort(): void;
  } {
    const id = surface.metadata.id;
    this.log.push(`stage:${id}`);
    this.onStage?.();
    return {
      commit: (): void => {
        this.log.push(`commit:${id}`);
        this.committed.push(id);
      },
      abort: (): void => {
        this.log.push(`abort:${id}`);
        this.aborted.push(id);
      },
    };
  }

  remove(surface: bigint): void {
    this.log.push(`remove:${surface}`);
    const failures = this.removeFailures.get(surface) ?? 0;
    if (failures > 0) {
      this.removeFailures.set(surface, failures - 1);
      throw new Error("injected presentation removal failure");
    }
    this.removed.push(surface);
  }
}

class FakeReleases implements BrowserSurfaceReleaseSink {
  readonly requests: BrowserSurfaceReleaseRequest[] = [];
  disposition: BrowserSurfaceReleaseDisposition = "Queued";
  readonly dispositions =
    new Map<bigint, BrowserSurfaceReleaseDisposition>();
  readonly failures = new Map<bigint, number>();
  onRequest:
    | ((request: BrowserSurfaceReleaseRequest) => void)
    | undefined;

  requestRelease(
    request: BrowserSurfaceReleaseRequest,
  ): BrowserSurfaceReleaseDisposition {
    this.requests.push(request);
    const failures = this.failures.get(request.surface) ?? 0;
    if (failures > 0) {
      this.failures.set(request.surface, failures - 1);
      throw new Error("injected release failure");
    }
    this.onRequest?.(request);
    return this.dispositions.get(request.surface) ?? this.disposition;
  }
}

const metadata = (
  id: bigint,
  surfaceGeneration: bigint,
  alpha: AlphaMode,
  byteOffset: bigint,
  byteLength: bigint,
  bufferStride: number,
  worker = 1n,
  session = 2n,
): SurfaceMetadata => ({
  id,
  lease_token: id + 100n,
  owner: {
    worker,
    session,
  },
  generation: surfaceGeneration,
  region: { ...REGION },
  width: 2,
  height: 1,
  stride: bufferStride,
  format: PixelFormat.Rgba8,
  alpha,
  byte_offset: byteOffset,
  byte_length: byteLength,
  render_config: hash(1),
  renderer_epoch: 9,
  plan_id: 21n,
  plan_hash: hash(2),
  scene_hash: hash(3),
  decision_hash: hash(4),
  backend: NativeBackend.ReferenceCpu,
});

type FixtureTransport = "bitmap" | "array" | "shared";

const surfaceEvent = (
  kind: FixtureTransport,
  id = 1n,
  surfaceGeneration = 3n,
  worker = 1n,
  session = 2n,
): SurfaceReadyEvent => {
  switch (kind) {
    case "bitmap":
      return {
        metadata: metadata(
          id,
          surfaceGeneration,
          AlphaMode.Premultiplied,
          0n,
          8n,
          8,
          worker,
          session,
        ),
        transport: {
          kind: "BrowserImageBitmap",
          slot: 0,
          width: 2,
          height: 1,
        },
      };
    case "array":
      return {
        metadata: metadata(
          id,
          surfaceGeneration,
          AlphaMode.Straight,
          4n,
          8n,
          8,
          worker,
          session,
        ),
        transport: {
          kind: "BrowserArrayBuffer",
          slot: 0,
          buffer_length: 12n,
        },
      };
    case "shared":
      return {
        metadata: metadata(
          id,
          surfaceGeneration,
          AlphaMode.Straight,
          8n,
          8n,
          8,
          worker,
          session,
        ),
        transport: {
          kind: "BrowserSharedArrayBuffer",
          attachment_slot: 0,
          buffer_length: 16n,
          fence_byte_offset: 0n,
          publication_epoch: 7,
        },
      };
  }
};

const publication = (
  surface: SurfaceReadyEvent,
  resource: unknown,
  workerEpoch = 11n,
): {
  worker: bigint;
  workerEpoch: bigint;
  session: bigint;
  generation: bigint;
  surface: SurfaceReadyEvent;
  resources: unknown[];
} => ({
  worker: surface.metadata.owner.worker,
  workerEpoch,
  session: surface.metadata.owner.session,
  generation: surface.metadata.generation,
  surface,
  resources: [resource],
});

interface BridgeFixture {
  readonly bridge: BrowserSurfaceBridge;
  readonly adapters: FakeAdapterBundle;
  readonly presentation: FakePresentation;
  readonly releases: FakeReleases;
}

const bridgeFixture = (
  epochValue: BrowserSurfaceEpoch = epoch(),
  adapterBundle: FakeAdapterBundle = fakeAdapters("consumer"),
  limits: BrowserSurfaceLimits = LIMITS,
): BridgeFixture => {
  const presentation = new FakePresentation();
  const releases = new FakeReleases();
  return {
    bridge: new BrowserSurfaceBridge({
      limits,
      adapters: adapterBundle.adapters,
      presentation,
      releases,
      epoch: epochValue,
    }),
    adapters: adapterBundle,
    presentation,
    releases,
  };
};

const assertCode = (
  operation: () => unknown,
  code: BrowserSurfaceBridgeErrorCode,
): void => {
  assert.throws(
    operation,
    (error: unknown) =>
      error instanceof BrowserSurfaceBridgeError
      && error.code === code
      && error.message === code,
  );
};

test("capability matrix is exhaustive, ImageBitmap-first, and policy-neutral", () => {
  const pdfDecision = Object.freeze({
    status: "native-policy-result",
    hash: hash(9),
  });
  for (let negotiated = 0; negotiated < 8; negotiated += 1) {
    for (let runtime = 0; runtime < 8; runtime += 1) {
      for (const isolated of [false, true]) {
        const environment: BrowserSurfaceCapabilityEnvironment = {
          endpointCapabilities: capabilitiesForMask(negotiated),
          executionCapabilities: 0n,
          crossOriginIsolated: isolated,
          runtimeSupport: runtimeSupport(runtime),
        };
        const result = evaluateBrowserSurfaceCapabilities(
          environment,
          pdfDecision,
        );
        const expected: string[] = [];
        if ((negotiated & runtime & 0b001) !== 0) {
          expected.push("BrowserImageBitmap");
        }
        if (isolated && (negotiated & runtime & 0b100) !== 0) {
          expected.push("BrowserSharedArrayBuffer");
        }
        if ((negotiated & runtime & 0b010) !== 0) {
          expected.push("BrowserArrayBuffer");
        }
        assert.deepEqual(result.supportedTransports, expected);
        assert.equal(result.preferredTransport, expected[0]);
        assert.equal(result.pdfCapabilityDecision, pdfDecision);
      }
    }
  }

  for (const negotiated of [false, true]) {
    for (const runtime of [false, true]) {
      const result = evaluateBrowserSurfaceCapabilities(
        {
          endpointCapabilities:
            EndpointCapability.TransferableArrayBuffer,
          executionCapabilities: negotiated
            ? EngineExecutionCapability.OffscreenCanvasStaging
            : 0n,
          crossOriginIsolated: false,
          runtimeSupport: runtimeSupport(0b010, runtime),
        },
        pdfDecision,
      );
      assert.equal(
        result.workerPrivateOffscreenCanvasStaging,
        negotiated && runtime,
      );
      assert.deepEqual(
        result.supportedTransports,
        ["BrowserArrayBuffer"],
      );
    }
  }
});

test("producer and consumer independently validate before callback drain adopts", () => {
  const bytes = arrayBuffer();
  const message = publication(surfaceEvent("array"), bytes);
  const producer = fakeAdapters("producer");
  const validator = new BrowserSurfacePublicationValidator(
    LIMITS,
    producer.adapters,
  );
  validator.validate(message, epoch(), generation());
  assert.deepEqual(producer.log, ["producer:array:describe"]);

  const fixture = bridgeFixture();
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.enqueueSurfaceReady(message);
  assert.equal(fixture.bridge.queuedCallbacks, 1);
  assert.deepEqual(fixture.adapters.log, []);
  assert.deepEqual(fixture.presentation.log, []);
  assert.deepEqual(fixture.releases.requests, []);

  const result = fixture.bridge.drain();
  assert.equal(result.presented, 1);
  assert.equal(result.rejected, 0);
  assert.deepEqual(fixture.adapters.log, [
    "consumer:array:describe",
    "consumer:array:adopt:4:8",
  ]);
  assert.deepEqual(fixture.presentation.log, ["stage:1", "commit:1"]);
});

test("owner, epoch, session, generation, renderer, config, layout, slot, and resource are exact", () => {
  const bundle = fakeAdapters("validator");
  const validator = new BrowserSurfacePublicationValidator(
    LIMITS,
    bundle.adapters,
  );
  const validate = (
    value: unknown,
    expected: BrowserSurfaceGeneration = generation(),
  ): void => {
    validator.validate(value, epoch(), expected);
  };

  const ownerMismatch = publication(surfaceEvent("array"), arrayBuffer());
  ownerMismatch.worker = 9n;
  assertCode(
    () => validate(ownerMismatch),
    "InvalidSurfaceOwner",
  );

  assertCode(
    () => validate(
      publication(surfaceEvent("array"), arrayBuffer(), 12n),
    ),
    "StaleWorker",
  );

  assertCode(
    () => validate(
      publication(surfaceEvent("array", 1n, 3n, 1n, 8n), arrayBuffer()),
    ),
    "InvalidSession",
  );

  assertCode(
    () => validate(
      publication(surfaceEvent("array", 1n, 4n), arrayBuffer()),
    ),
    "StaleGeneration",
  );

  const renderer = surfaceEvent("array");
  renderer.metadata.renderer_epoch = 10;
  assertCode(
    () => validate(publication(renderer, arrayBuffer())),
    "InvalidRendererEpoch",
  );

  const config = surfaceEvent("array");
  config.metadata.render_config = hash(8);
  assertCode(
    () => validate(publication(config, arrayBuffer())),
    "InvalidRenderConfig",
  );

  const plan = surfaceEvent("array");
  plan.metadata.decision_hash = hash(8);
  assertCode(
    () => validate(publication(plan, arrayBuffer())),
    "InvalidPlanIdentity",
  );

  const badStride = surfaceEvent("array");
  badStride.metadata.stride = 4;
  badStride.metadata.byte_length = 4n;
  assertCode(
    () => validate(publication(badStride, arrayBuffer())),
    "InvalidPublication",
  );

  const badAlpha = surfaceEvent("array");
  badAlpha.metadata.alpha = AlphaMode.Premultiplied;
  assertCode(
    () => validate(publication(badAlpha, arrayBuffer())),
    "InvalidPublication",
  );

  const badRange = surfaceEvent("array");
  badRange.metadata.byte_offset = 8n;
  assertCode(
    () => validate(publication(badRange, arrayBuffer())),
    "InvalidPublication",
  );

  const badSlot = surfaceEvent("array");
  if (badSlot.transport.kind !== "BrowserArrayBuffer") {
    throw new Error("fixture transport");
  }
  badSlot.transport.slot = 1;
  assertCode(
    () => validate(publication(badSlot, arrayBuffer())),
    "InvalidSurfaceSlot",
  );

  assertCode(
    () => validate(publication(surfaceEvent("array"), arrayBuffer(13n))),
    "InvalidResourceExtent",
  );
  const nonExclusive = arrayBuffer();
  nonExclusive.receiverOwned = false;
  assertCode(
    () => validate(publication(surfaceEvent("array"), nonExclusive)),
    "InvalidResourceExtent",
  );
  assertCode(
    () => validate(publication(surfaceEvent("bitmap"), bitmap(3, 1))),
    "InvalidResourceExtent",
  );

  const wasmMemory: FakeResource = {
    kind: "memory",
    closes: 0,
    releases: 0,
  };
  assertCode(
    () => validate(publication(surfaceEvent("array"), wasmMemory)),
    "InvalidResourceType",
  );
});

test("transport capability, runtime support, and isolation fail closed", () => {
  const bitmapValidator = new BrowserSurfacePublicationValidator(
    LIMITS,
    fakeAdapters("bitmap").adapters,
  );
  assertCode(
    () => bitmapValidator.validate(
      publication(surfaceEvent("bitmap"), bitmap()),
      epoch(EndpointCapability.TransferableArrayBuffer),
      {
        ...generation(),
        identity: {
          ...generation().identity,
          alpha: AlphaMode.Premultiplied,
        },
      },
    ),
    "MissingEndpointCapability",
  );

  const sharedValidator = new BrowserSurfacePublicationValidator(
    LIMITS,
    fakeAdapters("shared").adapters,
  );
  assertCode(
    () => sharedValidator.validate(
      publication(surfaceEvent("shared"), sharedBuffer()),
      epoch(
        EndpointCapability.SharedArrayBuffer,
        false,
        runtimeSupport(0b100),
      ),
      generation(),
    ),
    "CrossOriginIsolationRequired",
  );

  assertCode(
    () => bitmapValidator.validate(
      publication(surfaceEvent("array"), arrayBuffer()),
      epoch(
        EndpointCapability.TransferableArrayBuffer,
        true,
        runtimeSupport(0),
      ),
      generation(),
    ),
    "UnsupportedTransport",
  );
});

test("fenced shared presentation commits only after matching acquire loads", () => {
  const stableAdapters = fakeAdapters("stable", [7, 7, 7]);
  const stable = bridgeFixture(
    epoch(
      EndpointCapability.SharedArrayBuffer,
      true,
      runtimeSupport(0b100),
    ),
    stableAdapters,
  );
  stable.bridge.activateGeneration(generation());
  stable.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("shared"), sharedBuffer()),
  );
  const accepted = stable.bridge.drain();
  assert.equal(accepted.presented, 1);
  assert.deepEqual(stableAdapters.log, [
    "stable:shared:describe",
    "stable:shared:load:7",
    "stable:shared:load:7",
    "stable:shared:adopt:8:8",
    "stable:shared:load:7",
  ]);
  assert.deepEqual(stable.presentation.log, ["stage:1", "commit:1"]);

  const changedResource = sharedBuffer();
  const changedAdapters = fakeAdapters("changed", [7, 7, 8]);
  const changed = bridgeFixture(
    epoch(
      EndpointCapability.SharedArrayBuffer,
      true,
      runtimeSupport(0b100),
    ),
    changedAdapters,
  );
  changed.bridge.activateGeneration(generation());
  changed.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("shared"), changedResource),
  );
  const rejected = changed.bridge.drain();
  assert.deepEqual(
    rejected.errors.map((error) => error.code),
    ["SharedPublicationChanged"],
  );
  assert.equal(rejected.presented, 0);
  assert.deepEqual(changed.presentation.committed, []);
  assert.deepEqual(changed.presentation.aborted, [1n]);
  assert.equal(changedResource.releases, 1);
  assert.equal(changed.releases.requests.length, 1);
});

test("stale Surface never stages but always closes and requests release", () => {
  const staleBitmap = bitmap();
  const fixture = bridgeFixture();
  fixture.bridge.activateGeneration(generation(4n));
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("bitmap", 1n, 3n), staleBitmap),
  );
  const result = fixture.bridge.drain();

  assert.deepEqual(
    result.errors.map((error) => error.code),
    ["StaleGeneration"],
  );
  assert.equal(result.presented, 0);
  assert.deepEqual(fixture.presentation.log, []);
  assert.equal(staleBitmap.closes, 1);
  assert.equal(fixture.releases.requests.length, 1);
  assert.equal(
    fixture.releases.requests[0]?.surface,
    1n,
  );

  fixture.bridge.enqueueLifecycle({
    worker: 1n,
    workerEpoch: 11n,
    session: 2n,
    event: {
      surface: 1n,
      lease_token: 101n,
      status: OperationAckStatus.Applied,
    },
  });
  fixture.bridge.enqueueLifecycle({
    worker: 1n,
    workerEpoch: 11n,
    session: 2n,
    event: {
      surface: 1n,
      lease_token: 101n,
      status: OperationAckStatus.AlreadyApplied,
    },
  });
  const ack = fixture.bridge.drain();
  assert.equal(ack.acknowledged, 1);
  assert.equal(staleBitmap.closes, 1);
});

test("replacement, duplicate release, bitmap close, and reclaim are idempotent", () => {
  const first = bitmap();
  const second = bitmap();
  const fixture = bridgeFixture();
  fixture.bridge.activateGeneration({
    ...generation(),
    identity: {
      ...generation().identity,
      alpha: AlphaMode.Premultiplied,
    },
  });

  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("bitmap", 1n), first),
  );
  assert.equal(fixture.bridge.drain().presented, 1);
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("bitmap", 2n), second),
  );
  const replacement = fixture.bridge.drain();
  assert.equal(replacement.presented, 1);
  assert.equal(replacement.released, 1);
  assert.equal(first.closes, 1);
  assert.deepEqual(fixture.presentation.committed, [1n, 2n]);
  assert.deepEqual(fixture.presentation.removed, []);

  fixture.bridge.releaseSurface(2n, 102n);
  fixture.bridge.releaseSurface(2n, 102n);
  assert.equal(second.closes, 1);
  assert.deepEqual(fixture.presentation.removed, [2n]);
  assert.equal(fixture.releases.requests.length, 2);

  const third = bitmap();
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("bitmap", 3n), third),
  );
  assert.equal(fixture.bridge.drain().presented, 1);
  fixture.bridge.enqueueLifecycle({
    worker: 1n,
    workerEpoch: 11n,
    session: 2n,
    event: {
      surface: 3n,
      lease_token: 103n,
      reason: SurfaceReclaimReason.MemoryPressure,
    },
  });
  fixture.bridge.enqueueLifecycle({
    worker: 1n,
    workerEpoch: 11n,
    session: 2n,
    event: {
      surface: 3n,
      lease_token: 103n,
      reason: SurfaceReclaimReason.MemoryPressure,
    },
  });
  const reclaimed = fixture.bridge.drain();
  assert.equal(reclaimed.reclaimed, 1);
  assert.equal(third.closes, 1);
});

test("replacement identity is scoped by Worker epoch, session, and generation", () => {
  const first = arrayBuffer();
  const second = arrayBuffer();
  const fixture = bridgeFixture();
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.activateGeneration(generation(3n, 8n));
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 1n), first),
  );
  fixture.bridge.enqueueSurfaceReady(
    publication(
      surfaceEvent("array", 2n, 3n, 1n, 8n),
      second,
    ),
  );
  const result = fixture.bridge.drain();
  assert.equal(result.presented, 2);
  assert.equal(result.released, 0);
  assert.equal(fixture.bridge.liveSurfaces, 2);

  fixture.bridge.closeSession(2n);
  assert.equal(first.releases, 1);
  assert.equal(second.releases, 0);
  assert.equal(fixture.bridge.liveSurfaces, 1);
  assert.equal(fixture.releases.requests.length, 1);
});

test("session close and Worker fault clean live and queued resources once", () => {
  const sessionBitmap = bitmap();
  const session = bridgeFixture();
  session.bridge.activateGeneration({
    ...generation(),
    identity: {
      ...generation().identity,
      alpha: AlphaMode.Premultiplied,
    },
  });
  session.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("bitmap"), sessionBitmap),
  );
  session.bridge.drain();
  session.bridge.closeSession(2n);
  session.bridge.closeSession(2n);
  assert.equal(sessionBitmap.closes, 1);
  assert.equal(session.releases.requests.length, 1);
  assert.equal(session.bridge.liveSurfaces, 0);

  const liveBitmap = bitmap();
  const queuedBitmap = bitmap();
  const fault = bridgeFixture();
  fault.releases.disposition = "WorkerTerminal";
  fault.bridge.activateGeneration({
    ...generation(),
    identity: {
      ...generation().identity,
      alpha: AlphaMode.Premultiplied,
    },
  });
  fault.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("bitmap", 1n), liveBitmap),
  );
  fault.bridge.drain();
  fault.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("bitmap", 2n), queuedBitmap),
  );
  fault.bridge.workerFault(1n, 11n);
  fault.bridge.workerFault(1n, 11n);
  assert.equal(liveBitmap.closes, 1);
  assert.equal(queuedBitmap.closes, 1);
  assert.equal(fault.releases.requests.length, 2);
  assert.equal(fault.bridge.queuedCallbacks, 0);
  assert.equal(fault.bridge.liveSurfaces, 0);

  fault.bridge.startEpoch({
    ...epoch(),
    worker: 4n,
    workerEpoch: 12n,
  });
  assertCode(
    () => fault.bridge.workerFault(1n, 11n),
    "InvalidWorkerEpoch",
  );
});

test("OffscreenCanvas remains Worker-private and cannot be a DOM Surface", () => {
  const offscreen: FakeResource = {
    kind: "offscreen",
    closes: 0,
    releases: 0,
  };
  const fixture = bridgeFixture(
    epoch(
      EndpointCapability.TransferableArrayBuffer,
      false,
      runtimeSupport(0b010, true),
    ),
  );
  assert.equal(fixture.bridge.workerPrivateOffscreenCanvasStaging, true);
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array"), offscreen),
  );
  assert.deepEqual(fixture.presentation.log, []);
  const result = fixture.bridge.drain();
  assert.deepEqual(
    result.errors.map((error) => error.code),
    ["InvalidResourceType"],
  );
  assert.deepEqual(fixture.presentation.log, []);
  assert.equal(fixture.releases.requests.length, 1);
});

test("queue pressure and malformed lifecycle use stable codes only", () => {
  const fixture = bridgeFixture(
    epoch(),
    fakeAdapters("queue"),
    {
      ...LIMITS,
      maxQueuedCallbacks: 1,
    },
  );
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array"), arrayBuffer()),
  );
  assertCode(
    () => fixture.bridge.enqueueLifecycle({}),
    "QueueFull",
  );
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.drain();
  fixture.bridge.enqueueLifecycle({
    worker: 1n,
    workerEpoch: 11n,
    session: 2n,
    event: {
      surface: 9n,
      lease_token: 10n,
      status: 99,
    },
  });
  const invalid = fixture.bridge.drain();
  assert.deepEqual(
    invalid.errors.map((error) => error.code),
    ["InvalidRelease"],
  );
  assert.equal(invalid.errors[0]?.code.includes("99"), false);
});

test("drain rejects reentry and rechecks generation immediately before commit", () => {
  const bytes = arrayBuffer();
  const fixture = bridgeFixture();
  fixture.bridge.activateGeneration(generation());
  fixture.presentation.onStage = (): void => {
    assertCode(
      () => fixture.bridge.drain(),
      "InvalidLifecycle",
    );
    fixture.bridge.activateGeneration(generation(4n));
  };
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array"), bytes),
  );

  const result = fixture.bridge.drain();
  assert.deepEqual(
    result.errors.map((error) => error.code),
    ["StaleGeneration"],
  );
  assert.deepEqual(fixture.presentation.committed, []);
  assert.deepEqual(fixture.presentation.aborted, [1n]);
  assert.equal(fixture.bridge.liveSurfaces, 0);
  assert.equal(bytes.releases, 1);
  assert.equal(fixture.releases.requests.length, 1);
});

test("replacement release failure cannot retire the committed replacement", () => {
  const oldBytes = arrayBuffer();
  const newBytes = arrayBuffer();
  const fixture = bridgeFixture();
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 1n), oldBytes),
  );
  fixture.bridge.drain();
  fixture.releases.failures.set(1n, 1);
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 2n), newBytes),
  );

  const replacement = fixture.bridge.drain();
  assert.equal(replacement.presented, 1);
  assert.equal(replacement.rejected, 0);
  assert.equal(
    replacement.errors.some((error) => error.code === "ReleaseFailure"),
    true,
  );
  assert.deepEqual(fixture.presentation.committed, [1n, 2n]);
  assert.equal(fixture.bridge.liveSurfaces, 1);
  assert.equal(oldBytes.releases, 1);
  assert.equal(newBytes.releases, 0);

  const retry = fixture.bridge.drain();
  assert.deepEqual(retry.errors, []);
  assert.equal(
    fixture.releases.requests.filter(
      (request) => request.surface === 1n,
    ).length,
    2,
  );
  fixture.bridge.releaseSurface(2n, 102n);
  assert.equal(newBytes.releases, 1);
});

test("cleanup follows actual resource type and survives publication parse failure", () => {
  const wrongKind = bitmap();
  const malformed = bitmap();
  const fixture = bridgeFixture();
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 1n), wrongKind),
  );
  const invalidType = fixture.bridge.drain();
  assert.equal(
    invalidType.errors.some(
      (error) => error.code === "InvalidResourceType",
    ),
    true,
  );
  assert.equal(wrongKind.closes, 1);
  assert.equal(wrongKind.releases, 0);

  const invalidSurface = surfaceEvent("bitmap", 2n);
  invalidSurface.metadata.stride = 4;
  invalidSurface.metadata.byte_length = 4n;
  fixture.bridge.enqueueSurfaceReady(
    publication(invalidSurface, malformed),
  );
  const parseFailure = fixture.bridge.drain();
  assert.equal(
    parseFailure.errors.some(
      (error) => error.code === "InvalidPublication",
    ),
    true,
  );
  assert.equal(malformed.closes, 1);
  assert.equal(
    fixture.releases.requests.some(
      (request) => request.surface === 2n,
    ),
    false,
  );
});

test("exclusive backing cannot be republished under a new Surface before terminal", () => {
  const backing = {};
  const first = arrayBuffer();
  const second = arrayBuffer();
  const third = arrayBuffer();
  first.backingIdentity = backing;
  second.backingIdentity = backing;
  third.backingIdentity = backing;
  const fixture = bridgeFixture();
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 1n), first),
  );
  fixture.bridge.drain();

  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 2n), second),
  );
  const duplicate = fixture.bridge.drain();
  assert.deepEqual(
    duplicate.errors.map((error) => error.code),
    ["DuplicateSurface"],
  );
  assert.deepEqual(fixture.presentation.committed, [1n]);
  assert.equal(first.releases, 0);
  assert.equal(second.releases, 0);

  fixture.bridge.releaseSurface(1n, 101n);
  fixture.bridge.enqueueLifecycle({
    worker: 1n,
    workerEpoch: 11n,
    session: 2n,
    event: {
      surface: 1n,
      lease_token: 101n,
      status: OperationAckStatus.Applied,
    },
  });
  fixture.bridge.drain();
  fixture.bridge.drain();
  assert.equal(first.releases, 1);
  assert.equal(second.releases, 1);

  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 3n), third),
  );
  const stillPending = fixture.bridge.drain();
  assert.equal(
    stillPending.errors.some(
      (error) => error.code === "DuplicateSurface",
    ),
    true,
  );
  assert.deepEqual(fixture.presentation.committed, [1n]);
  assert.equal(third.releases, 0);
});

test("release ledger binds session and evicts Terminal before Pending", () => {
  const fixture = bridgeFixture(
    epoch(),
    fakeAdapters("ledger"),
    {
      ...LIMITS,
      maxLiveSurfaces: 1,
      maxTrackedLeases: 2,
    },
  );
  fixture.bridge.activateGeneration(generation());

  for (const id of [1n, 2n, 3n]) {
    fixture.bridge.enqueueSurfaceReady(
      publication(surfaceEvent("array", id), arrayBuffer()),
    );
    assert.equal(fixture.bridge.drain().presented, 1);
    if (id === 2n) {
      fixture.releases.dispositions.set(
        id,
        "AlreadyAcknowledged",
      );
    }
    fixture.bridge.releaseSurface(id, id + 100n);
  }
  assert.equal(fixture.releases.requests.length, 3);

  fixture.bridge.enqueueLifecycle({
    worker: 1n,
    workerEpoch: 11n,
    session: 8n,
    event: {
      surface: 1n,
      lease_token: 101n,
      status: OperationAckStatus.Applied,
    },
  });
  fixture.bridge.enqueueLifecycle({
    worker: 1n,
    workerEpoch: 11n,
    session: 2n,
    event: {
      surface: 1n,
      lease_token: 101n,
      status: OperationAckStatus.Applied,
    },
  });
  const acknowledgements = fixture.bridge.drain();
  assert.equal(
    acknowledgements.errors.some(
      (error) => error.code === "InvalidRelease",
    ),
    true,
  );
  assert.equal(acknowledgements.acknowledged, 1);
});

test("close drains queued Surfaces, bounds sessions, and forbids epoch reuse", () => {
  const queued = bitmap();
  const fixture = bridgeFixture(
    epoch(),
    fakeAdapters("bounds"),
    {
      ...LIMITS,
      maxSessionsPerEpoch: 2,
      maxWorkerEpochs: 2,
    },
  );
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("bitmap"), queued),
  );
  fixture.bridge.closeSession(2n);
  fixture.bridge.closeSession(2n);
  assert.equal(fixture.bridge.queuedCallbacks, 0);
  assert.equal(queued.closes, 1);
  assert.deepEqual(fixture.presentation.committed, []);
  assert.equal(
    fixture.releases.requests[0]?.reason,
    SurfaceReclaimReason.SessionClosed,
  );
  assertCode(
    () => fixture.bridge.activateGeneration(generation()),
    "InvalidSession",
  );

  fixture.bridge.activateGeneration(generation(1n, 3n));
  assertCode(
    () => fixture.bridge.activateGeneration(generation(1n, 4n)),
    "SessionLimit",
  );
  fixture.releases.disposition = "WorkerTerminal";
  fixture.bridge.workerFault(1n, 11n);
  assertCode(
    () => fixture.bridge.startEpoch(epoch()),
    "InvalidWorkerEpoch",
  );
  fixture.bridge.startEpoch({
    ...epoch(),
    worker: 4n,
    workerEpoch: 12n,
  });
});

test("checked range budget and cleanup/release failures remain retryable", () => {
  const validator = new BrowserSurfacePublicationValidator(
    {
      ...LIMITS,
      maxSurfaceBytes: 10n,
    },
    fakeAdapters("budget").adapters,
  );
  assertCode(
    () => validator.validate(
      publication(surfaceEvent("array"), arrayBuffer()),
      epoch(),
      generation(),
    ),
    "InvalidSurfaceLayout",
  );

  const cleanupBitmap = bitmap();
  cleanupBitmap.cleanupFailures = 1;
  const cleanup = bridgeFixture();
  cleanup.bridge.activateGeneration({
    ...generation(),
    identity: {
      ...generation().identity,
      alpha: AlphaMode.Premultiplied,
    },
  });
  cleanup.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("bitmap"), cleanupBitmap),
  );
  cleanup.bridge.drain();
  assertCode(
    () => cleanup.bridge.releaseSurface(1n, 101n),
    "AdapterFailure",
  );
  assert.equal(cleanupBitmap.closes, 0);
  cleanup.bridge.drain();
  assert.equal(cleanupBitmap.closes, 1);

  const releaseBytes = arrayBuffer();
  const release = bridgeFixture();
  release.bridge.activateGeneration(generation());
  release.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array"), releaseBytes),
  );
  release.bridge.drain();
  release.releases.failures.set(1n, 1);
  assertCode(
    () => release.bridge.releaseSurface(1n, 101n),
    "ReleaseFailure",
  );
  assert.equal(release.releases.requests.length, 1);
  release.bridge.drain();
  assert.equal(release.releases.requests.length, 2);
});

test("Wasm view creation rechecks memory epoch before publishing the view", () => {
  const released: object[] = [];
  let memoryEpoch = 1;
  let growDuringCreate = true;
  const memory = {};
  const adapter: BrowserWasmLocalMemoryAdapter = {
    describe: (): BrowserWasmLocalMemoryDescription => ({
      worker: 1n,
      workerEpoch: 11n,
      memoryEpoch,
      byteLength: memoryEpoch === 1 ? 64n : 128n,
      sameRealm: true,
    }),
    createReadOnlyView: (): object => {
      const view = { memoryEpoch };
      if (growDuringCreate) {
        growDuringCreate = false;
        memoryEpoch = 2;
      }
      return view;
    },
    release: (view: object): void => {
      released.push(view);
    },
  };
  const local = new BrowserWasmLocalViewBridge(1n, 11n, adapter);
  assertCode(
    () => local.acquire({
      worker: 1n,
      workerEpoch: 11n,
      memory,
      memoryEpoch: 1,
      byteOffset: 0n,
      byteLength: 16n,
    }),
    "InvalidMemoryEpoch",
  );
  assert.equal(released.length, 1);
  const rebuilt = local.acquire({
    worker: 1n,
    workerEpoch: 11n,
    memory,
    memoryEpoch: 2,
    byteOffset: 0n,
    byteLength: 16n,
  });
  assert.equal(typeof rebuilt, "object");
  local.close();
  assert.equal(released.length, 2);
});

test("Wasm-local views require same realm/Worker/epoch and rebuild after grow", () => {
  const released: object[] = [];
  let created = 0;
  const adapter: BrowserWasmLocalMemoryAdapter = {
    describe: (
      memory: unknown,
    ): BrowserWasmLocalMemoryDescription | undefined => {
      const resource = memory as FakeResource;
      if (resource.kind !== "memory") {
        return undefined;
      }
      return {
        worker: resource.worker ?? 0n,
        workerEpoch: resource.workerEpoch ?? 0n,
        memoryEpoch: resource.memoryEpoch ?? 0,
        byteLength: resource.byteLength ?? 0n,
        sameRealm: resource.sameRealm === true,
      };
    },
    createReadOnlyView: (
      memory: unknown,
      byteOffset: bigint,
      byteLength: bigint,
    ): object => {
      created += 1;
      return { memory, byteOffset, byteLength, created };
    },
    release: (view: object): void => {
      released.push(view);
    },
  };
  const memory: FakeResource = {
    kind: "memory",
    worker: 1n,
    workerEpoch: 11n,
    memoryEpoch: 1,
    byteLength: 64n,
    sameRealm: true,
    closes: 0,
    releases: 0,
  };
  const local = new BrowserWasmLocalViewBridge(1n, 11n, adapter);
  const request: BrowserWasmLocalViewRequest = {
    worker: 1n,
    workerEpoch: 11n,
    memory,
    memoryEpoch: 1,
    byteOffset: 8n,
    byteLength: 16n,
  };
  assert.equal("ptr" in request, false);
  const first = local.acquire(request);
  assert.equal(local.acquire(request), first);
  assert.equal(created, 1);

  memory.memoryEpoch = 2;
  memory.byteLength = 128n;
  assertCode(
    () => local.acquire(request),
    "InvalidMemoryEpoch",
  );
  assert.deepEqual(released, [first]);
  const rebuilt = local.acquire({
    ...request,
    memoryEpoch: 2,
  });
  assert.notEqual(rebuilt, first);
  assert.equal(created, 2);

  const secondMemory: FakeResource = {
    ...memory,
    closes: 0,
    releases: 0,
  };
  const secondMemoryView = local.acquire({
    ...request,
    memory: secondMemory,
    memoryEpoch: 2,
  });
  assert.notEqual(secondMemoryView, rebuilt);
  assert.equal(created, 3);

  assertCode(
    () => local.acquire({
      ...request,
      worker: 2n,
      memoryEpoch: 2,
    }),
    "InvalidWorkerEpoch",
  );
  assertCode(
    () => local.acquire({
      ...request,
      memoryEpoch: 2,
      byteOffset: 120n,
      byteLength: 16n,
    }),
    "InvalidMemoryRange",
  );

  memory.sameRealm = false;
  assertCode(
    () => local.acquire({
      ...request,
      memoryEpoch: 2,
    }),
    "InvalidMemory",
  );
  memory.sameRealm = true;
  local.close();
  local.close();
  assert.equal(released.length, 3);
  assertCode(
    () => local.acquire({
      ...request,
      memoryEpoch: 2,
    }),
    "InvalidLifecycle",
  );
});

test("synchronous release acknowledgement cannot resurrect a Terminal lease", () => {
  const fixture = bridgeFixture(
    epoch(),
    fakeAdapters("synchronous-ack"),
    {
      ...LIMITS,
      maxLiveSurfaces: 1,
      maxTrackedLeases: 1,
    },
  );
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 1n), arrayBuffer()),
  );
  assert.equal(fixture.bridge.drain().presented, 1);

  fixture.releases.onRequest = (
    request: BrowserSurfaceReleaseRequest,
  ): void => {
    fixture.releases.onRequest = undefined;
    fixture.bridge.enqueueLifecycle({
      worker: request.worker,
      workerEpoch: request.workerEpoch,
      session: request.session,
      event: {
        surface: request.surface,
        lease_token: request.leaseToken,
        status: OperationAckStatus.Applied,
      },
    });
    try {
      fixture.bridge.drain();
    } catch (error: unknown) {
      assert.equal(
        error instanceof BrowserSurfaceBridgeError
          && error.code === "InvalidLifecycle",
        true,
      );
    }
  };
  fixture.bridge.releaseSurface(1n, 101n);
  fixture.bridge.drain();

  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 2n), arrayBuffer()),
  );
  assert.equal(fixture.bridge.drain().presented, 1);
  assert.doesNotThrow(
    () => fixture.bridge.releaseSurface(2n, 102n),
  );
  assert.deepEqual(
    fixture.releases.requests.map((request) => request.surface),
    [1n, 2n],
  );
});

test("lease identity includes Worker when WorkerEpoch values coincide", () => {
  const fixture = bridgeFixture();
  fixture.releases.disposition = "AlreadyAcknowledged";
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 1n), arrayBuffer()),
  );
  assert.equal(fixture.bridge.drain().presented, 1);
  fixture.bridge.releaseSurface(1n, 101n);
  fixture.bridge.workerFault(1n, 11n);

  fixture.bridge.startEpoch({
    ...epoch(),
    worker: 2n,
    workerEpoch: 11n,
  });
  fixture.bridge.activateGeneration(generation(3n, 2n, 2n, 11n));
  fixture.bridge.enqueueSurfaceReady(
    publication(
      surfaceEvent("array", 1n, 3n, 2n, 2n),
      arrayBuffer(),
      11n,
    ),
  );
  const nextWorker = fixture.bridge.drain();
  assert.equal(nextWorker.presented, 1);
  assert.equal(
    nextWorker.errors.some((error) => error.code === "DuplicateSurface"),
    false,
  );
});

test("duplicate cleanup unlocks its backing after the new lease is acknowledged", () => {
  const duplicateBacking = {};
  const fixture = bridgeFixture();
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 1n), arrayBuffer()),
  );
  assert.equal(fixture.bridge.drain().presented, 1);

  const duplicateResource = arrayBuffer();
  duplicateResource.backingIdentity = duplicateBacking;
  const duplicateEvent = surfaceEvent("array", 1n);
  duplicateEvent.metadata.lease_token = 201n;
  fixture.bridge.enqueueSurfaceReady(
    publication(duplicateEvent, duplicateResource),
  );
  const duplicate = fixture.bridge.drain();
  assert.equal(
    duplicate.errors.some((error) => error.code === "DuplicateSurface"),
    true,
  );
  assert.equal(duplicateResource.releases, 1);

  fixture.bridge.enqueueLifecycle({
    worker: 1n,
    workerEpoch: 11n,
    session: 2n,
    event: {
      surface: 1n,
      lease_token: 201n,
      status: OperationAckStatus.Applied,
    },
  });
  assert.equal(fixture.bridge.drain().acknowledged, 1);

  const reusedResource = arrayBuffer();
  reusedResource.backingIdentity = duplicateBacking;
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 2n), reusedResource),
  );
  const reused = fixture.bridge.drain();
  assert.equal(reused.presented, 1);
  assert.equal(
    reused.errors.some((error) => error.code === "DuplicateSurface"),
    false,
  );
});

test("actual ArrayBuffer and SharedArrayBuffer backing obey maxSurfaceBytes", () => {
  const validator = new BrowserSurfacePublicationValidator(
    {
      ...LIMITS,
      maxSurfaceBytes: 16n,
    },
    fakeAdapters("actual-backing-budget").adapters,
  );

  const oversizedArray = surfaceEvent("array");
  if (oversizedArray.transport.kind !== "BrowserArrayBuffer") {
    throw new Error("fixture transport");
  }
  oversizedArray.transport.buffer_length = 32n;
  assertCode(
    () => validator.validate(
      publication(oversizedArray, arrayBuffer(32n)),
      epoch(),
      generation(),
    ),
    "InvalidResourceExtent",
  );

  const oversizedShared = surfaceEvent("shared");
  if (oversizedShared.transport.kind !== "BrowserSharedArrayBuffer") {
    throw new Error("fixture transport");
  }
  oversizedShared.transport.buffer_length = 32n;
  assertCode(
    () => validator.validate(
      publication(oversizedShared, sharedBuffer(32n)),
      epoch(),
      generation(),
    ),
    "InvalidResourceExtent",
  );
});

test("failed presentation removal blocks SurfaceId reuse until retry succeeds", () => {
  const fixture = bridgeFixture();
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 1n), arrayBuffer()),
  );
  assert.equal(fixture.bridge.drain().presented, 1);

  fixture.presentation.removeFailures.set(1n, 1);
  fixture.bridge.enqueueLifecycle({
    worker: 1n,
    workerEpoch: 11n,
    session: 2n,
    event: {
      surface: 1n,
      lease_token: 101n,
      reason: SurfaceReclaimReason.MemoryPressure,
    },
  });
  const blockedResource = arrayBuffer();
  const blockedEvent = surfaceEvent("array", 1n);
  blockedEvent.metadata.lease_token = 201n;
  fixture.bridge.enqueueSurfaceReady(
    publication(blockedEvent, blockedResource),
  );

  const blocked = fixture.bridge.drain();
  assert.equal(blocked.presented, 0);
  assert.equal(fixture.bridge.liveSurfaces, 0);
  assert.deepEqual(fixture.presentation.committed, [1n]);
  assert.equal(blockedResource.releases, 1);

  const retry = fixture.bridge.drain();
  assert.equal(
    retry.errors.some((error) => error.code === "PresentationFailure"),
    false,
  );
  assert.deepEqual(fixture.presentation.removed, [1n]);

  const acceptedEvent = surfaceEvent("array", 1n);
  acceptedEvent.metadata.lease_token = 301n;
  fixture.bridge.enqueueSurfaceReady(
    publication(acceptedEvent, arrayBuffer()),
  );
  assert.equal(fixture.bridge.drain().presented, 1);
  assert.deepEqual(fixture.presentation.committed, [1n, 1n]);
});

test("idempotent close drains late queued Surfaces without an active generation", () => {
  const fixture = bridgeFixture();
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.closeSession(2n);

  const late = bitmap();
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("bitmap"), late),
  );
  fixture.bridge.closeSession(2n);

  assert.equal(fixture.bridge.queuedCallbacks, 0);
  assert.equal(late.closes, 1);
  assert.deepEqual(fixture.presentation.committed, []);
  assert.equal(
    fixture.releases.requests.at(-1)?.reason,
    SurfaceReclaimReason.SessionClosed,
  );
});

test("malformed resources arrays still clean indexed transferred resources", () => {
  const transferred = bitmap();
  const fixture = bridgeFixture();
  fixture.bridge.activateGeneration(generation());
  const message = publication(surfaceEvent("bitmap"), transferred);
  Object.defineProperty(message.resources, "extra", {
    value: "untrusted-extra-property",
    enumerable: true,
    configurable: true,
  });
  fixture.bridge.enqueueSurfaceReady(message);

  const result = fixture.bridge.drain();
  assert.equal(
    result.errors.some((error) => error.code === "InvalidSurfaceSlot"),
    true,
  );
  assert.equal(transferred.closes, 1);
  assert.deepEqual(fixture.releases.requests, []);
});

test("permanent cleanup and Wasm orphan failures stop at their exact limits", () => {
  const cleanupAdapters = fakeAdapters("bounded-cleanup");
  const cleanup = bridgeFixture(
    epoch(),
    cleanupAdapters,
    {
      ...LIMITS,
      maxLiveSurfaces: 1,
      maxTrackedLeases: 2,
    },
  );
  cleanup.bridge.activateGeneration({
    ...generation(),
    identity: {
      ...generation().identity,
      alpha: AlphaMode.Premultiplied,
    },
  });
  for (const id of [1n, 2n]) {
    const resource = bitmap();
    resource.cleanupFailures = Number.POSITIVE_INFINITY;
    const invalid = surfaceEvent("bitmap", id);
    invalid.metadata.stride = 4;
    invalid.metadata.byte_length = 4n;
    cleanup.bridge.enqueueSurfaceReady(publication(invalid, resource));
    const retained = cleanup.bridge.drain();
    assert.equal(
      retained.errors.some((error) => error.code === "AdapterFailure"),
      true,
    );
  }
  for (const id of [3n, 4n]) {
    const resource = bitmap();
    resource.cleanupFailures = Number.POSITIVE_INFINITY;
    const invalid = surfaceEvent("bitmap", id);
    invalid.metadata.stride = 4;
    invalid.metadata.byte_length = 4n;
    assertCode(
      () => cleanup.bridge.enqueueSurfaceReady(
        publication(invalid, resource),
      ),
      "LeaseLimit",
    );
  }
  assert.equal(cleanup.bridge.queuedCallbacks, 0);

  let memoryEpoch = 1;
  let created = 0;
  const memory = {};
  const adapter: BrowserWasmLocalMemoryAdapter = {
    describe: (): BrowserWasmLocalMemoryDescription => ({
      worker: 1n,
      workerEpoch: 11n,
      memoryEpoch,
      byteLength: 64n,
      sameRealm: true,
    }),
    createReadOnlyView: (): object => {
      created += 1;
      const view = { created };
      memoryEpoch += 1;
      return view;
    },
    release: (): void => {
      throw new Error("injected permanent Wasm view release failure");
    },
  };
  const local = new BrowserWasmLocalViewBridge(1n, 11n, adapter, 2);
  const acquire = (): object => local.acquire({
    worker: 1n,
    workerEpoch: 11n,
    memory,
    memoryEpoch,
    byteOffset: 0n,
    byteLength: 16n,
  });
  assertCode(acquire, "InvalidMemoryEpoch");
  assertCode(acquire, "InvalidMemoryEpoch");
  assertCode(acquire, "SurfaceLimit");
  assert.equal(created, 2);
});

test("critical release delivery retains its slot until acknowledgement", () => {
  const fixture = bridgeFixture(
    epoch(),
    fakeAdapters("critical-release-capacity"),
    {
      ...LIMITS,
      maxLiveSurfaces: 1,
      maxTrackedLeases: 2,
    },
  );
  fixture.bridge.activateGeneration(generation());

  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 1n), arrayBuffer()),
  );
  assert.equal(fixture.bridge.drain().presented, 1);
  fixture.bridge.releaseSurface(1n, 101n);

  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 2n), arrayBuffer()),
  );
  assert.equal(fixture.bridge.drain().presented, 1);
  fixture.bridge.releaseSurface(2n, 102n);
  assert.deepEqual(
    fixture.releases.requests.map((request) => request.surface),
    [1n, 2n],
  );

  const third = arrayBuffer();
  assertCode(
    () => fixture.bridge.enqueueSurfaceReady(
      publication(surfaceEvent("array", 3n), third),
    ),
    "LeaseLimit",
  );
  assert.equal(fixture.bridge.queuedCallbacks, 0);
  assert.equal(third.releases, 0);

  fixture.bridge.enqueueLifecycle({
    worker: 1n,
    workerEpoch: 11n,
    session: 2n,
    event: {
      surface: 1n,
      lease_token: 101n,
      status: OperationAckStatus.Applied,
    },
  });
  assert.equal(fixture.bridge.drain().acknowledged, 1);

  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 3n), third),
  );
  assert.equal(fixture.bridge.drain().presented, 1);
  assert.equal(third.releases, 0);
});

test("delayed duplicate cleanup releases its maintenance charge after acknowledgement", () => {
  const fixture = bridgeFixture(
    epoch(),
    fakeAdapters("delayed-duplicate-cleanup"),
    {
      ...LIMITS,
      maxLiveSurfaces: 1,
      maxTrackedLeases: 2,
    },
  );
  const backing = {};
  const first = sharedBuffer();
  const replay = sharedBuffer();
  first.backingIdentity = backing;
  replay.backingIdentity = backing;
  fixture.bridge.activateGeneration(generation());

  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("shared", 1n), first),
  );
  assert.equal(fixture.bridge.drain().presented, 1);
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("shared", 1n), replay),
  );
  const duplicate = fixture.bridge.drain();
  assert.equal(
    duplicate.errors.some(
      (error) => error.code === "DuplicateSurface",
    ),
    true,
  );
  assert.equal(replay.releases, 0);

  fixture.bridge.enqueueLifecycle({
    worker: 1n,
    workerEpoch: 11n,
    session: 2n,
    event: {
      surface: 1n,
      lease_token: 101n,
      status: OperationAckStatus.Applied,
    },
  });
  assert.equal(fixture.bridge.drain().acknowledged, 1);
  fixture.bridge.drain();
  assert.equal(replay.releases, 1);

  assert.doesNotThrow(() => {
    fixture.bridge.enqueueSurfaceReady(
      publication(surfaceEvent("array", 2n), arrayBuffer()),
    );
    fixture.bridge.enqueueSurfaceReady(
      publication(surfaceEvent("array", 3n), arrayBuffer()),
    );
  });
});

test("malformed multi-resource callbacks charge and clean every indexed resource", () => {
  const fixture = bridgeFixture(
    epoch(),
    fakeAdapters("multi-resource-cleanup"),
    {
      ...LIMITS,
      maxLiveSurfaces: 1,
      maxTrackedLeases: 2,
    },
  );
  fixture.bridge.activateGeneration(generation());
  const first = bitmap();
  const second = bitmap();
  const malformed = publication(
    surfaceEvent("bitmap", 1n),
    first,
  );
  malformed.resources.push(second);
  fixture.bridge.enqueueSurfaceReady(malformed);

  const blocked = arrayBuffer();
  assertCode(
    () => fixture.bridge.enqueueSurfaceReady(
      publication(surfaceEvent("array", 2n), blocked),
    ),
    "LeaseLimit",
  );
  assert.equal(blocked.releases, 0);

  const rejected = fixture.bridge.drain();
  assert.equal(
    rejected.errors.some(
      (error) => error.code === "InvalidSurfaceSlot",
    ),
    true,
  );
  assert.equal(first.closes, 1);
  assert.equal(second.closes, 1);

  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 2n), blocked),
  );
  assert.equal(fixture.bridge.drain().presented, 1);
});

test("old lease acknowledgement does not retire a reused live SurfaceId", () => {
  const fixture = bridgeFixture(
    epoch(),
    fakeAdapters("old-lease-ack"),
    {
      ...LIMITS,
      maxLiveSurfaces: 1,
      maxTrackedLeases: 2,
    },
  );
  fixture.bridge.activateGeneration(generation());
  fixture.bridge.enqueueSurfaceReady(
    publication(surfaceEvent("array", 1n), arrayBuffer()),
  );
  assert.equal(fixture.bridge.drain().presented, 1);
  fixture.bridge.releaseSurface(1n, 101n);

  const replacement = surfaceEvent("array", 1n);
  replacement.metadata.lease_token = 201n;
  fixture.bridge.enqueueSurfaceReady(
    publication(replacement, arrayBuffer()),
  );
  assert.equal(fixture.bridge.drain().presented, 1);

  fixture.bridge.enqueueLifecycle({
    worker: 1n,
    workerEpoch: 11n,
    session: 2n,
    event: {
      surface: 1n,
      lease_token: 101n,
      status: OperationAckStatus.Applied,
    },
  });
  const acknowledged = fixture.bridge.drain();
  assert.equal(acknowledged.acknowledged, 1);
  assert.equal(
    acknowledged.errors.some(
      (error) => error.code === "InvalidRelease",
    ),
    false,
  );
  assert.equal(fixture.bridge.liveSurfaces, 1);
  assert.deepEqual(fixture.presentation.committed, [1n, 1n]);
  assert.deepEqual(fixture.presentation.removed, [1n]);
  assert.doesNotThrow(
    () => fixture.bridge.releaseSurface(1n, 101n),
  );
  assert.equal(fixture.bridge.liveSurfaces, 1);
  assert.doesNotThrow(
    () => fixture.bridge.releaseSurface(1n, 201n),
  );
});

test("malformed cleanup deduplicates aliased resource table entries", () => {
  const fixture = bridgeFixture(
    epoch(),
    fakeAdapters("aliased-malformed-cleanup"),
    {
      ...LIMITS,
      maxLiveSurfaces: 1,
      maxTrackedLeases: 2,
    },
  );
  fixture.bridge.activateGeneration(generation());
  const transferred = bitmap();
  const malformed = publication(
    surfaceEvent("bitmap", 1n),
    transferred,
  );
  malformed.resources.push(transferred);
  fixture.bridge.enqueueSurfaceReady(malformed);

  const rejected = fixture.bridge.drain();
  assert.equal(
    rejected.errors.some(
      (error) => error.code === "InvalidSurfaceSlot",
    ),
    true,
  );
  assert.equal(transferred.closes, 1);
});

test("current live release wins over ambiguous old cross-session tombstones", () => {
  const fixture = bridgeFixture(
    epoch(),
    fakeAdapters("current-live-release"),
    {
      ...LIMITS,
      maxTrackedLeases: 4,
    },
  );
  fixture.releases.disposition = "AlreadyAcknowledged";

  for (const session of [2n, 3n]) {
    fixture.bridge.activateGeneration(
      generation(3n, session),
    );
    fixture.bridge.enqueueSurfaceReady(
      publication(
        surfaceEvent("array", 1n, 3n, 1n, session),
        arrayBuffer(),
      ),
    );
    assert.equal(fixture.bridge.drain().presented, 1);
    fixture.bridge.releaseSurface(1n, 101n);
    assert.equal(fixture.bridge.liveSurfaces, 0);
  }

  fixture.bridge.activateGeneration(generation(3n, 4n));
  fixture.bridge.enqueueSurfaceReady(
    publication(
      surfaceEvent("array", 1n, 3n, 1n, 4n),
      arrayBuffer(),
    ),
  );
  assert.equal(fixture.bridge.drain().presented, 1);
  assert.doesNotThrow(
    () => fixture.bridge.releaseSurface(1n, 101n),
  );
  assert.equal(fixture.bridge.liveSurfaces, 0);
});
