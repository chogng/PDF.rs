import { spawnSync } from "node:child_process";
import {
  cp,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  MAX_NATIVE_WORKER_ENTRY_BYTES,
  MAX_NATIVE_WORKER_HOST_BYTES,
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

const outputDirectory = new URL("../dist/native/", import.meta.url);
const entries = (await readdir(outputDirectory)).sort();
if (
  entries.join("\u0000")
  !== [
    "engine-manifest.json",
    "engine-worker-entry.generated.js",
    "engine-worker-host.generated.js",
    "engine-worker.generated.js",
    "engine.wasm",
  ].join("\u0000")
) {
  throw new Error("Native Worker output resource registry drifted");
}
const manifestText = await readFile(
  new URL("engine-manifest.json", outputDirectory),
  "utf8",
);
const manifest = JSON.parse(manifestText);
if (manifestText !== canonicalJson(manifest)) {
  throw new Error("Native Worker artifact manifest is not canonical JSON");
}
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
    "entry",
    "glue",
    "host",
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
  || !exactKeys(manifest.entry, [
    "file",
    "byte_length",
    "sha256",
  ])
  || manifest.entry.file !== "engine-worker-entry.generated.js"
  || !Number.isSafeInteger(manifest.entry.byte_length)
  || manifest.entry.byte_length <= 0
  || manifest.entry.byte_length > MAX_NATIVE_WORKER_ENTRY_BYTES
  || !/^[0-9a-f]{64}$/u.test(manifest.entry.sha256)
  || !exactKeys(manifest.host, [
    "file",
    "byte_length",
    "sha256",
  ])
  || manifest.host.file !== "engine-worker-host.generated.js"
  || !Number.isSafeInteger(manifest.host.byte_length)
  || manifest.host.byte_length <= 0
  || manifest.host.byte_length > MAX_NATIVE_WORKER_HOST_BYTES
  || !/^[0-9a-f]{64}$/u.test(manifest.host.sha256)
) {
  throw new Error("invalid Native Worker artifact manifest");
}

const engine = new Uint8Array(await readFile(
  new URL(manifest.engine.file, outputDirectory),
));
const glue = new Uint8Array(await readFile(
  new URL(manifest.glue.file, outputDirectory),
));
const entry = new Uint8Array(await readFile(
  new URL(manifest.entry.file, outputDirectory),
));
const host = new Uint8Array(await readFile(
  new URL(manifest.host.file, outputDirectory),
));
const contract = await validateNativeWorkerModule(engine);
if (
  engine.byteLength !== manifest.engine.byte_length
  || sha256(engine) !== manifest.engine.sha256
  || canonicalJson(contract.memory)
    !== canonicalJson(manifest.engine.memory)
  || glue.byteLength !== manifest.glue.byte_length
  || sha256(glue) !== manifest.glue.sha256
  || entry.byteLength !== manifest.entry.byte_length
  || sha256(entry) !== manifest.entry.sha256
  || host.byteLength !== manifest.host.byte_length
  || sha256(host) !== manifest.host.sha256
) {
  throw new Error("Native Worker artifact manifest hash mismatch");
}
const glueText = new TextDecoder("utf-8", { fatal: true }).decode(glue);
const canonicalGlue = renderNativeWorkerGlue({
  byteLength: manifest.engine.byte_length,
  sha256: manifest.engine.sha256,
  minimumMemoryPages: manifest.engine.memory.minimum,
  maximumMemoryPages: manifest.engine.memory.maximum,
  entryByteLength: manifest.entry.byte_length,
  entrySha256: manifest.entry.sha256,
});
const entryText = new TextDecoder("utf-8", { fatal: true }).decode(entry);
const canonicalEntry = renderNativeWorkerEntry();
const hostText = new TextDecoder("utf-8", { fatal: true }).decode(host);
const canonicalHost = renderNativeWorkerHost();
if (
  glueText !== canonicalGlue
  || !glueText.includes("createNativeWorkerEngineLoader")
  || /^\s*import\s/mu.test(glueText)
  || /pdfium|pdfjs|pdf\.js|mupdf/iu.test(glueText)
) {
  throw new Error("generated Native Worker glue drifted");
}
if (
  entryText !== canonicalEntry
  || !entryText.includes("installBrowserNativeWorkerEntry")
  || !entryText.includes("NATIVE_WORKER_ARTIFACT")
  || /pdfium|pdfjs|pdf\.js|mupdf/iu.test(entryText)
) {
  throw new Error("generated Native Worker entry drifted");
}
if (
  hostText !== canonicalHost
  || !hostText.includes(
    "createUnverifiedBrowserNativeWorkerEntryReference",
  )
  || !hostText.includes("NATIVE_WORKER_ENTRY_ARTIFACT")
  || /pdfium|pdfjs|pdf\.js|mupdf/iu.test(hostText)
) {
  throw new Error("generated Native Worker Host registration drifted");
}

