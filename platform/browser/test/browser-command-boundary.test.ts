import assert from "node:assert/strict";
import test from "node:test";

import {
  BROWSER_CONTROL_HEADER_BYTES,
  BrowserBoundaryError,
  BrowserCommandBoundary,
  type BrowserBoundaryErrorCode,
} from "../src/browser-command-boundary.js";
import {
  BrowserCommandAdmission,
} from "../src/browser-command-admission.js";
import {
  DataAttachmentRole,
  EndpointCapability,
  EndpointRole,
  EnvelopeSequenceTracker,
  MAX_MESSAGE_BYTES,
  MAX_TRANSFER_SLOTS,
  MESSAGE_ID_CANCEL,
  MESSAGE_ID_GET_PAGE_METRICS,
  MESSAGE_ID_OPEN,
  MESSAGE_ID_PROVIDE_DATA,
  MESSAGE_ID_RELEASE_SURFACE,
  MESSAGE_ID_SET_VIEWPORT,
  MESSAGE_ID_SHUTDOWN,
  OutputProfile,
  PageRotation,
  PROTOCOL_MAJOR,
  PROTOCOL_MINOR,
  QualityPolicy,
  SCHEMA_HASH,
  encodeCancelCommandPayload,
  encodeCommandPayload,
  encodeCorrelationPayload,
  encodeGetPageMetricsCommandPayload,
  encodeOpenCommandPayload,
  encodeProvideDataCommandPayload,
  encodeReleaseSurfaceCommandPayload,
  encodeSetViewportCommandPayload,
  encodeShutdownCommandPayload,
  negotiateHandshake,
  type Command,
  type CommandEnvelope,
  type CompatibleHandshake,
  type Correlation,
  type PayloadCodecResult,
  type ProvideDataCommand,
  type ProtocolHello,
} from "../generated/engine-protocol.js";

const sourceId = new Uint8Array(32).fill(0x51);
const TEST_ADMISSION_LIMITS = Object.freeze({
  maxSessions: 4,
  maxRequests: 8,
  maxSurfaces: 8,
});

interface HandshakeOptions {
  readonly supported?: bigint;
  readonly maxMessageBytes?: number;
  readonly maxTransferSlots?: number;
}

const hello = (
  endpointRole: EndpointRole,
  options: HandshakeOptions = {},
): ProtocolHello => ({
  major: PROTOCOL_MAJOR,
  minor: PROTOCOL_MINOR,
  schema_hash: SCHEMA_HASH.slice(),
  endpoint_role: endpointRole,
  capabilities: {
    supported:
      options.supported ?? EndpointCapability.TransferableArrayBuffer,
    mandatory: 0n,
  },
  max_message_bytes: options.maxMessageBytes ?? MAX_MESSAGE_BYTES,
  max_transfer_slots: options.maxTransferSlots ?? MAX_TRANSFER_SLOTS,
});

const compatibleHandshake = (
  options: HandshakeOptions = {},
): CompatibleHandshake => {
  const connection = negotiateHandshake(
    hello(EndpointRole.Host, options),
    hello(EndpointRole.Engine, options),
  );
  if (connection === undefined) {
    throw new Error("test handshake must be compatible");
  }
  return connection;
};

const readyAdmission = (): BrowserCommandAdmission => {
  const admission = new BrowserCommandAdmission(
    "Ready",
    TEST_ADMISSION_LIMITS,
  );
  admission.setSessionState(2n, "Ready");
  return admission;
};

const boundaryFor = (
  connection = compatibleHandshake(),
  admission = readyAdmission(),
): BrowserCommandBoundary =>
  new BrowserCommandBoundary(1n, connection, admission);

const unwrap = <T>(result: PayloadCodecResult<T>): T => {
  if (result.ok) {
    return result.value;
  }
  throw new Error(result.error.code);
};

