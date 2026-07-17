# Browser host baseline

M5-01 establishes a reproducible browser-host package around the generated
Engine protocol. It does not yet claim a deployable viewer or Worker lifecycle.

## Pinned tools

- Node.js `24.18.0` (the exact version is repeated in `.node-version`, `.nvmrc`,
  and `package.json`)
- npm `11.16.0` (bundled with the pinned Node release)
- TypeScript `5.9.3`
- Rust `1.93.0` from the repository `rust-toolchain.toml`
- Rust target `wasm32-unknown-unknown`

Install dependencies and run the baseline:

```sh
npm ci
npm run check
```

The Wasm target is an explicit prerequisite:

```sh
rustup target add --toolchain 1.93.0 wasm32-unknown-unknown
```

`npm run check` first rejects a Node or TypeScript version mismatch, then proves
that generated TypeScript is unchanged, type-checks the generated declarations
and strict browser code with `noEmit`, executes protocol and runtime-vector
tests with Node's built-in test runner, tests the Rust worker crate on the native
host, and performs the Wasm target check.

`npm run wasm:check` is also available separately for a fast target check.
`npm run wasm:build` links `target/wasm32-unknown-unknown/debug/pdf-rs-browser-worker.wasm`.
The package retains an `rlib` for the native product release closure and adds a
minimal Wasm binary link target. This M5-01 artifact binds the generated
protocol identity but deliberately exposes no raw Wasm ABI or memory pointer; it
is build evidence, not yet a message-dispatching Worker. A later Worker
integration milestone must choose and review the binding strategy before
exposing engine operations.

## Boundary

Control traffic is one canonical binary frame: the generated 20-byte
little-endian header followed by exactly one `fixed_le_v1` payload. The
header's `payload_len` must equal the remaining `ArrayBuffer.byteLength`;
generated message limits and the smaller handshake-negotiated limit both apply.
The current registry authenticates only protocol minor `2`; minor `1` remains
rejected until an immutable historical schema hash and decoder are registered.

Browser objects never appear inside the binary payload. The current command
boundary accepts a separate out-of-band resource table with this fixed indexing
rule:

```text
physical resource table index 0 = binary control ArrayBuffer
logical protocol resource slot n = physical resource table index n + 1
```

Index `0` is reserved even when the message has no logical resources. A payload
slot can therefore never alias the control frame. `ProvideData` slots bind to
fixed, non-resizable `ArrayBuffer` objects of the exact declared length.
Missing, duplicate, extra, aliased, wrongly typed, wrongly sized, or
capability-incompatible entries are rejected before the boundary returns a
validated command. This receiver check binds logical slot numbers, table
length, and declared byte lengths; it cannot distinguish two same-length
buffers that were swapped before receipt.

Command ingress checks binary framing and negotiated limits, runs the generated
codec and descriptor validator, verifies the exact generated handshake object,
and checks the expected Worker epoch. A supervisor-owned admission object then
applies the generated descriptor lifecycle precondition to known sessions,
requests, generations, and Surface release targets. Resource validation follows
that admission check. Only after every check succeeds does the boundary commit
the direction-local sequence number or return resources to its caller. A stale
identity, duplicate or regressing sequence, malformed codec value, lifecycle
mismatch, unsupported capability, or resource failure leaves the sequence
uncommitted.

The admission object is a state gate, not the M5-02 Worker supervisor. Its
caller remains responsible for authoritative transitions and must record a new
Open or GetPageMetrics request and a newly accepted SetViewport generation
immediately after successful decoding. Construction requires explicit session,
request, and Surface capacities within module hard maxima. Identities and
terminal tombstones remain counted for the full Worker epoch so a closed
session, terminal request, or reclaimed Surface cannot be revived or reused;
a new Worker epoch gets a new admission object. Generation high-watermarks are
monotonic and keyed by their already-counted session.

This receiver-side API cannot determine whether an `ArrayBuffer` was transferred
or cloned: structured-clone delivery exposes no transfer-list provenance.
M5-04 must provide a sender ledger that binds each ticket/range/slot to the
exact buffer identity and provenance, then verify detachment or loss of sender
ownership. If receiver validation fails after a real transfer, the sender is
already detached; the receiver discards the rejected resource rather than
claiming that ownership was rolled back.

Surface event decoding, JavaScript `ImageBitmap` or SharedArrayBuffer adoption,
DOM presentation, atomic publication, and the complete lease ledger are not
implemented by M5-01. The Rust worker crate contains pointer-free manifest
primitives that validate declared capabilities, extents, isolation facts, and
fence observations supplied by a future adapter. M5-05 must bind those facts to
the decoded `SurfaceReady` event, perform the actual browser-object and atomic
checks, and keep stale resources on the release or reclaim path. OffscreenCanvas
remains a future Worker-private staging option and is never a wire Surface.

The Rust adapter receives only pointer-free control and resource metadata.
Neither side allows a raw Wasm pointer or `WebAssembly.Memory` to cross a Worker
realm. PDF parsing, capability policy, rendering, and raster semantics remain
in Rust product crates; the TypeScript package owns only browser transport and
Host presentation.
