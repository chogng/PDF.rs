import assert from "node:assert/strict";
import test from "node:test";

import {
  CapabilityProfileId,
  EndpointRole,
  EnvelopeSequenceTracker,
  KNOWN_ENDPOINT_CAPABILITIES,
  MAX_MESSAGE_BYTES,
  MAX_TRANSFER_SLOTS,
  MESSAGE_ID_ENGINE_HELLO,
  MESSAGE_ID_HELLO,
  MESSAGE_ID_HELLO_ACCEPT,
  MESSAGE_ID_READY,
  OutputProfile,
  PROTOCOL_MAJOR,
  PROTOCOL_MINOR,
  SCHEMA_HASH_HEX,
  encodeCommandPayload,
  encodeCorrelationPayload,
  encodeEngineHelloEventPayload,
  encodeEventPayload,
  encodeHelloAcceptCommandPayload,
  encodeHelloCommandPayload,
  encodeReadyEventPayload,
  type Command,
  type CommandEnvelope,
  type CompatibleHandshake,
  type Correlation,
  type Event,
  type EventEnvelope,
  type PayloadCodecResult,
  type ProtocolHello,
} from "../generated/engine-protocol.js";
import {
  NATIVE_WORKER_ABI_HASH_WORDS,
  NATIVE_WORKER_ABI_VERSION,
  NATIVE_WORKER_POLL_PENDING,
  NATIVE_WORKER_STATUS_INTERNAL_UNWIND,
  NATIVE_WORKER_STATUS_REJECTED,
} from "../src/native-worker-abi.generated.js";
import {
  BrowserNativeWorkerError,
  BrowserNativeWorkerInstance,
  BrowserNativeWorkerLoader,
  inspectNativeWorkerMemory,
  nativeWorkerMaximumArtifactBytes,
  nativeWorkerSha256,
  type BrowserNativeWorkerSupervisorIdentity,
} from "../src/browser-native-worker-loader.js";
import {
  decodeBrowserEngineHello,
} from "../src/browser-event-boundary.js";
import { negotiateBrowserHello } from "../src/browser-handshake.js";

const uleb = (input: number): number[] => {
  let value = input >>> 0;
  const output: number[] = [];
  do {
    let byte = value & 0x7f;
    value >>>= 7;
    if (value !== 0) {
      byte |= 0x80;
    }
    output.push(byte);
  } while (value !== 0);
  return output;
};

const sleb32 = (input: number): number[] => {
  let value = BigInt.asIntN(32, BigInt(input));
  const output: number[] = [];
  while (true) {
    let byte = Number(value & 0x7fn);
    value >>= 7n;
    const sign = (byte & 0x40) !== 0;
    if (
      (value === 0n && !sign)
      || (value === -1n && sign)
    ) {
      output.push(byte);
      return output;
    }
    byte |= 0x80;
    output.push(byte);
  }
};

const utf8 = (value: string): number[] => {
  const bytes = new TextEncoder().encode(value);
  return [...uleb(bytes.byteLength), ...bytes];
};

const section = (id: number, bytes: readonly number[]): number[] => [
  id,
  ...uleb(bytes.length),
  ...bytes,
];

interface TestModuleOptions {
  readonly maximumPages?: number;
  readonly initializeStatus?: number;
  readonly initializeTraps?: boolean;
  readonly engineHelloStatus?: number;
  readonly engineHelloTraps?: boolean;
  readonly engineHelloOutput?: (
    worker: bigint,
  ) => Uint8Array<ArrayBuffer> | undefined;
  readonly readyStatus?: number;
  readonly readyTraps?: boolean;
  readonly readyOutput?: (
    worker: bigint,
  ) => Uint8Array<ArrayBuffer> | undefined;
  readonly dispatchStatus?: number;
  readonly dispatchTraps?: boolean;
  readonly pollStatus?: number;
  readonly pollTraps?: boolean;
  readonly dispatchGrowsMemory?: boolean;
  readonly outputLength?: number;
  readonly transferCount?: number;
  readonly includeExtraExport?: boolean;
  readonly wrongPrepareInputSignature?: boolean;
  readonly wrongAbiHash?: boolean;
}

