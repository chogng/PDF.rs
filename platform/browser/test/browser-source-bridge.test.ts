import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";

import {
  DataAttachmentRole,
  DataPriority,
  SourceFailureCode,
  validateFailDataCommand,
  validateProvideDataCommand,
  validateProvideDataTransferLengths,
  type ByteRange,
  type NeedDataEvent,
  type SourceDescriptor,
  type SourceIdentity,
} from "../generated/engine-protocol.js";
import {
  BrowserSourceBridge,
  BrowserSourceBridgeError,
  MAX_BROWSER_HTTP_VALIDATOR_HEADER_NAME_LENGTH,
  MAX_BROWSER_HTTP_VALIDATOR_VALUE_LENGTH,
  deriveBrowserHttpValidatorBinding,
  type BrowserHttpFetcher,
  type BrowserHttpHeaders,
  type BrowserHttpRequest,
  type BrowserHttpResponse,
  type BrowserHttpResult,
  type BrowserHttpSnapshotValidator,
  type BrowserLocalFileReader,
  type BrowserLocalReadRequest,
  type BrowserLocalReadResult,
  type BrowserSourceAbortFactory,
  type BrowserSourceAbortHandle,
  type BrowserSourceAcquirer,
  type BrowserSourceBridgeConfiguration,
  type BrowserSourceBridgeLimits,
  type BrowserSourceCommand,
  type BrowserSourceCommandSink,
  type BrowserSourceSinkCorrelation,
  type BrowserSourceSinkOwnership,
  type BrowserSourceSinkReceipt,
  type BrowserSourceTicketOwner,
} from "../src/browser-source-bridge.js";

const SOURCE_ID = new Uint8Array(32).fill(0x51);
const SOURCE_VALIDATOR = new Uint8Array(32).fill(0x52);
const HTTP_ETAG = "\"pdf-rs-v1\"";

const DEFAULT_OWNER: BrowserSourceTicketOwner = Object.freeze({
  worker: 1n,
  session: 2n,
  request: 3n,
});

const sourceIdentity = (
  revision = 1n,
  fill = 0x51,
): SourceIdentity => Object.freeze({
  stable_id: new Uint8Array(32).fill(fill),
  revision,
});

const descriptor = (
  length: bigint | null = 100n,
): SourceDescriptor => Object.freeze({
  identity: Object.freeze({
    stable_id: SOURCE_ID.slice(),
    revision: 1n,
  }),
  ...(length === null ? {} : { length }),
  validator: SOURCE_VALIDATOR.slice(),
});

const httpDescriptor = (
  length: bigint | null,
  validator: BrowserHttpSnapshotValidator,
  identity = sourceIdentity(),
): SourceDescriptor => Object.freeze({
  identity,
  ...(length === null ? {} : { length }),
  validator: deriveBrowserHttpValidatorBinding(identity, validator),
});

const range = (start: bigint, len: bigint): ByteRange =>
  Object.freeze({ start, len });

const needData = (
  ticket: bigint,
  ranges: readonly ByteRange[],
  source = sourceIdentity(),
): NeedDataEvent => Object.freeze({
  ticket,
  source,
  ranges: Object.freeze([...ranges]) as ByteRange[],
  priority: DataPriority.VisiblePage,
  checkpoint: ticket,
});

const submitNeed = (
  bridge: BrowserSourceBridge,
  need: NeedDataEvent,
  owner: BrowserSourceTicketOwner = DEFAULT_OWNER,
) => bridge.request(Object.freeze({ need, owner }));

const bytes = (...values: number[]): ArrayBuffer =>
  Uint8Array.from(values).buffer;

const sequenceBytes = (length: number): ArrayBuffer =>
  Uint8Array.from(
    { length },
    (_, index) => index,
  ).buffer;

const DEFAULT_LIMITS: BrowserSourceBridgeLimits = Object.freeze({
  maxActiveTickets: 4,
  maxQueuedResults: 8,
  maxTrackedTickets: 32,
  maxBufferedBytes: 1_024,
  maxWholeSourceBytes: 256,
  maxDrainTurn: 8,
});

class Deferred<T> {
  readonly promise: Promise<T>;
  readonly resolve: (value: T) => void;
  readonly reject: (reason?: unknown) => void;

  constructor() {
    let resolvePromise: ((value: T) => void) | undefined;
    let rejectPromise: ((reason?: unknown) => void) | undefined;
    this.promise = new Promise<T>((resolve, reject) => {
      resolvePromise = resolve;
      rejectPromise = reject;
    });
    if (
      resolvePromise === undefined
      || rejectPromise === undefined
    ) {
      throw new Error("deferred initialization failed");
    }
    this.resolve = resolvePromise;
    this.reject = rejectPromise;
  }
}

class FakeAbortHandle implements BrowserSourceAbortHandle {
  readonly signal: Readonly<{ id: number }>;
  abortCalls = 0;

  constructor(id: number) {
    this.signal = Object.freeze({ id });
  }

  abort(): void {
    this.abortCalls += 1;
  }
}

class FakeAbortFactory implements BrowserSourceAbortFactory {
  readonly handles: FakeAbortHandle[] = [];
  throwOnCreate = false;

  create(): BrowserSourceAbortHandle {
    if (this.throwOnCreate) {
      throw new Error("opaque abort factory failure");
    }
    const handle = new FakeAbortHandle(this.handles.length + 1);
    this.handles.push(handle);
    return handle;
  }
}

class FakeHeaders implements BrowserHttpHeaders {
  readonly #values: ReadonlyMap<string, string>;

  constructor(values: Readonly<Record<string, string>>) {
    this.#values = new Map(
      Object.entries(values).map(
        ([name, value]) => [name.toLowerCase(), value],
      ),
    );
  }

  get(name: string): string | null {
    return this.#values.get(name.toLowerCase()) ?? null;
  }
}

