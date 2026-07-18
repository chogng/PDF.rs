import assert from "node:assert/strict";
import test from "node:test";

import {
  MAX_MESSAGE_BYTES,
  MAX_TRANSFER_SLOTS,
} from "../generated/engine-protocol.js";
import {
  BROWSER_CONTROL_HEADER_BYTES,
} from "../src/browser-command-boundary.js";
import {
  createBrowserDedicatedWorkerFactory,
  createUnverifiedBrowserNativeWorkerEntryReference,
  type BrowserDedicatedWorker,
} from "../src/browser-dedicated-worker.js";
import {
  BROWSER_NATIVE_WORKER_MAX_CONTROL_BYTES,
  BrowserNativeWorkerEntryController,
  BrowserNativeWorkerEntryError,
  createBrowserNativeWorkerStart,
  type BrowserNativeWorkerEntryFaultCode,
  type BrowserNativeWorkerEntryInstance,
  type BrowserNativeWorkerEntryLoader,
  type BrowserNativeWorkerEntryScheduler,
  type BrowserNativeWorkerEntryScope,
} from "../src/browser-native-worker-entry.js";
import type {
  BrowserNativeWorkerDispatch,
  BrowserNativeWorkerPoll,
  BrowserNativeWorkerSupervisorIdentity,
} from "../src/browser-native-worker-loader.js";
import type {
  BrowserWorkerHandlers,
} from "../src/browser-worker-supervisor.js";

const identity = Object.freeze({
  rendererEpoch: 7,
  worker: 3n,
  workerEpoch: 3n,
});

const entryRegistration = (
  href = "https://viewer.example/native/engine-worker-entry.generated.js",
): Readonly<{
  byteLength: number;
  sha256: string;
  url: URL;
}> => Object.freeze({
  byteLength: 1_024,
  sha256: "a".repeat(64),
  url: new URL(href),
});

const frame = (byte: number): Uint8Array<ArrayBuffer> =>
  Uint8Array.from([byte]);

const dispatch = (
  byte: number,
  transfers: readonly ArrayBuffer[] = Object.freeze([]),
): BrowserNativeWorkerDispatch =>
  Object.freeze({
    frame: frame(byte),
    transfers: Object.freeze([...transfers]),
  });

class FakeScheduler implements BrowserNativeWorkerEntryScheduler {
  readonly #callbacks = new Map<number, () => void>();
  readonly cancelled: number[] = [];
  #next = 1;

  get pending(): number {
    return this.#callbacks.size;
  }

  request(callback: () => void): number {
    const handle = this.#next;
    this.#next += 1;
    this.#callbacks.set(handle, callback);
    return handle;
  }

  cancel(handle: number): void {
    this.cancelled.push(handle);
    this.#callbacks.delete(handle);
  }

  runOne(): void {
    const next = this.#callbacks.entries().next();
    if (next.done) {
      throw new Error("no scheduled callback");
    }
    const [handle, callback] = next.value;
    this.#callbacks.delete(handle);
    callback();
  }

  runAll(maximum = 64): void {
    let count = 0;
    while (this.#callbacks.size > 0) {
      if (count >= maximum) {
        throw new Error("scheduler did not become idle");
      }
      this.runOne();
      count += 1;
    }
  }
}

class FakeScope implements BrowserNativeWorkerEntryScope {
  readonly listeners = new Map<string, Set<EventListener>>();
  readonly posted: Readonly<{
    value: unknown[];
    transfer: ArrayBuffer[];
  }>[] = [];
  closeCount = 0;
  throwPost = false;

  addEventListener(type: string, listener: EventListener): void {
    let listeners = this.listeners.get(type);
    if (listeners === undefined) {
      listeners = new Set();
      this.listeners.set(type, listeners);
    }
    listeners.add(listener);
  }

  removeEventListener(type: string, listener: EventListener): void {
    this.listeners.get(type)?.delete(listener);
  }

  postMessage(value: unknown[], transfer: ArrayBuffer[]): void {
    if (this.throwPost) {
      throw new Error("post");
    }
    this.posted.push(Object.freeze({
      value: [...value],
      transfer: [...transfer],
    }));
  }

  close(): void {
    this.closeCount += 1;
  }

  emit(value: unknown): void {
    const event = new MessageEvent("message", { data: value });
    for (const listener of this.listeners.get("message") ?? []) {
      listener(event);
    }
  }