const provideDataCommand = (
  lengths: readonly number[] = [4],
): ProvideDataCommand => ({
  ticket: 3n,
  source: {
    stable_id: sourceId.slice(),
    revision: 4n,
  },
  segments: lengths.map((length, index) => ({
    range: {
      start: index === 0 ? 0n : BigInt(index * 5),
      len: BigInt(length),
    },
    slot: index,
    byte_length: BigInt(length),
    role: DataAttachmentRole.ImmutableRangeBytes,
  })),
});

const encodePayloadRecord = (
  command: Command,
): Uint8Array => {
  switch (command.type) {
    case "Cancel":
      return unwrap(encodeCancelCommandPayload(command.payload));
    case "GetPageMetrics":
      return unwrap(encodeGetPageMetricsCommandPayload(command.payload));
    case "Open":
      return unwrap(encodeOpenCommandPayload(command.payload));
    case "ProvideData":
      return unwrap(encodeProvideDataCommandPayload(command.payload));
    case "ReleaseSurface":
      return unwrap(encodeReleaseSurfaceCommandPayload(command.payload));
    case "SetViewport":
      return unwrap(encodeSetViewportCommandPayload(command.payload));
    case "Shutdown":
      return unwrap(encodeShutdownCommandPayload(command.payload));
    default:
      throw new Error("unsupported test command");
  }
};

const messageId = (command: Command): number => {
  switch (command.type) {
    case "Cancel":
      return MESSAGE_ID_CANCEL;
    case "GetPageMetrics":
      return MESSAGE_ID_GET_PAGE_METRICS;
    case "Open":
      return MESSAGE_ID_OPEN;
    case "ProvideData":
      return MESSAGE_ID_PROVIDE_DATA;
    case "ReleaseSurface":
      return MESSAGE_ID_RELEASE_SURFACE;
    case "SetViewport":
      return MESSAGE_ID_SET_VIEWPORT;
    case "Shutdown":
      return MESSAGE_ID_SHUTDOWN;
    default:
      throw new Error("unsupported test command");
  }
};

const binaryFrame = (
  command: Command,
  correlation: Correlation,
  sequence: bigint,
  resources: readonly unknown[] = [],
): unknown[] => {
  const encodedCorrelation = unwrap(encodeCorrelationPayload(correlation));
  const encodedRecord = encodePayloadRecord(command);
  const payloadLength =
    encodedCorrelation.byteLength + encodedRecord.byteLength;
  const envelope: CommandEnvelope = {
    header: {
      major: PROTOCOL_MAJOR,
      minor: PROTOCOL_MINOR,
      message_type: messageId(command),
      flags: 0,
      payload_len: payloadLength,
      sequence,
    },
    correlation,
    command,
  };
  const encoded = unwrap(encodeCommandPayload(envelope));
  assert.equal(encoded.bytes.byteLength, payloadLength);

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
  new Uint8Array(control, BROWSER_CONTROL_HEADER_BYTES).set(encoded.bytes);
  return [control, ...resources];
};

const provideDataFrame = (
  sequence = 1n,
  worker = 1n,
  buffers: readonly unknown[] = [new ArrayBuffer(4)],
  lengths: readonly number[] = [4],
  session = 2n,
): unknown[] =>
  binaryFrame(
    { type: "ProvideData", payload: provideDataCommand(lengths) },
    { worker, session },
    sequence,
    buffers,
  );

const openFrame = (
  sequence: bigint,
  request: bigint,
): unknown[] =>
  binaryFrame(
    {
      type: "Open",
      payload: {
        source: {
          identity: {
            stable_id: sourceId.slice(),
            revision: 1n,
          },
          length: 4n,
          validator: new Uint8Array(32).fill(0x52),
        },
      },
    },
    { worker: 1n, request },
    sequence,
  );

const getPageMetricsFrame = (
  sequence: bigint,
  request: bigint,
  session = 2n,
): unknown[] =>
  binaryFrame(
    {
      type: "GetPageMetrics",
      payload: {
        document_revision: 1n,
        start_index: 0,
        max_count: 1,
      },
    },
    { worker: 1n, session, request },
    sequence,
  );