const nativeTestModule = (
  options: TestModuleOptions = {},
): Uint8Array<ArrayBuffer> => {
  const hashWords: number[] = [...NATIVE_WORKER_ABI_HASH_WORDS];
  if (options.wrongAbiHash === true) {
    hashWords[0] = (hashWords[0] ?? 0) ^ 1;
  }
  const specs = [
    {
      name: "pdf_rs_worker_initialize",
      type: 4,
      value: options.initializeStatus ?? 0,
    },
    {
      name: "pdf_rs_worker_prepare_input",
      type: options.wrongPrepareInputSignature === true ? 2 : 0,
      value: 1_024,
    },
    {
      name: "pdf_rs_worker_prepare_transfer",
      type: 1,
      value: 2_048,
    },
    {
      name: "pdf_rs_worker_dispatch",
      type: 1,
      value: options.dispatchStatus ?? 0,
    },
    {
      name: "pdf_rs_worker_poll",
      type: 2,
      value: options.pollStatus ?? 0,
    },
    {
      name: "pdf_rs_worker_output_pointer",
      type: 2,
      value: 4_096,
    },
    {
      name: "pdf_rs_worker_output_length",
      type: 2,
      value: options.outputLength ?? 0,
    },
    {
      name: "pdf_rs_worker_transfer_count",
      type: 2,
      value: options.transferCount ?? 0,
    },
    {
      name: "pdf_rs_worker_transfer_pointer",
      type: 0,
      value: 8_192,
    },
    {
      name: "pdf_rs_worker_transfer_length",
      type: 0,
      value: 0,
    },
    {
      name: "pdf_rs_worker_memory_epoch",
      type: 2,
      value: 1,
    },
    {
      name: "pdf_rs_worker_shutdown",
      type: 2,
      value: 0,
    },
    {
      name: "pdf_rs_worker_abi_version",
      type: 2,
      value: NATIVE_WORKER_ABI_VERSION,
    },
    ...hashWords.map((value, index) => ({
      name: `pdf_rs_worker_abi_hash_${index}`,
      type: 2,
      value,
    })),
    { name: "main", type: 1, value: 0 },
  ] as const;
  const type = section(1, [
    5,
    0x60,
    1,
    0x7f,
    1,
    0x7f,
    0x60,
    2,
    0x7f,
    0x7f,
    1,
    0x7f,
    0x60,
    0,
    1,
    0x7f,
    0x60,
    0,
    0,
    0x60,
    5,
    0x7f,
    0x7f,
    0x7f,
    0x7f,
    0x7f,
    1,
    0x7f,
  ]);
  const functions = section(3, [
    ...uleb(specs.length),
    ...specs.map((spec) => spec.type),
  ]);
  const memory = section(5, [
    1,
    1,
    1,
    ...uleb(options.maximumPages ?? 1_024),
  ]);
  const globals = section(6, [
    2,
    0x7f,
    0,
    0x41,
    ...sleb32(8_192),
    0x0b,
    0x7f,
    0,
    0x41,
    ...sleb32(16_384),
    0x0b,
  ]);
  const exportRecords: number[] = [
    ...utf8("memory"),
    2,
    0,
    ...specs.flatMap((spec, index) => [
      ...utf8(spec.name),
      0,
      ...uleb(index),
    ]),
    ...utf8("__data_end"),
    3,
    0,
    ...utf8("__heap_base"),
    3,
    1,
  ];
  if (options.includeExtraExport === true) {
    exportRecords.push(...utf8("unexpected"), 0, 0);
  }
  const exports = section(7, [
    ...uleb(
      1 + specs.length + 2
        + (options.includeExtraExport === true ? 1 : 0),
    ),
    ...exportRecords,
  ]);
  const bodies = specs.map((spec) => {
    const instructions = spec.value === undefined
      ? []
      : spec.name === "pdf_rs_worker_initialize"
      && options.initializeTraps === true
      ? [0x00]
      : spec.name === "pdf_rs_worker_dispatch"
      && options.dispatchTraps === true
      ? [0x00]
      : spec.name === "pdf_rs_worker_poll"
      && options.pollTraps === true
      ? [0x00]
      : spec.name === "pdf_rs_worker_dispatch"
      && options.dispatchGrowsMemory === true
      ? [0x41, 1, 0x40, 0, 0x1a, 0x41, 0]
      : [0x41, ...sleb32(spec.value)];
    const body = [0, ...instructions, 0x0b];
    return [...uleb(body.length), ...body];
  });
  const code = section(10, [
    ...uleb(bodies.length),
    ...bodies.flat(),
  ]);
  return Uint8Array.from([
    0,
    0x61,
    0x73,
    0x6d,
    1,
    0,
    0,
    0,
    ...type,
    ...functions,
    ...memory,
    ...globals,
    ...exports,
    ...code,
  ]);
};

const schemaHash = (): Uint8Array<ArrayBuffer> =>
  Uint8Array.from(
    SCHEMA_HASH_HEX.match(/.{2}/gu)?.map((byte) =>
      Number.parseInt(byte, 16),
    ) ?? [],
  );

const hello = (role: EndpointRole): ProtocolHello => ({
  major: PROTOCOL_MAJOR,
  minor: PROTOCOL_MINOR,
  schema_hash: schemaHash(),
  endpoint_role: role,
  capabilities: {
    supported: KNOWN_ENDPOINT_CAPABILITIES,
    mandatory: 0n,
  },
  max_message_bytes: MAX_MESSAGE_BYTES,
  max_transfer_slots: MAX_TRANSFER_SLOTS,
});

const connection = (): CompatibleHandshake =>
  negotiateBrowserHello(
    hello(EndpointRole.Host),
    hello(EndpointRole.Engine),
  );

const supervisorIdentity = (
  overrides: Partial<BrowserNativeWorkerSupervisorIdentity> = {},
): BrowserNativeWorkerSupervisorIdentity => Object.freeze({
  worker: 1n,
  workerEpoch: 1n,
  rendererEpoch: 1,
  ...overrides,
});

const unwrap = <T>(result: PayloadCodecResult<T>): T => {
  if (result.ok) {
    return result.value;
  }
  throw new Error(result.error.code);
};

const encodedFrame = (
  messageId: number,
  payload: Uint8Array,
  sequence: bigint,
): Uint8Array<ArrayBuffer> => {
  const frame = new Uint8Array(20 + payload.byteLength);
  const header = new DataView(frame.buffer);
  header.setUint16(0, PROTOCOL_MAJOR, true);
  header.setUint16(2, PROTOCOL_MINOR, true);
  header.setUint16(4, messageId, true);
  header.setUint16(6, 0, true);
  header.setUint32(8, payload.byteLength, true);
  header.setBigUint64(12, sequence, true);
  frame.set(payload, 20);
  return frame;
};

const commandRecord = (command: Command): Uint8Array => {
  switch (command.type) {
    case "Hello":
      return unwrap(encodeHelloCommandPayload(command.payload));
    case "HelloAccept":
      return unwrap(
        encodeHelloAcceptCommandPayload(command.payload),
      );
    default:
      throw new Error("unsupported native test command");
  }
};

const commandMessageId = (command: Command): number => {
  switch (command.type) {
    case "Hello":
      return MESSAGE_ID_HELLO;
    case "HelloAccept":
      return MESSAGE_ID_HELLO_ACCEPT;
    default:
      throw new Error("unsupported native test command");
  }
};

const commandFrame = (
  command: Command,
  worker: bigint,
  sequence: bigint,
): Uint8Array<ArrayBuffer> => {
  const correlation: Correlation = { worker };
  const record = commandRecord(command);
  const payloadLength = unwrap(
    encodeCorrelationPayload(correlation),
  ).byteLength + record.byteLength;
  const envelope: CommandEnvelope = {
    header: {
      major: PROTOCOL_MAJOR,
      minor: PROTOCOL_MINOR,
      message_type: commandMessageId(command),
      flags: 0,
      payload_len: payloadLength,
      sequence,
    },
    correlation,
    command,
  };
  const encoded = unwrap(encodeCommandPayload(envelope));
  return encodedFrame(encoded.messageId, encoded.bytes, sequence);
};