  emitMessageError(): void {
    const event = new MessageEvent("messageerror");
    for (
      const listener of this.listeners.get("messageerror") ?? []
    ) {
      listener(event);
    }
  }
}

class FakeInstance implements BrowserNativeWorkerEntryInstance {
  readonly engineHello = dispatch(0x11);
  ready = false;
  stopped = false;
  readonly accepted: Uint8Array<ArrayBuffer>[] = [];
  readonly dispatched: Readonly<{
    control: Uint8Array<ArrayBuffer>;
    transfers: readonly ArrayBuffer[];
  }>[] = [];
  readonly pollResults: BrowserNativeWorkerPoll[] = [];
  pollCount = 0;
  stopOnDispatch = false;

  accept(control: Uint8Array): BrowserNativeWorkerDispatch {
    this.accepted.push(control.slice());
    this.ready = true;
    return dispatch(0x22);
  }

  dispatch(
    control: Uint8Array,
    transfers: readonly ArrayBuffer[] = Object.freeze([]),
  ): BrowserNativeWorkerDispatch | undefined {
    this.dispatched.push(Object.freeze({
      control: control.slice(),
      transfers: Object.freeze([...transfers]),
    }));
    if (this.stopOnDispatch) {
      this.stopped = true;
    }
    return dispatch(0x33, transfers);
  }

  poll(): BrowserNativeWorkerPoll {
    this.pollCount += 1;
    return this.pollResults.shift()
      ?? Object.freeze({ output: undefined, pending: false });
  }
}

class DeferredLoader implements BrowserNativeWorkerEntryLoader {
  readonly hello: Uint8Array<ArrayBuffer>[] = [];
  readonly identities: BrowserNativeWorkerSupervisorIdentity[] = [];
  closeCount = 0;
  readonly promise: Promise<BrowserNativeWorkerEntryInstance>;
  resolve:
    ((instance: BrowserNativeWorkerEntryInstance) => void) | undefined;
  reject: (() => void) | undefined;

  constructor() {
    this.promise = new Promise((resolve, reject) => {
      this.resolve = resolve;
      this.reject = (): void => reject(new Error("rejected"));
    });
  }

  bootstrap(
    hostHelloFrame: Uint8Array,
    supervisorIdentity: BrowserNativeWorkerSupervisorIdentity,
  ): Promise<BrowserNativeWorkerEntryInstance> {
    this.hello.push(hostHelloFrame.slice());
    this.identities.push(Object.freeze({ ...supervisorIdentity }));
    return this.promise;
  }

  close(): void {
    this.closeCount += 1;
  }
}

const fixture = (
  overrides: Readonly<{
    maxInboundMessages?: number;
    maxQueuedBytes?: number;
    maxTurn?: number;
    maxTransferBytes?: number;
    maxTransferSlots?: number;
  }> = {},
): Readonly<{
  controller: BrowserNativeWorkerEntryController;
  faults: BrowserNativeWorkerEntryFaultCode[];
  loader: DeferredLoader;
  scheduler: FakeScheduler;
  scope: FakeScope;
}> => {
  const scope = new FakeScope();
  const scheduler = new FakeScheduler();
  const loader = new DeferredLoader();
  const faults: BrowserNativeWorkerEntryFaultCode[] = [];
  const controller = new BrowserNativeWorkerEntryController({
    scope,
    scheduler,
    loader,
    limits: Object.freeze({
      maxInboundMessages: overrides.maxInboundMessages ?? 8,
      maxQueuedBytes: overrides.maxQueuedBytes ?? 2_048,
      maxTurn: overrides.maxTurn ?? 8,
      maxTransferBytes: overrides.maxTransferBytes ?? 1_024,
      maxTransferSlots: overrides.maxTransferSlots ?? 4,
    }),
    fatal: (code): void => {
      faults.push(code);
    },
  });
  return Object.freeze({
    controller,
    faults,
    loader,
    scheduler,
    scope,
  });
};

const physical = (
  control: Uint8Array<ArrayBuffer>,
  transfers: readonly ArrayBuffer[] = Object.freeze([]),
): unknown[] => [control.buffer, ...transfers];

const settle = async (): Promise<void> => {
  await Promise.resolve();
  await Promise.resolve();
};