interface PendingHttpRequest {
  readonly request: BrowserHttpRequest;
  readonly deferred: Deferred<BrowserHttpResult>;
}

class FakeHttpFetcher implements BrowserHttpFetcher {
  readonly requests: PendingHttpRequest[] = [];
  throwOnFetch = false;

  fetch(request: BrowserHttpRequest): Promise<BrowserHttpResult> {
    if (this.throwOnFetch) {
      throw new Error("opaque fetch failure");
    }
    const deferred = new Deferred<BrowserHttpResult>();
    this.requests.push(Object.freeze({ request, deferred }));
    return deferred.promise;
  }
}

interface PendingLocalRead {
  readonly request: BrowserLocalReadRequest;
  readonly deferred: Deferred<BrowserLocalReadResult>;
}

class FakeLocalReader implements BrowserLocalFileReader {
  readonly reads: PendingLocalRead[] = [];
  throwOnRead = false;

  read(
    request: BrowserLocalReadRequest,
  ): Promise<BrowserLocalReadResult> {
    if (this.throwOnRead) {
      throw new Error("opaque file read failure");
    }
    const deferred = new Deferred<BrowserLocalReadResult>();
    this.reads.push(Object.freeze({ request, deferred }));
    return deferred.promise;
  }
}

interface SinkSubmission {
  readonly command: BrowserSourceCommand;
  readonly correlation: BrowserSourceSinkCorrelation;
  readonly resources: readonly ArrayBuffer[];
  readonly passedResources: readonly ArrayBuffer[];
}

type FakeSinkMode =
  | BrowserSourceSinkOwnership
  | "FalseTransferred"
  | "Throw";

class FakeSink implements BrowserSourceCommandSink {
  readonly submissions: SinkSubmission[] = [];
  mode: FakeSinkMode;

  constructor(mode: FakeSinkMode = "AdoptedOwnership") {
    this.mode = mode;
  }

  submit(
    command: BrowserSourceCommand,
    correlation: BrowserSourceSinkCorrelation,
    resources: readonly ArrayBuffer[],
  ): BrowserSourceSinkReceipt {
    if (this.mode === "Throw") {
      throw new Error("opaque sink failure");
    }
    const passedResources = Object.freeze([...resources]);
    let retainedResources = passedResources;
    if (this.mode === "Transferred") {
      retainedResources = Object.freeze(
        structuredClone(
          [...resources],
          { transfer: [...resources] },
        ),
      );
    }
    this.submissions.push(Object.freeze({
      command,
      correlation,
      resources: retainedResources,
      passedResources,
    }));
    return Object.freeze({
      ticket: command.payload.ticket,
      ownership: this.mode === "FalseTransferred"
        ? "Transferred"
        : this.mode,
    });
  }
}

interface HttpFixture {
  readonly bridge: BrowserSourceBridge;
  readonly fetcher: FakeHttpFetcher;
  readonly aborts: FakeAbortFactory;
  readonly sink: FakeSink;
}

const httpFixture = (
  options: Readonly<{
    length?: bigint | undefined;
    limits?: BrowserSourceBridgeLimits;
    sinkMode?: FakeSinkMode;
    validator?: BrowserHttpSnapshotValidator;
  }> = {},
): HttpFixture => {
  const fetcher = new FakeHttpFetcher();
  const aborts = new FakeAbortFactory();
  const sink = new FakeSink(options.sinkMode);
  const snapshotValidator = options.validator ?? Object.freeze({
    kind: "StrongEtag" as const,
    value: HTTP_ETAG,
  });
  const source: BrowserSourceAcquirer = Object.freeze({
    kind: "Http",
    url: "https://example.test/document.pdf",
    validator: snapshotValidator,
    fetcher,
  });
  const configuration: BrowserSourceBridgeConfiguration = {
    worker: 1n,
    session: 2n,
    descriptor: httpDescriptor(
      Object.hasOwn(options, "length")
        ? options.length ?? null
        : 100n,
      snapshotValidator,
    ),
    source,
    aborts,
    sink,
    limits: options.limits ?? DEFAULT_LIMITS,
  };
  return Object.freeze({
    bridge: new BrowserSourceBridge(configuration),
    fetcher,
    aborts,
    sink,
  });
};

interface LocalFixture {
  readonly bridge: BrowserSourceBridge;
  readonly reader: FakeLocalReader;
  readonly aborts: FakeAbortFactory;
  readonly sink: FakeSink;
}

const localFixture = (
  options: Readonly<{
    length?: bigint;
    limits?: BrowserSourceBridgeLimits;
    sinkMode?: FakeSinkMode;
  }> = {},
): LocalFixture => {
  const reader = new FakeLocalReader();
  const aborts = new FakeAbortFactory();
  const sink = new FakeSink(options.sinkMode);
  return Object.freeze({
    bridge: new BrowserSourceBridge({
      worker: 1n,
      session: 2n,
      descriptor: descriptor(options.length ?? 32n),
      source: Object.freeze({
        kind: "Local",
        reader,
      }),
      aborts,
      sink,
      limits: options.limits ?? DEFAULT_LIMITS,
    }),
    reader,
    aborts,
    sink,
  });
};

const httpResponse = (
  options: Readonly<{
    status?: number;
    headers: Readonly<Record<string, string>>;
    source?: SourceIdentity;
    validator?: Uint8Array;
    snapshotValidator?: BrowserHttpSnapshotValidator;
    body: ArrayBuffer;
  }>,
): BrowserHttpResponse => {
  const observedSource = options.source ?? sourceIdentity();
  const observedEtag = Object.entries(options.headers).find(
    ([name]) => name.toLowerCase() === "etag",
  )?.[1] ?? HTTP_ETAG;
  const observedValidator =
    options.snapshotValidator ?? Object.freeze({
      kind: "StrongEtag" as const,
      value: observedEtag,
    });
  return Object.freeze({
    type: "Response",
    status: options.status ?? 206,
    headers: new FakeHeaders(options.headers),
    source: observedSource,
    validator: options.validator
      ?? deriveBrowserHttpValidatorBinding(
        observedSource,
        observedValidator,
      ),
    body: options.body,
  });
};