const helloFrame = (
  sequence: bigint,
  worker = 1n,
): Uint8Array<ArrayBuffer> => commandFrame({
  type: "Hello",
  payload: { hello: hello(EndpointRole.Host) },
}, worker, sequence);

const helloAcceptFrame = (
  negotiated: CompatibleHandshake,
  sequence: bigint,
  worker = 1n,
): Uint8Array<ArrayBuffer> => commandFrame({
  type: "HelloAccept",
  payload: {
    negotiated_minor: negotiated.minor,
    schema_hash: schemaHash(),
  },
}, worker, sequence);

const eventRecord = (event: Event): Uint8Array => {
  switch (event.type) {
    case "EngineHello":
      return unwrap(encodeEngineHelloEventPayload(event.payload));
    case "Ready":
      return unwrap(encodeReadyEventPayload(event.payload));
    default:
      throw new Error("unsupported native test event");
  }
};

const eventMessageId = (event: Event): number => {
  switch (event.type) {
    case "EngineHello":
      return MESSAGE_ID_ENGINE_HELLO;
    case "Ready":
      return MESSAGE_ID_READY;
    default:
      throw new Error("unsupported native test event");
  }
};

const eventFrame = (
  event: Event,
  worker: bigint,
  sequence: bigint,
): Uint8Array<ArrayBuffer> => {
  const correlation: Correlation = { worker };
  const record = eventRecord(event);
  const payloadLength = unwrap(
    encodeCorrelationPayload(correlation),
  ).byteLength + record.byteLength;
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
  return encodedFrame(encoded.messageId, encoded.bytes, sequence);
};

const engineHelloEvent = (
  schema = schemaHash(),
  executionCapabilities = 0n,
): Event => ({
  type: "EngineHello",
  payload: {
    hello: {
      ...hello(EndpointRole.Engine),
      schema_hash: schema,
    },
    execution_capabilities: {
      supported: executionCapabilities,
    },
  },
});

const readyEvent = (
  worker: bigint,
  overrides: Readonly<{
    negotiatedMinor?: number;
    schema?: Uint8Array;
    executionCapabilities?: bigint;
  }> = {},
): Event => ({
  type: "Ready",
  payload: {
    worker,
    negotiated_minor: overrides.negotiatedMinor ?? PROTOCOL_MINOR,
    schema_hash: overrides.schema?.slice() ?? schemaHash(),
    execution_capabilities: {
      supported: overrides.executionCapabilities ?? 0n,
    },
    capability_profiles: [CapabilityProfileId.BaselineNative],
    output_profiles: [OutputProfile.Srgb],
  },
});

const engineHelloFrame = (
  worker: bigint,
  event: Event = engineHelloEvent(),
): Uint8Array<ArrayBuffer> => eventFrame(event, worker, 1n);

const readyFrame = (
  worker: bigint,
  event: Event = readyEvent(worker),
): Uint8Array<ArrayBuffer> => eventFrame(event, worker, 2n);

const assertCode = (
  operation: () => unknown,
  code: BrowserNativeWorkerError["code"],
): void => {
  assert.throws(
    operation,
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === code,
  );
};

const deferred = <T>(): Readonly<{
  promise: Promise<T>;
  resolve: (value: T) => void;
}> => {
  let resolvePromise: ((value: T) => void) | undefined;
  const promise = new Promise<T>((resolve) => {
    resolvePromise = resolve;
  });
  return Object.freeze({
    promise,
    resolve: (value: T): void => {
      assert.ok(resolvePromise !== undefined);
      resolvePromise(value);
    },
  });
};

interface NativeTestHarness {
  readonly instance: WebAssembly.Instance;
  readonly dispatches: number;
  readonly shutdowns: number;
}

type NumericWasmExport = (...values: number[]) => number;

const numericWasmExport = (
  instance: WebAssembly.Instance,
  name: string,
): NumericWasmExport => {
  const value = instance.exports[name];
  assert.equal(typeof value, "function");
  return value as NumericWasmExport;
};

const joinU64 = (low: number, high: number): bigint =>
  (BigInt(high >>> 0) << 32n) | BigInt(low >>> 0);

const wrapNativeTestInstance = (
  wasm: WebAssembly.Instance,
  options: TestModuleOptions = {},
): NativeTestHarness => {
  const memory = wasm.exports.memory;
  assert.ok(memory instanceof WebAssembly.Memory);
  const initialize = numericWasmExport(
    wasm,
    "pdf_rs_worker_initialize",
  );
  const poll = numericWasmExport(wasm, "pdf_rs_worker_poll");
  const shutdown = numericWasmExport(
    wasm,
    "pdf_rs_worker_shutdown",
  );
  let worker = 0n;
  let dispatches = 0;
  let shutdowns = 0;
  let outputLength = 0;
  let transferCount = 0;
  const clearOutput = (): void => {
    outputLength = 0;
    transferCount = 0;
  };
  const publish = (
    output: Uint8Array<ArrayBuffer> | undefined,
  ): void => {
    clearOutput();
    if (output === undefined) {
      return;
    }
    new Uint8Array(
      memory.buffer,
      4_096,
      output.byteLength,
    ).set(output);
    outputLength = output.byteLength;
  };
  const wrapped = {
    exports: {
      ...wasm.exports,
      pdf_rs_worker_initialize: (
        workerLow: number,
        workerHigh: number,
        workerEpochLow: number,
        workerEpochHigh: number,
        rendererEpoch: number,
      ): number => {
        worker = joinU64(workerLow, workerHigh);
        return initialize(
          workerLow,
          workerHigh,
          workerEpochLow,
          workerEpochHigh,
          rendererEpoch,
        );
      },
      pdf_rs_worker_dispatch: (): number => {
        dispatches += 1;
        clearOutput();
        if (
          (dispatches === 1 && options.engineHelloTraps === true)
          || (dispatches === 2 && options.readyTraps === true)
          || (dispatches >= 3 && options.dispatchTraps === true)
        ) {
          throw new WebAssembly.RuntimeError("native test trap");
        }
        const status = dispatches === 1
          ? options.engineHelloStatus ?? 0
          : dispatches === 2
          ? options.readyStatus ?? 0
          : options.dispatchStatus ?? 0;
        if (status !== 0) {
          return status;
        }
        if (dispatches === 1) {
          publish(
            options.engineHelloOutput === undefined
              ? engineHelloFrame(worker)
              : options.engineHelloOutput(worker),
          );
        } else if (dispatches === 2) {
          publish(
            options.readyOutput === undefined
              ? readyFrame(worker)
              : options.readyOutput(worker),
          );
        } else {
          if (options.dispatchGrowsMemory === true) {
            memory.grow(1);
          }
          outputLength = options.outputLength ?? 0;
          transferCount = options.transferCount ?? 0;
        }
        return 0;
      },
      pdf_rs_worker_poll: (): number => {
        clearOutput();
        return poll();
      },
      pdf_rs_worker_output_length: (): number => outputLength,
      pdf_rs_worker_transfer_count: (): number => transferCount,
      pdf_rs_worker_shutdown: (): number => {
        shutdowns += 1;
        clearOutput();
        return shutdown();
      },
    },
  } as unknown as WebAssembly.Instance;
  return Object.freeze({
    instance: wrapped,
    get dispatches(): number {
      return dispatches;
    },
    get shutdowns(): number {
      return shutdowns;
    },
  });
};

