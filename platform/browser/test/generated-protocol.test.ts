import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  AlphaMode,
  CAPABILITY_DECISION_HASH_DOMAIN,
  CollectionCompleteness,
  ENGINE_ERROR_DESCRIPTORS,
  EngineErrorCode,
  EndpointRole,
  ErrorCategory,
  ErrorRecoverability,
  ErrorSeverity,
  MESSAGE_DESCRIPTORS,
  MESSAGE_ID_PROVIDE_DATA,
  MIN_COMPATIBLE_MINOR,
  NativeBackend,
  OutputProfile,
  PageRotation,
  PixelFormat,
  PROTOCOL_MAJOR,
  PROTOCOL_MINOR,
  QualityPolicy,
  RENDER_PLAN_MANIFEST_HASH_DOMAIN,
  SCHEMA_HASH_HEX,
  SCHEMA_SHA256_HEX,
  SupportStatus,
  SurfaceCoordinateSpace,
  SurfaceReclaimReason,
  capabilityDecisionHashPreimage,
  decodeCapabilityDecisionPayload,
  decodeRenderPlanManifestPayload,
  descriptorById,
  encodeCapabilityDecisionPayload,
  encodeRenderPlanManifestPayload,
  renderPlanManifestHashPreimage,
  validateCapabilityDecision,
  validateEndpointCapabilities,
  validateEngineError,
  validateEnvelopeHeader,
  validateEnvelopeHeaderForMinor,
  validateProtocolHello,
  validateRenderPlanManifest,
  validateSurfaceReadyEvent,
  validateSurfaceReclaimedEvent,
  type PayloadCodecResult,
} from "../generated/engine-protocol.js";

const bytesFromHex = (value: string): Uint8Array =>
  Uint8Array.from(
    value.match(/.{2}/gu)?.map((byte) => Number.parseInt(byte, 16)) ?? [],
  );

const unwrapPayload = <T>(result: PayloadCodecResult<T>): T => {
  if (result.ok) {
    return result.value;
  }
  throw new Error(`unexpected payload codec failure: ${result.error.code}`);
};

interface PayloadHashKnownAnswer {
  readonly type: string;
  readonly domain: string;
  readonly payload_hex: string;
  readonly preimage_hex: string;
  readonly sha256: string;
}

const payloadHashKnownAnswers =
  async (): Promise<readonly PayloadHashKnownAnswer[]> => {
    const location = new URL(
      "../../../../protocol/generated/payload-codec-vectors.json",
      import.meta.url,
    );
    const parsed: unknown = JSON.parse(await readFile(location, "utf8"));
    if (
      typeof parsed !== "object"
      || parsed === null
      || !("schema_sha256" in parsed)
      || parsed.schema_sha256 !== SCHEMA_SHA256_HEX
      || !("hash_known_answers" in parsed)
      || !Array.isArray(parsed.hash_known_answers)
    ) {
      throw new TypeError("invalid generated payload hash vector document");
    }
    return parsed.hash_known_answers.map((raw: unknown) => {
      if (
        typeof raw !== "object"
        || raw === null
        || !("type" in raw)
        || typeof raw.type !== "string"
        || !("domain" in raw)
        || typeof raw.domain !== "string"
        || !("payload_hex" in raw)
        || typeof raw.payload_hex !== "string"
        || !("preimage_hex" in raw)
        || typeof raw.preimage_hex !== "string"
        || !("sha256" in raw)
        || typeof raw.sha256 !== "string"
      ) {
        throw new TypeError("invalid generated payload hash known answer");
      }
      return {
        type: raw.type,
        domain: raw.domain,
        payload_hex: raw.payload_hex,
        preimage_hex: raw.preimage_hex,
        sha256: raw.sha256,
      };
    });
  };

test("generated hello constants and strict unknown-field policy are executable", () => {
  const hello = {
    major: PROTOCOL_MAJOR,
    minor: PROTOCOL_MINOR,
    schema_hash: bytesFromHex(SCHEMA_HASH_HEX),
    endpoint_role: EndpointRole.Host,
    capabilities: {
      supported: 0x800000000000003fn,
      mandatory: 0n,
    },
    max_message_bytes: 16_777_216,
    max_transfer_slots: 64,
  };

  assert.equal(validateProtocolHello(hello), true);
  assert.equal(validateProtocolHello({ ...hello, ignored: true }), false);
  assert.equal(
    validateEndpointCapabilities({
      supported: 0x3fn,
      mandatory: 0x8000000000000000n,
    }),
    false,
  );
  assert.equal(MIN_COMPATIBLE_MINOR, PROTOCOL_MINOR);
  assert.equal(
    validateEnvelopeHeaderForMinor(
      {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        message_type: 1,
        flags: 0,
        payload_len: 0,
        sequence: 1n,
      },
      MIN_COMPATIBLE_MINOR,
    ),
    true,
  );
  assert.equal(
    validateEnvelopeHeaderForMinor(
      {
        major: PROTOCOL_MAJOR,
        minor: MIN_COMPATIBLE_MINOR - 1,
        message_type: 1,
        flags: 0,
        payload_len: 0,
        sequence: 1n,
      },
      MIN_COMPATIBLE_MINOR - 1,
    ),
    false,
  );
});