const partialResponse = (
  start: bigint,
  length: bigint,
  total: bigint,
  body: ArrayBuffer,
  options: Readonly<{
    source?: SourceIdentity;
    etag?: string;
    status?: number;
    validator?: Uint8Array;
    snapshotValidator?: BrowserHttpSnapshotValidator;
  }> = {},
): BrowserHttpResponse => httpResponse({
  ...(options.status === undefined
    ? {}
    : { status: options.status }),
  headers: {
    etag: options.etag ?? HTTP_ETAG,
    "content-range":
      `bytes ${start}-${start + length - 1n}/${total}`,
    "content-length": length.toString(),
  },
  ...(options.source === undefined
    ? {}
    : { source: options.source }),
  ...(options.validator === undefined
    ? {}
    : { validator: options.validator }),
  ...(options.snapshotValidator === undefined
    ? {}
    : { snapshotValidator: options.snapshotValidator }),
  body,
});

const settleCallbacks = async (): Promise<void> => {
  await Promise.resolve();
  await Promise.resolve();
};

const onlyFailure = (sink: FakeSink) => {
  assert.equal(sink.submissions.length, 1);
  const submission = sink.submissions[0]!;
  assert.equal(submission.command.type, "FailData");
  if (submission.command.type !== "FailData") {
    throw new Error("expected FailData");
  }
  assert.equal(validateFailDataCommand(submission.command.payload), true);
  assert.deepEqual(submission.resources, []);
  return submission.command.payload;
};

test("strong ETag range transport validates If-Range and queues exact ProvideData", async () => {
  const {
    bridge,
    fetcher,
    sink,
  } = httpFixture();
  assert.equal(
    submitNeed(bridge, needData(10n, [range(10n, 4n)])),
    "Accepted",
  );
  assert.equal(fetcher.requests.length, 1);
  const pending = fetcher.requests[0]!;
  assert.equal(pending.request.method, "GET");
  assert.equal(pending.request.headers.Range, "bytes=10-13");
  assert.equal(pending.request.headers["If-Range"], HTTP_ETAG);
  assert.deepEqual(pending.request.source, sourceIdentity());
  assert.equal(pending.request.maximumBytes, 256);

  const senderBytes = bytes(1, 2, 3, 4);
  pending.deferred.resolve(
    partialResponse(10n, 4n, 100n, senderBytes),
  );
  await settleCallbacks();
  assert.equal(senderBytes.byteLength, 0);
  assert.equal(sink.submissions.length, 0);
  assert.equal(bridge.queuedResults, 1);
  assert.equal(bridge.bufferedBytes, 4);
  assert.equal(
    bridge.cancel(10n, {
      ...DEFAULT_OWNER,
      request: 99n,
    }),
    false,
  );

  assert.equal(bridge.drain(), 1);
  assert.equal(bridge.bufferedBytes, 0);
  const submission = sink.submissions[0]!;
  assert.deepEqual(submission.correlation, {
    worker: 1n,
    session: 2n,
  });
  assert.equal(submission.command.type, "ProvideData");
  if (submission.command.type !== "ProvideData") {
    throw new Error("expected ProvideData");
  }
  assert.equal(validateProvideDataCommand(submission.command.payload), true);
  assert.equal(
    validateProvideDataTransferLengths(
      submission.command.payload,
      submission.resources.map(
        (resource) => BigInt(resource.byteLength),
      ),
    ),
    true,
  );
  assert.deepEqual(submission.command.payload.segments, [{
    range: { start: 10n, len: 4n },
    slot: 0,
    byte_length: 4n,
    role: DataAttachmentRole.ImmutableRangeBytes,
  }]);
  assert.equal(
    submission.resources.every(
      (resource) => resource instanceof ArrayBuffer,
    ),
    true,
  );
  assert.deepEqual(
    Array.from(new Uint8Array(submission.resources[0]!)),
    [1, 2, 3, 4],
  );
  assert.deepEqual(
    Object.keys(submission.command.payload.segments[0]!).sort(),
    ["byte_length", "range", "role", "slot"],
  );
});

test("equivalent immutable validator drives If-Range and response identity checks", async () => {
  const validator = Object.freeze({
    kind: "Equivalent" as const,
    header: "X-Source-Version",
    value: "immutable-revision-7",
  });
  const {
    bridge,
    fetcher,
    sink,
  } = httpFixture({ validator });
  assert.equal(
    submitNeed(bridge, needData(11n, [range(4n, 3n)])),
    "Accepted",
  );
  assert.equal(
    fetcher.requests[0]!.request.headers["If-Range"],
    "immutable-revision-7",
  );
  fetcher.requests[0]!.deferred.resolve(httpResponse({
    headers: {
      "x-source-version": "immutable-revision-7",
      "content-range": "bytes 4-6/100",
      "content-length": "3",
    },
    snapshotValidator: validator,
    body: bytes(4, 5, 6),
  }));
  await settleCallbacks();
  assert.equal(bridge.drain(), 1);
  assert.equal(
    sink.submissions[0]!.command.type,
    "ProvideData",
  );
});