const instantiateNativeTestModule = async (
  module: WebAssembly.Module,
  options: TestModuleOptions = {},
): Promise<NativeTestHarness> => wrapNativeTestInstance(
  await WebAssembly.instantiate(module, {}),
  options,
);

const awaitingInstance = async (
  options: TestModuleOptions = {},
  identity: BrowserNativeWorkerSupervisorIdentity = supervisorIdentity(),
): Promise<Readonly<{
  worker: BrowserNativeWorkerInstance;
  harness: NativeTestHarness;
}>> => {
  const bytes = nativeTestModule(options);
  const module = await WebAssembly.compile(bytes);
  const harness = await instantiateNativeTestModule(module, options);
  return Object.freeze({
    worker: new BrowserNativeWorkerInstance(
      helloFrame(1n, identity.worker),
      identity,
      harness.instance,
      1,
      1_024,
    ),
    harness,
  });
};

const instance = async (
  options: TestModuleOptions = {},
  identity: BrowserNativeWorkerSupervisorIdentity = supervisorIdentity(),
): Promise<BrowserNativeWorkerInstance> => {
  const awaiting = await awaitingInstance(options, identity);
  awaiting.worker.accept(
    helloAcceptFrame(
      awaiting.worker.connection,
      2n,
      identity.worker,
    ),
  );
  return awaiting.worker;
};

const artifactResponse = (
  chunks: readonly Uint8Array<ArrayBuffer>[],
  declaredLength: number,
): Pick<Response, "ok" | "headers" | "body"> => ({
  ok: true,
  headers: new Headers({
    "content-length": String(declaredLength),
  }),
  body: new ReadableStream<Uint8Array<ArrayBuffer>>({
    start(controller) {
      for (const chunk of chunks) {
        controller.enqueue(chunk);
      }
      controller.close();
    },
  }),
});

const loaderFor = async (
  bytes: Uint8Array<ArrayBuffer>,
  options: TestModuleOptions = {},
  observeHarness: (harness: NativeTestHarness) => void = () =>
    undefined,
): Promise<BrowserNativeWorkerLoader> =>
  new BrowserNativeWorkerLoader({
    url: "engine.wasm",
    byteLength: bytes.byteLength,
    sha256: await nativeWorkerSha256(bytes),
    minimumMemoryPages: 1,
    maximumMemoryPages: 1_024,
  }, {
    fetch: async () => artifactResponse(
      [bytes.slice()],
      bytes.byteLength,
    ),
    digestSha256: async (input) =>
      crypto.subtle.digest("SHA-256", input.slice()),
    compile: async (input) =>
      WebAssembly.compile(input.slice().buffer),
    instantiate: async (module) => {
      const harness = await instantiateNativeTestModule(
        module,
        options,
      );
      observeHarness(harness);
      return harness.instance;
    },
  });

test("loader admits one exact hash-bound import-free bounded module", async () => {
  const bytes = nativeTestModule();
  const digest = await nativeWorkerSha256(bytes);
  let fetches = 0;
  const loader = new BrowserNativeWorkerLoader(
    {
      url: "engine.wasm",
      byteLength: bytes.byteLength,
      sha256: digest,
      minimumMemoryPages: 1,
      maximumMemoryPages: 1_024,
    },
    {
      fetch: async () => {
        fetches += 1;
        return artifactResponse(
          [bytes.slice()],
          bytes.byteLength,
        );
      },
      digestSha256: async (input) =>
        crypto.subtle.digest("SHA-256", input.slice()),
      compile: async (input) =>
        WebAssembly.compile(input.slice().buffer),
      instantiate: async (module) =>
        (await instantiateNativeTestModule(module)).instance,
    },
  );
  const identity = supervisorIdentity();
  const first = await loader.bootstrap(helloFrame(1n), identity);
  const second = await loader.bootstrap(
    helloFrame(1n).slice(),
    identity,
  );
  assert.equal(first, second);
  assert.equal(fetches, 1);
  assert.equal(first.ready, false);
  assert.deepEqual(first.engineHello.transfers, []);
  assert.deepEqual(first.engineHello.frame, engineHelloFrame(1n));
  const decodedEngineHello = decodeBrowserEngineHello(
    Object.freeze([first.engineHello.frame.buffer]),
    1n,
    new EnvelopeSequenceTracker(),
    () => true,
  );
  assert.equal(decodedEngineHello.event.type, "EngineHello");
  assert.deepEqual(first.connection, connection());
  assertCode(() => first.dispatch(helloFrame(2n)), "InvalidLifecycle");
  assertCode(() => first.poll(), "InvalidLifecycle");
  const ready = first.accept(
    helloAcceptFrame(first.connection, 2n),
  );
  assert.deepEqual(ready.transfers, []);
  assert.deepEqual(ready.frame, readyFrame(1n));
  assert.equal(first.ready, true);
  assertCode(
    () => first.accept(helloAcceptFrame(first.connection, 3n)),
    "InvalidLifecycle",
  );
  assert.equal(first.dispatch(helloFrame(3n)), undefined);
  assertCode(
    () => first.dispatch(helloFrame(3n)),
    "InvalidMessage",
  );
});