test("entry performs real bootstrap ordering and pending-driven bounded turns", async () => {
  const session = fixture({ maxTurn: 2 });
  const hello = frame(0x41);
  session.scope.emit(createBrowserNativeWorkerStart(identity));
  session.scope.emit(physical(hello));
  hello[0] = 0xff;
  session.scheduler.runOne();
  assert.equal(session.controller.state, "Loading");
  assert.deepEqual(session.loader.identities, [identity]);
  assert.deepEqual(session.loader.hello[0], frame(0x41));

  const instance = new FakeInstance();
  instance.pollResults.push(
    Object.freeze({
      output: dispatch(0x44),
      pending: true,
    }),
    Object.freeze({
      output: undefined,
      pending: false,
    }),
  );
  session.loader.resolve?.(instance);
  await settle();
  assert.equal(session.scope.posted.length, 0);
  assert.equal(session.controller.state, "Loading");
  session.scheduler.runOne();
  assert.equal(session.controller.state, "AwaitingAccept");
  assert.deepEqual(
    session.scope.posted[0]?.value,
    session.scope.posted[0]?.transfer,
  );

  session.scope.emit(physical(frame(0x42)));
  session.scheduler.runOne();
  assert.equal(session.controller.state, "Ready");
  assert.equal(instance.ready, true);

  const resource = Uint8Array.from([7, 8]).buffer;
  session.scope.emit(physical(frame(0x43), [resource]));
  session.scheduler.runOne();
  assert.equal(instance.pollCount, 1);
  assert.equal(session.scheduler.pending, 1);
  session.scheduler.runOne();
  assert.equal(instance.pollCount, 2);
  assert.equal(session.scheduler.pending, 0);
  assert.equal(session.faults.length, 0);
  assert.equal(session.scope.posted.length, 4);
  for (const posted of session.scope.posted) {
    assert.deepEqual(posted.value, posted.transfer);
    assert.ok(
      posted.transfer.every((entry) => entry instanceof ArrayBuffer),
    );
  }
});

test("bootstrap fulfillment is committed only inside bounded actor turns", async () => {
  const session = fixture({ maxTurn: 1 });
  const instance = new FakeInstance();
  session.scope.emit(createBrowserNativeWorkerStart(identity));
  session.scope.emit(physical(frame(1)));
  session.scheduler.runOne();
  assert.equal(session.controller.state, "AwaitingHello");
  session.scheduler.runOne();
  assert.equal(session.controller.state, "Loading");

  session.loader.resolve?.(instance);
  await settle();
  assert.equal(session.controller.state, "Loading");
  assert.equal(session.scope.posted.length, 0);
  assert.equal(session.scheduler.pending, 1);

  session.scheduler.runOne();
  assert.equal(session.controller.state, "BootstrapReady");
  assert.equal(session.scope.posted.length, 0);
  assert.equal(session.scheduler.pending, 1);

  session.scheduler.runOne();
  assert.equal(session.controller.state, "AwaitingAccept");
  assert.equal(session.scope.posted.length, 1);
  assert.equal(session.scheduler.pending, 0);
});

test("entry rejects non-exact, accessor, proxy, and repeated start records", () => {
  const invalidRecords: unknown[] = [
    {
      ...createBrowserNativeWorkerStart(identity),
      extra: true,
    },
    Object.defineProperty(
      {
        kind: "pdf-rs-native-worker-start-v1",
        rendererEpoch: 7,
        worker: 3n,
      },
      "workerEpoch",
      { enumerable: true, get: () => 3n },
    ),
    new Proxy(
      { ...createBrowserNativeWorkerStart(identity) },
      {},
    ),
  ];
  for (const invalid of invalidRecords) {
    const session = fixture();
    session.scope.emit(invalid);
    session.scheduler.runAll();
    assert.deepEqual(session.faults, ["InvalidBootstrap"]);
    assert.equal(session.loader.closeCount, 1);
    assert.equal(session.scope.closeCount, 1);
  }

  const repeated = fixture();
  repeated.scope.emit(createBrowserNativeWorkerStart(identity));
  repeated.scope.emit(createBrowserNativeWorkerStart(identity));
  repeated.scheduler.runAll();
  assert.deepEqual(repeated.faults, ["InvalidBootstrap"]);
});

