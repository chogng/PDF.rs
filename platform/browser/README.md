# Browser host and Worker boundary

M5-01 establishes a reproducible browser-host package around the generated
Engine protocol. M5-02 adds the single-Worker host supervisor, negotiated event
boundary, bounded command/event queues, explicit processing turns, virtual
clock, fault containment, and epoch replacement. M5-04 adds the immutable
Range-source bridge and sender-side transfer ledger; M5-05 adds the
host-mediated Surface bridge. The remaining viewer and three-browser release
gates are deliberately outside this package baseline.

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
host, builds the fixed Native Wasm Worker ABI, and verifies its one-engine,
one-generated-glue manifest.

`npm run wasm:check` is also available separately for a fast target check.
`npm run wasm:build` links `target/wasm32-unknown-unknown/release/pdf-rs-browser-worker.wasm`,
hash-binds it, and emits exactly one generated loader entry plus a manifest.
The generated entry has no build-tree imports: application code injects the
package's exported `BrowserNativeWorkerLoader` into its artifact-bound factory.
ABI v2 requires `bootstrap(hostHelloFrame, supervisorIdentity)`, snapshots the
canonical Host Hello, splits the explicit Worker and Worker-epoch `u64` values
into the five-`u32` initialize call, and then performs the real two-stage Native
handshake. Rust must synchronously return EngineHello; only the generated
negotiator's branded connection is exposed for constructing the actual
HelloAccept. The instance remains pre-Ready until Rust returns Ready and the
generated transcript validator accepts all four messages. No injected
`CompatibleHandshake`, fabricated Engine identity, pre-Ready dispatch, or
pre-Ready poll is a production path. Handshake mismatch, idle output, rejected
status, malformed output, and traps poison and shut down the instance.

Polling returns an output/pending bit result so the Worker entry can schedule
bounded turns without blind spinning. Production links with `panic=abort`; a
Rust panic is a WebAssembly trap and the loader maps it to `EngineTrap` for
supervisor restart. Status `0xffff` remains reserved only for unwind-capable
internal embeddings. The Wasm ABI exposes same-instance memory operations only
to that glue. Raw pointers and `WebAssembly.Memory` never enter a protocol frame
or cross a Worker realm. Every engine control/event message still uses the
generated boundary; the one private exact start record carries only supervisor
identity before Host Hello. The fixture and verifier exercise this same real
EngineHello/Ready bootstrap rather than injecting protocol state.

`browser-native-worker-entry.ts` is the production-ready Worker-realm
controller prerequisite. One exact own-data start record binds the supervisor
identity before canonical Host Hello; the controller then copies the
transferred control buffer, calls the real loader bootstrap, forwards Native
EngineHello, accepts only the supervisor's HelloAccept, forwards Native Ready,
and moves subsequent physical resource tables without decoding PDF semantics.
Callbacks perform only bounded structural snapshot/byte charging, then queue an
admitted start/physical record or a stable fault; no protocol or Native work
runs inline. Control length is rejected before copying above the generated
message maximum plus its 20-byte header; resource bytes use a checked
per-message sum bounded by the Native maximum memory, so the count-bounded queue
also charges an independent cumulative byte budget no larger than that memory
before any control copy. Shifting or releasing a record returns its charge.
Cancelable actor turns bound Native and poll work, and another turn is requested
only for queued input, queued bootstrap completion, a required post-dispatch
probe, or Native's pending bit. WorkerStopped, fault, explicit close, and late
completion each release listeners, scheduled work, loader ownership, and the
Worker realm once.

`browser-dedicated-worker.ts` adapts that controller to the existing
`BrowserWorkerFactory` consumed by the supervisor and reader. It installs all
Host listeners before posting the one start record, snapshots the injected
constructor function, invalidates late callbacks on termination, and validates
the exact `[control, ...resources]` transfer table. The private canonical entry
URL is unaffected if an embedding mutates the reference's public diagnostic
`URL` object. Browsers expose no normal-exit event for Dedicated Workers, so
the adapter never fabricates `onTerminated`: protocol WorkerStopped closes the
clean path, browser error/messageerror feeds the fault path, and Host teardown
actively calls the idempotent port termination. Silent UA termination requires
the request/watchdog liveness proof still outstanding in M5-07/M5-09.

The entry URL reference is deliberately named
`UnverifiedBrowserNativeWorkerEntryReference`: the brand prevents an accidental
raw string but proves neither same-origin deployment, integrity, registration,
nor executable entry installation. The current generated
`engine-worker.generated.js` only exports the artifact record and loader
factory; it is not a top-level Worker entry and must not be passed to the
adapter. Its resource kind and network class are `native-loader-module` and
`native-loader-glue`; precaching only makes that immutable dependency available
offline and grants it no Worker-entry role. Accordingly,
`worker_graph.entrypoints` remains exactly empty. M5-09 must emit and
hash-register a real module bundle that invokes
`installBrowserNativeWorkerEntry`, then create the unverified reference from
that registered URL and exercise it in real browsers.

## Product resource closure

`product/browser-product-policy.json` is the canonical, sorted browser product
registry. It binds the exact TypeScript module graph, three Native output
resources, generated Wasm ABI, single-Worker graph, closed CSP, service-Worker
precache, bounded network classes, and build-only npm leaves. Every registered
leaf records source, integrity, license, semantic ownership, an installed-byte
budget, and a replacement plan. The shipped third-party browser leaf set is
empty: the npm leaves are build-only, while the Wasm target's Cargo graph
contains only repository-owned PDF.rs crates.