test("instance initializes once with the exact supervisor u64 words before mailbox access", async () => {
  const module = await WebAssembly.compile(nativeTestModule());
  const harness = await instantiateNativeTestModule(module);
  const calls: number[][] = [];
  const order: string[] = [];
  let initialized = false;
  const initialize = numericWasmExport(
    harness.instance,
    "pdf_rs_worker_initialize",
  );
  const memoryEpoch = numericWasmExport(
    harness.instance,
    "pdf_rs_worker_memory_epoch",
  );
  const dispatch = numericWasmExport(
    harness.instance,
    "pdf_rs_worker_dispatch",
  );
  const wrapped = {
    exports: {
      ...harness.instance.exports,
      pdf_rs_worker_initialize: (...values: number[]): number => {
        order.push("initialize");
        calls.push(values);
        initialized = true;
        return initialize(...values);
      },
      pdf_rs_worker_memory_epoch: (): number => {
        order.push("memoryEpoch");
        assert.equal(initialized, true);
        return memoryEpoch();
      },
      pdf_rs_worker_dispatch: (...values: number[]): number => {
        order.push("dispatch");
        return dispatch(...values);
      },
    },
  } as unknown as WebAssembly.Instance;
  const identity = supervisorIdentity({
    worker: 0x0123_4567_89ab_cdefn,
    workerEpoch: 0xfedc_ba98_7654_3210n,
    rendererEpoch: 0x7654_3210,
  });
  const worker = new BrowserNativeWorkerInstance(
    helloFrame(1n, identity.worker),
    identity,
    wrapped,
    1,
    1_024,
  );
  assert.deepEqual(calls, [[
    0x89ab_cdef,
    0x0123_4567,
    0x7654_3210,
    0xfedc_ba98,
    0x7654_3210,
  ]]);
  assert.equal(order[0], "initialize");
  assert.equal(
    order.filter((operation) => operation === "initialize").length,
    1,
  );
  assert.ok(order.indexOf("memoryEpoch") > 0);
  assert.ok(order.indexOf("dispatch") > order.indexOf("memoryEpoch"));
  assert.deepEqual(worker.supervisorIdentity, identity);
  worker.shutdown();
});

test("memory growth during initialize establishes the post-initialize backing", async () => {
  const module = await WebAssembly.compile(nativeTestModule());
  const harness = await instantiateNativeTestModule(module);
  const memory = harness.instance.exports.memory;
  assert.ok(memory instanceof WebAssembly.Memory);
  const initialBuffer = memory.buffer;
  const initialize = numericWasmExport(
    harness.instance,
    "pdf_rs_worker_initialize",
  );
  const wrapped = {
    exports: {
      ...harness.instance.exports,
      pdf_rs_worker_initialize: (...values: number[]): number => {
        assert.equal(memory.grow(1), 1);
        return initialize(...values);
      },
      pdf_rs_worker_memory_epoch: (): number => 1,
    },
  } as unknown as WebAssembly.Instance;
  const worker = new BrowserNativeWorkerInstance(
    helloFrame(1n),
    supervisorIdentity(),
    wrapped,
    1,
    1_024,
  );
  assert.notEqual(memory.buffer, initialBuffer);
  assert.equal(memory.buffer.byteLength, 2 * 65_536);
  worker.accept(helloAcceptFrame(worker.connection, 2n));
  assert.equal(worker.dispatch(helloFrame(3n)), undefined);
  worker.shutdown();
});

test("initialization rejection, reserved internal unwind, and Wasm trap are fail closed", async () => {
  for (const [options, code] of [
    [
      { initializeStatus: NATIVE_WORKER_STATUS_REJECTED },
      "EngineRejected",
    ],
    [
      { initializeStatus: NATIVE_WORKER_STATUS_INTERNAL_UNWIND },
      "EngineTrap",
    ],
    [
      { initializeTraps: true },
      "EngineTrap",
    ],
  ] as const) {
    await assert.rejects(
      instance(options),
      (error: unknown) =>
        error instanceof BrowserNativeWorkerError
        && error.code === code,
    );
  }
});

test("bootstrap requires one immediate compatible native EngineHello and poisons once", async () => {
  const incompatibleSchema = schemaHash();
  incompatibleSchema[0] = (incompatibleSchema[0] ?? 0) ^ 1;
  const cases: readonly Readonly<{
    options: TestModuleOptions;
    code: BrowserNativeWorkerError["code"];
  }>[] = [
    {
      options: {
        engineHelloOutput: () => undefined,
      },
      code: "NegotiationMismatch",
    },
    {
      options: {
        engineHelloOutput: (worker) => engineHelloFrame(
          worker,
          engineHelloEvent(incompatibleSchema),
        ),
      },
      code: "NegotiationMismatch",
    },
    {
      options: {
        engineHelloOutput: (worker) => eventFrame(
          readyEvent(worker),
          worker,
          1n,
        ),
      },
      code: "InvalidMessage",
    },
    {
      options: {
        engineHelloOutput: (worker) =>
          engineHelloFrame(worker + 1n),
      },
      code: "InvalidMessage",
    },
    {
      options: {
        engineHelloStatus: NATIVE_WORKER_STATUS_REJECTED,
      },
      code: "EngineRejected",
    },
    {
      options: {
        engineHelloStatus: NATIVE_WORKER_STATUS_INTERNAL_UNWIND,
      },
      code: "EngineTrap",
    },
    {
      options: {
        engineHelloTraps: true,
      },
      code: "EngineTrap",
    },
  ];

  for (const { options, code } of cases) {
    const module = await WebAssembly.compile(
      nativeTestModule(options),
    );
    const harness = await instantiateNativeTestModule(
      module,
      options,
    );
    assertCode(
      () => new BrowserNativeWorkerInstance(
        helloFrame(1n),
        supervisorIdentity(),
        harness.instance,
        1,
        1_024,
      ),
      code,
    );
    assert.equal(harness.dispatches, 1);
    assert.equal(harness.shutdowns, 1);
  }
});