test("entry validates small start primitives before any clone", () => {
  const session = fixture();
  const originalStructuredClone = globalThis.structuredClone;
  let cloneCalls = 0;
  let nestedReads = 0;
  const countingStructuredClone = <T>(
    value: T,
    options?: StructuredSerializeOptions,
  ): T => {
    cloneCalls += 1;
    return originalStructuredClone(value, options);
  };
  globalThis.structuredClone = countingStructuredClone;
  try {
    const nested = Object.defineProperty({}, "payload", {
      enumerable: true,
      get: (): ArrayBuffer => {
        nestedReads += 1;
        return new ArrayBuffer(8 * 1_024 * 1_024);
      },
    });
    session.scope.emit({
      kind: "pdf-rs-native-worker-start-v1",
      rendererEpoch: 7,
      worker: nested,
      workerEpoch: 3n,
    });
    session.scope.emit({
      kind: "pdf-rs-native-worker-start-v1",
      rendererEpoch: 7,
      worker: new ArrayBuffer(8 * 1_024 * 1_024),
      workerEpoch: 3n,
    });
  } finally {
    globalThis.structuredClone = originalStructuredClone;
  }
  assert.equal(cloneCalls, 0);
  assert.equal(nestedReads, 0);
  assert.equal(session.controller.queueDepth, 0);
  session.scheduler.runAll();
  assert.deepEqual(session.faults, ["InvalidBootstrap"]);
});

test("entry bounds callback queues and maps messageerror without inline Native work", () => {
  const overflow = fixture({ maxInboundMessages: 1 });
  overflow.scope.emit(createBrowserNativeWorkerStart(identity));
  overflow.scope.emit(physical(frame(1)));
  assert.equal(overflow.faults.length, 0);
  overflow.scheduler.runAll();
  assert.deepEqual(overflow.faults, ["InboundQueueOverflow"]);
  assert.equal(overflow.loader.hello.length, 0);

  const messageError = fixture();
  messageError.scope.emitMessageError();
  assert.equal(messageError.faults.length, 0);
  messageError.scheduler.runAll();
  assert.deepEqual(messageError.faults, ["MessageError"]);
});

test("entry rejects oversized control and aggregate transfers before retaining them", () => {
  const control = fixture();
  control.scope.emit(physical(
    new Uint8Array(
      MAX_MESSAGE_BYTES + BROWSER_CONTROL_HEADER_BYTES + 1,
    ),
  ));
  assert.equal(control.controller.queueDepth, 0);
  control.scheduler.runAll();
  assert.deepEqual(control.faults, ["InvalidBootstrap"]);

  const resources = fixture({ maxTransferBytes: 4 });
  resources.scope.emit(physical(
    frame(1),
    [new ArrayBuffer(3), new ArrayBuffer(2)],
  ));
  assert.equal(resources.controller.queueDepth, 0);
  resources.scheduler.runAll();
  assert.deepEqual(resources.faults, ["InvalidBootstrap"]);
});

test("entry charges an exact cumulative queue byte budget and releases it", () => {
  const exact = fixture({
    maxQueuedBytes: 4,
    maxTransferBytes: 4,
  });
  exact.scope.emit(physical(
    frame(1),
    [new ArrayBuffer(3)],
  ));
  assert.equal(exact.controller.queueDepth, 1);
  assert.equal(exact.controller.queuedPhysicalBytes, 4);
  exact.controller.close();
  assert.equal(exact.controller.queueDepth, 0);
  assert.equal(exact.controller.queuedPhysicalBytes, 0);

  const cumulative = fixture({
    maxQueuedBytes: 4,
    maxTransferBytes: 4,
  });
  cumulative.scope.emit(physical(frame(1), [new ArrayBuffer(1)]));
  cumulative.scope.emit(physical(frame(2), [new ArrayBuffer(1)]));
  cumulative.scope.emit(physical(frame(3)));
  assert.equal(cumulative.controller.queueDepth, 2);
  assert.equal(cumulative.controller.queuedPhysicalBytes, 4);
  cumulative.scheduler.runAll();
  assert.deepEqual(cumulative.faults, ["InvalidBootstrap"]);
  assert.equal(cumulative.controller.queueDepth, 0);
  assert.equal(cumulative.controller.queuedPhysicalBytes, 0);
});