test("bounded no-Range 200 freezes one whole HTTP snapshot and serves later tickets", async () => {
  const {
    bridge,
    fetcher,
    aborts,
    sink,
  } = httpFixture({ length: undefined });
  const whole = sequenceBytes(20);
  assert.equal(
    submitNeed(
      bridge,
      needData(1n, [range(0n, 4n), range(10n, 3n)]),
    ),
    "Accepted",
  );
  assert.equal(
    submitNeed(bridge, needData(2n, [range(5n, 2n)])),
    "Accepted",
  );
  assert.equal(fetcher.requests.length, 2);
  fetcher.requests[0]!.deferred.resolve(httpResponse({
    status: 200,
    headers: {
      etag: HTTP_ETAG,
      "content-length": "20",
    },
    body: whole,
  }));
  await settleCallbacks();

  assert.equal(bridge.knownLength, 20n);
  assert.equal(bridge.hasWholeSourceSnapshot, true);
  assert.equal(aborts.handles[1]!.abortCalls, 1);
  assert.equal(bridge.queuedResults, 2);
  assert.equal(bridge.drain(), 2);
  const first = sink.submissions[0]!;
  assert.deepEqual(
    first.resources.map((resource) =>
      Array.from(new Uint8Array(resource)),
    ),
    [
      [0, 1, 2, 3],
      [10, 11, 12],
    ],
  );
  assert.deepEqual(
    Array.from(new Uint8Array(
      sink.submissions[1]!.resources[0]!,
    )),
    [5, 6],
  );

  const redundantWhole = new Uint8Array(20).fill(99).buffer;
  fetcher.requests[1]!.deferred.resolve(httpResponse({
    status: 200,
    headers: {
      etag: HTTP_ETAG,
      "content-length": "20",
    },
    body: redundantWhole,
  }));
  await settleCallbacks();
  assert.equal(redundantWhole.byteLength, 0);
  assert.equal(bridge.queuedResults, 0);
  assert.equal(
    submitNeed(bridge, needData(3n, [range(5n, 2n)])),
    "Accepted",
  );
  assert.equal(fetcher.requests.length, 2);
  assert.equal(bridge.queuedResults, 1);
  assert.equal(bridge.drain(), 1);
  assert.deepEqual(
    Array.from(new Uint8Array(
      sink.submissions[2]!.resources[0]!,
    )),
    [5, 6],
  );

  bridge.close();
  assert.equal(whole.byteLength, 0);
  assert.equal(bridge.hasWholeSourceSnapshot, false);
});

test("HTTP failures are stable for truncation, CORS, disconnect, and source validators", async () => {
  {
    const {
      bridge,
      fetcher,
      sink,
    } = httpFixture();
    submitNeed(bridge, needData(1n, [range(0n, 4n)]));
    const truncated = bytes(1, 2);
    fetcher.requests[0]!.deferred.resolve(
      partialResponse(0n, 4n, 100n, truncated),
    );
    await settleCallbacks();
    assert.equal(truncated.byteLength, 0);
    bridge.drain();
    const failure = onlyFailure(sink);
    assert.equal(failure.code, SourceFailureCode.Truncated);
    assert.equal(failure.retryable, true);
  }

  for (const [code, retryable] of [
    [SourceFailureCode.PermissionDenied, false],
    [SourceFailureCode.TransportFailure, true],
  ] as const) {
    const {
      bridge,
      fetcher,
      sink,
    } = httpFixture();
    submitNeed(
      bridge,
      needData(BigInt(code), [range(0n, 4n)]),
    );
    fetcher.requests[0]!.deferred.resolve(Object.freeze({
      type: "Failure",
      code,
    }));
    await settleCallbacks();
    assert.equal(sink.submissions.length, 0);
    bridge.drain();
    const failure = onlyFailure(sink);
    assert.equal(failure.code, code);
    assert.equal(failure.retryable, retryable);
  }

  {
    const {
      bridge,
      fetcher,
      sink,
    } = httpFixture();
    submitNeed(bridge, needData(7n, [range(0n, 4n)]));
    fetcher.requests[0]!.deferred.resolve(httpResponse({
      status: 404,
      headers: {},
      body: new ArrayBuffer(0),
    }));
    await settleCallbacks();
    bridge.drain();
    const failure = onlyFailure(sink);
    assert.equal(failure.code, SourceFailureCode.Unavailable);
    assert.equal(failure.retryable, true);
  }

  {
    const {
      bridge,
      fetcher,
      sink,
    } = httpFixture();
    submitNeed(bridge, needData(8n, [range(0n, 4n)]));
    const changed = bytes(1, 2, 3, 4);
    fetcher.requests[0]!.deferred.resolve(partialResponse(
      0n,
      4n,
      100n,
      changed,
      {
        source: sourceIdentity(2n, 0x61),
        etag: "\"pdf-rs-v2\"",
      },
    ));
    await settleCallbacks();
    assert.equal(changed.byteLength, 0);
    bridge.drain();
    const failure = onlyFailure(sink);
    assert.equal(failure.code, SourceFailureCode.SourceChanged);
    assert.equal(failure.retryable, false);
    assert.deepEqual(failure.observed, sourceIdentity(2n, 0x61));
  }

  {
    const {
      bridge,
      fetcher,
      sink,
    } = httpFixture();
    submitNeed(bridge, needData(9n, [range(0n, 4n)]));
    fetcher.requests[0]!.deferred.resolve(partialResponse(
      0n,
      4n,
      100n,
      bytes(1, 2, 3, 4),
      { etag: "\"changed-without-identity\"" },
    ));
    await settleCallbacks();
    bridge.drain();
    assert.equal(
      onlyFailure(sink).code,
      SourceFailureCode.SourceChanged,
    );
  }

  for (const response of [
    partialResponse(
      0n,
      4n,
      100n,
      bytes(1, 2, 3, 4),
      { validator: new Uint8Array(32) },
    ),
    httpResponse({
      headers: {
        "content-range": "bytes 0-3/100",
        "content-length": "4",
      },
      body: bytes(1, 2, 3, 4),
    }),
  ]) {
    const {
      bridge,
      fetcher,
      sink,
    } = httpFixture();
    submitNeed(bridge, needData(10n, [range(0n, 4n)]));
    fetcher.requests[0]!.deferred.resolve(response);
    await settleCallbacks();
    assert.equal(bridge.lifecycle, "SourceChanged");
    bridge.drain();
    assert.equal(
      onlyFailure(sink).code,
      SourceFailureCode.SourceChanged,
    );
  }
});