test("HelloAccept requires an immediate transcript-matching native Ready", async () => {
  const cases: readonly Readonly<{
    options: TestModuleOptions;
    code: BrowserNativeWorkerError["code"];
  }>[] = [
    {
      options: {
        readyOutput: () => undefined,
      },
      code: "NegotiationMismatch",
    },
    {
      options: {
        readyOutput: (worker) => readyFrame(
          worker,
          readyEvent(worker, { executionCapabilities: 1n }),
        ),
      },
      code: "NegotiationMismatch",
    },
    {
      options: {
        readyOutput: (worker) => readyFrame(worker + 1n),
      },
      code: "NegotiationMismatch",
    },
    {
      options: {
        readyOutput: (worker) => eventFrame(
          engineHelloEvent(),
          worker,
          2n,
        ),
      },
      code: "NegotiationMismatch",
    },
    {
      options: {
        readyStatus: NATIVE_WORKER_STATUS_REJECTED,
      },
      code: "EngineRejected",
    },
    {
      options: {
        readyStatus: NATIVE_WORKER_STATUS_INTERNAL_UNWIND,
      },
      code: "EngineTrap",
    },
    {
      options: {
        readyTraps: true,
      },
      code: "EngineTrap",
    },
  ];

  for (const { options, code } of cases) {
    const awaiting = await awaitingInstance(options);
    assert.equal(awaiting.worker.ready, false);
    assertCode(
      () => awaiting.worker.accept(
        helloAcceptFrame(awaiting.worker.connection, 2n),
      ),
      code,
    );
    assert.equal(awaiting.worker.ready, false);
    assert.equal(awaiting.worker.closed, true);
    assert.equal(awaiting.harness.shutdowns, 1);
  }
});

test("accept rejects host transcript mismatches without entering Ready", async () => {
  const wrongCommand = await awaitingInstance();
  assertCode(
    () => wrongCommand.worker.accept(helloFrame(2n)),
    "NegotiationMismatch",
  );
  assert.equal(wrongCommand.worker.closed, true);
  assert.equal(wrongCommand.harness.shutdowns, 1);

  const wrongMinor = await awaitingInstance();
  const accept: Command = {
    type: "HelloAccept",
    payload: {
      negotiated_minor: wrongMinor.worker.connection.minor + 1,
      schema_hash: schemaHash(),
    },
  };
  assertCode(
    () => wrongMinor.worker.accept(commandFrame(accept, 1n, 2n)),
    "NegotiationMismatch",
  );
  assert.equal(wrongMinor.worker.ready, false);
  assert.equal(wrongMinor.worker.closed, true);
  assert.equal(wrongMinor.harness.shutdowns, 1);
});

test("handshake snapshots stay authoritative and shutdown remains idempotent", async () => {
  const awaiting = await awaitingInstance();
  awaiting.worker.engineHello.frame.fill(0);
  const ready = awaiting.worker.accept(
    helloAcceptFrame(awaiting.worker.connection, 2n),
  );
  assert.deepEqual(ready.frame, readyFrame(1n));
  assert.equal(awaiting.worker.ready, true);
  awaiting.worker.shutdown();
  awaiting.worker.shutdown();
  assert.equal(awaiting.worker.closed, true);
  assert.equal(awaiting.worker.ready, false);
  assert.equal(awaiting.harness.shutdowns, 1);
});

test("loader requires one canonical Host Hello and exact supervisor identity", async () => {
  const bytes = nativeTestModule();
  const loader = new BrowserNativeWorkerLoader({
    url: "engine.wasm",
    byteLength: bytes.byteLength,
    sha256: await nativeWorkerSha256(bytes),
    minimumMemoryPages: 1,
    maximumMemoryPages: 1_024,
  }, {
    fetch: async () => artifactResponse(
      [bytes.slice()],
      bytes.byteLength,
    ),
    digestSha256: async (input) =>
      crypto.subtle.digest("SHA-256", input.slice()),
    compile: async (input) =>
      WebAssembly.compile(input.slice().buffer),
    instantiate: async (module) =>
      (await instantiateNativeTestModule(module)).instance,
  });
  await assert.rejects(
    loader.bootstrap(
      {} as Uint8Array,
      supervisorIdentity(),
    ),
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "InvalidMessage",
  );
  await assert.rejects(
    loader.bootstrap(
      helloFrame(1n),
      {} as BrowserNativeWorkerSupervisorIdentity,
    ),
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "InvalidIdentity",
  );
  let identityReads = 0;
  const accessorIdentity = {};
  Object.defineProperties(accessorIdentity, {
    worker: {
      enumerable: true,
      get: () => {
        identityReads += 1;
        return 1n;
      },
    },
    workerEpoch: {
      enumerable: true,
      get: () => {
        identityReads += 1;
        return 1n;
      },
    },
    rendererEpoch: {
      enumerable: true,
      get: () => {
        identityReads += 1;
        return 1;
      },
    },
  });
  await assert.rejects(
    loader.bootstrap(
      helloFrame(1n),
      accessorIdentity as BrowserNativeWorkerSupervisorIdentity,
    ),
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "InvalidIdentity",
  );
  assert.equal(identityReads, 0);
  await assert.rejects(
    loader.bootstrap(
      helloFrame(1n, 2n),
      supervisorIdentity(),
    ),
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "InvalidMessage",
  );

  const originalHello = helloFrame(1n);
  const firstLoad = loader.bootstrap(
    originalHello,
    supervisorIdentity(),
  );
  originalHello.fill(0);
  const worker = await firstLoad;
  assert.equal(worker.closed, false);
  assert.equal(
    await loader.bootstrap(helloFrame(1n), supervisorIdentity()),
    worker,
  );
  await assert.rejects(
    loader.bootstrap(helloFrame(2n), supervisorIdentity()),
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "NegotiationMismatch",
  );
  await assert.rejects(
    loader.bootstrap(
      helloFrame(1n),
      supervisorIdentity({ workerEpoch: 2n }),
    ),
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "IdentityMismatch",
  );
});

