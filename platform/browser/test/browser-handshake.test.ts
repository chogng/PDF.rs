import assert from "node:assert/strict";
import test from "node:test";

import {
  EndpointRole,
  KNOWN_ENDPOINT_CAPABILITIES,
  MAX_MESSAGE_BYTES,
  MAX_TRANSFER_SLOTS,
  PROTOCOL_MAJOR,
  PROTOCOL_MINOR,
  SCHEMA_HASH,
  SCHEMA_HASH_HEX,
  isCompatibleHandshake,
} from "../generated/engine-protocol.js";
import {
  BrowserHandshakeError,
  negotiateBrowserHello,
} from "../src/browser-handshake.js";

const schemaHash = (): Uint8Array =>
  Uint8Array.from(
    SCHEMA_HASH_HEX.match(/.{2}/gu)?.map((byte) =>
      Number.parseInt(byte, 16),
    ) ?? [],
  );

const hello = (
  role: EndpointRole,
  supported = KNOWN_ENDPOINT_CAPABILITIES,
  mandatory = 0n,
): unknown => ({
  major: PROTOCOL_MAJOR,
  minor: PROTOCOL_MINOR,
  schema_hash: schemaHash(),
  endpoint_role: role,
  capabilities: {
    supported,
    mandatory,
  },
  max_message_bytes: MAX_MESSAGE_BYTES,
  max_transfer_slots: MAX_TRANSFER_SLOTS,
});

test("negotiates exact schema, opposite roles, limits, and known capabilities", () => {
  const negotiated = negotiateBrowserHello(
    hello(EndpointRole.Host),
    hello(
      EndpointRole.Engine,
      KNOWN_ENDPOINT_CAPABILITIES | 0x8000000000000000n,
    ),
  );

  assert.equal(negotiated.minor, PROTOCOL_MINOR);
  assert.equal(negotiated.capabilities, KNOWN_ENDPOINT_CAPABILITIES);
  assert.equal(negotiated.max_message_bytes, MAX_MESSAGE_BYTES);
  assert.equal(negotiated.max_transfer_slots, MAX_TRANSFER_SLOTS);
  assert.ok(Object.isFrozen(negotiated));
});

test("generated negotiation rejects a schema fork and a same-role peer", () => {
  const fork = hello(EndpointRole.Engine) as {
    schema_hash: Uint8Array;
  };
  fork.schema_hash[0] = (fork.schema_hash[0] ?? 0) ^ 0xff;
  assert.throws(
    () => negotiateBrowserHello(hello(EndpointRole.Host), fork),
    (error: unknown) =>
      error instanceof BrowserHandshakeError &&
      error.code === "InvalidHandshake",
  );

  assert.throws(
    () =>
      negotiateBrowserHello(
        hello(EndpointRole.Host),
        hello(EndpointRole.Host),
      ),
    (error: unknown) =>
      error instanceof BrowserHandshakeError &&
      error.code === "InvalidHandshake",
  );
});

test("rejects each direction of a missing mandatory capability", () => {
  assert.throws(
    () =>
      negotiateBrowserHello(
        hello(EndpointRole.Host, 0x3n, 0x2n),
        hello(EndpointRole.Engine, 0x1n),
      ),
    (error: unknown) =>
      error instanceof BrowserHandshakeError &&
      error.code === "InvalidHandshake",
  );

  assert.throws(
    () =>
      negotiateBrowserHello(
        hello(EndpointRole.Host, 0x1n),
        hello(EndpointRole.Engine, 0x3n, 0x2n),
      ),
    (error: unknown) =>
      error instanceof BrowserHandshakeError &&
      error.code === "InvalidHandshake",
  );
});

test("rejects an unregistered minor before negotiation", () => {
  const oldPeer = hello(EndpointRole.Engine) as {
    minor: number;
  };
  oldPeer.minor = PROTOCOL_MINOR - 1;

  assert.throws(
    () => negotiateBrowserHello(hello(EndpointRole.Host), oldPeer),
    (error: unknown) =>
      error instanceof BrowserHandshakeError &&
      error.code === "InvalidHandshake",
  );
});

test("rejects accessor-backed Hellos without invoking untrusted getters", () => {
  const peer = hello(EndpointRole.Engine) as Record<string, unknown>;
  let reads = 0;
  Object.defineProperty(peer, "max_transfer_slots", {
    configurable: true,
    enumerable: true,
    get: () => {
      reads += 1;
      return MAX_TRANSFER_SLOTS;
    },
  });

  assert.throws(
    () => negotiateBrowserHello(hello(EndpointRole.Host), peer),
    (error: unknown) =>
      error instanceof BrowserHandshakeError
      && error.code === "InvalidHandshake",
  );
  assert.equal(reads, 0);
});

test("mutating the exported schema-hash copy cannot move the trust anchor", () => {
  const canonical = schemaHash();
  SCHEMA_HASH.fill(0);
  try {
    const host = hello(EndpointRole.Host) as {
      schema_hash: Uint8Array;
    };
    const engine = hello(EndpointRole.Engine) as {
      schema_hash: Uint8Array;
    };
    host.schema_hash = SCHEMA_HASH.slice();
    engine.schema_hash = SCHEMA_HASH.slice();

    assert.throws(
      () => negotiateBrowserHello(host, engine),
      (error: unknown) =>
        error instanceof BrowserHandshakeError
        && error.code === "InvalidHandshake",
    );
    assert.doesNotThrow(
      () =>
        negotiateBrowserHello(
          hello(EndpointRole.Host),
          hello(EndpointRole.Engine),
        ),
    );
  } finally {
    SCHEMA_HASH.set(canonical);
  }
});

test("handshake authenticity ignores WeakSet prototype pollution", () => {
  const originalAdd = WeakSet.prototype.add;
  const originalHas = WeakSet.prototype.has;
  assert.equal(
    Reflect.set(WeakSet.prototype, "add", () => new WeakSet<object>()),
    true,
  );
  assert.equal(
    Reflect.set(WeakSet.prototype, "has", () => true),
    true,
  );
  try {
    const negotiated = negotiateBrowserHello(
      hello(EndpointRole.Host),
      hello(EndpointRole.Engine),
    );
    assert.equal(isCompatibleHandshake(negotiated), true);
    assert.equal(isCompatibleHandshake({ ...negotiated }), false);
  } finally {
    const addRestored = Reflect.set(
      WeakSet.prototype,
      "add",
      originalAdd,
    );
    const hasRestored = Reflect.set(
      WeakSet.prototype,
      "has",
      originalHas,
    );
    assert.equal(addRestored, true);
    assert.equal(hasRestored, true);
  }
});