const shutdownFrame = (
  sequence = 1n,
  resources: readonly unknown[] = [],
): unknown[] =>
  binaryFrame(
    { type: "Shutdown", payload: { deadline_ms: 1_000 } },
    { worker: 1n },
    sequence,
    resources,
  );

const cancelFrame = (
  sequence: bigint,
  request: bigint,
  session = 2n,
): unknown[] =>
  binaryFrame(
    { type: "Cancel", payload: { target: request } },
    { worker: 1n, session, request },
    sequence,
  );

const setViewportFrame = (
  sequence: bigint,
  generation: bigint,
  session = 2n,
): unknown[] =>
  binaryFrame(
    {
      type: "SetViewport",
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
    },
    { worker: 1n, session, generation },
    sequence,
  );

const releaseSurfaceFrame = (
  sequence: bigint,
  surface: bigint,
  session = 2n,
): unknown[] =>
  binaryFrame(
    {
      type: "ReleaseSurface",
      payload: { surface, lease_token: 7n },
    },
    { worker: 1n, session },
    sequence,
  );

const controlOf = (frame: readonly unknown[]): ArrayBuffer => {
  const control = frame[0];
  assert.ok(control instanceof ArrayBuffer);
  return control;
};

const copyWithControlMutation = (
  frame: readonly unknown[],
  mutate: (control: ArrayBuffer) => void,
): unknown[] => {
  const control = controlOf(frame).slice(0);
  mutate(control);
  return [control, ...frame.slice(1)];
};

const assertBoundaryError = (
  operation: () => unknown,
  code: BrowserBoundaryErrorCode,
): void => {
  assert.throws(
    operation,
    (error: unknown) =>
      error instanceof BrowserBoundaryError
      && error.code === code
      && error.message === code,
  );
};

test("decodes a canonical binary ProvideData frame and maps physical n+1 to logical n", () => {
  const connection = compatibleHandshake();
  const boundary = boundaryFor(connection);
  const first = new ArrayBuffer(4);
  const second = new ArrayBuffer(6);
  const accepted = boundary.decode(
    provideDataFrame(7n, 1n, [first, second], [4, 6]),
  );

  assert.equal(accepted.envelope.command.type, "ProvideData");
  assert.deepEqual(accepted.resources, [first, second]);
  assert.ok(Object.isFrozen(accepted.resources));
  assert.equal(boundary.lastAcceptedSequence, 7n);
});

test("requires authentic handshake and admission objects", () => {
  const connection = compatibleHandshake();
  const admission = readyAdmission();
  const boundary = new BrowserCommandBoundary(1n, connection, admission);
  assert.ok(Object.isFrozen(boundary));
  assert.ok(Object.isFrozen(BrowserCommandBoundary.prototype));
  assert.throws(
    () =>
      Object.defineProperty(boundary, "decode", {
        value: () => ({ forged: true }),
      }),
    TypeError,
  );
  assert.throws(
    () =>
      Object.defineProperty(BrowserCommandBoundary.prototype, "decode", {
        value: () => ({ forged: true }),
      }),
    TypeError,
  );
  assert.doesNotThrow(
    () => new BrowserCommandBoundary(1n, connection, admission),
  );

  assertBoundaryError(
    () => new BrowserCommandBoundary(0n, connection, admission),
    "InvalidConfiguration",
  );
  assertBoundaryError(
    () =>
      new BrowserCommandBoundary(
        1n,
        { ...connection, minor: PROTOCOL_MINOR - 1 },
        admission,
      ),
    "InvalidConfiguration",
  );

  const restricted = compatibleHandshake({
    supported: 0n,
    maxMessageBytes: 1,
    maxTransferSlots: 1,
  });
  assertBoundaryError(
    () =>
      new BrowserCommandBoundary(
        1n,
        {
          ...restricted,
          capabilities: EndpointCapability.TransferableArrayBuffer,
          max_message_bytes: MAX_MESSAGE_BYTES,
          max_transfer_slots: MAX_TRANSFER_SLOTS,
        },
        admission,
      ),
    "InvalidConfiguration",
  );

  const forgedAdmission = Object.create(
    BrowserCommandAdmission.prototype,
  ) as BrowserCommandAdmission;
  Object.defineProperty(forgedAdmission, "validate", {
    value: (): undefined => undefined,
  });
  assertBoundaryError(
    () =>
      new BrowserCommandBoundary(
        1n,
        connection,
        forgedAdmission,
      ),
    "InvalidConfiguration",
  );

  class DerivedBoundary extends BrowserCommandBoundary {}
  assertBoundaryError(
    () => new DerivedBoundary(1n, connection, admission),
    "InvalidConfiguration",
  );

  function ExoticBoundary(): void {}
  assertBoundaryError(
    () =>
      Reflect.construct(
        BrowserCommandBoundary,
        [1n, connection, admission],
        ExoticBoundary,
      ),
    "InvalidConfiguration",
  );
});

