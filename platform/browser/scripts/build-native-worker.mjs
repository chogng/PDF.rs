import { spawnSync } from "node:child_process";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import {
  NATIVE_WORKER_ABI_SHA256,
  NATIVE_WORKER_ABI_VERSION,
  NATIVE_WORKER_EXPORTS,
  renderNativeWorkerEntry,
  renderNativeWorkerGlue,
  renderNativeWorkerHost,
  sha256,
  validateNativeWorkerModule,
} from "./native-worker-contract.mjs";
import { canonicalJson } from "./browser-product-purity.mjs";

const browserRoot = fileURLToPath(new URL("../", import.meta.url));
const repositoryRoot = fileURLToPath(new URL("../../../", import.meta.url));
const sourceArtifact = fileURLToPath(new URL(
  "../../../target/wasm32-unknown-unknown/release/pdf-rs-browser-worker.wasm",
  import.meta.url,
));
const outputDirectory = new URL("../dist/native/", import.meta.url);
const engineUrl = new URL("engine.wasm", outputDirectory);
const entryUrl = new URL(
  "engine-worker-entry.generated.js",
  outputDirectory,
);
const glueUrl = new URL("engine-worker.generated.js", outputDirectory);
const hostUrl = new URL(
  "engine-worker-host.generated.js",
  outputDirectory,
);
const manifestUrl = new URL("engine-manifest.json", outputDirectory);

const exportedFunctions = NATIVE_WORKER_EXPORTS.filter(
  (name) =>
    name !== "memory"
    && name !== "__data_end"
    && name !== "__heap_base",
);
const rustFlags = [
  "-C",
  "debuginfo=0",
  "-C",
  "panic=abort",
  "-C",
  "link-arg=--max-memory=67108864",
  "-C",
  "link-arg=--strip-debug",
  ...exportedFunctions.flatMap((name) => [
    "-C",
    `link-arg=--export=${name}`,
  ]),
].join("\u001f");
const environment = {
  ...process.env,
  CARGO_ENCODED_RUSTFLAGS: rustFlags,
  CARGO_INCREMENTAL: "0",
  SOURCE_DATE_EPOCH: "0",
};
delete environment.RUSTFLAGS;

const build = spawnSync(
  "cargo",
  [
    "build",
    "--release",
    "--locked",
    "--manifest-path",
    `${repositoryRoot}Cargo.toml`,
    "--package",
    "pdf-rs-browser-worker",
    "--bin",
    "pdf-rs-browser-worker",
    "--target",
    "wasm32-unknown-unknown",
  ],
  {
    cwd: browserRoot,
    encoding: "utf8",
    env: environment,
  },
);
if (build.status !== 0) {
  process.stderr.write(build.stdout);
  process.stderr.write(build.stderr);
  process.exit(build.status ?? 1);
}

const engine = new Uint8Array(await readFile(sourceArtifact));
const contract = await validateNativeWorkerModule(engine);
const engineSha256 = sha256(engine);
const generatedProtocol = await readFile(
  new URL("../generated/engine-protocol.ts", import.meta.url),
  "utf8",
);
const schemaMatch = generatedProtocol.match(
  /SCHEMA_SHA256_HEX = "([0-9a-f]{64})"/u,
);
if (schemaMatch === null) {
  throw new Error("generated protocol schema hash was not found");
}

const entry = renderNativeWorkerEntry();
const entryBytes = new TextEncoder().encode(entry);
const glue = renderNativeWorkerGlue({
  byteLength: engine.byteLength,
  sha256: engineSha256,
  minimumMemoryPages: contract.memory.minimum,
  maximumMemoryPages: contract.memory.maximum,
  entryByteLength: entryBytes.byteLength,
  entrySha256: sha256(entryBytes),
});
const glueBytes = new TextEncoder().encode(glue);
const host = renderNativeWorkerHost();
const hostBytes = new TextEncoder().encode(host);
const manifest = {
  schema: 1,
  product: "PDF.rs Native Wasm Engine Worker",
  protocol_schema_sha256: schemaMatch[1],
  engine: {
    file: "engine.wasm",
    byte_length: engine.byteLength,
    sha256: engineSha256,
    abi_version: NATIVE_WORKER_ABI_VERSION,
    abi_sha256: NATIVE_WORKER_ABI_SHA256,
    imports: [],
    exports: NATIVE_WORKER_EXPORTS,
    memory: contract.memory,
  },
  glue: {
    file: "engine-worker.generated.js",
    byte_length: glueBytes.byteLength,
    sha256: sha256(glueBytes),
  },
  entry: {
    file: "engine-worker-entry.generated.js",
    byte_length: entryBytes.byteLength,
    sha256: sha256(entryBytes),
  },
  host: {
    file: "engine-worker-host.generated.js",
    byte_length: hostBytes.byteLength,
    sha256: sha256(hostBytes),
  },
};

await rm(outputDirectory, { recursive: true, force: true });
await mkdir(outputDirectory, { recursive: true });
await writeFile(engineUrl, engine);
await writeFile(entryUrl, entryBytes);
await writeFile(glueUrl, glueBytes);
await writeFile(hostUrl, hostBytes);
await writeFile(
  manifestUrl,
  canonicalJson(manifest),
);