test("source change poisons the session before any previously queued bytes can publish", async () => {
  const {
    bridge,
    fetcher,
    aborts,
    sink,
  } = httpFixture();
  submitNeed(bridge, needData(1n, [range(0n, 4n)]));
  submitNeed(bridge, needData(2n, [range(8n, 4n)]));

  const previouslyValid = bytes(0, 1, 2, 3);
  fetcher.requests[0]!.deferred.resolve(
    partialResponse(0n, 4n, 100n, previouslyValid),
  );
  await settleCallbacks();
  assert.equal(bridge.queuedResults, 1);

  const changed = bytes(8, 9, 10, 11);
  fetcher.requests[1]!.deferred.resolve(partialResponse(
    8n,
    4n,
    100n,
    changed,
    {
      source: sourceIdentity(2n, 0x61),
      etag: "\"pdf-rs-v2\"",
    },
  ));
  await settleCallbacks();

  assert.equal(bridge.lifecycle, "SourceChanged");
  assert.equal(aborts.handles[1]!.abortCalls, 1);
  assert.equal(bridge.bufferedBytes, 0);
  assert.equal(bridge.queuedResults, 1);
  assert.equal(
    submitNeed(bridge, needData(3n, [range(16n, 4n)])),
    "Inactive",
  );
  assert.equal(bridge.drain(), 1);
  const failure = onlyFailure(sink);
  assert.equal(failure.ticket, 2n);
  assert.equal(failure.code, SourceFailureCode.SourceChanged);
  assert.equal(sink.submissions.length, 1);
  assert.throws(
    () => {
      bridge.restart(2n, 3n);
    },
    (error: unknown) =>
      error instanceof BrowserSourceBridgeError
      && error.code === "InvalidLifecycle",
  );
});

test("Content-Range, returned range, total, status, and body length are exact", async () => {
  const cases: ReadonlyArray<Readonly<{
    response: () => BrowserHttpResponse;
    expected: SourceFailureCode;
  }>> = [
    {
      response: () => httpResponse({
        headers: {
          etag: HTTP_ETAG,
          "content-range": "bytes 1-4/100",
          "content-length": "4",
        },
        body: bytes(1, 2, 3, 4),
      }),
      expected: SourceFailureCode.InvalidRangeResponse,
    },
    {
      response: () => partialResponse(
        0n,
        4n,
        101n,
        bytes(1, 2, 3, 4),
      ),
      expected: SourceFailureCode.InvalidRangeResponse,
    },
    {
      response: () => httpResponse({
        status: 204,
        headers: {
          etag: HTTP_ETAG,
          "content-length": "0",
        },
        body: new ArrayBuffer(0),
      }),
      expected: SourceFailureCode.InvalidRangeResponse,
    },
    {
      response: () => httpResponse({
        headers: {
          etag: HTTP_ETAG,
          "content-range": "bytes 0-3/100",
          "content-length": "5",
        },
        body: bytes(1, 2, 3, 4),
      }),
      expected: SourceFailureCode.InvalidRangeResponse,
    },
    {
      response: () => httpResponse({
        headers: {
          etag: HTTP_ETAG,
          "content-range": "bytes 0-3/*",
          "content-length": "4",
        },
        body: bytes(1, 2, 3, 4),
      }),
      expected: SourceFailureCode.InvalidRangeResponse,
    },
  ];

  for (const [index, entry] of cases.entries()) {
    const {
      bridge,
      fetcher,
      sink,
    } = httpFixture();
    submitNeed(
      bridge,
      needData(BigInt(index + 1), [range(0n, 4n)]),
    );
    fetcher.requests[0]!.deferred.resolve(entry.response());
    await settleCallbacks();
    bridge.drain();
    assert.equal(onlyFailure(sink).code, entry.expected);
  }
});

test("slow acquisition times out without a clock and late bytes are released", async () => {
  const {
    bridge,
    fetcher,
    aborts,
    sink,
  } = httpFixture();
  submitNeed(bridge, needData(5n, [range(0n, 4n)]));
  assert.equal(bridge.timeout(5n, DEFAULT_OWNER), true);
  assert.equal(aborts.handles[0]!.abortCalls, 1);
  assert.equal(bridge.queuedResults, 1);

  const late = bytes(1, 2, 3, 4);
  fetcher.requests[0]!.deferred.resolve(
    partialResponse(0n, 4n, 100n, late),
  );
  await settleCallbacks();
  assert.equal(late.byteLength, 0);
  assert.equal(bridge.queuedResults, 1);
  bridge.drain();
  const failure = onlyFailure(sink);
  assert.equal(failure.code, SourceFailureCode.Timeout);
  assert.equal(failure.retryable, true);
  assert.equal(bridge.drain(), 0);
});

test("local file reads bind exact source, range, total, slots, and transferred ownership", async () => {
  const {
    bridge,
    reader,
    sink,
  } = localFixture({ sinkMode: "Transferred" });
  assert.equal(
    submitNeed(
      bridge,
      needData(7n, [range(0n, 3n), range(8n, 2n)]),
    ),
    "Accepted",
  );
  assert.equal(reader.reads.length, 1);
  assert.deepEqual(reader.reads[0]!.request.range, {
    start: 0n,
    len: 3n,
  });
  assert.equal(reader.reads[0]!.request.maximumBytes, 3);

  reader.reads[0]!.deferred.resolve(Object.freeze({
    type: "Bytes",
    source: sourceIdentity(),
    range: range(0n, 3n),
    totalLength: 32n,
    bytes: bytes(1, 2, 3),
  }));
  await settleCallbacks();
  assert.equal(reader.reads.length, 2);
  assert.equal(sink.submissions.length, 0);

  reader.reads[1]!.deferred.resolve(Object.freeze({
    type: "Bytes",
    source: sourceIdentity(),
    range: range(8n, 2n),
    totalLength: 32n,
    bytes: bytes(8, 9),
  }));
  await settleCallbacks();
  assert.equal(bridge.queuedResults, 1);
  assert.equal(bridge.drain(), 1);

  const submission = sink.submissions[0]!;
  assert.equal(submission.command.type, "ProvideData");
  assert.equal(
    submission.passedResources.every(
      (resource) => resource.byteLength === 0,
    ),
    true,
  );
  assert.deepEqual(
    submission.resources.map((resource) =>
      Array.from(new Uint8Array(resource)),
    ),
    [[1, 2, 3], [8, 9]],
  );
  if (submission.command.type !== "ProvideData") {
    throw new Error("expected ProvideData");
  }
  assert.deepEqual(
    submission.command.payload.segments.map(
      (segment) => segment.slot,
    ),
    [0, 1],
  );
});