test("rejects global and handshake-negotiated oversized payloads before decoding", () => {
  const negotiatedBoundary = boundaryFor(
    compatibleHandshake({ maxMessageBytes: 14 }),
  );
  assertBoundaryError(
    () => negotiatedBoundary.decode(shutdownFrame()),
    "InvalidPayloadLength",
  );
  assert.equal(negotiatedBoundary.lastAcceptedSequence, undefined);

  const payloadLength = MAX_MESSAGE_BYTES + 1;
  const control = new ArrayBuffer(
    BROWSER_CONTROL_HEADER_BYTES + payloadLength,
  );
  const header = new DataView(control);
  header.setUint16(0, PROTOCOL_MAJOR, true);
  header.setUint16(2, PROTOCOL_MINOR, true);
  header.setUint16(4, MESSAGE_ID_SHUTDOWN, true);
  header.setUint16(6, 0, true);
  header.setUint32(8, payloadLength, true);
  header.setBigUint64(12, 1n, true);

  const globalBoundary = boundaryFor();
  assertBoundaryError(
    () => globalBoundary.decode([control]),
    "InvalidPayloadLength",
  );
  assert.equal(globalBoundary.lastAcceptedSequence, undefined);
});

test("admits only active lifecycle and correlation state before sequence commit", () => {
  const admission = readyAdmission();
  const boundary = boundaryFor(compatibleHandshake(), admission);

  assertBoundaryError(
    () =>
      boundary.decode(
        provideDataFrame(
          10n,
          1n,
          [new ArrayBuffer(4)],
          [4],
          3n,
        ),
      ),
    "StaleSession",
  );
  assert.equal(boundary.lastAcceptedSequence, undefined);

  boundary.decode(provideDataFrame(10n));
  assert.equal(boundary.lastAcceptedSequence, 10n);

  admission.setSessionState(3n, "Closing");
  assertBoundaryError(
    () =>
      boundary.decode(
        provideDataFrame(
          11n,
          1n,
          [new ArrayBuffer(4)],
          [4],
          3n,
        ),
      ),
    "StaleSession",
  );
  assert.equal(boundary.lastAcceptedSequence, 10n);

  assertBoundaryError(
    () => boundary.decode(cancelFrame(11n, 9n)),
    "StaleRequest",
  );
  assert.equal(boundary.lastAcceptedSequence, 10n);
  admission.setRequestState(9n, "Active", 2n);
  assertBoundaryError(
    () => boundary.decode(cancelFrame(11n, 9n, 3n)),
    "StaleRequest",
  );
  assert.equal(boundary.lastAcceptedSequence, 10n);
  boundary.decode(cancelFrame(11n, 9n));
  assert.equal(boundary.lastAcceptedSequence, 11n);

  admission.setActiveGeneration(2n, 5n);
  assertBoundaryError(
    () => boundary.decode(setViewportFrame(12n, 5n)),
    "StaleGeneration",
  );
  assert.equal(boundary.lastAcceptedSequence, 11n);
  boundary.decode(setViewportFrame(12n, 6n));
  assert.equal(boundary.lastAcceptedSequence, 12n);

  assertBoundaryError(
    () => boundary.decode(releaseSurfaceFrame(13n, 4n)),
    "StaleSurface",
  );
  assert.equal(boundary.lastAcceptedSequence, 12n);
  admission.setSurfaceState(4n, 2n, "Alive");
  assertBoundaryError(
    () => boundary.decode(releaseSurfaceFrame(13n, 4n, 3n)),
    "StaleSurface",
  );
  assert.equal(boundary.lastAcceptedSequence, 12n);
  boundary.decode(releaseSurfaceFrame(13n, 4n));
  assert.equal(boundary.lastAcceptedSequence, 13n);

  const startingAdmission = new BrowserCommandAdmission(
    "Starting",
    TEST_ADMISSION_LIMITS,
  );
  const startingBoundary = boundaryFor(
    compatibleHandshake(),
    startingAdmission,
  );
  assertBoundaryError(
    () => startingBoundary.decode(shutdownFrame(14n)),
    "InvalidLifecycle",
  );
  assert.equal(startingBoundary.lastAcceptedSequence, undefined);

  admission.setWorkerState("Draining");
  boundary.decode(shutdownFrame(14n));
  assert.equal(boundary.lastAcceptedSequence, 14n);

  startingAdmission.setWorkerState("Ready");
  startingAdmission.setWorkerState("Draining");
  startingBoundary.decode(shutdownFrame(14n));
  assert.equal(startingBoundary.lastAcceptedSequence, 14n);
});

