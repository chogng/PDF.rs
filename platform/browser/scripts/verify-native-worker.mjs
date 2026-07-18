import { readFile, readdir } from "node:fs/promises";

import {
  NATIVE_WORKER_ABI_SHA256,
  NATIVE_WORKER_ABI_VERSION,
  NATIVE_WORKER_EXPORTS,
  renderNativeWorkerGlue,
  sha256,
  validateNativeWorkerModule,
} from "./native-worker-contract.mjs";

const outputDirectory = new URL("../dist/native/", import.meta.url);
const entries = (await readdir(outputDirectory)).sort();
if (
  entries.join("\u0000")
  !== [
    "engine-manifest.json",
    "engine-worker.generated.js",
    "engine.wasm",
  ].join("\u0000")
) {
  throw new Error("Native Worker output must contain one generated glue entry");
}
const manifestText = await readFile(
  new URL("engine-manifest.json", outputDirectory),
  "utf8",
);
const manifest = JSON.parse(manifestText);
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
const exactKeys = (value, expected) =>
  typeof value === "object"
  && value !== null
  && !Array.isArray(value)
  && Object.keys(value).sort().join("\u0000")
    === [...expected].sort().join("\u0000");
if (
  !exactKeys(manifest, [
    "schema",
    "product",
    "protocol_schema_sha256",
    "engine",
    "glue",
  ])
  || manifest.schema !== 1
  || manifest.product !== "PDF.rs Native Wasm Engine Worker"
  || manifest.protocol_schema_sha256 !== schemaMatch[1]
  || !exactKeys(manifest.engine, [
    "file",
    "byte_length",
    "sha256",
    "abi_version",
    "abi_sha256",
    "imports",
    "exports",
    "memory",
  ])
  || manifest.engine.file !== "engine.wasm"
  || !Number.isSafeInteger(manifest.engine.byte_length)
  || !/^[0-9a-f]{64}$/u.test(manifest.engine.sha256)
  || manifest.engine.abi_version !== NATIVE_WORKER_ABI_VERSION
  || manifest.engine.abi_sha256 !== NATIVE_WORKER_ABI_SHA256
  || !Array.isArray(manifest.engine.imports)
  || manifest.engine.imports.length !== 0
  || JSON.stringify(manifest.engine.exports)
    !== JSON.stringify(NATIVE_WORKER_EXPORTS)
  || !exactKeys(manifest.engine.memory, [
    "minimum",
    "maximum",
    "shared",
  ])
  || !exactKeys(manifest.glue, [
    "file",
    "byte_length",
    "sha256",
  ])
  || manifest.glue.file !== "engine-worker.generated.js"
  || !Number.isSafeInteger(manifest.glue.byte_length)
  || !/^[0-9a-f]{64}$/u.test(manifest.glue.sha256)
) {
  throw new Error("invalid Native Worker artifact manifest");
}

const engine = new Uint8Array(await readFile(
  new URL(manifest.engine.file, outputDirectory),
));
const glue = new Uint8Array(await readFile(
  new URL(manifest.glue.file, outputDirectory),
));
const contract = await validateNativeWorkerModule(engine);
if (
  engine.byteLength !== manifest.engine.byte_length
  || sha256(engine) !== manifest.engine.sha256
  || JSON.stringify(contract.memory)
    !== JSON.stringify(manifest.engine.memory)
  || glue.byteLength !== manifest.glue.byte_length
  || sha256(glue) !== manifest.glue.sha256
) {
  throw new Error("Native Worker artifact manifest hash mismatch");
}
const glueText = new TextDecoder("utf-8", { fatal: true }).decode(glue);
const canonicalGlue = renderNativeWorkerGlue({
  byteLength: manifest.engine.byte_length,
  sha256: manifest.engine.sha256,
  minimumMemoryPages: manifest.engine.memory.minimum,
  maximumMemoryPages: manifest.engine.memory.maximum,
});
if (
  glueText !== canonicalGlue
  || !glueText.includes("createNativeWorkerEngineLoader")
  || /^\s*import\s/mu.test(glueText)
  || /pdfium|pdfjs|pdf\.js|mupdf/iu.test(glueText)
) {
  throw new Error("generated Native Worker glue drifted");
}

const glueModule = await import(
  new URL(`engine-worker.generated.js?verify=${manifest.glue.sha256}`, outputDirectory)
);
if (
  Object.keys(glueModule).sort().join("\u0000")
  !== [
    "NATIVE_WORKER_ARTIFACT",
    "createNativeWorkerEngineLoader",
  ].join("\u0000")
  || !Object.isFrozen(glueModule.NATIVE_WORKER_ARTIFACT)
) {
  throw new Error("generated Native Worker glue export surface drifted");
}
const loaderModule = await import(
  new URL("../.test-dist/src/browser-native-worker-loader.js", import.meta.url)
);
const protocolModule = await import(
  new URL("../.test-dist/generated/engine-protocol.js", import.meta.url)
);
const hostHello = {
  major: protocolModule.PROTOCOL_MAJOR,
  minor: protocolModule.PROTOCOL_MINOR,
  schema_hash: protocolModule.SCHEMA_HASH.slice(),
  endpoint_role: protocolModule.EndpointRole.Host,
  capabilities: {
    supported: protocolModule.EndpointCapability.TransferableArrayBuffer,
    mandatory: 0n,
  },
  max_message_bytes: protocolModule.MAX_MESSAGE_BYTES,
  max_transfer_slots: protocolModule.MAX_TRANSFER_SLOTS,
};
const engineHello = {
  ...hostHello,
  schema_hash: protocolModule.SCHEMA_HASH.slice(),
  endpoint_role: protocolModule.EndpointRole.Engine,
};
const connection = protocolModule.negotiateHandshake(hostHello, engineHello);
if (connection === undefined) {
  throw new Error("Native Worker smoke handshake failed");
}
const runtime = {
  fetch: async () => ({
    ok: true,
    headers: new Headers({
      "content-length": String(engine.byteLength),
    }),
    body: new Response(engine.slice()).body,
  }),
  digestSha256: async (bytes) =>
    crypto.subtle.digest("SHA-256", bytes.slice()),
  compile: async (bytes) => WebAssembly.compile(bytes.slice()),
  instantiate: async (module, imports) =>
    WebAssembly.instantiate(module, imports),
};
const loader = glueModule.createNativeWorkerEngineLoader(
  loaderModule.BrowserNativeWorkerLoader,
  runtime,
);
const worker = await loader.load(connection);
loader.close();
await Promise.resolve();
if (!worker.closed) {
  throw new Error("Native Worker generated glue smoke did not close");
}
