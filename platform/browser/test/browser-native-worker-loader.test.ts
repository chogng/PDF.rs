import assert from "node:assert/strict";
import test from "node:test";

import {
  EndpointRole,
  KNOWN_ENDPOINT_CAPABILITIES,
  MAX_MESSAGE_BYTES,
  MAX_TRANSFER_SLOTS,
  PROTOCOL_MAJOR,
  PROTOCOL_MINOR,
  SCHEMA_HASH_HEX,
  encodeCommandPayload,
  encodeCorrelationPayload,
  encodeHelloCommandPayload,
  type CommandEnvelope,
  type CompatibleHandshake,
  type PayloadCodecResult,
  type ProtocolHello,
} from "../generated/engine-protocol.js";
import { NATIVE_WORKER_ABI_HASH_WORDS } from "../src/native-worker-abi.generated.js";
import {
  BrowserNativeWorkerError,
  BrowserNativeWorkerInstance,
  BrowserNativeWorkerLoader,
  inspectNativeWorkerMemory,
  nativeWorkerMaximumArtifactBytes,
  nativeWorkerSha256,
} from "../src/browser-native-worker-loader.js";
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
  readonly dispatchStatus?: number;
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
    { name: "pdf_rs_worker_poll", type: 2, value: 0 },
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
      value: 1,
    },
    ...hashWords.map((value, index) => ({
      name: `pdf_rs_worker_abi_hash_${index}`,
      type: 2,
      value,
    })),
    { name: "main", type: 1, value: 0 },
  ] as const;
  const type = section(1, [
    4,
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
  const bodies = specs.map((spec, index) => {
    const instructions = spec.value === undefined
      ? []
      : index === 2
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

const unwrap = <T>(result: PayloadCodecResult<T>): T => {
  if (result.ok) {
    return result.value;
  }
  throw new Error(result.error.code);
};

const helloFrame = (sequence: bigint): Uint8Array<ArrayBuffer> => {
  const correlation = { worker: 1n };
  const command = {
    type: "Hello" as const,
    payload: { hello: hello(EndpointRole.Host) },
  };
  const payloadLength = unwrap(
    encodeCorrelationPayload(correlation),
  ).byteLength + unwrap(
    encodeHelloCommandPayload(command.payload),
  ).byteLength;
  const envelope: CommandEnvelope = {
    header: {
      major: PROTOCOL_MAJOR,
      minor: PROTOCOL_MINOR,
      message_type: 1,
      flags: 0,
      payload_len: payloadLength,
      sequence,
    },
    correlation,
    command,
  };
  const encoded = unwrap(encodeCommandPayload(envelope));
  const frame = new Uint8Array(20 + encoded.bytes.byteLength);
  const header = new DataView(frame.buffer);
  header.setUint16(0, PROTOCOL_MAJOR, true);
  header.setUint16(2, PROTOCOL_MINOR, true);
  header.setUint16(4, encoded.messageId, true);
  header.setUint16(6, 0, true);
  header.setUint32(8, encoded.bytes.byteLength, true);
  header.setBigUint64(12, sequence, true);
  frame.set(encoded.bytes, 20);
  return frame;
};

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

const instance = async (
  options: TestModuleOptions = {},
): Promise<BrowserNativeWorkerInstance> => {
  const bytes = nativeTestModule(options);
  const module = await WebAssembly.compile(bytes);
  const wasm = await WebAssembly.instantiate(module, {});
  return new BrowserNativeWorkerInstance(
    connection(),
    wasm,
    1,
    1_024,
  );
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
    instantiate: async (module, imports) =>
      WebAssembly.instantiate(module, imports),
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
      instantiate: async (module, imports) =>
        WebAssembly.instantiate(module, imports),
    },
  );
  const negotiated = connection();
  const first = await loader.load(negotiated);
  const second = await loader.load(negotiated);
  assert.equal(first, second);
  assert.equal(fetches, 1);
  assert.equal(first.dispatch(helloFrame(1n)), undefined);
  assertCode(
    () => first.dispatch(helloFrame(1n)),
    "InvalidMessage",
  );
});

test("loader requires negotiation and one exact connection identity", async () => {
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
    instantiate: async (module, imports) =>
      WebAssembly.instantiate(module, imports),
  });
  await assert.rejects(
    loader.load({} as CompatibleHandshake),
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "NegotiationRequired",
  );
  await loader.load(connection());
  await assert.rejects(
    loader.load(connection()),
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "NegotiationMismatch",
  );
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
  const fetchLoad = fetching.load(connection());
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
  const bodyLoad = reading.load(connection());
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
    const load = loader.load(connection());
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
    loader.load(connection()),
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
    mismatched.load(connection()),
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
    loader.load(connection()),
    (error: unknown) =>
      error instanceof BrowserNativeWorkerError
      && error.code === "InvalidWasmExports",
  );

  for (const invalid of [
    nativeTestModule({ wrongPrepareInputSignature: true }),
    nativeTestModule({ wrongAbiHash: true }),
  ]) {
    await assert.rejects(
      (await loaderFor(invalid)).load(connection()),
      (error: unknown) =>
        error instanceof BrowserNativeWorkerError
        && error.code === "InvalidWasmExports",
    );
  }
});

test("dispatch rejects resizable transfers, engine rejection, traps, and untracked growth", async () => {
  const fixed = await instance();
  const resizable = new ArrayBuffer(1, { maxByteLength: 2 });
  assertCode(
    () => fixed.dispatch(helloFrame(1n), [resizable]),
    "TransferLimit",
  );

  const rejected = await instance({ dispatchStatus: 1 });
  assertCode(
    () => rejected.dispatch(helloFrame(1n)),
    "EngineRejected",
  );
  assert.equal(rejected.closed, true);

  const grown = await instance({ dispatchGrowsMemory: true });
  assertCode(
    () => grown.dispatch(helloFrame(1n)),
    "InvalidWasmMemory",
  );
  assert.equal(grown.closed, true);

  const tooMany = await instance({
    outputLength: 20,
    transferCount: MAX_TRANSFER_SLOTS + 1,
  });
  assertCode(
    () => tooMany.dispatch(helloFrame(1n)),
    "TransferLimit",
  );
});