test("rejects new request IDs at capacity before sequence commit", () => {
  const zeroAdmission = new BrowserCommandAdmission("Ready", {
    maxSessions: 1,
    maxRequests: 0,
    maxSurfaces: 0,
  });
  zeroAdmission.setSessionState(2n, "Ready");
  const zeroBoundary = boundaryFor(compatibleHandshake(), zeroAdmission);
  assertBoundaryError(
    () => zeroBoundary.decode(openFrame(1n, 7n)),
    "AdmissionCapacityExceeded",
  );
  assert.equal(zeroBoundary.lastAcceptedSequence, undefined);

  const fullAdmission = new BrowserCommandAdmission("Ready", {
    maxSessions: 1,
    maxRequests: 1,
    maxSurfaces: 0,
  });
  fullAdmission.setSessionState(2n, "Ready");
  fullAdmission.setRequestState(7n, "Active", 2n);
  const fullBoundary = boundaryFor(compatibleHandshake(), fullAdmission);
  assertBoundaryError(
    () => fullBoundary.decode(getPageMetricsFrame(1n, 7n)),
    "StaleRequest",
  );
  assert.equal(fullBoundary.lastAcceptedSequence, undefined);
  assertBoundaryError(
    () => fullBoundary.decode(getPageMetricsFrame(1n, 8n)),
    "AdmissionCapacityExceeded",
  );
  assert.equal(fullBoundary.lastAcceptedSequence, undefined);

  const availableAdmission = new BrowserCommandAdmission("Ready", {
    maxSessions: 1,
    maxRequests: 1,
    maxSurfaces: 0,
  });
  availableAdmission.setSessionState(2n, "Ready");
  const availableBoundary = boundaryFor(
    compatibleHandshake(),
    availableAdmission,
  );
  availableBoundary.decode(getPageMetricsFrame(1n, 8n));
  assert.doesNotThrow(
    () => availableAdmission.setRequestState(8n, "Active", 2n),
  );
  assert.equal(availableBoundary.lastAcceptedSequence, 1n);
});

test("ProvideData requires negotiated transferable ArrayBuffer capability", () => {
  const boundary = boundaryFor(
    compatibleHandshake({ supported: 0n }),
  );
  assertBoundaryError(
    () => boundary.decode(provideDataFrame()),
    "MissingCapability",
  );
  assert.equal(boundary.lastAcceptedSequence, undefined);
});