test("default fatal throw still releases every local owner exactly once", () => {
  const scope = new FakeScope();
  const scheduler = new FakeScheduler();
  const loader = new DeferredLoader();
  const controller = new BrowserNativeWorkerEntryController({
    scope,
    scheduler,
    loader,
    limits: Object.freeze({
      maxInboundMessages: 1,
      maxQueuedBytes: 1,
      maxTurn: 1,
      maxTransferBytes: 1,
      maxTransferSlots: 1,
    }),
  });
  scope.emit(Object.freeze({ invalid: true }));
  assert.throws(
    () => scheduler.runOne(),
    (error) =>
      error instanceof BrowserNativeWorkerEntryError
      && error.code === "InvalidBootstrap",
  );
  assert.equal(controller.state, "Failed");
  assert.equal(loader.closeCount, 1);
  assert.equal(scope.closeCount, 1);
  assert.equal(scheduler.pending, 0);
  assert.equal(scope.listeners.get("message")?.size ?? 0, 0);
  assert.equal(scope.listeners.get("messageerror")?.size ?? 0, 0);
  controller.close();
  assert.equal(loader.closeCount, 1);
  assert.equal(scope.closeCount, 1);
});

test("close cancels the turn and makes a late loader resolution inert", async () => {
  const session = fixture();
  session.scope.emit(createBrowserNativeWorkerStart(identity));
  session.scope.emit(physical(frame(1)));
  session.scheduler.runOne();
  assert.equal(session.controller.state, "Loading");
  session.controller.close();
  session.controller.close();
  const instance = new FakeInstance();
  session.loader.resolve?.(instance);
  await settle();
  assert.equal(session.controller.state, "Closed");
  assert.equal(session.loader.closeCount, 1);
  assert.equal(session.scope.closeCount, 1);
  assert.equal(session.scope.posted.length, 0);
  assert.equal(session.scheduler.pending, 0);
});

test("loader rejection, Native trap, and output failure map to stable faults", async () => {
  const rejected = fixture();
  rejected.scope.emit(createBrowserNativeWorkerStart(identity));
  rejected.scope.emit(physical(frame(1)));
  rejected.scheduler.runOne();
  rejected.loader.reject?.();
  await settle();
  rejected.scheduler.runAll();
  assert.deepEqual(rejected.faults, ["NativeFailure"]);
  assert.equal(rejected.loader.closeCount, 1);

  const trapped = fixture();
  const trapInstance = new FakeInstance();
  trapInstance.dispatch = (): never => {
    throw new Error("trap payload must not escape");
  };
  trapped.scope.emit(createBrowserNativeWorkerStart(identity));
  trapped.scope.emit(physical(frame(1)));
  trapped.scheduler.runOne();
  trapped.loader.resolve?.(trapInstance);
  await settle();
  trapped.scheduler.runOne();
  trapped.scope.emit(physical(frame(2)));
  trapped.scheduler.runOne();
  trapped.scope.emit(physical(frame(3)));
  trapped.scheduler.runAll();
  assert.deepEqual(trapped.faults, ["NativeFailure"]);

  const output = fixture();
  output.scope.emit(createBrowserNativeWorkerStart(identity));
  output.scope.emit(physical(frame(1)));
  output.scheduler.runOne();
  output.loader.resolve?.(new FakeInstance());
  await settle();
  output.scope.throwPost = true;
  output.scheduler.runAll();
  assert.deepEqual(output.faults, ["OutboundTransportFailure"]);
});

test("WorkerStopped output closes loader, listeners, scheduler, and scope once", async () => {
  const session = fixture();
  const instance = new FakeInstance();
  session.scope.emit(createBrowserNativeWorkerStart(identity));
  session.scope.emit(physical(frame(1)));
  session.scheduler.runOne();
  session.loader.resolve?.(instance);
  await settle();
  session.scheduler.runOne();
  session.scope.emit(physical(frame(2)));
  session.scheduler.runOne();
  instance.stopOnDispatch = true;
  session.scope.emit(physical(frame(3)));
  session.scheduler.runOne();
  assert.equal(session.controller.state, "Closed");
  assert.equal(session.loader.closeCount, 1);
  assert.equal(session.scope.closeCount, 1);
  assert.equal(session.scheduler.pending, 0);
  assert.equal(
    session.scope.listeners.get("message")?.size ?? 0,
    0,
  );
  session.controller.close();
  assert.equal(session.loader.closeCount, 1);
});

class FakeDedicatedWorker implements BrowserDedicatedWorker {
  readonly listeners = new Map<string, Set<EventListener>>();
  readonly transferTables: ArrayBuffer[][] = [];
  readonly posted: Readonly<{
    value: unknown;
    transfer: ArrayBuffer[];
  }>[] = [];
  terminateCount = 0;

  addEventListener(type: string, listener: EventListener): void {
    let listeners = this.listeners.get(type);
    if (listeners === undefined) {
      listeners = new Set();
      this.listeners.set(type, listeners);
    }
    listeners.add(listener);
  }