test("loader bootstrap failure and close reclaim each native instance once", async () => {
  const bytes = nativeTestModule();
  let failedHarness: NativeTestHarness | undefined;
  const failing = await loaderFor(
    bytes,
    {
      engineHelloOutput: () => undefined,
    },
    (harness) => {
      failedHarness = harness;
    },
  );
  await assert.rejects(
    failing.bootstrap(helloFrame(1n), supervisorIdentity()),
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "NegotiationMismatch",
  );
  assert.ok(failedHarness !== undefined);
  assert.equal(failedHarness.shutdowns, 1);

  let liveHarness: NativeTestHarness | undefined;
  const live = await loaderFor(bytes, {}, (harness) => {
    liveHarness = harness;
  });
  const worker = await live.bootstrap(
    helloFrame(1n),
    supervisorIdentity(),
  );
  live.close();
  await Promise.resolve();
  assert.ok(liveHarness !== undefined);
  assert.equal(worker.closed, true);
  assert.equal(liveHarness.shutdowns, 1);
});

test("close aborts fetch and body waits and rejects the load immediately", async () => {
  const bytes = nativeTestModule();
  const pendingFetch = deferred<
    Pick<Response, "ok" | "headers" | "body">
  >();
  let fetchSignal: AbortSignal | undefined;
  const fetching = new BrowserNativeWorkerLoader({
    url: "engine.wasm",
    byteLength: bytes.byteLength,
    sha256: await nativeWorkerSha256(bytes),
    minimumMemoryPages: 1,
    maximumMemoryPages: 1_024,
  }, {
    fetch: async (_input, signal) => {
      fetchSignal = signal;
      return pendingFetch.promise;
    },
    digestSha256: async (input) =>
      crypto.subtle.digest("SHA-256", input.slice()),
    compile: async (input) =>
      WebAssembly.compile(input.slice().buffer),
    instantiate: async (module, imports) =>
      WebAssembly.instantiate(module, imports),
  });
  const fetchLoad = fetching.bootstrap(
    helloFrame(1n),
    supervisorIdentity(),
  );
  await Promise.resolve();
  fetching.close();
  assert.equal(fetchSignal?.aborted, true);
  await assert.rejects(
    fetchLoad,
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "InvalidLifecycle",
  );

  let bodyCancelled = 0;
  const bodyCancellationObserved = deferred<void>();
  const bodyEntered = deferred<void>();
  const pendingBodyRead = deferred<
    ReadableStreamReadResult<Uint8Array<ArrayBuffer>>
  >();
  const body = {
    locked: false,
    getReader: () => ({
      read: () => {
        bodyEntered.resolve();
        return pendingBodyRead.promise;
      },
      cancel: async () => {
        bodyCancelled += 1;
        bodyCancellationObserved.resolve();
      },
      releaseLock: () => undefined,
    }),
  } as unknown as ReadableStream<Uint8Array<ArrayBuffer>>;
  const reading = new BrowserNativeWorkerLoader({
    url: "engine.wasm",
    byteLength: bytes.byteLength,
    sha256: await nativeWorkerSha256(bytes),
    minimumMemoryPages: 1,
    maximumMemoryPages: 1_024,
  }, {
    fetch: async () => ({
      ok: true,
      headers: new Headers({
        "content-length": String(bytes.byteLength),
      }),
      body,
    }),
    digestSha256: async (input) =>
      crypto.subtle.digest("SHA-256", input.slice()),
    compile: async (input) =>
      WebAssembly.compile(input.slice().buffer),
    instantiate: async (module, imports) =>
      WebAssembly.instantiate(module, imports),
  });
  const bodyLoad = reading.bootstrap(
    helloFrame(1n),
    supervisorIdentity(),
  );
  await bodyEntered.promise;
  reading.close();
  await assert.rejects(
    bodyLoad,
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "InvalidLifecycle",
  );
  await bodyCancellationObserved.promise;
  assert.equal(bodyCancelled, 1);
});

test("close covers digest, compile, and instantiate waits and reclaims a late instance", async () => {
  const bytes = nativeTestModule();
  const digest = await crypto.subtle.digest(
    "SHA-256",
    bytes.slice(),
  );
  const module = await WebAssembly.compile(bytes);

  const assertStageAborts = async (
    stage: "digest" | "compile" | "instantiate",
  ): Promise<void> => {
    const entered = deferred<void>();
    const pendingDigest = deferred<ArrayBuffer>();
    const pendingCompile = deferred<WebAssembly.Module>();
    const pendingInstantiate = deferred<WebAssembly.Instance>();
    const shutdownObserved = deferred<void>();
    let shutdowns = 0;
    const loader = new BrowserNativeWorkerLoader({
      url: "engine.wasm",
      byteLength: bytes.byteLength,
      sha256: await nativeWorkerSha256(bytes),
      minimumMemoryPages: 1,
      maximumMemoryPages: 1_024,
    }, {
      fetch: async () => artifactResponse(
        [bytes.slice()],
        bytes.byteLength,
      ),
      digestSha256: async () => {
        if (stage === "digest") {
          entered.resolve();
          return pendingDigest.promise;
        }
        return digest.slice(0);
      },
      compile: async () => {
        if (stage === "compile") {
          entered.resolve();
          return pendingCompile.promise;
        }
        return module;
      },
      instantiate: async () => {
        if (stage === "instantiate") {
          entered.resolve();
          return pendingInstantiate.promise;
        }
        return WebAssembly.instantiate(module, {});
      },
    });
    const load = loader.bootstrap(
      helloFrame(1n),
      supervisorIdentity(),
    );
    await entered.promise;
    loader.close();
    await assert.rejects(
      load,
      (error: unknown) =>
        error instanceof BrowserNativeWorkerError
        && error.code === "InvalidLifecycle",
    );
    if (stage === "digest") {
      pendingDigest.resolve(digest.slice(0));
    } else if (stage === "compile") {
      pendingCompile.resolve(module);
    } else {
      pendingInstantiate.resolve({
        exports: {
          pdf_rs_worker_shutdown: (): number => {
            shutdowns += 1;
            shutdownObserved.resolve();
            return 0;
          },
        },
      } as unknown as WebAssembly.Instance);
      await shutdownObserved.promise;
      assert.equal(shutdowns, 1);
    }
  };

  await assertStageAborts("digest");
  await assertStageAborts("compile");
  await assertStageAborts("instantiate");
});