test("enforces negotiated and global resource-slot limits at exact boundaries", () => {
  const oneSlotBoundary = boundaryFor(
    compatibleHandshake({ maxTransferSlots: 1 }),
  );
  const accepted = oneSlotBoundary.decode(provideDataFrame());
  assert.equal(accepted.resources.length, 1);
  assert.equal(oneSlotBoundary.lastAcceptedSequence, 1n);

  const negotiatedOverflow = boundaryFor(
    compatibleHandshake({ maxTransferSlots: 1 }),
  );
  assertBoundaryError(
    () =>
      negotiatedOverflow.decode(
        provideDataFrame(
          1n,
          1n,
          [new ArrayBuffer(4), new ArrayBuffer(4)],
          [4, 4],
        ),
      ),
    "InvalidEnvelope",
  );
  assert.equal(negotiatedOverflow.lastAcceptedSequence, undefined);

  const globalExact = Array.from(
    { length: MAX_TRANSFER_SLOTS },
    () => new ArrayBuffer(0),
  );
  const globalBoundary = boundaryFor();
  assertBoundaryError(
    () => globalBoundary.decode(shutdownFrame(1n, globalExact)),
    "InvalidResourceBinding",
  );
  assert.equal(globalBoundary.lastAcceptedSequence, undefined);

  assertBoundaryError(
    () =>
      globalBoundary.decode(
        shutdownFrame(
          1n,
          [...globalExact, new ArrayBuffer(0)],
        ),
      ),
    "InvalidResourceTable",
  );
  assert.equal(globalBoundary.lastAcceptedSequence, undefined);
});

test("rejects truncation, payload-length mismatch, unknown messages, and noncanonical codec markers", () => {
  const cases: readonly [
    readonly unknown[],
    BrowserBoundaryErrorCode,
  ][] = [
    [[controlOf(provideDataFrame()).slice(0, 19)], "InvalidPayloadLength"],
    [
      copyWithControlMutation(provideDataFrame(), (control) => {
        new DataView(control).setUint32(
          8,
          control.byteLength - BROWSER_CONTROL_HEADER_BYTES - 1,
          true,
        );
      }),
      "InvalidPayloadLength",
    ],
    [
      copyWithControlMutation(provideDataFrame(), (control) => {
        new DataView(control).setUint16(4, 0xffff, true);
      }),
      "InvalidHeader",
    ],
    [
      copyWithControlMutation(provideDataFrame(), (control) => {
        // Correlation begins with WorkerId; byte 8 is the SessionId marker.
        new Uint8Array(control)[BROWSER_CONTROL_HEADER_BYTES + 8] = 2;
      }),
      "InvalidPayload",
    ],
  ];

  for (const [frame, code] of cases) {
    const boundary = boundaryFor();
    assertBoundaryError(() => boundary.decode(frame), code);
    assert.equal(boundary.lastAcceptedSequence, undefined);
  }
});

test("rejects stale Workers and commits a sequence only after every check succeeds", () => {
  const boundary = boundaryFor();

  assertBoundaryError(
    () => boundary.decode(provideDataFrame(8n, 2n)),
    "StaleWorker",
  );
  assert.equal(boundary.lastAcceptedSequence, undefined);

  boundary.decode(provideDataFrame(8n));
  assert.equal(boundary.lastAcceptedSequence, 8n);

  assert.equal(Object.isFrozen(EnvelopeSequenceTracker.prototype), true);
  assert.equal(
    Reflect.set(
      EnvelopeSequenceTracker.prototype,
      "pending",
      () => Object.freeze({ commit: () => true }),
    ),
    false,
  );
  assertBoundaryError(
    () => boundary.decode(provideDataFrame(8n)),
    "NonMonotonicSequence",
  );
  assertBoundaryError(
    () => boundary.decode(provideDataFrame(7n)),
    "NonMonotonicSequence",
  );
  assert.equal(boundary.lastAcceptedSequence, 8n);

  assertBoundaryError(
    () => boundary.decode(provideDataFrame(9n, 1n, [new ArrayBuffer(3)])),
    "InvalidResourceLength",
  );
  assert.equal(boundary.lastAcceptedSequence, 8n);

  boundary.decode(provideDataFrame(9n));
  assert.equal(boundary.lastAcceptedSequence, 9n);
});