test("local mismatched snapshot and returned range become structured failures", async () => {
  {
    const {
      bridge,
      reader,
      sink,
    } = localFixture();
    submitNeed(bridge, needData(1n, [range(0n, 4n)]));
    const returned = bytes(1, 2, 3, 4);
    reader.reads[0]!.deferred.resolve(Object.freeze({
      type: "Bytes",
      source: sourceIdentity(2n, 0x61),
      range: range(0n, 4n),
      totalLength: 32n,
      bytes: returned,
    }));
    await settleCallbacks();
    assert.equal(returned.byteLength, 0);
    bridge.drain();
    assert.equal(
      onlyFailure(sink).code,
      SourceFailureCode.SourceChanged,
    );
  }

  {
    const {
      bridge,
      reader,
      sink,
    } = localFixture();
    submitNeed(bridge, needData(2n, [range(0n, 4n)]));
    reader.reads[0]!.deferred.resolve(Object.freeze({
      type: "Bytes",
      source: sourceIdentity(),
      range: range(1n, 4n),
      totalLength: 32n,
      bytes: bytes(1, 2, 3, 4),
    }));
    await settleCallbacks();
    bridge.drain();
    assert.equal(
      onlyFailure(sink).code,
      SourceFailureCode.InvalidRangeResponse,
    );
  }
});

test("ticket ledger rejects duplicate, overlap, foreign, out-of-range, and bounded work", () => {
  const limits: BrowserSourceBridgeLimits = Object.freeze({
    ...DEFAULT_LIMITS,
    maxActiveTickets: 1,
    maxQueuedResults: 1,
    maxTrackedTickets: 2,
    maxBufferedBytes: 8,
  });
  const {
    bridge,
    aborts,
    reader,
  } = localFixture({ length: 16n, limits });
  assert.equal(
    submitNeed(bridge, needData(9n, [range(8n, 1n)]), {
      worker: 1n,
      session: 2n,
      request: 0n,
    }),
    "InvalidOwner",
  );
  assert.equal(
    submitNeed(bridge, needData(9n, [range(8n, 1n)]), {
      worker: 1n,
      session: 99n,
      request: 3n,
    }),
    "ForeignSession",
  );
  assert.equal(
    submitNeed(bridge, Object.freeze({
      ...needData(9n, [range(8n, 1n)]),
      checkpoint: 0n,
    })),
    "InvalidNeed",
  );
  assert.equal(reader.reads.length, 0);
  assert.equal(aborts.handles.length, 0);
  const first = needData(1n, [range(0n, 4n)]);
  assert.equal(submitNeed(bridge, first), "Accepted");
  assert.equal(
    bridge.cancel(1n, {
      ...DEFAULT_OWNER,
      request: 4n,
    }),
    false,
  );
  assert.equal(submitNeed(bridge, first), "DuplicateTicket");
  assert.equal(
    submitNeed(bridge, needData(2n, [range(2n, 2n)])),
    "OverlappingRange",
  );
  assert.equal(
    submitNeed(
      bridge,
      needData(
        2n,
        [range(8n, 2n)],
        sourceIdentity(2n, 0x61),
      ),
    ),
    "ForeignSource",
  );
  assert.equal(
    submitNeed(bridge, needData(2n, [range(15n, 2n)])),
    "RangeOutOfBounds",
  );
  assert.equal(
    submitNeed(bridge, needData(2n, [range(8n, 2n)])),
    "ActiveTicketLimit",
  );
  assert.equal(
    submitNeed(
      bridge,
      needData(2n, [
        range(8n, 2n),
        range(9n, 2n),
      ]),
    ),
    "InvalidNeed",
  );
  assert.equal(bridge.cancel(1n, DEFAULT_OWNER), true);
  assert.equal(aborts.handles[0]!.abortCalls, 1);
  assert.equal(
    submitNeed(bridge, needData(2n, [range(8n, 9n)])),
    "RangeOutOfBounds",
  );
});