  removeEventListener(type: string, listener: EventListener): void {
    this.listeners.get(type)?.delete(listener);
  }

  postMessage(value: unknown, transfer: ArrayBuffer[]): void {
    this.transferTables.push(transfer);
    this.posted.push(Object.freeze({
      value,
      transfer: [...transfer],
    }));
  }

  terminate(): void {
    this.terminateCount += 1;
  }

  emit(type: "message" | "messageerror" | "error", value?: unknown): void {
    const event = type === "message"
      ? new MessageEvent(type, { data: value })
      : new Event(type);
    for (const listener of this.listeners.get(type) ?? []) {
      listener(event);
    }
  }
}

const handlers = (): Readonly<{
  handlers: BrowserWorkerHandlers;
  messages: unknown[];
  messageErrors: number[];
  errors: number[];
  terminated: number[];
}> => {
  const messages: unknown[] = [];
  const messageErrors: number[] = [];
  const errors: number[] = [];
  const terminated: number[] = [];
  return Object.freeze({
    handlers: Object.freeze({
      onMessage: (value: unknown): void => {
        messages.push(value);
      },
      onMessageError: (): void => {
        messageErrors.push(1);
      },
      onError: (): void => {
        errors.push(1);
      },
      onTerminated: (): void => {
        terminated.push(1);
      },
    }),
    messages,
    messageErrors,
    errors,
    terminated,
  });
};

test("Host factory posts one exact start record and invalidates late epoch callbacks", () => {
  const dedicated = new FakeDedicatedWorker();
  const constructed: Readonly<{
    entryUrl: string | URL;
    workerName: string;
  }>[] = [];
  const entry = createUnverifiedBrowserNativeWorkerEntryReference(
    entryRegistration(),
  );
  entry.url.href = "data:text/javascript,throw%200";
  Object.setPrototypeOf(entry.url, null);
  const factory = createBrowserDedicatedWorkerFactory({
    entry,
    rendererEpoch: 7,
    workerNamePrefix: "pdf-rs",
    runtime: {
      construct: (entryUrl, workerName) => {
        constructed.push(Object.freeze({ entryUrl, workerName }));
        return dedicated;
      },
    },
  });
  const port = factory(3n);
  assert.equal(
    constructed[0]?.entryUrl instanceof URL
      ? constructed[0].entryUrl.href
        === "https://viewer.example/native/engine-worker-entry.generated.js"
      : false,
    true,
  );
  assert.equal(constructed[0]?.workerName, "pdf-rs-3");
  assert.equal(dedicated.posted.length, 0);

  const observed = handlers();
  port.setHandlers(observed.handlers);
  assert.deepEqual(
    dedicated.posted[0],
    {
      value: createBrowserNativeWorkerStart(identity),
      transfer: [],
    },
  );
  const control = frame(1).buffer;
  const outboundValue = [control];
  const outboundTransfer = [control];
  port.postMessage(outboundValue, outboundTransfer);
  assert.notEqual(dedicated.posted[1]?.value, outboundValue);
  assert.notEqual(dedicated.transferTables[1], outboundTransfer);
  outboundValue.push(new ArrayBuffer(1));
  outboundTransfer.push(new ArrayBuffer(1));
  assert.deepEqual(dedicated.posted[1]?.value, [control]);
  assert.deepEqual(dedicated.transferTables[1], [control]);
  dedicated.emit("message", ["event"]);
  dedicated.emit("messageerror");
  dedicated.emit("error");
  assert.deepEqual(observed.messages, [["event"]]);
  assert.equal(observed.messageErrors.length, 1);
  assert.equal(observed.errors.length, 1);
  assert.equal(observed.terminated.length, 0);

  port.terminate();
  port.terminate();
  dedicated.emit("message", ["late"]);
  dedicated.emit("messageerror");
  dedicated.emit("error");
  assert.equal(dedicated.terminateCount, 1);
  assert.deepEqual(observed.messages, [["event"]]);
  assert.equal(observed.messageErrors.length, 1);
  assert.equal(observed.errors.length, 1);
  assert.equal(observed.terminated.length, 0);
  assert.throws(() => port.postMessage([control], [control]));
});

