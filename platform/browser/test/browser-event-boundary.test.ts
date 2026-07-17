import assert from "node:assert/strict";
import test from "node:test";

import {
  EndpointCapability,
  EndpointRole,
  EnvelopeSequenceTracker,
  MAX_MESSAGE_BYTES,
  MAX_TRANSFER_SLOTS,
  MESSAGE_ID_ENGINE_HELLO,
  MESSAGE_ID_READY,
  PROTOCOL_MAJOR,
  PROTOCOL_MINOR,
  SCHEMA_HASH,
  encodeCorrelationPayload,
  encodeEngineHelloEventPayload,
  encodeEventPayload,
  encodeReadyEventPayload,
  negotiateHandshake,
  type CompatibleHandshake,
  type Correlation,
  type Event,
  type EventEnvelope,
  type PayloadCodecResult,
  type ProtocolHello,
} from "../generated/engine-protocol.js";
import {
  BROWSER_CONTROL_HEADER_BYTES,
} from "../src/browser-command-boundary.js";
import {
  BrowserEventBoundary,
  BrowserEventBoundaryError,
  decodeBrowserEngineHello,
  type BrowserEventBoundaryErrorCode,
} from "../src/browser-event-boundary.js";

const unwrap = <T>(result: PayloadCodecResult<T>): T => {
  if (result.ok) {
    return result.value;
  }
  throw new Error(result.error.code);
};

const hello = (role: EndpointRole): ProtocolHello => ({
  major: PROTOCOL_MAJOR,
  minor: PROTOCOL_MINOR,
  schema_hash: SCHEMA_HASH.slice(),
  endpoint_role: role,
  capabilities: {
    supported: EndpointCapability.TransferableArrayBuffer,
    mandatory: 0n,
  },
  max_message_bytes: MAX_MESSAGE_BYTES,
  max_transfer_slots: MAX_TRANSFER_SLOTS,
});

const connection = (): CompatibleHandshake => {
  const result = negotiateHandshake(
    hello(EndpointRole.Host),
    hello(EndpointRole.Engine),
  );
  if (result === undefined) {
    throw new Error("test handshake must be compatible");
  }
  return result;
};

const eventRecord = (event: Event): Uint8Array => {
  switch (event.type) {
    case "EngineHello":
      return unwrap(encodeEngineHelloEventPayload(event.payload));
    case "Ready":
      return unwrap(encodeReadyEventPayload(event.payload));
    default:
      throw new Error("unsupported test event");
  }
};

const eventMessageId = (event: Event): number => {
  switch (event.type) {
    case "EngineHello":
      return MESSAGE_ID_ENGINE_HELLO;
    case "Ready":
      return MESSAGE_ID_READY;
    default:
      throw new Error("unsupported test event");
  }
};

const frame = (
  event: Event,
  correlation: Correlation,
  sequence: bigint,
  resources: readonly unknown[] = [],
): unknown[] => {
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
      message_type: eventMessageId(event),
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

const engineHello = (): Event => ({
  type: "EngineHello",
  payload: {
    hello: hello(EndpointRole.Engine),
    execution_capabilities: { supported: 0n },
  },
});

const ready = (worker: bigint): Event => ({
  type: "Ready",
  payload: {
    worker,
    negotiated_minor: PROTOCOL_MINOR,
    schema_hash: SCHEMA_HASH.slice(),
    execution_capabilities: { supported: 0n },
    capability_profiles: [1],
    output_profiles: [1],
  },
});

const assertBoundaryError = (
  operation: () => unknown,
  code: BrowserEventBoundaryErrorCode,
): void => {
  assert.throws(
    operation,
    (error: unknown) =>
      error instanceof BrowserEventBoundaryError
      && error.code === code
      && error.message === code,
  );
};

test("accepts exactly one bootstrap EngineHello and commits its sequence", () => {
  const sequence = new EnvelopeSequenceTracker();
  const input = frame(
    engineHello(),
    { worker: 7n },
    1n,
  );
  const accepted = decodeBrowserEngineHello(
    input,
    7n,
    sequence,
    () => true,
  );

  assert.equal(accepted.event.type, "EngineHello");
  assert.equal(sequence.lastAccepted, 1n);
  assertBoundaryError(
    () => decodeBrowserEngineHello(input, 7n, sequence, () => true),
    "NonMonotonicSequence",
  );
});

test("EngineHello negotiation rejection does not consume its sequence", () => {
  const sequence = new EnvelopeSequenceTracker();
  const input = frame(engineHello(), { worker: 7n }, 1n);

  assertBoundaryError(
    () => decodeBrowserEngineHello(input, 7n, sequence, () => false),
    "InvalidLifecycle",
  );
  assert.equal(sequence.lastAccepted, undefined);
  assert.equal(
    decodeBrowserEngineHello(input, 7n, sequence, () => true)
      .header.sequence,
    1n,
  );
});

test("lifecycle and stale-epoch rejection do not consume a sequence", () => {
  const sequence = new EnvelopeSequenceTracker();
  let allow = false;
  const boundary = new BrowserEventBoundary(
    7n,
    connection(),
    sequence,
    () => allow ? undefined : "InvalidLifecycle",
  );
  const first = frame(ready(7n), { worker: 7n }, 1n);

  assertBoundaryError(() => boundary.decode(first), "InvalidLifecycle");
  assert.equal(boundary.lastAcceptedSequence, undefined);
  allow = true;
  assert.equal(boundary.decode(first).envelope.event.type, "Ready");
  assert.equal(boundary.lastAcceptedSequence, 1n);

  const stale = frame(ready(8n), { worker: 8n }, 2n);
  assertBoundaryError(() => boundary.decode(stale), "StaleWorker");
  assert.equal(boundary.lastAcceptedSequence, 1n);
  const current = frame(ready(7n), { worker: 7n }, 2n);
  assert.equal(boundary.decode(current).envelope.header.sequence, 2n);
});

test("rejects unexpected resources and accessor-backed tables before use", () => {
  const boundary = new BrowserEventBoundary(
    7n,
    connection(),
    new EnvelopeSequenceTracker(),
    () => undefined,
  );
  assertBoundaryError(
    () =>
      boundary.decode(
        frame(ready(7n), { worker: 7n }, 1n, [new ArrayBuffer(1)]),
      ),
    "InvalidEnvelope",
  );

  const accessorFrame = frame(ready(7n), { worker: 7n }, 1n);
  let reads = 0;
  Object.defineProperty(accessorFrame, "0", {
    enumerable: true,
    configurable: true,
    get: () => {
      reads += 1;
      return new ArrayBuffer(1);
    },
  });
  assertBoundaryError(
    () => boundary.decode(accessorFrame),
    "InvalidResourceTable",
  );
  assert.equal(reads, 0);
});

test("rejects a prototype-forged sequence tracker as configuration", () => {
  const forged = Object.create(
    EnvelopeSequenceTracker.prototype,
  ) as EnvelopeSequenceTracker;
  assertBoundaryError(
    () =>
      decodeBrowserEngineHello(
        frame(engineHello(), { worker: 7n }, 1n),
        7n,
        forged,
        () => true,
      ),
    "InvalidConfiguration",
  );
});