test("requires an exact dense ordinary physical resource table", () => {
  const valid = provideDataFrame();
  const control = controlOf(valid);

  const hole: unknown[] = [control];
  hole.length = 2;

  const accessor: unknown[] = [control, new ArrayBuffer(4)];
  let accessorReads = 0;
  Object.defineProperty(accessor, "1", {
    configurable: true,
    enumerable: true,
    get: () => {
      accessorReads += 1;
      return new ArrayBuffer(4);
    },
  });

  const extraProperty = [control, new ArrayBuffer(4)] as unknown[] & {
    metadata?: string;
  };
  extraProperty.metadata = "not allowed";

  class ExoticResourceTable extends Array<unknown> {}
  const exotic = new ExoticResourceTable(control, new ArrayBuffer(4));
  const throwingProxy = new Proxy([control, new ArrayBuffer(4)], {
    ownKeys: () => {
      throw new Error("must be redacted");
    },
  });

  for (const table of [
    [],
    hole,
    accessor,
    extraProperty,
    exotic,
    throwingProxy,
  ]) {
    const boundary = boundaryFor();
    assertBoundaryError(
      () => boundary.decode(table),
      "InvalidResourceTable",
    );
    assert.equal(boundary.lastAcceptedSequence, undefined);
  }
  assert.equal(accessorReads, 0);
});

test("rejects missing, extra, aliased, and control-reused logical slots", () => {
  const normal = provideDataFrame();
  const control = controlOf(normal);
  const shared = new ArrayBuffer(4);
  const cases: readonly unknown[][] = [
    [control],
    [control, new ArrayBuffer(4), new ArrayBuffer(4)],
    [control, control],
    provideDataFrame(1n, 1n, [shared, shared], [4, 4]),
  ];

  for (const table of cases) {
    const boundary = boundaryFor();
    assertBoundaryError(
      () => boundary.decode(table),
      "InvalidResourceBinding",
    );
    assert.equal(boundary.lastAcceptedSequence, undefined);
  }
});

test("accepts only fixed non-resizable ArrayBuffer data resources of exact length", () => {
  const wrongResources: readonly unknown[] = [
    new Uint8Array(4),
    new DataView(new ArrayBuffer(4)),
    new SharedArrayBuffer(4),
    new ArrayBuffer(4, { maxByteLength: 8 }),
    { byteLength: 4 },
  ];
  for (const resource of wrongResources) {
    const boundary = boundaryFor();
    assertBoundaryError(
      () => boundary.decode(provideDataFrame(1n, 1n, [resource])),
      "InvalidResourceType",
    );
    assert.equal(boundary.lastAcceptedSequence, undefined);
  }

  const boundary = boundaryFor();
  assertBoundaryError(
    () =>
      boundary.decode(
        provideDataFrame(1n, 1n, [new ArrayBuffer(3)], [4]),
      ),
    "InvalidResourceLength",
  );
  assert.equal(boundary.lastAcceptedSequence, undefined);
});

test("requires physical index zero to be a fixed control ArrayBuffer", () => {
  const resizableControl = new ArrayBuffer(64, { maxByteLength: 128 });
  const cases: readonly unknown[][] = [
    [new Uint8Array(64)],
    [new SharedArrayBuffer(64)],
    [resizableControl],
  ];
  for (const table of cases) {
    const boundary = boundaryFor();
    assertBoundaryError(
      () => boundary.decode(table),
      "InvalidControlResource",
    );
    assert.equal(boundary.lastAcceptedSequence, undefined);
  }
});

test("commands without resource descriptors reject every logical resource", () => {
  const boundary = boundaryFor();
  assertBoundaryError(
    () => boundary.decode(shutdownFrame(1n, [new ArrayBuffer(1)])),
    "InvalidResourceBinding",
  );
  assert.equal(boundary.lastAcceptedSequence, undefined);

  const accepted = boundary.decode(shutdownFrame());
  assert.equal(accepted.envelope.command.type, "Shutdown");
  assert.deepEqual(accepted.resources, []);
  assert.equal(boundary.lastAcceptedSequence, 1n);
});