test("cancel, fault, restart, and close abort old work and release late or queued bytes", async () => {
  const {
    bridge,
    fetcher,
    aborts,
    sink,
  } = httpFixture();

  submitNeed(bridge, needData(1n, [range(0n, 4n)]));
  assert.equal(bridge.cancel(1n, DEFAULT_OWNER), true);
  assert.equal(aborts.handles[0]!.abortCalls, 1);
  const cancelledLate = bytes(1, 2, 3, 4);
  fetcher.requests[0]!.deferred.resolve(
    partialResponse(0n, 4n, 100n, cancelledLate),
  );
  await settleCallbacks();
  assert.equal(cancelledLate.byteLength, 0);
  assert.equal(bridge.queuedResults, 0);

  submitNeed(bridge, needData(2n, [range(8n, 4n)]));
  const queuedBeforeFault = bytes(8, 9, 10, 11);
  fetcher.requests[1]!.deferred.resolve(
    partialResponse(8n, 4n, 100n, queuedBeforeFault),
  );
  await settleCallbacks();
  assert.equal(bridge.queuedResults, 1);
  bridge.fault();
  assert.equal(bridge.lifecycle, "Faulted");
  assert.equal(queuedBeforeFault.byteLength, 0);
  assert.equal(bridge.drain(), 0);
  assert.equal(sink.submissions.length, 0);

  bridge.restart(2n, 3n);
  assert.equal(bridge.lifecycle, "Active");
  assert.equal(bridge.worker, 2n);
  assert.equal(bridge.session, 3n);
  assert.equal(
    submitNeed(
      bridge,
      needData(2n, [range(16n, 4n)]),
      DEFAULT_OWNER,
    ),
    "StaleWorker",
  );
  assert.equal(fetcher.requests.length, 2);
  const restartedOwner = Object.freeze({
    worker: 2n,
    session: 3n,
    request: 4n,
  });
  assert.equal(
    submitNeed(
      bridge,
      needData(2n, [range(16n, 4n)]),
      restartedOwner,
    ),
    "Accepted",
  );
  assert.equal(fetcher.requests.length, 3);
  assert.equal(bridge.cancel(2n, DEFAULT_OWNER), false);
  const lateAfterClose = bytes(16, 17, 18, 19);
  bridge.close();
  assert.equal(bridge.lifecycle, "Closed");
  assert.equal(aborts.handles[2]!.abortCalls, 1);
  fetcher.requests[2]!.deferred.resolve(
    partialResponse(16n, 4n, 100n, lateAfterClose),
  );
  await settleCallbacks();
  assert.equal(lateAfterClose.byteLength, 0);
  assert.equal(
    submitNeed(
      bridge,
      needData(3n, [range(0n, 1n)]),
      restartedOwner,
    ),
    "Inactive",
  );
  assert.throws(
    () => {
      bridge.restart(3n, 4n);
    },
    (error: unknown) =>
      error instanceof BrowserSourceBridgeError
      && error.code === "InvalidLifecycle",
  );
});

test("sink must detach resources or explicitly adopt exclusive ownership", async () => {
  {
    const {
      bridge,
      fetcher,
      sink,
    } = httpFixture({ sinkMode: "FalseTransferred" });
    submitNeed(bridge, needData(1n, [range(0n, 4n)]));
    fetcher.requests[0]!.deferred.resolve(
      partialResponse(0n, 4n, 100n, bytes(1, 2, 3, 4)),
    );
    await settleCallbacks();
    assert.throws(
      () => bridge.drain(),
      (error: unknown) =>
        error instanceof BrowserSourceBridgeError
        && error.code === "SinkOwnershipFailure",
    );
    assert.equal(bridge.lifecycle, "Faulted");
    assert.equal(
      sink.submissions[0]!.passedResources[0]!.byteLength,
      0,
    );
  }

  {
    const {
      bridge,
      fetcher,
    } = httpFixture({ sinkMode: "Throw" });
    submitNeed(bridge, needData(2n, [range(0n, 4n)]));
    fetcher.requests[0]!.deferred.resolve(
      partialResponse(0n, 4n, 100n, bytes(1, 2, 3, 4)),
    );
    await settleCallbacks();
    assert.throws(
      () => bridge.drain(),
      (error: unknown) =>
        error instanceof BrowserSourceBridgeError
        && error.code === "SinkFailure",
    );
    assert.equal(bridge.lifecycle, "Faulted");
  }
});

test("validator binding is canonical SHA-256 and descriptor mismatches are rejected", () => {
  const identity = sourceIdentity();
  const validator = Object.freeze({
    kind: "StrongEtag" as const,
    value: HTTP_ETAG,
  });
  const header = new TextEncoder().encode("etag");
  const value = new TextEncoder().encode(HTTP_ETAG);
  const u32 = (number: number): Uint8Array => {
    const encoded = new Uint8Array(4);
    new DataView(encoded.buffer).setUint32(0, number, false);
    return encoded;
  };
  const revision = new Uint8Array(8);
  new DataView(revision.buffer).setBigUint64(
    0,
    identity.revision,
    false,
  );
  const expected = createHash("sha256")
    .update("pdf-rs.browser-source-validator.v1")
    .update(Uint8Array.of(1))
    .update(u32(header.byteLength))
    .update(header)
    .update(u32(value.byteLength))
    .update(value)
    .update(identity.stable_id)
    .update(revision)
    .digest();
  assert.deepEqual(
    deriveBrowserHttpValidatorBinding(identity, validator),
    new Uint8Array(expected),
  );

  assert.throws(
    () => new BrowserSourceBridge({
      worker: 1n,
      session: 2n,
      descriptor: descriptor(),
      source: {
        kind: "Http",
        url: "https://example.test/document.pdf",
        validator,
        fetcher: new FakeHttpFetcher(),
      },
      aborts: new FakeAbortFactory(),
      sink: new FakeSink(),
      limits: DEFAULT_LIMITS,
    }),
    (error: unknown) =>
      error instanceof BrowserSourceBridgeError
      && error.code === "InvalidConfiguration",
  );
});