test("artifact and digest inputs are fixed, exact, hash-bound, and capped", async () => {
  const bytes = nativeTestModule();
  assert.equal(nativeWorkerMaximumArtifactBytes(), 64 * 1_024 * 1_024);
  assertCode(
    () => new BrowserNativeWorkerLoader({
      url: "engine.wasm",
      byteLength: nativeWorkerMaximumArtifactBytes() + 1,
      sha256: "00".repeat(32),
      minimumMemoryPages: 1,
      maximumMemoryPages: 1_024,
    }),
    "InvalidConfiguration",
  );

  const oversized = new Uint8Array(bytes.byteLength + 1);
  oversized.set(bytes);
  const loader = new BrowserNativeWorkerLoader({
    url: "engine.wasm",
    byteLength: bytes.byteLength,
    sha256: await nativeWorkerSha256(bytes),
    minimumMemoryPages: 1,
    maximumMemoryPages: 1_024,
  }, {
    fetch: async () => artifactResponse(
      [oversized],
      bytes.byteLength,
    ),
    digestSha256: async () => new ArrayBuffer(32),
    compile: async (input) =>
      WebAssembly.compile(input.slice().buffer),
    instantiate: async (module, imports) =>
      WebAssembly.instantiate(module, imports),
  });
  await assert.rejects(
    loader.bootstrap(helloFrame(1n), supervisorIdentity()),
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "ArtifactLengthMismatch",
  );

  const mismatched = new BrowserNativeWorkerLoader({
    url: "engine.wasm",
    byteLength: bytes.byteLength,
    sha256: await nativeWorkerSha256(bytes),
    minimumMemoryPages: 1,
    maximumMemoryPages: 1_024,
  }, {
    fetch: async () => artifactResponse(
      [bytes.slice()],
      bytes.byteLength + 1,
    ),
    digestSha256: async (input) =>
      crypto.subtle.digest("SHA-256", input.slice()),
    compile: async (input) =>
      WebAssembly.compile(input.slice().buffer),
    instantiate: async (module, imports) =>
      WebAssembly.instantiate(module, imports),
  });
  await assert.rejects(
    mismatched.bootstrap(helloFrame(1n), supervisorIdentity()),
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "ArtifactLengthMismatch",
  );
});

test("module memory and export surface are exact", async () => {
  assert.deepEqual(inspectNativeWorkerMemory(nativeTestModule()), {
    minimum: 1,
    maximum: 1_024,
    shared: false,
  });
  assertCode(
    () => inspectNativeWorkerMemory(
      nativeTestModule({ maximumPages: 1_025 }),
    ),
    "InvalidWasmMemory",
  );

  const extra = nativeTestModule({ includeExtraExport: true });
  const loader = new BrowserNativeWorkerLoader({
    url: "engine.wasm",
    byteLength: extra.byteLength,
    sha256: await nativeWorkerSha256(extra),
    minimumMemoryPages: 1,
    maximumMemoryPages: 1_024,
  }, {
    fetch: async () => artifactResponse(
      [extra.slice()],
      extra.byteLength,
    ),
    digestSha256: async (input) =>
      crypto.subtle.digest("SHA-256", input.slice()),
    compile: async (input) =>
      WebAssembly.compile(input.slice().buffer),
    instantiate: async (module, imports) =>
      WebAssembly.instantiate(module, imports),
  });
  await assert.rejects(
    loader.bootstrap(helloFrame(1n), supervisorIdentity()),
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "InvalidWasmExports",
  );

  for (const invalid of [
    nativeTestModule({ wrongPrepareInputSignature: true }),
    nativeTestModule({ wrongAbiHash: true }),
  ]) {
    await assert.rejects(
      (await loaderFor(invalid)).bootstrap(
        helloFrame(1n),
        supervisorIdentity(),
      ),
      (error: unknown) =>
        error instanceof BrowserNativeWorkerError
        && error.code === "InvalidWasmExports",
    );
  }
});

test("poll exposes bounded pending work and maps rejection or traps", async () => {
  const pending = await instance({
    pollStatus: NATIVE_WORKER_POLL_PENDING,
  });
  assert.deepEqual(pending.poll(), {
    output: undefined,
    pending: true,
  });

  for (const [options, code] of [
    [
      { pollStatus: NATIVE_WORKER_STATUS_REJECTED },
      "EngineRejected",
    ],
    [
      { pollStatus: NATIVE_WORKER_STATUS_INTERNAL_UNWIND },
      "EngineTrap",
    ],
    [
      { pollTraps: true },
      "EngineTrap",
    ],
  ] as const) {
    const worker = await instance(options);
    assertCode(() => worker.poll(), code);
    assert.equal(worker.closed, true);
  }
});

test("dispatch rejects resizable transfers, engine rejection, traps, and untracked growth", async () => {
  const fixed = await instance();
  const resizable = new ArrayBuffer(1, { maxByteLength: 2 });
  assertCode(
    () => fixed.dispatch(helloFrame(3n), [resizable]),
    "TransferLimit",
  );
  const staleIdentity = await instance(
    {},
    supervisorIdentity({ worker: 2n }),
  );
  assertCode(
    () => staleIdentity.dispatch(helloFrame(3n)),
    "InvalidMessage",
  );

  const rejected = await instance({
    dispatchStatus: NATIVE_WORKER_STATUS_REJECTED,
  });
  assertCode(
    () => rejected.dispatch(helloFrame(3n)),
    "EngineRejected",
  );
  assert.equal(rejected.closed, true);

  for (const options of [
    { dispatchStatus: NATIVE_WORKER_STATUS_INTERNAL_UNWIND },
    { dispatchTraps: true },
  ] as const) {
    const trapped = await instance(options);
    assertCode(
      () => trapped.dispatch(helloFrame(3n)),
      "EngineTrap",
    );
    assert.equal(trapped.closed, true);
  }

  const grown = await instance({ dispatchGrowsMemory: true });
  assertCode(
    () => grown.dispatch(helloFrame(3n)),
    "InvalidWasmMemory",
  );
  assert.equal(grown.closed, true);

  const tooMany = await instance({
    outputLength: 20,
    transferCount: MAX_TRANSFER_SLOTS + 1,
  });
  assertCode(
    () => tooMany.dispatch(helloFrame(3n)),
    "TransferLimit",
  );
});