const glueModule = await import(
  new URL(`engine-worker.generated.js?verify=${manifest.glue.sha256}`, outputDirectory)
);
if (
  Object.keys(glueModule).sort().join("\u0000")
  !== [
    "NATIVE_WORKER_ARTIFACT",
    "NATIVE_WORKER_ENTRY_ARTIFACT",
    "createNativeWorkerEngineLoader",
  ].join("\u0000")
  || !Object.isFrozen(glueModule.NATIVE_WORKER_ARTIFACT)
  || !Object.isFrozen(glueModule.NATIVE_WORKER_ENTRY_ARTIFACT)
  || glueModule.NATIVE_WORKER_ENTRY_ARTIFACT.byteLength
    !== manifest.entry.byte_length
  || glueModule.NATIVE_WORKER_ENTRY_ARTIFACT.sha256
    !== manifest.entry.sha256
  || glueModule.NATIVE_WORKER_ENTRY_ARTIFACT.url.href
    !== new URL(manifest.entry.file, outputDirectory).href
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
const supervisorIdentity = Object.freeze({
  worker: 1n,
  workerEpoch: 1n,
  rendererEpoch: 1,
});
const unwrap = (result) => {
  if (!result.ok) {
    throw new Error("Native Worker smoke protocol codec failed");
  }
  return result.value;
};
let inputSequence = 1n;
const encodeCommandFrame = (command) => {
  const descriptor = protocolModule.MESSAGE_DESCRIPTORS.find(
    (candidate) =>
      candidate.kind === "command"
      && candidate.name === command.type,
  );
  if (descriptor === undefined) {
    throw new Error("Native Worker smoke descriptor missing");
  }
  const correlation = { worker: supervisorIdentity.worker };
  const payload = command.type === "Hello"
    ? protocolModule.encodeHelloCommandPayload(command.payload)
    : protocolModule.encodeHelloAcceptCommandPayload(command.payload);
  const payloadLength = unwrap(
    protocolModule.encodeCorrelationPayload(correlation),
  ).byteLength + unwrap(payload).byteLength;
  const header = {
    major: protocolModule.PROTOCOL_MAJOR,
    minor: protocolModule.PROTOCOL_MINOR,
    message_type: descriptor.id,
    flags: 0,
    payload_len: payloadLength,
    sequence: inputSequence,
  };
  inputSequence += 1n;
  const encoded = unwrap(protocolModule.encodeCommandPayload({
    header,
    correlation,
    command,
  }));
  const frame = new Uint8Array(20 + encoded.bytes.byteLength);
  const view = new DataView(frame.buffer);
  view.setUint16(0, header.major, true);
  view.setUint16(2, header.minor, true);
  view.setUint16(4, header.message_type, true);
  view.setUint16(6, header.flags, true);
  view.setUint32(8, header.payload_len, true);
  view.setBigUint64(12, header.sequence, true);
  frame.set(encoded.bytes, 20);
  return frame;
};
const decodeEventType = (dispatch) => {
  const view = new DataView(
    dispatch.frame.buffer,
    dispatch.frame.byteOffset,
    dispatch.frame.byteLength,
  );
  const header = {
    major: view.getUint16(0, true),
    minor: view.getUint16(2, true),
    message_type: view.getUint16(4, true),
    flags: view.getUint16(6, true),
    payload_len: view.getUint32(8, true),
    sequence: view.getBigUint64(12, true),
  };
  return unwrap(
    protocolModule.decodeEventPayload(
      header,
      dispatch.frame.subarray(20),
    ),
  ).event.type;
};
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
const worker = await loader.bootstrap(
  encodeCommandFrame({
    type: "Hello",
    payload: { hello: hostHello },
  }),
  supervisorIdentity,
);
if (decodeEventType(worker.engineHello) !== "EngineHello") {
  throw new Error("Native Worker smoke EngineHello missing");
}
const ready = worker.accept(encodeCommandFrame({
  type: "HelloAccept",
  payload: {
    negotiated_minor: worker.connection.minor,
    schema_hash: protocolModule.SCHEMA_HASH.slice(),
  },
}));
if (decodeEventType(ready) !== "Ready" || !worker.ready) {
  throw new Error("Native Worker smoke Ready missing");
}
loader.close();
await Promise.resolve();
if (!worker.closed) {
  throw new Error("Native Worker generated glue smoke did not close");
}

const temporary = await mkdtemp(
  join(tmpdir(), "pdf-rs-native-worker-entry-"),
);
try {
  const deployment = join(temporary, "deployment");
  await mkdir(deployment);
  await cp(
    fileURLToPath(outputDirectory),
    join(deployment, "native"),
    { recursive: true },
  );
  await cp(
    fileURLToPath(
      new URL("../.test-dist/generated/", import.meta.url),
    ),
    join(deployment, "generated"),
    { recursive: true },
  );
  await cp(
    fileURLToPath(new URL("../.test-dist/src/", import.meta.url)),
    join(deployment, "src"),
    { recursive: true },
  );
  const deployedEntry = pathToFileURL(
    join(deployment, "native", manifest.entry.file),
  ).href;
  const deployedHost = pathToFileURL(
    join(deployment, "native", manifest.host.file),
  ).href;
  const probe = `
const counts = { message: 0, messageerror: 0 };
Object.defineProperties(globalThis, {
  addEventListener: {
    configurable: true,
    value: (type) => {
      if (type in counts) counts[type] += 1;
    },
  },
  close: { configurable: true, value: () => undefined },
  postMessage: { configurable: true, value: () => undefined },
  removeEventListener: {
    configurable: true,
    value: () => undefined,
  },
});
const installed = await import(${JSON.stringify(deployedEntry)});
if (
  Object.keys(installed).length !== 0
  || counts.message !== 1
  || counts.messageerror !== 1
) {
  throw new Error("generated entry did not install exactly once");
}
`;
  const result = spawnSync(
    process.execPath,
    ["--input-type=module", "--eval", probe],
    { encoding: "utf8" },
  );
  if (result.status !== 0) {
    process.stderr.write(result.stdout);
    process.stderr.write(result.stderr);
    throw new Error("generated Native Worker entry installer smoke failed");
  }
  const deployedGluePath = join(
    deployment,
    "native",
    manifest.glue.file,
  );
  const deployedGlueText = await readFile(deployedGluePath, "utf8");
  const hostSmokeEntry =
    "https://viewer.example/native/engine-worker-entry.generated.js";
  const hostSmokeGlue = deployedGlueText.replace(
    'new URL("./engine-worker-entry.generated.js", import.meta.url)',
    `new URL(${JSON.stringify(hostSmokeEntry)}, import.meta.url)`,
  );
  if (hostSmokeGlue === deployedGlueText) {
    throw new Error("Host registration smoke URL substitution failed");
  }
  await writeFile(
    deployedGluePath,
    hostSmokeGlue,
  );
  const hostProbe = `
const host = await import(${JSON.stringify(deployedHost)});
if (
  Object.keys(host).join("\\u0000") !== "NATIVE_WORKER_ENTRY_REFERENCE"
  || !Object.isFrozen(host.NATIVE_WORKER_ENTRY_REFERENCE)
  || host.NATIVE_WORKER_ENTRY_REFERENCE.url.href
    !== ${JSON.stringify(hostSmokeEntry)}
) {
  throw new Error("generated Host registration did not bind entry artifact");
}
`;
  const hostResult = spawnSync(
    process.execPath,
    ["--input-type=module", "--eval", hostProbe],
    { encoding: "utf8" },
  );
  if (hostResult.status !== 0) {
    process.stderr.write(hostResult.stdout);
    process.stderr.write(hostResult.stderr);
    throw new Error("generated Native Worker Host registration smoke failed");
  }
} finally {
  await rm(temporary, { force: true, recursive: true });
}