test("HTTP validator length bounds reject oversized construction inputs", () => {
  const identity = sourceIdentity();
  const validAtBounds = [
    Object.freeze({
      kind: "StrongEtag" as const,
      value: `"${"s".repeat(
        MAX_BROWSER_HTTP_VALIDATOR_VALUE_LENGTH - 2,
      )}"`,
    }),
    Object.freeze({
      kind: "Equivalent" as const,
      header: "x".repeat(
        MAX_BROWSER_HTTP_VALIDATOR_HEADER_NAME_LENGTH,
      ),
      value: "v".repeat(
        MAX_BROWSER_HTTP_VALIDATOR_VALUE_LENGTH,
      ),
    }),
  ] satisfies readonly BrowserHttpSnapshotValidator[];
  for (const validator of validAtBounds) {
    assert.equal(
      deriveBrowserHttpValidatorBinding(
        identity,
        validator,
      ).byteLength,
      32,
    );
  }

  const oversized = [
    Object.freeze({
      kind: "StrongEtag" as const,
      value: `"${"s".repeat(
        MAX_BROWSER_HTTP_VALIDATOR_VALUE_LENGTH - 1,
      )}"`,
    }),
    Object.freeze({
      kind: "Equivalent" as const,
      header: "x".repeat(
        MAX_BROWSER_HTTP_VALIDATOR_HEADER_NAME_LENGTH + 1,
      ),
      value: "revision",
    }),
    Object.freeze({
      kind: "Equivalent" as const,
      header: "x-revision",
      value: "v".repeat(
        MAX_BROWSER_HTTP_VALIDATOR_VALUE_LENGTH + 1,
      ),
    }),
  ] satisfies readonly BrowserHttpSnapshotValidator[];
  for (const validator of oversized) {
    assert.throws(
      () => deriveBrowserHttpValidatorBinding(identity, validator),
      (error: unknown) =>
        error instanceof BrowserSourceBridgeError
        && error.code === "InvalidConfiguration",
    );
    assert.throws(
      () => new BrowserSourceBridge({
        worker: 1n,
        session: 2n,
        descriptor: descriptor(),
        source: {
          kind: "Http",
          url: "https://example.test/document.pdf",
          validator,
          fetcher: new FakeHttpFetcher(),
        },
        aborts: new FakeAbortFactory(),
        sink: new FakeSink(),
        limits: DEFAULT_LIMITS,
      }),
      (error: unknown) =>
        error instanceof BrowserSourceBridgeError
        && error.code === "InvalidConfiguration",
    );
  }
});

test("oversized HTTP response validators fail closed as SourceChanged", async () => {
  const equivalent = Object.freeze({
    kind: "Equivalent" as const,
    header: "X-Source-Version",
    value: "immutable-revision-7",
  });
  for (const entry of [
    Object.freeze({
      fixture: httpFixture(),
      headers: Object.freeze({
        etag: `"${"s".repeat(
          MAX_BROWSER_HTTP_VALIDATOR_VALUE_LENGTH - 1,
        )}"`,
        "content-range": "bytes 0-3/100",
        "content-length": "4",
      }),
    }),
    Object.freeze({
      fixture: httpFixture({ validator: equivalent }),
      headers: Object.freeze({
        "x-source-version": "v".repeat(
          MAX_BROWSER_HTTP_VALIDATOR_VALUE_LENGTH + 1,
        ),
        "content-range": "bytes 0-3/100",
        "content-length": "4",
      }),
    }),
  ]) {
    const body = bytes(1, 2, 3, 4);
    assert.equal(
      submitNeed(
        entry.fixture.bridge,
        needData(1n, [range(0n, 4n)]),
      ),
      "Accepted",
    );
    entry.fixture.fetcher.requests[0]!.deferred.resolve(
      httpResponse({
        headers: entry.headers,
        validator: new Uint8Array(32),
        body,
      }),
    );
    await settleCallbacks();
    assert.equal(body.byteLength, 0);
    assert.equal(
      entry.fixture.bridge.lifecycle,
      "SourceChanged",
    );
    assert.equal(entry.fixture.bridge.drain(), 1);
    assert.equal(
      onlyFailure(entry.fixture.sink).code,
      SourceFailureCode.SourceChanged,
    );
  }
});

test("construction rejects weak validators, non-HTTP URLs, unbounded limits, and lengthless local files", () => {
  const invalidLimits: BrowserSourceBridgeLimits = {
    ...DEFAULT_LIMITS,
    maxQueuedResults: 1,
    maxActiveTickets: 2,
  };
  const strongValidator = Object.freeze({
    kind: "StrongEtag" as const,
    value: HTTP_ETAG,
  });
  for (const entry of [
    Object.freeze({
      descriptor: descriptor(),
      source: Object.freeze({
        kind: "Http" as const,
        url: "https://example.test/document.pdf",
        validator: Object.freeze({
          kind: "StrongEtag" as const,
          value: "W/\"weak\"",
        }),
        fetcher: new FakeHttpFetcher(),
      }),
    }),
    Object.freeze({
      descriptor: httpDescriptor(100n, strongValidator),
      source: Object.freeze({
        kind: "Http" as const,
        url: "file:///tmp/document.pdf",
        validator: strongValidator,
        fetcher: new FakeHttpFetcher(),
      }),
    }),
  ] satisfies readonly Readonly<{
    descriptor: SourceDescriptor;
    source: BrowserSourceAcquirer;
  }>[]) {
    assert.throws(
      () => new BrowserSourceBridge({
        worker: 1n,
        session: 2n,
        descriptor: entry.descriptor,
        source: entry.source,
        aborts: new FakeAbortFactory(),
        sink: new FakeSink(),
        limits: DEFAULT_LIMITS,
      }),
      (error: unknown) =>
        error instanceof BrowserSourceBridgeError
        && error.code === "InvalidConfiguration",
    );
  }

  assert.throws(
    () => new BrowserSourceBridge({
      worker: 1n,
      session: 2n,
      descriptor: httpDescriptor(100n, strongValidator),
      source: {
        kind: "Http",
        url: "https://example.test/document.pdf",
        validator: strongValidator,
        fetcher: new FakeHttpFetcher(),
      },
      aborts: new FakeAbortFactory(),
      sink: new FakeSink(),
      limits: invalidLimits,
    }),
    (error: unknown) =>
      error instanceof BrowserSourceBridgeError
      && error.code === "InvalidConfiguration",
  );

  assert.throws(
    () => new BrowserSourceBridge({
      worker: 1n,
      session: 2n,
      descriptor: descriptor(null),
      source: {
        kind: "Local",
        reader: new FakeLocalReader(),
      },
      aborts: new FakeAbortFactory(),
      sink: new FakeSink(),
      limits: DEFAULT_LIMITS,
    }),
    (error: unknown) =>
      error instanceof BrowserSourceBridgeError
      && error.code === "InvalidConfiguration",
  );
});