test("Host factory fails closed on start, listener, and transfer-table faults", () => {
  class ThrowingDedicatedWorker extends FakeDedicatedWorker {
    throwPost = false;
    throwListener = false;

    override addEventListener(
      type: string,
      listener: EventListener,
    ): void {
      super.addEventListener(type, listener);
      if (this.throwListener && type === "messageerror") {
        throw new Error("listener");
      }
    }

    override postMessage(
      value: unknown,
      transfer: ArrayBuffer[],
    ): void {
      if (this.throwPost) {
        this.emit("error");
        throw new Error("post");
      }
      super.postMessage(value, transfer);
    }
  }

  let invalidConstructTerminateCount = 0;
  const invalidConstructFactory = createBrowserDedicatedWorkerFactory({
    entry: createUnverifiedBrowserNativeWorkerEntryReference(
      entryRegistration(),
    ),
    rendererEpoch: 1,
    workerNamePrefix: "pdf-rs",
    runtime: {
      construct: () => ({
        addEventListener: (): void => undefined,
        removeEventListener: (): void => undefined,
        postMessage: undefined,
        terminate: (): void => {
          invalidConstructTerminateCount += 1;
        },
      } as unknown as BrowserDedicatedWorker),
    },
  });
  assert.throws(() => invalidConstructFactory(1n));
  assert.equal(invalidConstructTerminateCount, 1);

  const startup = new ThrowingDedicatedWorker();
  startup.throwPost = true;
  const startupFactory = createBrowserDedicatedWorkerFactory({
    entry: createUnverifiedBrowserNativeWorkerEntryReference(
      entryRegistration(),
    ),
    rendererEpoch: 1,
    workerNamePrefix: "pdf-rs",
    runtime: { construct: () => startup },
  });
  const startupPort = startupFactory(1n);
  const startupObserved = handlers();
  assert.throws(
    () => startupPort.setHandlers(startupObserved.handlers),
  );
  assert.equal(startup.terminateCount, 1);
  assert.equal(startupObserved.errors.length, 1);

  const listener = new ThrowingDedicatedWorker();
  listener.throwListener = true;
  const listenerFactory = createBrowserDedicatedWorkerFactory({
    entry: createUnverifiedBrowserNativeWorkerEntryReference(
      entryRegistration(),
    ),
    rendererEpoch: 1,
    workerNamePrefix: "pdf-rs",
    runtime: { construct: () => listener },
  });
  const listenerPort = listenerFactory(1n);
  assert.throws(() => listenerPort.setHandlers(handlers().handlers));
  assert.equal(listener.terminateCount, 1);
  assert.equal(listener.listeners.get("message")?.size ?? 0, 0);
  assert.equal(listener.listeners.get("messageerror")?.size ?? 0, 0);

  const transfer = new ThrowingDedicatedWorker();
  const transferFactory = createBrowserDedicatedWorkerFactory({
    entry: createUnverifiedBrowserNativeWorkerEntryReference(
      entryRegistration(),
    ),
    rendererEpoch: 1,
    workerNamePrefix: "pdf-rs",
    runtime: { construct: () => transfer },
  });
  const transferPort = transferFactory(1n);
  transferPort.setHandlers(handlers().handlers);
  const first = new ArrayBuffer(1);
  const second = new ArrayBuffer(1);
  assert.throws(
    () => transferPort.postMessage([first], [second]),
  );
  assert.throws(
    () => transferPort.postMessage([first, first], [first, first]),
  );
  let valuesCalls = 0;
  const ownValues = [first];
  Object.defineProperty(ownValues, "values", {
    configurable: true,
    value: (): never => {
      valuesCalls += 1;
      throw new Error("values must not run");
    },
  });
  assert.throws(
    () => transferPort.postMessage(ownValues, [first]),
  );
  assert.equal(valuesCalls, 0);
  const accessor = [] as ArrayBuffer[];
  let accessorReads = 0;
  Object.defineProperty(accessor, "0", {
    configurable: true,
    enumerable: true,
    get: (): ArrayBuffer => {
      accessorReads += 1;
      return first;
    },
  });
  assert.throws(
    () => transferPort.postMessage(accessor, [first]),
  );
  assert.equal(accessorReads, 0);
  const sparse = new Array<ArrayBuffer>(1);
  assert.throws(
    () => transferPort.postMessage(sparse, [first]),
  );
  let proxyGets = 0;
  const transparentProxy = new Proxy([first], {
    get: (): never => {
      proxyGets += 1;
      throw new Error("ordinary get must not run");
    },
  });
  transferPort.postMessage(transparentProxy, [first]);
  assert.equal(proxyGets, 0);
  assert.ok(Array.isArray(transfer.posted.at(-1)?.value));
  assert.notEqual(transfer.posted.at(-1)?.value, transparentProxy);
  const throwingDescriptorProxy = new Proxy([first], {
    getOwnPropertyDescriptor: (): never => {
      throw new Error("descriptor");
    },
  });
  const postedBeforeDescriptorTrap = transfer.posted.length;
  assert.throws(
    () => transferPort.postMessage(
      throwingDescriptorProxy,
      [first],
    ),
  );
  assert.equal(transfer.posted.length, postedBeforeDescriptorTrap);
  const tooMany = Array.from(
    { length: MAX_TRANSFER_SLOTS + 2 },
    () => new ArrayBuffer(1),
  );
  assert.throws(
    () => transferPort.postMessage(tooMany, tooMany),
  );
  const shared = new SharedArrayBuffer(1) as unknown as ArrayBuffer;
  assert.throws(
    () => transferPort.postMessage([shared], [shared]),
  );
  let byteLengthReads = 0;
  let resizableReads = 0;
  class LyingArrayBuffer extends ArrayBuffer {
    override get byteLength(): number {
      byteLengthReads += 1;
      return 1;
    }

    override get resizable(): boolean {
      resizableReads += 1;
      return false;
    }
  }
  const intrinsicOversize = new LyingArrayBuffer(
    BROWSER_NATIVE_WORKER_MAX_CONTROL_BYTES + 1,
  );
  const postedBeforeIntrinsicOversize = transfer.posted.length;
  assert.throws(
    () => transferPort.postMessage(
      [intrinsicOversize],
      [intrinsicOversize],
    ),
  );
  assert.equal(byteLengthReads, 0);
  assert.equal(resizableReads, 0);
  assert.equal(
    transfer.posted.length,
    postedBeforeIntrinsicOversize,
  );

  assert.throws(
    () => createBrowserDedicatedWorkerFactory({
      entry: {
        url: new URL("https://viewer.example/native/raw.js"),
      } as ReturnType<
        typeof createUnverifiedBrowserNativeWorkerEntryReference
      >,
      rendererEpoch: 1,
      workerNamePrefix: "pdf-rs",
      runtime: { construct: () => transfer },
    }),
  );
  assert.throws(
    () => createUnverifiedBrowserNativeWorkerEntryReference(
      entryRegistration("data:text/javascript,void%200"),
    ),
  );
  for (const invalidRegistration of [
    Object.freeze({
      ...entryRegistration(),
      byteLength: 0,
    }),
    Object.freeze({
      ...entryRegistration(),
      sha256: "not-a-hash",
    }),
    entryRegistration(
      "https://viewer.example/native/engine-worker.generated.js",
    ),
    Object.freeze({
      ...entryRegistration(),
      extra: true,
    }),
    {
      ...entryRegistration(),
    },
    Object.freeze({
      ...entryRegistration(),
      url: Object.create(URL.prototype) as URL,
    }),
  ]) {
    assert.throws(
      () => createUnverifiedBrowserNativeWorkerEntryReference(
        invalidRegistration,
      ),
    );
  }
  const accessorCandidate = {
    byteLength: 1_024,
    sha256: "a".repeat(64),
  } as {
    byteLength: number;
    sha256: string;
    url: URL;
  };
  Object.defineProperty(accessorCandidate, "url", {
    configurable: false,
    enumerable: true,
    get: () =>
      new URL(
        "https://viewer.example/native/engine-worker-entry.generated.js",
      ),
  });
  Object.freeze(accessorCandidate);
  assert.throws(
    () => createUnverifiedBrowserNativeWorkerEntryReference(
      accessorCandidate,
    ),
  );

  const bound = new FakeDedicatedWorker();
  const mutableRuntime = {
    construct: (): BrowserDedicatedWorker => bound,
  };
  const boundFactory = createBrowserDedicatedWorkerFactory({
    entry: createUnverifiedBrowserNativeWorkerEntryReference(
      entryRegistration(),
    ),
    rendererEpoch: 1,
    workerNamePrefix: "pdf-rs",
    runtime: mutableRuntime,
  });
  mutableRuntime.construct = (): BrowserDedicatedWorker => {
    throw new Error("mutated");
  };
  const boundPort = boundFactory(1n);
  boundPort.setHandlers(handlers().handlers);
  assert.equal(bound.posted.length, 1);
});