test("generated validation registries are deeply immutable at runtime", () => {
  const descriptor = descriptorById(MESSAGE_ID_PROVIDE_DATA);
  assert.notEqual(descriptor, undefined);
  assert.equal(
    descriptor,
    MESSAGE_DESCRIPTORS.find(
      (candidate) => candidate.id === MESSAGE_ID_PROVIDE_DATA,
    ),
  );
  assert.ok(Object.isFrozen(MESSAGE_DESCRIPTORS));
  assert.ok(Object.isFrozen(descriptor));
  assert.ok(Object.isFrozen(descriptor?.correlation_shape));
  assert.ok(Object.isFrozen(descriptor?.outcomes));
  assert.ok(Object.isFrozen(descriptor?.outcomes[0]));
  assert.equal(
    Reflect.set(
      descriptor as object,
      "allowed_flags",
      0xffff,
    ),
    false,
  );
  assert.equal(
    Reflect.set(
      descriptor?.correlation_shape as object,
      "session",
      "forbidden",
    ),
    false,
  );
  assert.equal(
    validateEnvelopeHeader({
      major: PROTOCOL_MAJOR,
      minor: PROTOCOL_MINOR,
      message_type: MESSAGE_ID_PROVIDE_DATA,
      flags: 1,
      payload_len: 0,
      sequence: 1n,
    }),
    false,
  );

  const engineErrorDescriptor = ENGINE_ERROR_DESCRIPTORS[0];
  assert.notEqual(engineErrorDescriptor, undefined);
  assert.ok(Object.isFrozen(ENGINE_ERROR_DESCRIPTORS));
  assert.ok(Object.isFrozen(engineErrorDescriptor));
  assert.equal(
    Reflect.set(
      engineErrorDescriptor as object,
      "category",
      ErrorCategory.Source,
    ),
    false,
  );
  assert.equal(
    validateEngineError({
      code: EngineErrorCode.InvalidDocument,
      category: ErrorCategory.Source,
      severity: ErrorSeverity.Fatal,
      recoverability: ErrorRecoverability.ReopenSession,
      diagnostic_id: 1n,
    }),
    false,
  );
});

test("Rust-generated decision and RenderPlan KATs replay byte-exactly in TypeScript", async () => {
  const answers = await payloadHashKnownAnswers();
  assert.equal(answers.length, 2);

  const decisionAnswer = answers.find(
    (answer) => answer.type === "CapabilityDecision",
  );
  assert.notEqual(decisionAnswer, undefined);
  assert.equal(
    decisionAnswer?.domain,
    CAPABILITY_DECISION_HASH_DOMAIN,
  );
  const decisionPayload = bytesFromHex(decisionAnswer?.payload_hex ?? "");
  const decision = unwrapPayload(
    decodeCapabilityDecisionPayload(decisionPayload),
  );
  assert.equal(validateCapabilityDecision(decision), true);
  assert.deepEqual(
    unwrapPayload(encodeCapabilityDecisionPayload(decision)),
    decisionPayload,
  );
  const decisionPreimage = unwrapPayload(
    capabilityDecisionHashPreimage(decision),
  );
  assert.deepEqual(
    decisionPreimage,
    bytesFromHex(decisionAnswer?.preimage_hex ?? ""),
  );
  assert.equal(
    createHash("sha256").update(decisionPreimage).digest("hex"),
    decisionAnswer?.sha256,
  );

  const manifestAnswer = answers.find(
    (answer) => answer.type === "RenderPlanManifest",
  );
  assert.notEqual(manifestAnswer, undefined);
  assert.equal(
    manifestAnswer?.domain,
    RENDER_PLAN_MANIFEST_HASH_DOMAIN,
  );
  const manifestPayload = bytesFromHex(manifestAnswer?.payload_hex ?? "");
  const manifest = unwrapPayload(
    decodeRenderPlanManifestPayload(manifestPayload),
  );
  assert.equal(validateRenderPlanManifest(manifest), true);
  assert.deepEqual(
    unwrapPayload(encodeRenderPlanManifestPayload(manifest)),
    manifestPayload,
  );
  const manifestPreimage = unwrapPayload(
    renderPlanManifestHashPreimage(manifest),
  );
  assert.deepEqual(
    manifestPreimage,
    bytesFromHex(manifestAnswer?.preimage_hex ?? ""),
  );
  assert.equal(
    createHash("sha256").update(manifestPreimage).digest("hex"),
    manifestAnswer?.sha256,
  );
});