`npm run purity:check` hashes the canonical policy, dependency lock, complete
module graph, CSP/precache/network/Worker registries, and artifact manifest. A
bounded TypeScript lexical inventory decodes Unicode identifiers and separates
comments, strings, templates, and regular-expression literals before rejecting
ambient or aliased network, Worker, service-worker, and dynamic-execution
primitives. Bounded static-string propagation covers constant fragments and
template interpolation, while every computed member access and direct Reflect
member is held to the exact reviewed per-module inventory; computed capability
invocation is forbidden. The five currently reviewed `fetch` call shapes are
also exact. The check rejects comment-separated external module specifiers,
constant-built external URLs, dynamic imports, source maps, additional
executables or Wasm payloads, nonzero Wasm imports, ABI/export drift, and any
resource outside `engine-manifest.json`,
`engine-worker.generated.js`, and `engine.wasm`. Source files that define
rejection tests are not treated as shipped resources; only the exact registered
module and artifact closure is scanned. The module graph and every Native
artifact are pinned by exact SHA-256 and byte length.

`npm run purity:closure` copies at most 4,096 repository files and 128 MiB into
a fresh temporary repository, excluding every target directory, `node_modules`,
the separate `tools/baseline` graph, and the real repository's neighboring
checkout. It creates a private empty `CARGO_HOME`, strips inherited Cargo and
Rust compiler wrappers and registry overrides, asserts that no sibling
`pdfium` directory exists, performs the Native Wasm build with Cargo network
access forced offline, then re-runs the same product-purity check inside that
isolated closure using a freshly generated browser-only workspace lock. The
command never moves, renames, or reads the real `../pdfium` checkout.

The network trace validator requires a canonical product base URL, an exact
Host-selected immutable source identity, and the exact viewer-module URLs
derived from the pinned module graph. It classifies requests by URL rather than
trusting a trace-supplied resource label, binds every trace record to its
registered identity, requires exact Native artifact byte lengths and hashes,
and rejects arbitrary same-origin suffixes, fragments, cross-origin,
missing-resource, zero-byte, identity, and length substitutions. Viewer
requests cover each of the fourteen registered ESM projections exactly once;
Worker and Wasm byte and request budgets cover the initial artifact epoch plus
sixteen bounded restarts.

This is currently the static purity and trace-validation foundation, not the
M5-08 runtime proof. The graph now registers one exact Dedicated/module Worker
constructor site and its controller/adapter source, but deliberately has no
installed executable entry bundle, no integrity-bound reference creation, no
production viewer bundle, and no real browser trace. M5-09 must register those
final resources and feed observed Chromium/Firefox/WebKit requests into the
validator before M5-08 can complete.

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

The M5-02 supervisor owns separate queued-admission and successfully-sent
ledgers. Queue admission reserves identities and monotonic sequence numbers,
but a Worker outcome is authorized only after the corresponding `postMessage`
succeeds. The sent ledger records Open, GetPageMetrics, SetViewport, close,
shutdown, and event-side lifecycle transitions. Construction requires explicit
session, request, Surface, inbound, outbound, critical, and replaceable
capacities within module hard maxima.
Identities and terminal tombstones remain counted for the full Worker epoch so
a closed session, terminal request, or reclaimed Surface cannot be revived or
reused; restart terminates the old port before creating a new admission object
and Worker identity. Generation high-watermarks are monotonic and keyed by
their already-counted session.

Worker callbacks only enqueue an opaque physical resource table or a stable
fault marker. Handshake decoding, generated validation, lifecycle mutation,
application event delivery, and outbound sending occur only through explicit
bounded turns. Host Hello must be sent before EngineHello is accepted, and
HelloAccept must be sent before Ready is accepted. Independent queue capacities
reserve command and event space without changing canonical sequence order:
draining always chooses the smallest retained sequence across ordinary,
critical, and coalesced viewport queues. Replaced viewport frames may leave a
legal sequence gap but can never make an older retained frame regress.
Application delivery likewise selects the smallest retained event sequence
across critical and coalesced progress queues; replacing progress updates its
delivery position to the replacement's sequence.

The injected monotonic clock makes startup and graceful-shutdown deadlines
deterministic. A malformed frame, messageerror, Worker error, unexpected
termination, queue overflow, transport failure, or deadline expiry terminates
the live port, clears pending epoch work, and publishes one bounded,
content-free supervisor `WorkerFault` with an explicit host-transport or
engine-protocol origin. Old callbacks carry an epoch token and are ignored after
restart. Graceful-shutdown time begins only after its command is successfully
sent.

This receiver-side API cannot determine whether an `ArrayBuffer` was transferred
or cloned: structured-clone delivery exposes no transfer-list provenance. The
M5-04 source bridge therefore keeps a sender ledger that binds each
ticket/range/slot to the exact buffer identity and provenance, then verifies
detachment or loss of sender ownership. If receiver validation fails after a
real transfer, the sender is already detached; the receiver discards the
rejected resource rather than claiming that ownership was rolled back.

M5-02 decodes Surface event control data and validates the negotiated resource
slot count; M5-05 binds decoded `SurfaceReady` metadata to actual
`ImageBitmap`, `ArrayBuffer`, or negotiated fenced `SharedArrayBuffer` objects,
performs the browser-object and atomic checks, and keeps stale resources on the
release or reclaim path. The Rust worker crate supplies pointer-free manifest
primitives for declared capabilities, extents, isolation facts, and fence
observations. OffscreenCanvas is an optional Worker-private staging optimization
and is never a DOM-bound wire Surface.

The Rust adapter receives only pointer-free control and resource metadata.
Neither side allows a raw Wasm pointer or `WebAssembly.Memory` to cross a Worker
realm. PDF parsing, capability policy, rendering, and raster semantics remain
in Rust product crates; the TypeScript package owns only browser transport and
Host presentation.