test("capability collections distinguish complete from explicit truncation", () => {
  const requirement = {
    id: 1,
    capability: 7,
    parameter: 0n,
    context: {
      code: 1,
      value: 0n,
    },
    dependencies: [],
    scope: {
      kind: 1,
    },
    contributor_ids: [],
  };
  const decision = {
    decision_schema_version: 1,
    status: SupportStatus.Unsupported,
    profile: 1,
    profile_version: 1,
    policy_version: 1,
    subject: {
      source: {
        stable_id: new Uint8Array(32).fill(1),
        revision: 1n,
      },
      document_revision: 1n,
      revision_startxref: 10n,
      page_index: 0,
      page_object_number: 1,
      page_object_generation: 0,
      scene_schema_major: 1,
      scene_schema_minor: 0,
      scene_hash: new Uint8Array(32).fill(2),
    },
    missing: [requirement],
    missing_total: 2,
    missing_completeness: CollectionCompleteness.Complete,
    contributors: [],
    contributors_total: 0,
    contributors_completeness: CollectionCompleteness.Complete,
    locations_total: 2,
    locations_completeness: CollectionCompleteness.Truncated,
    evaluated_requirements: 2,
    evaluated_dependencies: 0,
    evaluated_parameters: 2,
    evaluated_commands: 0,
    evaluated_resources: 0,
    scope: {
      kind: 1,
    },
  };

  assert.equal(validateCapabilityDecision(decision), false);
  assert.equal(
    validateCapabilityDecision({
      ...decision,
      missing_completeness: CollectionCompleteness.Truncated,
    }),
    true,
  );
});

test("render plan fixtures bind viewport context and tile content hashes", () => {
  const manifest = {
    plan_schema_version: 1,
    document_revision: 1n,
    render_config: new Uint8Array(32).fill(1),
    renderer_epoch: 1,
    plan_id: 5n,
    generation: 5n,
    scene_hash: new Uint8Array(32).fill(2),
    decision_hash: new Uint8Array(32).fill(3),
    geometry_hash: new Uint8Array(32).fill(4),
    viewport_clip: {
      page_index: 0,
      x: 0,
      y: 0,
      width: 4,
      height: 2,
      coordinate_space: SurfaceCoordinateSpace.DevicePixelsTopLeft,
    },
    zoom_numerator: 3,
    zoom_denominator: 2,
    device_scale_milli: 1_000,
    rotation: PageRotation.Degrees0,
    optional_content: 0n,
    annotation_revision: 0n,
    backend: NativeBackend.ReferenceCpu,
    output_profile: OutputProfile.Srgb,
    quality: QualityPolicy.Preview,
    regions: [
      {
        page_index: 0,
        x: 0,
        y: 0,
        width: 2,
        height: 2,
        coordinate_space: SurfaceCoordinateSpace.DevicePixelsTopLeft,
      },
      {
        page_index: 0,
        x: 2,
        y: 0,
        width: 2,
        height: 2,
        coordinate_space: SurfaceCoordinateSpace.DevicePixelsTopLeft,
      },
    ],
    tile_content_hashes: [
      new Uint8Array(32).fill(5),
      new Uint8Array(32).fill(6),
    ],
  };

  assert.equal(validateRenderPlanManifest(manifest), true);
  assert.equal(
    validateRenderPlanManifest({
      ...manifest,
      tile_content_hashes: manifest.tile_content_hashes.slice(0, 1),
    }),
    false,
  );
  assert.equal(
    validateRenderPlanManifest({
      ...manifest,
      zoom_numerator: 6,
      zoom_denominator: 4,
    }),
    false,
  );
});

test("surface layout, range, and reclaim reason are enforced", () => {
  const surface = {
    metadata: {
      id: 1n,
      lease_token: 5n,
      owner: {
        worker: 1n,
        session: 2n,
      },
      generation: 3n,
      region: {
        page_index: 0,
        x: 0,
        y: 0,
        width: 2,
        height: 1,
        coordinate_space: SurfaceCoordinateSpace.DevicePixelsTopLeft,
      },
      width: 2,
      height: 1,
      stride: 8,
      format: PixelFormat.Rgba8,
      alpha: AlphaMode.Premultiplied,
      byte_offset: 0n,
      byte_length: 8n,
      render_config: new Uint8Array(32).fill(1),
      renderer_epoch: 1,
      plan_id: 1n,
      plan_hash: new Uint8Array(32).fill(2),
      scene_hash: new Uint8Array(32).fill(3),
      decision_hash: new Uint8Array(32).fill(4),
      backend: NativeBackend.ReferenceCpu,
    },
    transport: {
      kind: "BrowserImageBitmap",
      slot: 0,
      width: 2,
      height: 1,
    },
  };

  assert.equal(validateSurfaceReadyEvent(surface), true);
  assert.equal(
    validateSurfaceReadyEvent({
      ...surface,
      metadata: {
        ...surface.metadata,
        stride: 4,
        byte_length: 4n,
      },
    }),
    false,
  );
  assert.equal(
    validateSurfaceReadyEvent({
      ...surface,
      metadata: {
        ...surface.metadata,
        byte_offset: 1n,
      },
    }),
    false,
  );
  assert.equal(
    validateSurfaceReclaimedEvent({
      surface: 1n,
      lease_token: 5n,
      reason: SurfaceReclaimReason.ReleasedByHost,
    }),
    true,
  );
  assert.equal(validateSurfaceReclaimedEvent({ surface: 1n }), false);
  assert.equal(SCHEMA_SHA256_HEX.length, 64);
});
