import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { lstat, readFile, readdir } from "node:fs/promises";
import { dirname, relative, resolve, sep } from "node:path";
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

const DEFAULT_BROWSER_ROOT = fileURLToPath(new URL("../", import.meta.url));
const POLICY_RELATIVE_PATH = "product/browser-product-policy.json";
const PACKAGE_LOCK_RELATIVE_PATH = "package-lock.json";
const PACKAGE_RELATIVE_PATH = "package.json";
const NATIVE_OUTPUT_RELATIVE_PATH = "dist/native";
const MAX_POLICY_BYTES = 256 * 1024;
const MAX_LOCK_BYTES = 4 * 1024 * 1024;
const MAX_SOURCE_BYTES = 4 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES = 256;
const MAX_CARGO_METADATA_BYTES = 16 * 1024 * 1024;
const MAX_LEXICAL_TOKENS = 500_000;
const MAX_LEXICAL_TOKEN_BYTES = 1024 * 1024;
const MAX_LEXICAL_NESTING = 256;
const MAX_STATIC_STRING_LENGTH = 64 * 1024;
const MAX_STATIC_STRING_BINDINGS = 4_096;
const MAX_STATIC_EVALUATION_STEPS = 1_000_000;
const MAX_PRODUCT_MODULE_NETWORK_BYTES = 16 * 1_024 * 1_024;
const MAX_WORKER_EPOCHS_PER_OPEN = 17;
const FORBIDDEN_ENGINE_TOKENS = Object.freeze([
  "pdfium",
  "pdf.js",
  "pdfjs",
  "mupdf",
  "poppler",
  "remote-renderer",
  "remote_renderer",
]);
const POLICY_KEYS = Object.freeze([
  "build_dependencies",
  "budgets",
  "cargo_graph",
  "csp",
  "module_graph",
  "network_manifest",
  "product",
  "resources",
  "schema",
  "service_worker",
  "shipped_third_party_leaves",
  "wasm_policy",
  "worker_graph",
]);
const REQUIRED_CSP = Object.freeze({
  "base-uri": ["'none'"],
  "connect-src": ["'self'", "selected-source:"],
  "default-src": ["'none'"],
  "object-src": ["'none'"],
  "script-src": ["'self'", "'wasm-unsafe-eval'"],
  "worker-src": ["'self'"],
});
const REQUIRED_NATIVE_FILES = Object.freeze([
  "engine-manifest.json",
  "engine-worker-entry.generated.js",
  "engine-worker-host.generated.js",
  "engine-worker.generated.js",
  "engine.wasm",
]);
const REQUIRED_RESOURCE_PATHS = Object.freeze(
  REQUIRED_NATIVE_FILES.map((path) => `dist/native/${path}`),
);
const REQUIRED_RESOURCE_BINDINGS = Object.freeze({
  "dist/native/engine-manifest.json": Object.freeze({
    executable: false,
    hash_binding: "canonical-sha256",
    kind: "native-artifact-manifest",
    ownership: "PDF.rs browser release",
  }),
  "dist/native/engine-worker.generated.js": Object.freeze({
    executable: true,
    hash_binding: "engine-manifest.json#/glue/sha256",
    kind: "native-loader-module",
    ownership: "PDF.rs browser host",
  }),
  "dist/native/engine-worker-entry.generated.js": Object.freeze({
    executable: true,
    hash_binding: "engine-manifest.json#/entry/sha256",
    kind: "native-worker-entry-module",
    ownership: "PDF.rs browser host",
  }),
  "dist/native/engine-worker-host.generated.js": Object.freeze({
    executable: true,
    hash_binding: "engine-manifest.json#/host/sha256",
    kind: "native-worker-host-registration-module",
    ownership: "PDF.rs browser host",
  }),
  "dist/native/engine.wasm": Object.freeze({
    executable: true,
    hash_binding: "engine-manifest.json#/engine/sha256",
    kind: "native-wasm-engine",
    ownership: "PDF.rs Native engine",
  }),
});
const REQUIRED_PRECACHE = Object.freeze(
  REQUIRED_NATIVE_FILES.map((path) => `./native/${path}`),
);
const REQUIRED_NETWORK_IDS = Object.freeze([
  "native-loader-glue",
  "native-wasm",
  "native-worker-entry",
  "native-worker-host",
  "product-module-graph",
  "selected-source",
]);
const REQUIRED_NETWORK_BINDINGS = Object.freeze({
  "native-loader-glue": Object.freeze({
    kind: "integrity-bound-product-resource",
    location: "./native/engine-worker.generated.js",
    ownership: "PDF.rs browser host",
  }),
  "native-wasm": Object.freeze({
    kind: "integrity-bound-product-resource",
    location: "./native/engine.wasm",
    ownership: "PDF.rs Native engine",
  }),
  "native-worker-entry": Object.freeze({
    kind: "integrity-bound-product-resource",
    location: "./native/engine-worker-entry.generated.js",
    ownership: "PDF.rs browser host",
  }),
  "native-worker-host": Object.freeze({
    kind: "integrity-bound-product-resource",
    location: "./native/engine-worker-host.generated.js",
    ownership: "PDF.rs browser host",
  }),
  "product-module-graph": Object.freeze({
    kind: "same-origin-static-product-modules",
    location: "module-graph:",
    ownership: "PDF.rs browser product",
  }),
  "selected-source": Object.freeze({
    kind: "host-selected-immutable-pdf-source",
    location: "selected-source:",
    ownership: "PDF.rs source bridge",
  }),
});
const REQUIRED_WORKER_MODULE_FILES = Object.freeze([
  "generated/engine-protocol.ts",
  "src/browser-command-admission.ts",
  "src/browser-command-boundary.ts",
  "src/browser-event-boundary.ts",
  "src/browser-handshake.ts",
  "src/browser-native-worker-entry.ts",
  "src/browser-native-worker-loader.ts",
  "src/native-worker-abi.generated.ts",
]);
const REQUIRED_HOST_REGISTRATION = Object.freeze({
  artifact: "dist/native/engine-worker-host.generated.js",
  entry_artifact_export: "NATIVE_WORKER_ENTRY_ARTIFACT",
  entry_reference_export: "NATIVE_WORKER_ENTRY_REFERENCE",
});
const REQUIRED_WORKER_CONSTRUCTOR_SITE = Object.freeze({
  constructor_kind: "DedicatedWorker",
  credentials: "same-origin",
  entry_binding: "UnverifiedBrowserNativeWorkerEntryReference",
  path: "src/browser-dedicated-worker.ts",
  registration_binding:
    "dist/native/engine-worker-host.generated.js#NATIVE_WORKER_ENTRY_REFERENCE",
  worker_type: "module",
});
const REQUIRED_WORKER_CONSTRUCTOR_SITE_KEYS = Object.freeze(
  Object.keys(REQUIRED_WORKER_CONSTRUCTOR_SITE).sort(),
);
const FORBIDDEN_AMBIENT_IDENTIFIERS = Object.freeze(new Set([
  "EventSource",
  "Function",
  "SharedWorker",
  "WebSocket",
  "XMLHttpRequest",
  "document",
  "eval",
  "globalThis",
  "importScripts",
  "navigator",
  "require",
  "self",
  "sendBeacon",
  "window",
]));
const EXPECTED_FETCH_IDENTIFIER_COUNTS = Object.freeze({
  "src/browser-native-worker-loader.ts": 5,
  "src/browser-reader-client.ts": 4,
  "src/browser-source-bridge.ts": 2,
});
const EXPECTED_COMPUTED_MEMBER_ACCESS_COUNTS = Object.freeze({
  "generated/engine-protocol.ts": 77,
  "src/browser-command-admission.ts": 4,
  "src/browser-command-boundary.ts": 10,
  "src/browser-dedicated-worker.ts": 14,
  "src/browser-event-boundary.ts": 8,
  "src/browser-handshake.ts": 0,
  "src/browser-native-worker-entry.ts": 11,
  "src/browser-native-worker-loader.ts": 52,
  "src/browser-reader-client.ts": 7,
  "src/browser-source-bridge.ts": 63,
  "src/browser-surface-bridge.ts": 54,
  "src/browser-viewer.ts": 15,
  "src/browser-worker-supervisor.ts": 30,
  "src/native-worker-abi.generated.ts": 0,
});
const EXPECTED_REFLECT_MEMBER_COUNTS = Object.freeze({
  "generated/engine-protocol.ts": Object.freeze({ apply: 1, ownKeys: 1 }),
  "src/browser-command-admission.ts": Object.freeze({
    apply: 1,
    ownKeys: 1,
  }),
  "src/browser-command-boundary.ts": Object.freeze({ ownKeys: 1 }),
  "src/browser-dedicated-worker.ts": Object.freeze({
    apply: 1,
    ownKeys: 3,
  }),
  "src/browser-event-boundary.ts": Object.freeze({ apply: 3, ownKeys: 1 }),
  "src/browser-native-worker-entry.ts": Object.freeze({ ownKeys: 1 }),
  "src/browser-reader-client.ts": Object.freeze({ get: 1 }),
  "src/browser-source-bridge.ts": Object.freeze({ get: 1 }),
  "src/browser-surface-bridge.ts": Object.freeze({ ownKeys: 2 }),
  "src/browser-viewer.ts": Object.freeze({ get: 1 }),
});
const DANGEROUS_CONSTANT_VALUES = Object.freeze(new Set([
  "constructor",
  "document",
  "eventsource",
  "eval",
  "fetch",
  "function",
  "globalthis",
  "importscripts",
  "navigator",
  "require",
  "sendbeacon",
  "serviceworker",
  "sharedworker",
  "webassembly",
  "websocket",
  "window",
  "worker",
  "xmlhttprequest",
]));
const REQUIRED_BUILD_DEPENDENCY_KEYS = Object.freeze([
  "dependency_range",
  "direct",
  "integrity",
  "license",
  "max_installed_bytes",
  "name",
  "ownership",
  "required_by",
  "replacement",
  "source",
  "version",
]);
const REQUIRED_RESOURCE_KEYS = Object.freeze([
  "byte_length",
  "executable",
  "hash_binding",
  "kind",
  "max_bytes",
  "ownership",
  "path",
  "sha256",
]);
const REQUIRED_NETWORK_KEYS = Object.freeze([
  "id",
  "kind",
  "location",
  "max_bytes",
  "max_requests_per_open",
  "ownership",
]);

export class BrowserProductPurityError extends Error {
  constructor(code, detail) {
    super(`${code}: ${detail}`);
    this.name = "BrowserProductPurityError";
    this.code = code;
  }
}

const fail = (code, detail) => {
  throw new BrowserProductPurityError(code, detail);
};

const isPlainObject = (value) =>
  typeof value === "object"
  && value !== null
  && !Array.isArray(value)
  && Object.getPrototypeOf(value) === Object.prototype;

const exactKeys = (value, expected) =>
  isPlainObject(value)
  && JSON.stringify(Object.keys(value).sort())
    === JSON.stringify([...expected].sort());

const sortedUniqueStrings = (value, maximum, label) => {
  if (
    !Array.isArray(value)
    || value.length > maximum
    || value.some((entry) => typeof entry !== "string" || entry.length === 0)
  ) {
    fail("RPE-BROWSER-PURITY-0002", `invalid ${label}`);
  }
  const sorted = [...value].sort();
  if (
    new Set(value).size !== value.length
    || JSON.stringify(value) !== JSON.stringify(sorted)
  ) {
    fail("RPE-BROWSER-PURITY-0002", `${label} must be sorted and unique`);
  }
  return value;
};

const safePositiveInteger = (value, maximum = Number.MAX_SAFE_INTEGER) =>
  Number.isSafeInteger(value) && value > 0 && value <= maximum;

const canonicalValue = (value) => {
  if (Array.isArray(value)) {
    return value.map(canonicalValue);
  }
  if (isPlainObject(value)) {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, canonicalValue(value[key])]),
    );
  }
  return value;
};

export const canonicalJson = (value) =>
  `${JSON.stringify(canonicalValue(value), null, 2)}\n`;

const digestText = (text) =>
  createHash("sha256").update(text, "utf8").digest("hex");

const readBounded = async (path, maximum, label) => {
  const metadata = await lstat(path).catch(() => undefined);
  if (
    metadata === undefined
    || !metadata.isFile()
    || metadata.isSymbolicLink()
    || metadata.size > maximum
  ) {
    fail("RPE-BROWSER-PURITY-0001", `invalid ${label}`);
  }
  return readFile(path);
};

const readBoundedText = async (path, maximum, label) => {
  const bytes = await readBounded(path, maximum, label);
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    fail("RPE-BROWSER-PURITY-0001", `${label} is not UTF-8`);
  }
};

const parseCanonicalJson = async (path, maximum, label) => {
  const text = await readBoundedText(path, maximum, label);
  let value;
  try {
    value = JSON.parse(text);
  } catch {
    fail("RPE-BROWSER-PURITY-0001", `${label} is not JSON`);
  }
  if (text !== canonicalJson(value)) {
    fail("RPE-BROWSER-PURITY-0002", `${label} is not canonical JSON`);
  }
  return { text, value };
};

const parseJson = async (path, maximum, label) => {
  const text = await readBoundedText(path, maximum, label);
  try {
    return { text, value: JSON.parse(text) };
  } catch {
    fail("RPE-BROWSER-PURITY-0001", `${label} is not JSON`);
  }
};

const checkRelativePath = (path, label) => {
  if (
    typeof path !== "string"
    || path.length === 0
    || path.includes("\\")
    || path.startsWith("/")
    || path.split("/").some((part) => part === "" || part === "." || part === "..")
  ) {
    fail("RPE-BROWSER-PURITY-0002", `unsafe ${label}`);
  }
};

const assertNoForbiddenToken = (value, label) => {
  const normalized = value.toLowerCase();
  const token = FORBIDDEN_ENGINE_TOKENS.find((entry) =>
    normalized.includes(entry)
  );
  if (token !== undefined) {
    fail("RPE-BROWSER-PURITY-0003", `${label} contains ${token}`);
  }
};

const validatePolicy = (policy) => {
  if (
    !exactKeys(policy, POLICY_KEYS)
    || policy.schema !== 1
    || policy.product !== "PDF.rs Native Browser"
    || !exactKeys(policy.budgets, [
      "max_bundle_files",
      "max_cargo_packages",
      "max_module_edges",
      "max_module_files",
      "max_native_artifact_bytes",
      "max_network_requests_per_open",
      "max_precache_entries",
      "max_shipped_third_party_leaves",
      "max_worker_count",
    ])
  ) {
    fail("RPE-BROWSER-PURITY-0002", "invalid product policy envelope");
  }
  for (const [key, value] of Object.entries(policy.budgets)) {
    if (!Number.isSafeInteger(value) || value < 0) {
      fail("RPE-BROWSER-PURITY-0002", `invalid budget ${key}`);
    }
  }
  if (
    policy.budgets.max_bundle_files !== REQUIRED_NATIVE_FILES.length
    || policy.budgets.max_cargo_packages > 64
    || policy.budgets.max_module_files > 256
    || policy.budgets.max_module_edges > 1_024
    || policy.budgets.max_native_artifact_bytes > 128 * 1024 * 1024
    || policy.budgets.max_network_requests_per_open > 8_192
    || policy.budgets.max_precache_entries !== REQUIRED_PRECACHE.length
    || policy.budgets.max_shipped_third_party_leaves !== 0
    || policy.budgets.max_worker_count !== 1
  ) {
    fail("RPE-BROWSER-PURITY-0002", "product budgets exceed hard bounds");
  }

  if (
    !exactKeys(policy.cargo_graph, ["packages", "root"])
    || policy.cargo_graph.root !== "pdf-rs-browser-worker"
  ) {
    fail("RPE-BROWSER-PURITY-0010", "invalid Cargo graph policy");
  }
  const cargoPackages = sortedUniqueStrings(
    policy.cargo_graph.packages,
    policy.budgets.max_cargo_packages,
    "Cargo packages",
  );
  if (
    cargoPackages.length !== policy.budgets.max_cargo_packages
    || !cargoPackages.includes(policy.cargo_graph.root)
    || cargoPackages.some((name) => !name.startsWith("pdf-rs-"))
  ) {
    fail("RPE-BROWSER-PURITY-0010", "Cargo package registry drifted");
  }

  if (canonicalJson(policy.csp) !== canonicalJson(REQUIRED_CSP)) {
    fail("RPE-BROWSER-PURITY-0007", "CSP policy drifted");
  }

  if (
    !exactKeys(policy.module_graph, ["entrypoints", "files", "sha256"])
    || typeof policy.module_graph.sha256 !== "string"
    || !/^[0-9a-f]{64}$/u.test(policy.module_graph.sha256)
  ) {
    fail("RPE-BROWSER-PURITY-0004", "invalid module graph policy");
  }
  const moduleFiles = sortedUniqueStrings(
    policy.module_graph.files,
    policy.budgets.max_module_files,
    "module files",
  );
  const entrypoints = sortedUniqueStrings(
    policy.module_graph.entrypoints,
    moduleFiles.length,
    "module entrypoints",
  );
  for (const path of moduleFiles) {
    checkRelativePath(path, "module path");
    if (!path.endsWith(".ts") || path.startsWith("test/")) {
      fail("RPE-BROWSER-PURITY-0004", `invalid product module ${path}`);
    }
  }
  if (entrypoints.some((path) => !moduleFiles.includes(path))) {
    fail("RPE-BROWSER-PURITY-0004", "module entrypoint is not registered");
  }

  if (
    !Array.isArray(policy.resources)
    || policy.resources.length !== policy.budgets.max_bundle_files
    || policy.resources.some((entry) => !exactKeys(entry, REQUIRED_RESOURCE_KEYS))
  ) {
    fail("RPE-BROWSER-PURITY-0005", "invalid resource registry");
  }
  const resourcePaths = policy.resources.map((entry) => entry.path);
  if (JSON.stringify(resourcePaths) !== JSON.stringify(REQUIRED_RESOURCE_PATHS)) {
    fail("RPE-BROWSER-PURITY-0005", "resource registry is not exact");
  }
  let resourceBytes = 0;
  for (const resource of policy.resources) {
    checkRelativePath(resource.path, "resource path");
    const required = REQUIRED_RESOURCE_BINDINGS[resource.path];
    if (
      required === undefined
      || !safePositiveInteger(resource.byte_length, 128 * 1024 * 1024)
      || typeof resource.executable !== "boolean"
      || typeof resource.hash_binding !== "string"
      || typeof resource.kind !== "string"
      || typeof resource.ownership !== "string"
      || !safePositiveInteger(resource.max_bytes, 128 * 1024 * 1024)
      || resource.byte_length > resource.max_bytes
      || typeof resource.sha256 !== "string"
      || !/^[0-9a-f]{64}$/u.test(resource.sha256)
      || resource.executable !== required.executable
      || resource.hash_binding !== required.hash_binding
      || resource.kind !== required.kind
      || resource.ownership !== required.ownership
    ) {
      fail("RPE-BROWSER-PURITY-0005", `invalid resource ${resource.path}`);
    }
    resourceBytes += resource.max_bytes;
  }
  if (resourceBytes > policy.budgets.max_native_artifact_bytes) {
    fail("RPE-BROWSER-PURITY-0005", "resource byte budget is inconsistent");
  }

  if (
    !exactKeys(policy.service_worker, ["precache", "registration_sites"])
    || JSON.stringify(policy.service_worker.precache)
      !== JSON.stringify(REQUIRED_PRECACHE)
    || !Array.isArray(policy.service_worker.registration_sites)
    || policy.service_worker.registration_sites.length !== 0
  ) {
    fail("RPE-BROWSER-PURITY-0008", "service Worker policy drifted");
  }

  if (
    !Array.isArray(policy.network_manifest)
    || policy.network_manifest.length !== REQUIRED_NETWORK_IDS.length
    || policy.network_manifest.some((entry) =>
      !exactKeys(entry, REQUIRED_NETWORK_KEYS)
    )
    || JSON.stringify(policy.network_manifest.map((entry) => entry.id))
      !== JSON.stringify(REQUIRED_NETWORK_IDS)
  ) {
    fail("RPE-BROWSER-PURITY-0009", "invalid network manifest");
  }
  let maximumRequests = 0;
  for (const resource of policy.network_manifest) {
    const required = REQUIRED_NETWORK_BINDINGS[resource.id];
    if (
      required === undefined
      || typeof resource.kind !== "string"
      || typeof resource.location !== "string"
      || typeof resource.ownership !== "string"
      || !safePositiveInteger(resource.max_bytes, 1024 * 1024 * 1024)
      || !safePositiveInteger(resource.max_requests_per_open, 8_192)
      || /^https?:/iu.test(resource.location)
      || resource.kind !== required.kind
      || resource.location !== required.location
      || resource.ownership !== required.ownership
    ) {
      fail("RPE-BROWSER-PURITY-0009", `invalid network resource ${resource.id}`);
    }
    assertNoForbiddenToken(resource.location, `network resource ${resource.id}`);
    maximumRequests += resource.max_requests_per_open;
  }
  const resourceByPath = new Map(
    policy.resources.map((resource) => [resource.path, resource]),
  );
  const nativeWasmNetwork = policy.network_manifest.find(
    (resource) => resource.id === "native-wasm",
  );
  const nativeLoaderNetwork = policy.network_manifest.find(
    (resource) => resource.id === "native-loader-glue",
  );
  const nativeEntryNetwork = policy.network_manifest.find(
    (resource) => resource.id === "native-worker-entry",
  );
  const nativeHostNetwork = policy.network_manifest.find(
    (resource) => resource.id === "native-worker-host",
  );
  const productModuleNetwork = policy.network_manifest.find(
    (resource) => resource.id === "product-module-graph",
  );
  if (
    maximumRequests !== policy.budgets.max_network_requests_per_open
    || nativeWasmNetwork.max_bytes
      !== resourceByPath.get("dist/native/engine.wasm").byte_length
        * nativeWasmNetwork.max_requests_per_open
    || nativeLoaderNetwork.max_bytes
      !== resourceByPath.get("dist/native/engine-worker.generated.js").byte_length
        * nativeLoaderNetwork.max_requests_per_open
    || nativeEntryNetwork.max_bytes
      !== resourceByPath
        .get("dist/native/engine-worker-entry.generated.js").byte_length
        * nativeEntryNetwork.max_requests_per_open
    || nativeHostNetwork.max_bytes
      !== resourceByPath
        .get("dist/native/engine-worker-host.generated.js").byte_length
        * nativeHostNetwork.max_requests_per_open
    || nativeHostNetwork.max_requests_per_open !== 1
    || nativeEntryNetwork.max_requests_per_open
      !== MAX_WORKER_EPOCHS_PER_OPEN
    || nativeWasmNetwork.max_requests_per_open
      !== MAX_WORKER_EPOCHS_PER_OPEN
    || nativeLoaderNetwork.max_requests_per_open
      !== nativeEntryNetwork.max_requests_per_open
        + nativeHostNetwork.max_requests_per_open
    || productModuleNetwork.max_bytes
      !== MAX_PRODUCT_MODULE_NETWORK_BYTES
    || productModuleNetwork.max_requests_per_open
      !== moduleFiles.length
        + REQUIRED_WORKER_MODULE_FILES.length
          * nativeEntryNetwork.max_requests_per_open
  ) {
    fail("RPE-BROWSER-PURITY-0009", "network request budget is inconsistent");
  }

  if (
    !Array.isArray(policy.build_dependencies)
    || policy.build_dependencies.length > 64
    || !Array.isArray(policy.shipped_third_party_leaves)
    || policy.shipped_third_party_leaves.length
      > policy.budgets.max_shipped_third_party_leaves
  ) {
    fail("RPE-BROWSER-PURITY-0010", "invalid third-party registry");
  }
  const dependencyNames = [];
  for (const dependency of policy.build_dependencies) {
    if (
      !exactKeys(dependency, REQUIRED_BUILD_DEPENDENCY_KEYS)
      || typeof dependency.name !== "string"
      || typeof dependency.dependency_range !== "string"
      || typeof dependency.direct !== "boolean"
      || typeof dependency.version !== "string"
      || typeof dependency.license !== "string"
      || typeof dependency.integrity !== "string"
      || typeof dependency.source !== "string"
      || typeof dependency.ownership !== "string"
      || typeof dependency.replacement !== "string"
      || !Array.isArray(dependency.required_by)
      || dependency.required_by.length !== 1
      || typeof dependency.required_by[0] !== "string"
      || !safePositiveInteger(dependency.max_installed_bytes, 64 * 1024 * 1024)
      || !dependency.source.startsWith("https://registry.npmjs.org/")
      || !dependency.integrity.startsWith("sha512-")
    ) {
      fail("RPE-BROWSER-PURITY-0010", "invalid build dependency registry");
    }
    dependencyNames.push(dependency.name);
  }
  if (
    JSON.stringify(dependencyNames)
      !== JSON.stringify([...dependencyNames].sort())
    || new Set(dependencyNames).size !== dependencyNames.length
  ) {
    fail("RPE-BROWSER-PURITY-0010", "build dependencies must be sorted and unique");
  }

  if (
    !exactKeys(policy.wasm_policy, [
      "allowed_exports",
      "allowed_imports",
      "dynamic_external_engines",
      "max_memory_pages",
      "payloads",
    ])
    || policy.wasm_policy.allowed_exports !== "generated-native-worker-abi"
    || !Array.isArray(policy.wasm_policy.allowed_imports)
    || policy.wasm_policy.allowed_imports.length !== 0
    || policy.wasm_policy.dynamic_external_engines !== false
    || policy.wasm_policy.max_memory_pages !== 1_024
    || JSON.stringify(policy.wasm_policy.payloads)
      !== JSON.stringify(["dist/native/engine.wasm"])
  ) {
    fail("RPE-BROWSER-PURITY-0006", "Wasm policy drifted");
  }

  if (
    !exactKeys(policy.worker_graph, [
      "entrypoints",
      "host_registration",
      "max_workers",
      "module_files",
      "wasm_payloads",
      "worker_constructor_sites",
    ])
    || policy.worker_graph.max_workers !== 1
    || JSON.stringify(policy.worker_graph.entrypoints)
      !== JSON.stringify([
        "dist/native/engine-worker-entry.generated.js",
      ])
    || canonicalJson(policy.worker_graph.host_registration)
      !== canonicalJson(REQUIRED_HOST_REGISTRATION)
    || JSON.stringify(policy.worker_graph.module_files)
      !== JSON.stringify(REQUIRED_WORKER_MODULE_FILES)
    || JSON.stringify(policy.worker_graph.wasm_payloads)
      !== JSON.stringify(["dist/native/engine.wasm"])
    || !Array.isArray(policy.worker_graph.worker_constructor_sites)
    || policy.worker_graph.worker_constructor_sites.length !== 1
    || policy.worker_graph.worker_constructor_sites.some((site) =>
      !exactKeys(site, REQUIRED_WORKER_CONSTRUCTOR_SITE_KEYS)
    )
    || canonicalJson(policy.worker_graph.worker_constructor_sites)
      !== canonicalJson([REQUIRED_WORKER_CONSTRUCTOR_SITE])
  ) {
    fail("RPE-BROWSER-PURITY-0008", "Worker graph drifted");
  }
};

export const validateBrowserProductPolicy = (policy) => {
  validatePolicy(policy);
  return true;
};

const IDENTIFIER_START = /^[$_\p{ID_Start}]$/u;
const IDENTIFIER_PART = /^[$_\u200c\u200d\p{ID_Continue}]$/u;
const REGEX_PREFIX_KEYWORDS = Object.freeze(new Set([
  "await",
  "case",
  "delete",
  "do",
  "else",
  "in",
  "instanceof",
  "new",
  "of",
  "return",
  "throw",
  "typeof",
  "void",
  "yield",
]));
const NON_REGEX_PREFIX_PUNCTUATORS = Object.freeze(new Set([
  ")",
  "]",
  "}",
]));

const lexicalFail = (path, detail) => {
  fail("RPE-BROWSER-PURITY-0004", `${path} has invalid lexical structure: ${detail}`);
};

const lexTypeScript = (path, text) => {
  if (
    typeof text !== "string"
    || new TextEncoder().encode(text).byteLength > MAX_SOURCE_BYTES
  ) {
    lexicalFail(path, "source byte bound exceeded");
  }
  const tokens = [];
  let cursor = 0;

  const push = (kind, value, start, end) => {
    if (
      end <= start
      || end - start > MAX_LEXICAL_TOKEN_BYTES
      || tokens.length >= MAX_LEXICAL_TOKENS
    ) {
      lexicalFail(path, "token bound exceeded");
    }
    tokens.push(Object.freeze({ end, kind, start, value }));
  };

  const unicodeEscape = (start) => {
    if (text[start] !== "\\" || text[start + 1] !== "u") {
      lexicalFail(path, "invalid Unicode escape");
    }
    if (text[start + 2] === "{") {
      const end = text.indexOf("}", start + 3);
      const digits = end === -1 ? "" : text.slice(start + 3, end);
      if (
        end === -1
        || !/^[0-9a-f]{1,6}$/iu.test(digits)
        || Number.parseInt(digits, 16) > 0x10ffff
      ) {
        lexicalFail(path, "invalid Unicode code point escape");
      }
      return {
        end: end + 1,
        value: String.fromCodePoint(Number.parseInt(digits, 16)),
      };
    }
    const digits = text.slice(start + 2, start + 6);
    if (!/^[0-9a-f]{4}$/iu.test(digits)) {
      lexicalFail(path, "invalid Unicode escape");
    }
    return {
      end: start + 6,
      value: String.fromCharCode(Number.parseInt(digits, 16)),
    };
  };

  const escapedStringValue = (start) => {
    const marker = text[start + 1];
    if (marker === undefined) lexicalFail(path, "unterminated escape");
    if (marker === "\r" || marker === "\n") {
      return {
        end: marker === "\r" && text[start + 2] === "\n"
          ? start + 3
          : start + 2,
        value: "",
      };
    }
    if (marker === "u") return unicodeEscape(start);
    if (marker === "x") {
      const digits = text.slice(start + 2, start + 4);
      if (!/^[0-9a-f]{2}$/iu.test(digits)) {
        lexicalFail(path, "invalid hexadecimal escape");
      }
      return {
        end: start + 4,
        value: String.fromCharCode(Number.parseInt(digits, 16)),
      };
    }
    const simple = {
      "0": "\0",
      b: "\b",
      f: "\f",
      n: "\n",
      r: "\r",
      t: "\t",
      v: "\v",
    };
    if (Object.hasOwn(simple, marker)) {
      if (marker === "0" && /[0-9]/u.test(text[start + 2] ?? "")) {
        lexicalFail(path, "legacy octal escape");
      }
      return { end: start + 2, value: simple[marker] };
    }
    return { end: start + 2, value: marker };
  };

  const scanIdentifier = () => {
    const start = cursor;
    let value = "";
    let first = true;
    while (cursor < text.length) {
      let unit;
      if (text[cursor] === "\\") {
        unit = unicodeEscape(cursor);
      } else {
        const codePoint = text.codePointAt(cursor);
        const character = String.fromCodePoint(codePoint);
        unit = { end: cursor + character.length, value: character };
      }
      const pattern = first ? IDENTIFIER_START : IDENTIFIER_PART;
      if (!pattern.test(unit.value)) {
        if (text[cursor] === "\\") {
          lexicalFail(path, "escaped non-identifier code point");
        }
        break;
      }
      value += unit.value;
      cursor = unit.end;
      first = false;
    }
    if (first) lexicalFail(path, "invalid identifier");
    push("identifier", value, start, cursor);
  };

  const scanString = (quote) => {
    const start = cursor;
    cursor += 1;
    let value = "";
    while (cursor < text.length) {
      if (text[cursor] === quote) {
        cursor += 1;
        push("string", value, start, cursor);
        return;
      }
      if (text[cursor] === "\\") {
        const escaped = escapedStringValue(cursor);
        value += escaped.value;
        cursor = escaped.end;
      } else {
        if (text[cursor] === "\r" || text[cursor] === "\n") {
          lexicalFail(path, "unterminated string");
        }
        const codePoint = text.codePointAt(cursor);
        const character = String.fromCodePoint(codePoint);
        value += character;
        cursor += character.length;
      }
      if (cursor - start > MAX_LEXICAL_TOKEN_BYTES) {
        lexicalFail(path, "string token bound exceeded");
      }
    }
    lexicalFail(path, "unterminated string");
  };

  const canStartRegex = () => {
    const previous = tokens.at(-1);
    if (previous === undefined) return true;
    if (
      (
        (previous.value === "+" || previous.value === "-")
        && tokens.at(-2)?.value === previous.value
      )
      || (
        previous.value === "!"
        && expressionEndToken(tokens.at(-2))
        && !(
          tokens.at(-2)?.kind === "identifier"
          && REGEX_PREFIX_KEYWORDS.has(tokens.at(-2).value)
        )
      )
    ) {
      return false;
    }
    if (previous.kind === "identifier") {
      return REGEX_PREFIX_KEYWORDS.has(previous.value);
    }
    if (
      previous.kind === "number"
      || previous.kind === "regex"
      || previous.kind === "string"
      || previous.kind === "template-close"
    ) {
      return false;
    }
    return !NON_REGEX_PREFIX_PUNCTUATORS.has(previous.value);
  };

  const scanRegex = () => {
    const start = cursor;
    cursor += 1;
    let characterClass = false;
    while (cursor < text.length) {
      const character = text[cursor];
      if (character === "\\") {
        cursor += 2;
        continue;
      }
      if (character === "\r" || character === "\n") {
        lexicalFail(path, "unterminated regular expression");
      }
      if (character === "[") {
        characterClass = true;
      } else if (character === "]") {
        characterClass = false;
      } else if (character === "/" && !characterClass) {
        cursor += 1;
        while (/[A-Za-z]/u.test(text[cursor] ?? "")) cursor += 1;
        push("regex", "", start, cursor);
        return;
      }
      cursor += 1;
      if (cursor - start > MAX_LEXICAL_TOKEN_BYTES) {
        lexicalFail(path, "regular expression token bound exceeded");
      }
    }
    lexicalFail(path, "unterminated regular expression");
  };

  const scanTemplate = (nesting, scanCode) => {
    const start = cursor;
    cursor += 1;
    push("template-open", "`", start, cursor);
    let quasiStart = cursor;
    let staticValue = "";
    const pushQuasi = () => {
      if (cursor > quasiStart) {
        push("template-quasi", staticValue, quasiStart, cursor);
      }
      staticValue = "";
    };
    while (cursor < text.length) {
      if (text[cursor] === "`") {
        pushQuasi();
        const close = cursor;
        cursor += 1;
        push("template-close", "`", close, cursor);
        return;
      }
      if (text[cursor] === "\\") {
        const escaped = escapedStringValue(cursor);
        staticValue += escaped.value;
        cursor = escaped.end;
      } else if (text[cursor] === "$" && text[cursor + 1] === "{") {
        pushQuasi();
        const expressionStart = cursor;
        cursor += 2;
        push(
          "template-expression-open",
          "${",
          expressionStart,
          cursor,
        );
        scanCode(true, nesting + 1);
        quasiStart = cursor;
      } else {
        const codePoint = text.codePointAt(cursor);
        const character = String.fromCodePoint(codePoint);
        staticValue += character;
        cursor += character.length;
      }
      if (cursor - start > MAX_LEXICAL_TOKEN_BYTES) {
        lexicalFail(path, "template token bound exceeded");
      }
    }
    lexicalFail(path, "unterminated template");
  };

  const scanCode = (stopAtTemplateBrace = false, nesting = 0) => {
    if (nesting > MAX_LEXICAL_NESTING) {
      lexicalFail(path, "nesting bound exceeded");
    }
    let braceDepth = 0;
    while (cursor < text.length) {
      const character = text[cursor];
      if (/\s/u.test(character)) {
        cursor += 1;
        continue;
      }
      if (character === "/" && text[cursor + 1] === "/") {
        const start = cursor;
        cursor += 2;
        while (
          cursor < text.length
          && text[cursor] !== "\r"
          && text[cursor] !== "\n"
        ) {
          cursor += 1;
        }
        if (cursor - start > MAX_LEXICAL_TOKEN_BYTES) {
          lexicalFail(path, "line comment bound exceeded");
        }
        continue;
      }
      if (character === "/" && text[cursor + 1] === "*") {
        const start = cursor;
        const end = text.indexOf("*/", cursor + 2);
        if (end === -1 || end + 2 - start > MAX_LEXICAL_TOKEN_BYTES) {
          lexicalFail(path, "unterminated or oversized block comment");
        }
        cursor = end + 2;
        continue;
      }
      if (character === "\"" || character === "'") {
        scanString(character);
        continue;
      }
      if (character === "`") {
        scanTemplate(nesting, scanCode);
        continue;
      }
      if (character === "/" && canStartRegex()) {
        scanRegex();
        continue;
      }
      if (
        character === "\\"
        || IDENTIFIER_START.test(
          String.fromCodePoint(text.codePointAt(cursor)),
        )
      ) {
        scanIdentifier();
        continue;
      }
      if (/[0-9]/u.test(character)) {
        const start = cursor;
        cursor += 1;
        while (/[0-9A-F_a-f.nobx]/u.test(text[cursor] ?? "")) cursor += 1;
        push("number", text.slice(start, cursor), start, cursor);
        continue;
      }
      if (character === "{") {
        const start = cursor;
        cursor += 1;
        braceDepth += 1;
        push("punctuator", character, start, cursor);
        continue;
      }
      if (character === "}") {
        if (stopAtTemplateBrace && braceDepth === 0) {
          const start = cursor;
          cursor += 1;
          push("template-expression-close", "}", start, cursor);
          return;
        }
        const start = cursor;
        cursor += 1;
        braceDepth = Math.max(0, braceDepth - 1);
        push("punctuator", character, start, cursor);
        continue;
      }
      const start = cursor;
      cursor += 1;
      push("punctuator", character, start, cursor);
    }
    if (stopAtTemplateBrace) lexicalFail(path, "unterminated template expression");
  };

  scanCode();
  const masked = [];
  let maskedCursor = 0;
  for (const token of tokens) {
    if (token.start < maskedCursor) lexicalFail(path, "overlapping lexical tokens");
    masked.push(" ".repeat(token.start - maskedCursor));
    masked.push(
      ["identifier", "number", "punctuator"].includes(token.kind)
        ? text.slice(token.start, token.end)
        : " ".repeat(token.end - token.start),
    );
    maskedCursor = token.end;
  }
  masked.push(" ".repeat(text.length - maskedCursor));
  return Object.freeze({
    code: masked.join(""),
    tokens: Object.freeze(tokens),
  });
};

const parseStaticImports = (path, tokens) => {
  const imports = new Set();
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];
    const previous = tokens[index - 1];
    if (
      token.kind !== "identifier"
      || token.value !== "import"
      || previous?.value === "."
    ) {
      continue;
    }
    const next = tokens[index + 1];
    if (next?.value === ".") continue;
    if (next?.value === "(") {
      fail("RPE-BROWSER-PURITY-0004", `${path} contains dynamic import`);
    }
    if (next?.kind === "string") {
      imports.add(next.value);
      continue;
    }
    let found = false;
    for (let cursor = index + 1; cursor < tokens.length; cursor += 1) {
      const candidate = tokens[cursor];
      if (candidate.value === ";") break;
      if (
        candidate.kind === "identifier"
        && candidate.value === "from"
        && tokens[cursor + 1]?.kind === "string"
      ) {
        imports.add(tokens[cursor + 1].value);
        found = true;
        break;
      }
    }
    if (!found) {
      fail("RPE-BROWSER-PURITY-0004", `${path} has unsupported import syntax`);
    }
  }
  for (let index = 0; index < tokens.length; index += 1) {
    if (
      tokens[index].kind !== "identifier"
      || tokens[index].value !== "export"
    ) {
      continue;
    }
    let cursor = index + 1;
    if (
      tokens[cursor]?.kind === "identifier"
      && tokens[cursor].value === "type"
    ) {
      cursor += 1;
    }
    if (!["*", "{"].includes(tokens[cursor]?.value)) continue;
    for (; cursor < tokens.length && tokens[cursor].value !== ";"; cursor += 1) {
      if (
        tokens[cursor].kind === "identifier"
        && tokens[cursor].value === "from"
        && tokens[cursor + 1]?.kind === "string"
      ) {
        imports.add(tokens[cursor + 1].value);
        break;
      }
    }
  }
  return [...imports].sort();
};

const expressionEndToken = (token) =>
  token !== undefined
  && (
    [
      "identifier",
      "number",
      "regex",
      "string",
      "template-close",
    ].includes(token.kind)
    || [")", "]", "}"].includes(token.value)
  );

const computedReceiverEndsAt = (tokens, index) => {
  let cursor = index;
  while (tokens[cursor]?.value === "!") cursor -= 1;
  return expressionEndToken(tokens[cursor]);
};

const isComputedMemberOpen = (tokens, index) =>
  tokens[index]?.value === "["
  && (
    computedReceiverEndsAt(tokens, index - 1)
    || (
      tokens[index - 1]?.value === "."
      && tokens[index - 2]?.value === "?"
      && computedReceiverEndsAt(tokens, index - 3)
    )
  );

const capabilityInventory = (tokens) => {
  let computedMemberAccessCount = 0;
  let invalidReflectAccess = false;
  const reflectMemberCounts = new Map();
  for (let index = 0; index < tokens.length; index += 1) {
    if (isComputedMemberOpen(tokens, index)) {
      computedMemberAccessCount += 1;
    }
    if (
      tokens[index].kind === "identifier"
      && tokens[index].value === "Reflect"
    ) {
      const member = tokens[index + 2];
      if (
        tokens[index + 1]?.value !== "."
        || member?.kind !== "identifier"
        || (member.value === "get" && tokens[index + 3]?.value !== "(")
      ) {
        invalidReflectAccess = true;
        continue;
      }
      reflectMemberCounts.set(
        member.value,
        (reflectMemberCounts.get(member.value) ?? 0) + 1,
      );
    }
  }
  return Object.freeze({
    computedMemberAccessCount,
    invalidReflectAccess,
    reflectMemberCounts: Object.freeze(
      Object.fromEntries(
        [...reflectMemberCounts].sort(([left], [right]) =>
          left.localeCompare(right)
        ),
      ),
    ),
  });
};

export const inspectProductModuleCapabilityInventory = (path, text) =>
  capabilityInventory(lexTypeScript(path, text).tokens);

const appendStaticString = (path, left, right) => {
  const combined = `${left}${right}`;
  if (combined.length > MAX_STATIC_STRING_LENGTH) {
    lexicalFail(path, "static string bound exceeded");
  }
  return combined;
};

const findMatchingToken = (
  path,
  tokens,
  start,
  openKind,
  closeKind,
  limit,
) => {
  let depth = 1;
  for (let cursor = start + 1; cursor < limit; cursor += 1) {
    if (tokens[cursor].kind === openKind) depth += 1;
    if (tokens[cursor].kind === closeKind) {
      depth -= 1;
      if (depth === 0) return cursor;
    }
  }
  lexicalFail(path, `unterminated ${openKind}`);
};

const evaluateStaticStringExpression = (
  path,
  tokens,
  start,
  limit,
  bindings,
  state,
  depth = 0,
) => {
  if (depth > MAX_LEXICAL_NESTING) {
    lexicalFail(path, "static expression nesting exceeded");
  }
  const parseTerm = (index) => {
    state.steps += 1;
    if (state.steps > MAX_STATIC_EVALUATION_STEPS || index >= limit) {
      if (state.steps > MAX_STATIC_EVALUATION_STEPS) {
        lexicalFail(path, "static evaluation step bound exceeded");
      }
      return undefined;
    }
    const token = tokens[index];
    if (token.kind === "string") {
      return { next: index + 1, value: token.value };
    }
    if (token.kind === "number") {
      return { next: index + 1, value: token.value.replace(/_/gu, "") };
    }
    if (token.kind === "identifier" && bindings.has(token.value)) {
      return { next: index + 1, value: bindings.get(token.value) };
    }
    if (token.value === "(") {
      let nesting = 1;
      let close = index + 1;
      for (; close < limit; close += 1) {
        if (tokens[close].value === "(") nesting += 1;
        if (tokens[close].value === ")") {
          nesting -= 1;
          if (nesting === 0) break;
        }
      }
      if (nesting !== 0) return undefined;
      const inner = evaluateStaticStringExpression(
        path,
        tokens,
        index + 1,
        close,
        bindings,
        state,
        depth + 1,
      );
      return inner?.next === close
        ? { next: close + 1, value: inner.value }
        : undefined;
    }
    if (token.kind !== "template-open") return undefined;
    let cursor = index + 1;
    let value = "";
    while (cursor < limit) {
      const current = tokens[cursor];
      state.steps += 1;
      if (state.steps > MAX_STATIC_EVALUATION_STEPS) {
        lexicalFail(path, "static evaluation step bound exceeded");
      }
      if (current.kind === "template-close") {
        return { next: cursor + 1, value };
      }
      if (current.kind === "template-quasi") {
        value = appendStaticString(path, value, current.value);
        cursor += 1;
        continue;
      }
      if (current.kind !== "template-expression-open") return undefined;
      const close = findMatchingToken(
        path,
        tokens,
        cursor,
        "template-expression-open",
        "template-expression-close",
        limit,
      );
      const expression = evaluateStaticStringExpression(
        path,
        tokens,
        cursor + 1,
        close,
        bindings,
        state,
        depth + 1,
      );
      if (expression?.next !== close) return undefined;
      value = appendStaticString(path, value, expression.value);
      cursor = close + 1;
    }
    return undefined;
  };

  let parsed = parseTerm(start);
  if (parsed === undefined) return undefined;
  while (parsed.next < limit && tokens[parsed.next].value === "+") {
    const right = parseTerm(parsed.next + 1);
    if (right === undefined) return undefined;
    parsed = {
      next: right.next,
      value: appendStaticString(path, parsed.value, right.value),
    };
  }
  return parsed;
};

const findDeclarationDelimiter = (tokens, start) => {
  const stack = [];
  const pairs = new Map([
    ["(", ")"],
    ["[", "]"],
    ["{", "}"],
    ["template-open", "template-close"],
  ]);
  for (let cursor = start; cursor < tokens.length; cursor += 1) {
    const token = tokens[cursor];
    const opening = token.kind === "template-open"
      ? "template-open"
      : token.value;
    const closing = token.kind === "template-close"
      ? "template-close"
      : token.value;
    if (pairs.has(opening)) {
      stack.push(pairs.get(opening));
      continue;
    }
    if (stack.at(-1) === closing) {
      stack.pop();
      continue;
    }
    if (
      stack.length === 0
      && (token.value === "," || token.value === ";")
    ) {
      return cursor;
    }
  }
  return tokens.length;
};

const collectStaticStringBindings = (path, text, tokens) => {
  const bindings = new Map();
  const state = { steps: 0 };
  for (let index = 0; index < tokens.length; index += 1) {
    if (
      tokens[index].kind !== "identifier"
      || tokens[index].value !== "const"
    ) {
      continue;
    }
    let cursor = index + 1;
    while (cursor < tokens.length) {
      const name = tokens[cursor];
      if (name?.kind !== "identifier") break;
      let assignment = cursor + 1;
      while (
        assignment < tokens.length
        && tokens[assignment].value !== "="
        && tokens[assignment].value !== ","
        && tokens[assignment].value !== ";"
      ) {
        assignment += 1;
      }
      if (tokens[assignment]?.value !== "=") break;
      const delimiter = findDeclarationDelimiter(tokens, assignment + 1);
      const evaluated = evaluateStaticStringExpression(
        path,
        tokens,
        assignment + 1,
        delimiter,
        bindings,
        state,
      );
      const next = evaluated?.next;
      const nextToken = next === undefined ? undefined : tokens[next];
      const automaticSemicolon = (
        evaluated !== undefined
        && nextToken !== undefined
        && /[\r\n\u2028\u2029]/u.test(
          text.slice(tokens[next - 1].end, nextToken.start),
        )
        && ![".", "(", "[", "?", ":"].includes(nextToken.value)
      );
      if (
        evaluated !== undefined
        && (
          evaluated.next === delimiter
          || automaticSemicolon
        )
      ) {
        if (
          !bindings.has(name.value)
          && bindings.size >= MAX_STATIC_STRING_BINDINGS
        ) {
          lexicalFail(path, "static binding bound exceeded");
        }
        bindings.set(name.value, evaluated.value);
      } else {
        bindings.delete(name.value);
      }
      if (tokens[delimiter]?.value !== ",") break;
      cursor = delimiter + 1;
    }
  }
  return { bindings, state };
};

const rejectSensitiveStaticValue = (path, value, constructed) => {
  const normalized = value.toLowerCase();
  const forbiddenEngine = FORBIDDEN_ENGINE_TOKENS.some((entry) =>
    normalized.includes(entry)
  );
  if (forbiddenEngine) {
    assertNoForbiddenToken(value, `${path} constructed string`);
  }
  if (
    ["constructor", "eval"].includes(normalized)
    || /^https?:\/\//u.test(normalized)
    || forbiddenEngine
    || (constructed && DANGEROUS_CONSTANT_VALUES.has(normalized))
  ) {
    fail(
      ["constructor", "eval"].includes(normalized)
        ? "RPE-BROWSER-PURITY-0004"
        : "RPE-BROWSER-PURITY-0009",
      `${path} constructs a sensitive executable or network capability`,
    );
  }
};

const rejectDangerousConstants = (path, text, tokens) => {
  const { bindings, state } = collectStaticStringBindings(path, text, tokens);
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];
    if (token.kind !== "string") continue;
    assertNoForbiddenToken(token.value, `${path} string`);
    rejectSensitiveStaticValue(path, token.value, false);
    if (
      tokens[index - 1]?.value === "["
      && tokens[index + 1]?.value === "]"
      && expressionEndToken(tokens[index - 2])
    ) {
      rejectSensitiveStaticValue(path, token.value, true);
    }
  }
  for (const value of bindings.values()) {
    rejectSensitiveStaticValue(path, value, true);
  }
  for (let index = 0; index < tokens.length; index += 1) {
    if (
      tokens[index - 1]?.value === "+"
      || !(
        tokens[index].kind === "string"
        || tokens[index].kind === "number"
        || tokens[index].kind === "identifier"
        || tokens[index].kind === "template-open"
        || tokens[index].value === "("
      )
    ) {
      continue;
    }
    const evaluated = evaluateStaticStringExpression(
      path,
      tokens,
      index,
      tokens.length,
      bindings,
      state,
    );
    if (evaluated === undefined) continue;
    const constructed = tokens[index].kind === "template-open"
      || tokens.slice(index, evaluated.next).some((entry) => entry.value === "+");
    if (constructed) {
      rejectSensitiveStaticValue(path, evaluated.value, true);
    }
  }
};

const inspectSensitiveCapabilities = (path, text, tokens) => {
  rejectDangerousConstants(path, text, tokens);
  const inventory = capabilityInventory(tokens);
  if (
    inventory.invalidReflectAccess
    || inventory.computedMemberAccessCount
      !== (EXPECTED_COMPUTED_MEMBER_ACCESS_COUNTS[path] ?? 0)
    || canonicalJson(inventory.reflectMemberCounts)
      !== canonicalJson(EXPECTED_REFLECT_MEMBER_COUNTS[path] ?? {})
  ) {
    fail(
      "RPE-BROWSER-PURITY-0009",
      `${path} dynamic capability inventory drifted`,
    );
  }
  for (let index = 0; index < tokens.length; index += 1) {
    if (tokens[index].value !== "]") continue;
    let cursor = index + 1;
    while (tokens[cursor]?.value === ")" || tokens[cursor]?.value === "!") {
      cursor += 1;
    }
    if (
      tokens[cursor]?.value === "("
      || (
        tokens[cursor]?.value === "?"
        && tokens[cursor + 1]?.value === "."
        && tokens[cursor + 2]?.value === "("
      )
    ) {
      fail(
        "RPE-BROWSER-PURITY-0009",
        `${path} invokes a computed capability`,
      );
    }
  }
  let fetchIdentifiers = 0;
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];
    if (token.kind !== "identifier") continue;
    const identifier = token.value;
    const before = tokens[index - 1];
    const after = tokens[index + 1];
    const bare = before?.value !== "." && before?.value !== "#";
    if (identifier === "fetch") fetchIdentifiers += 1;
    if (
      FORBIDDEN_AMBIENT_IDENTIFIERS.has(identifier)
      && bare
      && !(
        identifier === "document"
        && path === "src/browser-reader-client.ts"
        && before?.kind === "identifier"
        && before.value === "get"
      )
    ) {
      fail(
        "RPE-BROWSER-PURITY-0009",
        `${path} references ambient capability ${identifier}`,
      );
    }
    if (
      identifier === "WebAssembly"
      && (
        path !== "src/browser-native-worker-loader.ts"
        || after?.value !== "."
      )
    ) {
      fail(
        "RPE-BROWSER-PURITY-0006",
        `${path} references an unregistered WebAssembly capability`,
      );
    }
    if (identifier === "constructor" && before?.value === ".") {
      fail("RPE-BROWSER-PURITY-0004", `${path} accesses a dynamic constructor`);
    }
    if (
      identifier === "Worker"
      && !(
        (
          path === "generated/engine-protocol.ts"
          && (before?.value === "." || after?.value === "=")
        )
        || (
          path === REQUIRED_WORKER_CONSTRUCTOR_SITE.path
          && before?.value === "new"
          && after?.value === "("
        )
      )
    ) {
      fail("RPE-BROWSER-PURITY-0008", `${path} references an unregistered Worker`);
    }
  }
  const expectedFetchIdentifiers = EXPECTED_FETCH_IDENTIFIER_COUNTS[path] ?? 0;
  if (fetchIdentifiers !== expectedFetchIdentifiers) {
    fail("RPE-BROWSER-PURITY-0009", `${path} network capability references drifted`);
  }
};

const maskTypeScriptNonCode = (path, text) =>
  lexTypeScript(path, text).code;

const inspectRegisteredWorkerConstructorSites = (path, text) => {
  const lexical = lexTypeScript(path, text);
  const { code, tokens } = lexical;
  const constructors = [
    ...code.matchAll(/\bnew\s+(?:Shared)?Worker\s*\(/gu),
  ];
  if (constructors.length === 0) {
    return [];
  }
  const workerTokenIndex = tokens.findIndex(
    (token, index) =>
      token.value === "Worker"
      && tokens[index - 1]?.value === "new"
      && tokens[index + 1]?.value === "(",
  );
  let workerCallEnd = workerTokenIndex + 2;
  let workerCallDepth = 1;
  while (
    workerTokenIndex >= 0
    && workerCallEnd < tokens.length
    && workerCallDepth > 0
  ) {
    if (tokens[workerCallEnd].value === "(") workerCallDepth += 1;
    if (tokens[workerCallEnd].value === ")") workerCallDepth -= 1;
    workerCallEnd += 1;
  }
  const workerCall = workerTokenIndex < 0 || workerCallDepth !== 0
    ? []
    : tokens
      .slice(workerTokenIndex - 1, workerCallEnd)
      .map((token) => token.value);
  const exactWorkerCall = [
    "new",
    "Worker",
    "(",
    "entryUrl",
    ",",
    "Object",
    ".",
    "freeze",
    "(",
    "{",
    "credentials",
    ":",
    "same-origin",
    ",",
    "name",
    ":",
    "workerName",
    ",",
    "type",
    ":",
    "module",
    ",",
    "}",
    ")",
    ",",
    ")",
  ];
  const exactBrand =
    /\bENTRY_REFERENCES\s*=\s*new\s+WeakSet\s*<\s*object\s*>\s*\(\s*\)/u;
  const exactBrandAdmission =
    /\bENTRY_REFERENCES\.has\s*\(\s*value\s*\)/u;
  const exactBrandCreation =
    /\bENTRY_REFERENCES\.add\s*\(\s*reference\s*\)/u;
  const exactCanonicalStore =
    /\bENTRY_REFERENCE_URLS\s*=\s*new\s+WeakMap\s*<\s*object\s*,\s*string\s*>\s*\(\s*\)/u;
  const exactCanonicalSnapshot =
    /\bcanonical\s*=\s*REFLECT_APPLY\s*\(\s*URL_TO_STRING\s*,\s*snapshot\s*,\s*\[\s*\]\s*\)/u;
  const exactCanonicalSet =
    /\bENTRY_REFERENCE_URLS\.set\s*\(\s*reference\s*,\s*canonical\s*\)/u;
  const exactCanonicalGet =
    /\bcanonical\s*=\s*ENTRY_REFERENCE_URLS\.get\s*\(\s*value\s*\)/u;
  const exactCanonicalConstruction =
    /\bsnapshot\s*=\s*new\s+URL_CONSTRUCTOR\s*\(\s*canonical\s*\)/u;
  const exactRegistrationInput =
    /\bcreateUnverifiedBrowserNativeWorkerEntryReference\s*\(\s*candidate\s*:\s*BrowserNativeWorkerEntryArtifactCandidate\s*,?\s*\)/u;
  const exactRegistrationUrl =
    /\burlDescriptor\s*=\s*Object\.getOwnPropertyDescriptor\s*\(\s*candidate\s*,\s*["']url["']\s*\)/u;
  const exactRegistrationHash =
    /\bsha256Descriptor\s*=\s*Object\.getOwnPropertyDescriptor\s*\(\s*candidate\s*,\s*["']sha256["']\s*\)/u;
  const exactRegistrationLength =
    /\bbyteLengthDescriptor\s*=\s*Object\.getOwnPropertyDescriptor\s*\(\s*candidate\s*,\s*["']byteLength["']\s*\)/u;
  const exactRegistrationDescriptors =
    /\burlDescriptor\.configurable\s*!==\s*false[\s\S]*\burlDescriptor\.enumerable\s*!==\s*true[\s\S]*\burlDescriptor\.writable\s*!==\s*false/u;
  const exactUrlIntrinsic =
    /\bserialized\s*=\s*REFLECT_APPLY\s*\(\s*URL_TO_STRING\s*,\s*urlDescriptor\.value\s*,\s*\[\s*\]\s*,?\s*\)/u;
  const exactUrlIntrinsicsCapture =
    /\bURL_CONSTRUCTOR\s*=\s*URL\s*;[\s\S]*\bURL_TO_STRING\s*=\s*URL\.prototype\.toString\s*;/u;
  const exactRegisteredEntryFile =
    /\bREGISTERED_ENTRY_FILE\s*=\s*["']engine-worker-entry\.generated\.js["']/u;
  const mutableEntryUrlRead =
    /(?:\bconfiguration\.entry\.url\b|\bdescriptor\.value\s*\.\s*href\b|\bvalue\.url\b|\burlDescriptor\.value\s+instanceof\s+URL\b|\bURL\.prototype\.toString\.call\s*\(\s*descriptor\.value\s*\))/u;
  const exactFactoryBinding =
    /\bentryUrl\s*=\s*snapshotEntryReference\s*\(\s*configuration\.entry\s*\)/u;
  const exactConstructBinding =
    /\bdedicated\s*=\s*construct\s*\(\s*new\s+URL_CONSTRUCTOR\s*\(\s*entryUrl\s*\)\s*,\s*workerName\s*,?\s*\)/u;
  if (
    path !== REQUIRED_WORKER_CONSTRUCTOR_SITE.path
    || constructors.length !== 1
    || canonicalJson(workerCall) !== canonicalJson(exactWorkerCall)
    || !exactBrand.test(code)
    || !exactBrandAdmission.test(code)
    || !exactBrandCreation.test(code)
    || !exactCanonicalStore.test(code)
    || !exactCanonicalSnapshot.test(code)
    || !exactCanonicalSet.test(code)
    || !exactCanonicalGet.test(code)
    || !exactCanonicalConstruction.test(code)
    || !exactRegistrationInput.test(text)
    || !exactRegistrationUrl.test(text)
    || !exactRegistrationHash.test(text)
    || !exactRegistrationLength.test(text)
    || !exactRegistrationDescriptors.test(code)
    || !exactUrlIntrinsic.test(code)
    || !exactUrlIntrinsicsCapture.test(code)
    || !exactRegisteredEntryFile.test(text)
    || mutableEntryUrlRead.test(code)
    || !exactFactoryBinding.test(code)
    || !exactConstructBinding.test(code)
  ) {
    fail("RPE-BROWSER-PURITY-0008", `${path} contains an unregistered Worker`);
  }
  return [REQUIRED_WORKER_CONSTRUCTOR_SITE];
};

export const inspectProductModuleText = (path, text) => {
  if (/\bimport\s*\(/u.test(text)) {
    fail("RPE-BROWSER-PURITY-0004", `${path} contains dynamic import`);
  }
  if (
    /\brequire\s*\(/u.test(text)
    || /\bimportScripts\s*\(/u.test(text)
    || /\beval\s*\(/u.test(text)
    || /\bnew\s+Function\s*\(/u.test(text)
  ) {
    fail("RPE-BROWSER-PURITY-0004", `${path} contains dynamic executable code`);
  }
  inspectRegisteredWorkerConstructorSites(path, text);
  if (/\bnavigator\s*\.\s*serviceWorker\b/u.test(text)) {
    fail("RPE-BROWSER-PURITY-0008", `${path} registers a service Worker`);
  }
  if (/sourceMappingURL/iu.test(text)) {
    fail("RPE-BROWSER-PURITY-0005", `${path} embeds a source map`);
  }
  const lexical = lexTypeScript(path, text);
  inspectSensitiveCapabilities(path, text, lexical.tokens);
  for (const match of text.matchAll(/\bhttps?:\/\/[^\s"'`)<>{}]+/giu)) {
    assertNoForbiddenToken(match[0], `${path} URL`);
    fail("RPE-BROWSER-PURITY-0009", `${path} embeds an external URL`);
  }
  const imports = parseStaticImports(path, lexical.tokens);
  for (const specifier of imports) {
    assertNoForbiddenToken(specifier, `${path} import`);
    if (!specifier.startsWith(".")) {
      fail("RPE-BROWSER-PURITY-0004", `${path} imports external module ${specifier}`);
    }
  }
  return imports;
};

const resolveModuleSpecifier = (source, specifier) => {
  if (!specifier.endsWith(".js")) {
    fail("RPE-BROWSER-PURITY-0004", `${source} import must use a .js projection`);
  }
  const projected = resolve(dirname(source), `${specifier.slice(0, -3)}.ts`);
  return projected;
};

const checkNetworkCallSites = (modules) => {
  const observed = [];
  for (const module of modules) {
    const code = maskTypeScriptNonCode(module.path, module.text);
    for (const match of code.matchAll(/(?:[#.\w]+\.)?fetch\s*\(/gu)) {
      observed.push({
        path: module.path,
        call: match[0].replace(/\s+/gu, ""),
      });
    }
  }
  const expected = [
    { path: "src/browser-native-worker-loader.ts", call: "fetch(" },
    { path: "src/browser-native-worker-loader.ts", call: "this.#runtime.fetch(" },
    { path: "src/browser-reader-client.ts", call: "source.fetcher.fetch(" },
    { path: "src/browser-source-bridge.ts", call: "fetch(" },
    { path: "src/browser-source-bridge.ts", call: "this.#source.fetcher.fetch(" },
  ];
  observed.sort((left, right) =>
    left.path.localeCompare(right.path) || left.call.localeCompare(right.call)
  );
  expected.sort((left, right) =>
    left.path.localeCompare(right.path) || left.call.localeCompare(right.call)
  );
  if (canonicalJson(observed) !== canonicalJson(expected)) {
    fail("RPE-BROWSER-PURITY-0009", "network call sites drifted");
  }
};

const inspectModuleGraph = async (browserRoot, policy) => {
  const discovered = [];
  for (const directory of ["generated", "src"]) {
    const entries = await readdir(resolve(browserRoot, directory), {
      withFileTypes: true,
    });
    if (
      entries.length > policy.budgets.max_module_files
      || entries.some((entry) =>
        !entry.isFile()
        || entry.isSymbolicLink()
        || !entry.name.endsWith(".ts")
      )
    ) {
      fail("RPE-BROWSER-PURITY-0004", `invalid ${directory} module directory`);
    }
    discovered.push(
      ...entries.map((entry) => `${directory}/${entry.name}`),
    );
  }
  discovered.sort();
  if (
    JSON.stringify(discovered)
    !== JSON.stringify(policy.module_graph.files)
  ) {
    fail("RPE-BROWSER-PURITY-0004", "module file registry is not exact");
  }
  const registered = new Set(
    policy.module_graph.files.map((path) => resolve(browserRoot, path)),
  );
  const modules = [];
  const edges = [];
  const workerConstructorSites = [];
  for (const path of policy.module_graph.files) {
    const absolute = resolve(browserRoot, path);
    if (relative(browserRoot, absolute).startsWith(`..${sep}`)) {
      fail("RPE-BROWSER-PURITY-0004", `module escapes browser root: ${path}`);
    }
    const text = await readBoundedText(absolute, MAX_SOURCE_BYTES, `module ${path}`);
    const imports = inspectProductModuleText(path, text);
    workerConstructorSites.push(
      ...inspectRegisteredWorkerConstructorSites(path, text),
    );
    modules.push({ imports, path, sha256: digestText(text), text });
    for (const specifier of imports) {
      const target = resolveModuleSpecifier(absolute, specifier);
      if (!registered.has(target)) {
        fail("RPE-BROWSER-PURITY-0004", `${path} imports unregistered module`);
      }
      edges.push({
        from: path,
        to: relative(browserRoot, target).split(sep).join("/"),
      });
    }
  }
  if (edges.length > policy.budgets.max_module_edges) {
    fail("RPE-BROWSER-PURITY-0004", "module edge budget exceeded");
  }
  if (
    canonicalJson(workerConstructorSites)
      !== canonicalJson(policy.worker_graph.worker_constructor_sites)
  ) {
    fail("RPE-BROWSER-PURITY-0008", "Worker constructor registry drifted");
  }
  const importsByModule = new Map();
  for (const path of policy.module_graph.files) {
    importsByModule.set(path, []);
  }
  for (const edge of edges) {
    importsByModule.get(edge.from).push(edge.to);
  }
  const workerModuleClosure = new Set();
  const pendingWorkerModules = ["src/browser-native-worker-entry.ts"];
  while (pendingWorkerModules.length > 0) {
    const path = pendingWorkerModules.pop();
    if (workerModuleClosure.has(path)) continue;
    workerModuleClosure.add(path);
    for (const imported of importsByModule.get(path) ?? []) {
      pendingWorkerModules.push(imported);
    }
  }
  const workerModuleFiles = [...workerModuleClosure].sort();
  if (
    JSON.stringify(workerModuleFiles)
      !== JSON.stringify(policy.worker_graph.module_files)
  ) {
    fail("RPE-BROWSER-PURITY-0008", "Worker module closure drifted");
  }
  edges.sort((left, right) =>
    left.from.localeCompare(right.from) || left.to.localeCompare(right.to)
  );
  checkNetworkCallSites(modules);
  const graph = {
    edges,
    entrypoints: policy.module_graph.entrypoints,
    modules: modules.map(({ imports, path, sha256: moduleSha256 }) => ({
      imports,
      path,
      sha256: moduleSha256,
    })),
  };
  const graphSha256 = digestText(canonicalJson(graph));
  if (graphSha256 !== policy.module_graph.sha256) {
    fail("RPE-BROWSER-PURITY-0004", "module graph hash drifted");
  }
  return {
    edgeCount: edges.length,
    fileCount: modules.length,
    sha256: graphSha256,
    workerModuleFileCount: workerModuleFiles.length,
  };
};

const inspectCargoGraph = async (repositoryRoot, policy) => {
  const result = spawnSync(
    "cargo",
    [
      "metadata",
      "--locked",
      "--offline",
      "--format-version",
      "1",
      "--no-deps",
      "--filter-platform",
      "wasm32-unknown-unknown",
    ],
    {
      cwd: repositoryRoot,
      encoding: "utf8",
      env: {
        ...process.env,
        CARGO_NET_OFFLINE: "true",
        CARGO_TERM_COLOR: "never",
      },
      maxBuffer: MAX_CARGO_METADATA_BYTES,
    },
  );
  if (result.status !== 0) {
    fail(
      "RPE-BROWSER-PURITY-0010",
      `Cargo dependency metadata failed: ${result.stderr.trim()}`,
    );
  }
  let metadata;
  try {
    metadata = JSON.parse(result.stdout);
  } catch {
    fail("RPE-BROWSER-PURITY-0010", "Cargo dependency metadata is not JSON");
  }
  if (!Array.isArray(metadata.packages) || metadata.packages.length > 256) {
    fail("RPE-BROWSER-PURITY-0010", "Cargo package inventory is invalid");
  }
  const packages = new Map(
    metadata.packages.map((entry) => [entry.name, entry]),
  );
  const knownPackageNames = new Set(packages.keys());
  const pending = [policy.cargo_graph.root];
  const visited = new Set();
  const edges = [];
  const nodes = [];
  while (pending.length > 0) {
    const name = pending.pop();
    if (visited.has(name)) continue;
    const packageEntry = packages.get(name);
    if (
      !isPlainObject(packageEntry)
      || packageEntry.source !== null
      || typeof packageEntry.manifest_path !== "string"
      || !Array.isArray(packageEntry.dependencies)
      || !Array.isArray(packageEntry.targets)
      || packageEntry.links !== null
    ) {
      fail("RPE-BROWSER-PURITY-0010", `unregistered Cargo package ${name}`);
    }
    if (
      packageEntry.targets.some((target) =>
        !isPlainObject(target)
        || !Array.isArray(target.kind)
        || target.kind.includes("custom-build")
      )
    ) {
      fail("RPE-BROWSER-PURITY-0010", `unregistered Cargo build script ${name}`);
    }
    const manifestPath = resolve(packageEntry.manifest_path);
    const manifestRelative = relative(repositoryRoot, manifestPath)
      .split(sep)
      .join("/");
    if (
      manifestRelative.startsWith("../")
      || !(
        manifestRelative.startsWith("pdf-rs/")
        || manifestRelative.startsWith("runtime/")
        || manifestRelative === "platform/browser/worker/Cargo.toml"
      )
    ) {
      fail("RPE-BROWSER-PURITY-0010", `Cargo package escaped product roots: ${name}`);
    }
    visited.add(name);
    nodes.push({
      manifest: manifestRelative,
      name,
      version: packageEntry.version,
    });
    for (const dependency of packageEntry.dependencies) {
      const inspected = inspectProductCargoDependency(
        dependency,
        knownPackageNames,
      );
      if (inspected === undefined) continue;
      edges.push({
        from: name,
        kind: inspected.kind,
        to: inspected.name,
      });
      pending.push(inspected.name);
    }
  }
  nodes.sort((left, right) => left.name.localeCompare(right.name));
  edges.sort((left, right) =>
    left.from.localeCompare(right.from)
    || left.kind.localeCompare(right.kind)
    || left.to.localeCompare(right.to)
  );
  if (
    JSON.stringify(nodes.map((node) => node.name))
    !== JSON.stringify(policy.cargo_graph.packages)
  ) {
    fail("RPE-BROWSER-PURITY-0010", "Cargo product graph is not exact");
  }
  const lockBytes = await readBounded(
    resolve(repositoryRoot, "Cargo.lock"),
    MAX_LOCK_BYTES,
    "Cargo dependency lock",
  );
  return {
    cargoGraphSha256: digestText(canonicalJson({ edges, nodes })),
    cargoLockSha256: sha256(lockBytes),
    cargoPackageCount: nodes.length,
    cargoThirdPartyLeafCount: 0,
  };
};

export const inspectProductCargoDependency = (
  dependency,
  knownPackageNames,
) => {
  if (
    !isPlainObject(dependency)
    || !(knownPackageNames instanceof Set)
    || [...knownPackageNames].some((name) => typeof name !== "string")
    || typeof dependency.name !== "string"
  ) {
    fail("RPE-BROWSER-PURITY-0010", "invalid Cargo dependency record");
  }
  if (dependency.kind === "dev") {
    return undefined;
  }
  if (dependency.kind !== null && dependency.kind !== "build") {
    fail(
      "RPE-BROWSER-PURITY-0010",
      `unsupported Cargo dependency kind for ${dependency.name}`,
    );
  }
  if (dependency.optional === true) {
    fail(
      "RPE-BROWSER-PURITY-0010",
      `optional Cargo dependency is not feature-resolved: ${dependency.name}`,
    );
  }
  if (
    dependency.optional !== false
    || dependency.source !== null
    || !knownPackageNames.has(dependency.name)
  ) {
    fail(
      "RPE-BROWSER-PURITY-0010",
      `browser Wasm reaches third-party Cargo leaf ${dependency.name}`,
    );
  }
  return Object.freeze({
    kind: dependency.kind === "build" ? "build" : "normal",
    name: dependency.name,
  });
};

const validatePackageLock = async (browserRoot, policy) => {
  const packageDocument = await parseJson(
    resolve(browserRoot, PACKAGE_RELATIVE_PATH),
    MAX_POLICY_BYTES,
    "browser package",
  );
  const lockDocument = await parseJson(
    resolve(browserRoot, PACKAGE_LOCK_RELATIVE_PATH),
    MAX_LOCK_BYTES,
    "browser dependency lock",
  );
  const packageJson = packageDocument.value;
  const lock = lockDocument.value;
  if (
    packageJson.name !== "@pdf-rs/browser"
    || packageJson.private !== true
    || Object.hasOwn(packageJson, "dependencies")
    || !isPlainObject(packageJson.devDependencies)
    || lock.lockfileVersion !== 3
    || lock.requires !== true
    || !isPlainObject(lock.packages)
  ) {
    fail("RPE-BROWSER-PURITY-0010", "browser package boundary drifted");
  }
  const expectedLockPaths = [
    "",
    ...policy.build_dependencies.map((entry) => `node_modules/${entry.name}`),
  ];
  if (
    JSON.stringify(Object.keys(lock.packages).sort())
      !== JSON.stringify(expectedLockPaths.sort())
  ) {
    fail("RPE-BROWSER-PURITY-0010", "dependency lock has an unregistered leaf");
  }
  const root = lock.packages[""];
  if (
    !isPlainObject(root)
    || Object.hasOwn(root, "dependencies")
    || canonicalJson(root.devDependencies)
      !== canonicalJson(packageJson.devDependencies)
  ) {
    fail("RPE-BROWSER-PURITY-0010", "dependency lock root drifted");
  }
  for (const dependency of policy.build_dependencies) {
    if (
      dependency.direct
      && (
        dependency.required_by[0] !== "@pdf-rs/browser"
        || packageJson.devDependencies[dependency.name]
          !== dependency.dependency_range
        || dependency.dependency_range !== dependency.version
      )
    ) {
      fail("RPE-BROWSER-PURITY-0010", `unlocked direct build dependency ${dependency.name}`);
    }
    if (
      !dependency.direct
      && (
        Object.hasOwn(packageJson.devDependencies, dependency.name)
        || dependency.required_by[0] === "@pdf-rs/browser"
      )
    ) {
      fail("RPE-BROWSER-PURITY-0010", `invalid transitive build dependency ${dependency.name}`);
    }
    const locked = lock.packages[`node_modules/${dependency.name}`];
    if (
      !isPlainObject(locked)
      || locked.version !== dependency.version
      || locked.resolved !== dependency.source
      || locked.integrity !== dependency.integrity
      || locked.license !== dependency.license
      || locked.dev !== true
    ) {
      fail("RPE-BROWSER-PURITY-0010", `lock drift for ${dependency.name}`);
    }
    if (!dependency.direct) {
      const parent = lock.packages[`node_modules/${dependency.required_by[0]}`];
      if (
        !isPlainObject(parent)
        || !isPlainObject(parent.dependencies)
        || parent.dependencies[dependency.name] !== dependency.dependency_range
      ) {
        fail("RPE-BROWSER-PURITY-0010", `dependency edge drift for ${dependency.name}`);
      }
    }
  }
  for (const dependency of policy.build_dependencies) {
    const expectedDependencies = Object.fromEntries(
      policy.build_dependencies
        .filter((candidate) => candidate.required_by[0] === dependency.name)
        .map((candidate) => [
          candidate.name,
          candidate.dependency_range,
        ]),
    );
    const locked = lock.packages[`node_modules/${dependency.name}`];
    if (
      canonicalJson(locked.dependencies ?? {})
      !== canonicalJson(expectedDependencies)
    ) {
      fail(
        "RPE-BROWSER-PURITY-0010",
        `dependency graph drift for ${dependency.name}`,
      );
    }
  }
  return {
    buildDependencyCount: policy.build_dependencies.length,
    packageLockSha256: digestText(lockDocument.text),
    shippedThirdPartyLeafCount: policy.shipped_third_party_leaves.length,
  };
};

const inspectNativeArtifacts = async (browserRoot, policy) => {
  const output = resolve(browserRoot, NATIVE_OUTPUT_RELATIVE_PATH);
  const outputMetadata = await lstat(output).catch(() => undefined);
  if (
    outputMetadata === undefined
    || !outputMetadata.isDirectory()
    || outputMetadata.isSymbolicLink()
  ) {
    fail("RPE-BROWSER-PURITY-0005", "missing Native artifact directory");
  }
  const entries = await readdir(output, { withFileTypes: true });
  if (
    entries.length > MAX_DIRECTORY_ENTRIES
    || entries.some((entry) => !entry.isFile() || entry.isSymbolicLink())
  ) {
    fail("RPE-BROWSER-PURITY-0005", "Native artifact directory is not flat");
  }
  const names = entries.map((entry) => entry.name).sort();
  if (JSON.stringify(names) !== JSON.stringify(REQUIRED_NATIVE_FILES)) {
    fail("RPE-BROWSER-PURITY-0005", "unregistered bundled resource");
  }

  const resourceByPath = new Map(
    policy.resources.map((resource) => [resource.path, resource]),
  );
  const manifestResource = resourceByPath.get("dist/native/engine-manifest.json");
  const entryResource = resourceByPath.get(
    "dist/native/engine-worker-entry.generated.js",
  );
  const glueResource = resourceByPath.get("dist/native/engine-worker.generated.js");
  const hostResource = resourceByPath.get(
    "dist/native/engine-worker-host.generated.js",
  );
  const engineResource = resourceByPath.get("dist/native/engine.wasm");
  const manifestDocument = await parseCanonicalJson(
    resolve(output, "engine-manifest.json"),
    manifestResource.max_bytes,
    "Native artifact manifest",
  );
  const manifest = manifestDocument.value;
  const engine = new Uint8Array(await readBounded(
    resolve(output, "engine.wasm"),
    engineResource.max_bytes,
    "Native Wasm",
  ));
  const glueBytes = await readBounded(
    resolve(output, "engine-worker.generated.js"),
    glueResource.max_bytes,
    "Native Worker glue",
  );
  const entryBytes = await readBounded(
    resolve(output, "engine-worker-entry.generated.js"),
    entryResource.max_bytes,
    "Native Worker entry",
  );
  const hostBytes = await readBounded(
    resolve(output, "engine-worker-host.generated.js"),
    hostResource.max_bytes,
    "Native Worker Host registration",
  );
  const glue = new TextDecoder("utf-8", { fatal: true }).decode(glueBytes);
  const entry = new TextDecoder("utf-8", { fatal: true }).decode(entryBytes);
  const host = new TextDecoder("utf-8", { fatal: true }).decode(hostBytes);
  const contract = await validateNativeWorkerModule(engine);
  const manifestBytes = new TextEncoder().encode(manifestDocument.text);
  if (
    !exactKeys(manifest, [
      "engine",
      "entry",
      "glue",
      "host",
      "product",
      "protocol_schema_sha256",
      "schema",
    ])
    || !exactKeys(manifest.entry, [
      "byte_length",
      "file",
      "sha256",
    ])
    || !exactKeys(manifest.host, [
      "byte_length",
      "file",
      "sha256",
    ])
    || manifestResource.byte_length !== manifestBytes.byteLength
    || manifestResource.sha256 !== sha256(manifestBytes)
    || glueResource.byte_length !== glueBytes.byteLength
    || glueResource.sha256 !== sha256(glueBytes)
    || entryResource.byte_length !== entryBytes.byteLength
    || entryResource.sha256 !== sha256(entryBytes)
    || hostResource.byte_length !== hostBytes.byteLength
    || hostResource.sha256 !== sha256(hostBytes)
    || engineResource.byte_length !== engine.byteLength
    || engineResource.sha256 !== sha256(engine)
    || manifest.schema !== 1
    || manifest.product !== "PDF.rs Native Wasm Engine Worker"
    || manifest.engine?.file !== "engine.wasm"
    || manifest.engine?.byte_length !== engine.byteLength
    || manifest.engine?.sha256 !== sha256(engine)
    || manifest.engine?.abi_version !== NATIVE_WORKER_ABI_VERSION
    || manifest.engine?.abi_sha256 !== NATIVE_WORKER_ABI_SHA256
    || canonicalJson(manifest.engine?.imports) !== canonicalJson([])
    || canonicalJson(manifest.engine?.exports)
      !== canonicalJson(NATIVE_WORKER_EXPORTS)
    || canonicalJson(manifest.engine?.memory) !== canonicalJson(contract.memory)
    || manifest.glue?.file !== "engine-worker.generated.js"
    || manifest.glue?.byte_length !== glueBytes.byteLength
    || manifest.glue?.sha256 !== sha256(glueBytes)
    || manifest.entry?.file !== "engine-worker-entry.generated.js"
    || manifest.entry?.byte_length !== entryBytes.byteLength
    || manifest.entry?.byte_length > MAX_NATIVE_WORKER_ENTRY_BYTES
    || manifest.entry?.sha256 !== sha256(entryBytes)
    || manifest.host?.file !== "engine-worker-host.generated.js"
    || manifest.host?.byte_length !== hostBytes.byteLength
    || manifest.host?.byte_length > MAX_NATIVE_WORKER_HOST_BYTES
    || manifest.host?.sha256 !== sha256(hostBytes)
  ) {
    fail("RPE-BROWSER-PURITY-0006", "Native artifact manifest drifted");
  }
  const canonicalGlue = renderNativeWorkerGlue({
    byteLength: engine.byteLength,
    sha256: manifest.engine.sha256,
    minimumMemoryPages: contract.memory.minimum,
    maximumMemoryPages: contract.memory.maximum,
    entryByteLength: entryBytes.byteLength,
    entrySha256: sha256(entryBytes),
  });
  const canonicalEntry = renderNativeWorkerEntry();
  const canonicalHost = renderNativeWorkerHost();
  if (
    glue !== canonicalGlue
    || entry !== canonicalEntry
    || host !== canonicalHost
  ) {
    fail(
      "RPE-BROWSER-PURITY-0005",
      "Native Worker generated modules are not canonical",
    );
  }
  inspectProductModuleText("dist/native/engine-worker.generated.js", glue);
  inspectProductModuleText(
    "dist/native/engine-worker-host.generated.js",
    host,
  );
  assertNoForbiddenToken(entry, "Native Worker entry");
  const wasmImports = WebAssembly.Module.imports(contract.module);
  const wasmExports = WebAssembly.Module.exports(contract.module)
    .map((entry) => entry.name)
    .sort();
  if (
    contract.memory.maximum !== policy.wasm_policy.max_memory_pages
    || wasmImports.length !== 0
    || canonicalJson(wasmExports)
      !== canonicalJson([...NATIVE_WORKER_EXPORTS].sort())
  ) {
    fail("RPE-BROWSER-PURITY-0006", "Wasm import/export policy drifted");
  }
  return {
    bundleFileCount: names.length,
    bundleManifestSha256: digestText(manifestDocument.text),
    bundleTotalBytes:
      manifestDocument.text.length
      + engine.byteLength
      + glueBytes.byteLength
      + entryBytes.byteLength
      + hostBytes.byteLength,
    wasmImportCount: wasmImports.length,
  };
};

export const validateBrowserNetworkTrace = (
  policy,
  trace,
  options = {},
) => {
  validatePolicy(policy);
  if (
    !exactKeys(options, ["productBaseUrl", "selectedSource"])
    || !Array.isArray(trace)
    || trace.length === 0
    || trace.length > policy.budgets.max_network_requests_per_open
  ) {
    fail("RPE-BROWSER-PURITY-0009", "network trace exceeds request budget");
  }
  const registrations = new Map(
    policy.network_manifest.map((entry) => [entry.id, entry]),
  );
  const parseCanonicalUrl = (value, label) => {
    if (typeof value !== "string") {
      fail("RPE-BROWSER-PURITY-0009", `invalid ${label}`);
    }
    try {
      const parsed = new URL(value);
      if (
        !["http:", "https:"].includes(parsed.protocol)
        || parsed.username !== ""
        || parsed.password !== ""
        || parsed.hash !== ""
        || parsed.href !== value
      ) {
        fail("RPE-BROWSER-PURITY-0009", `invalid ${label}`);
      }
      return parsed;
    } catch (error) {
      if (error instanceof BrowserProductPurityError) throw error;
      fail("RPE-BROWSER-PURITY-0009", `invalid ${label}`);
    }
  };
  const productBase = parseCanonicalUrl(
    options.productBaseUrl,
    "product base URL",
  );
  if (
    productBase.search !== ""
    || !productBase.pathname.endsWith("/")
  ) {
    fail("RPE-BROWSER-PURITY-0009", "invalid product base URL");
  }
  const selectedSourceDescriptor = options.selectedSource;
  const selectedRegistration = registrations.get("selected-source");
  if (
    !exactKeys(selectedSourceDescriptor, [
      "length",
      "revision",
      "stable_id",
      "url",
      "validator_sha256",
    ])
    || !safePositiveInteger(
      selectedSourceDescriptor.length,
      selectedRegistration.max_bytes,
    )
    || typeof selectedSourceDescriptor.revision !== "string"
    || !/^[1-9][0-9]{0,19}$/u.test(selectedSourceDescriptor.revision)
    || BigInt(selectedSourceDescriptor.revision) > 0xffff_ffff_ffff_ffffn
    || typeof selectedSourceDescriptor.stable_id !== "string"
    || !/^[0-9a-f]{64}$/u.test(selectedSourceDescriptor.stable_id)
    || /^0{64}$/u.test(selectedSourceDescriptor.stable_id)
    || typeof selectedSourceDescriptor.validator_sha256 !== "string"
    || !/^[0-9a-f]{64}$/u.test(selectedSourceDescriptor.validator_sha256)
  ) {
    fail("RPE-BROWSER-PURITY-0009", "invalid selected source identity");
  }
  const selectedSource = parseCanonicalUrl(
    selectedSourceDescriptor.url,
    "selected source URL",
  );
  const productModuleRegistration = registrations.get(
    "product-module-graph",
  );
  const productModuleUrls = policy.module_graph.files.map((path) =>
    new URL(path.replace(/\.ts$/u, ".js"), productBase).href
  );
  const workerModuleUrls = new Set(
    policy.worker_graph.module_files.map((path) =>
      new URL(path.replace(/\.ts$/u, ".js"), productBase).href
    ),
  );

  const expectedUrls = new Map();
  for (
    const resourceId of [
      "native-loader-glue",
      "native-wasm",
      "native-worker-entry",
      "native-worker-host",
    ]
  ) {
    const registration = registrations.get(resourceId);
    const expected = new URL(registration.location, productBase).href;
    expectedUrls.set(resourceId, Object.freeze([expected]));
  }
  expectedUrls.set(
    "product-module-graph",
    Object.freeze([...productModuleUrls]),
  );
  expectedUrls.set("selected-source", Object.freeze([selectedSource.href]));

  const resourceByUrl = new Map();
  const expectedIdentities = new Map([
    [
      "native-wasm",
      policy.resources.find(
        (resource) => resource.path === "dist/native/engine.wasm",
      ).sha256,
    ],
    [
      "native-loader-glue",
      policy.resources.find(
        (resource) =>
          resource.path === "dist/native/engine-worker.generated.js",
      ).sha256,
    ],
    [
      "native-worker-entry",
      policy.resources.find(
        (resource) =>
          resource.path
            === "dist/native/engine-worker-entry.generated.js",
      ).sha256,
    ],
    [
      "native-worker-host",
      policy.resources.find(
        (resource) =>
          resource.path
            === "dist/native/engine-worker-host.generated.js",
      ).sha256,
    ],
    ["selected-source", digestText(canonicalJson(selectedSourceDescriptor))],
    ["product-module-graph", policy.module_graph.sha256],
  ]);
  for (const [resourceId, urls] of expectedUrls) {
    for (const url of urls) {
      const parsed = parseCanonicalUrl(url, `${resourceId} URL`);
      if (
        resourceId !== "selected-source"
        && parsed.origin !== productBase.origin
      ) {
        fail("RPE-BROWSER-PURITY-0009", "product resource crossed origin");
      }
      if (resourceByUrl.has(url)) {
        fail("RPE-BROWSER-PURITY-0009", "network resource URL is ambiguous");
      }
      resourceByUrl.set(url, resourceId);
    }
  }

  const counts = new Map();
  const countsByUrl = new Map();
  const byteTotals = new Map();
  for (const request of trace) {
    if (
      !exactKeys(request, ["bytes", "identity", "resource_id", "url"])
      || typeof request.identity !== "string"
      || typeof request.resource_id !== "string"
      || typeof request.url !== "string"
      || !Number.isSafeInteger(request.bytes)
      || request.bytes <= 0
    ) {
      fail("RPE-BROWSER-PURITY-0009", "malformed network trace entry");
    }
    const registration = registrations.get(request.resource_id);
    const expectedResourceId = resourceByUrl.get(request.url);
    const nextBytes =
      (byteTotals.get(request.resource_id) ?? 0) + request.bytes;
    if (
      registration === undefined
      || expectedResourceId !== request.resource_id
      || request.identity !== expectedIdentities.get(request.resource_id)
      || !Number.isSafeInteger(nextBytes)
      || nextBytes > registration.max_bytes
    ) {
      fail("RPE-BROWSER-PURITY-0009", "unregistered network request");
    }
    const staticPath = {
      "native-wasm": "dist/native/engine.wasm",
      "native-loader-glue": "dist/native/engine-worker.generated.js",
      "native-worker-entry":
        "dist/native/engine-worker-entry.generated.js",
      "native-worker-host":
        "dist/native/engine-worker-host.generated.js",
    }[request.resource_id];
    if (
      staticPath !== undefined
      && request.bytes !== policy.resources.find(
        (resource) => resource.path === staticPath,
      ).byte_length
    ) {
      fail("RPE-BROWSER-PURITY-0009", "static resource length drifted");
    }
    byteTotals.set(request.resource_id, nextBytes);
    assertNoForbiddenToken(request.url, "network trace URL");
    let parsedUrl;
    try {
      parsedUrl = new URL(request.url);
    } catch {
      fail("RPE-BROWSER-PURITY-0009", "invalid network trace URL");
    }
    if (
      !["http:", "https:"].includes(parsedUrl.protocol)
      || parsedUrl.username !== ""
      || parsedUrl.password !== ""
      || parsedUrl.href !== request.url
    ) {
      fail("RPE-BROWSER-PURITY-0009", "non-canonical network trace URL");
    }
    const nextCount = (counts.get(request.resource_id) ?? 0) + 1;
    if (nextCount > registration.max_requests_per_open) {
      fail("RPE-BROWSER-PURITY-0009", "network resource request budget exceeded");
    }
    counts.set(request.resource_id, nextCount);
    countsByUrl.set(request.url, (countsByUrl.get(request.url) ?? 0) + 1);
  }
  for (const resourceId of REQUIRED_NETWORK_IDS) {
    if ((counts.get(resourceId) ?? 0) === 0) {
      fail(
        "RPE-BROWSER-PURITY-0009",
        `network trace omitted required resource ${resourceId}`,
      );
    }
  }
  const workerRealmMultiplicity =
    registrations.get("native-worker-entry").max_requests_per_open;
  if (
    productModuleUrls.some((url) => {
      const count = countsByUrl.get(url);
      const maximum = workerModuleUrls.has(url)
        ? 1 + workerRealmMultiplicity
        : 1;
      return count === undefined || count < 1 || count > maximum;
    })
    || counts.get("product-module-graph")
      > productModuleRegistration.max_requests_per_open
  ) {
    fail(
      "RPE-BROWSER-PURITY-0009",
      "product module trace exceeds registered realm multiplicity",
    );
  }
  return {
    requestCount: trace.length,
    resourceCount: counts.size,
  };
};

export const checkBrowserProductPurity = async ({
  browserRoot = DEFAULT_BROWSER_ROOT,
  repositoryRoot = resolve(browserRoot, "../.."),
} = {}) => {
  const root = resolve(browserRoot);
  const cargoRoot = resolve(repositoryRoot);
  const policyDocument = await parseCanonicalJson(
    resolve(root, POLICY_RELATIVE_PATH),
    MAX_POLICY_BYTES,
    "browser product policy",
  );
  const policy = policyDocument.value;
  validatePolicy(policy);
  const [dependencies, moduleGraph, artifacts, cargoGraph] = await Promise.all([
    validatePackageLock(root, policy),
    inspectModuleGraph(root, policy),
    inspectNativeArtifacts(root, policy),
    inspectCargoGraph(cargoRoot, policy),
  ]);
  return Object.freeze({
    ...artifacts,
    ...cargoGraph,
    ...dependencies,
    moduleEdgeCount: moduleGraph.edgeCount,
    moduleFileCount: moduleGraph.fileCount,
    moduleGraphSha256: moduleGraph.sha256,
    workerModuleFileCount: moduleGraph.workerModuleFileCount,
    networkManifestSha256: digestText(canonicalJson(policy.network_manifest)),
    policySha256: digestText(policyDocument.text),
    precacheSha256: digestText(canonicalJson(policy.service_worker)),
    shippedThirdPartyLeafCount: policy.shipped_third_party_leaves.length,
    workerGraphSha256: digestText(canonicalJson(policy.worker_graph)),
  });
};

export const runBrowserProductPurityCli = async ({
  browserRoot = DEFAULT_BROWSER_ROOT,
} = {}) => {
  const report = await checkBrowserProductPurity({ browserRoot });
  for (const [key, value] of Object.entries(report).sort(([left], [right]) =>
    left.localeCompare(right)
  )) {
    process.stdout.write(`${key}=${value}\n`);
  }
  process.stdout.write("scope=browser-native-product-closure\n");
  return report;
};

const invokedPath = process.argv[1] === undefined
  ? undefined
  : pathToFileURL(resolve(process.argv[1])).href;
if (invokedPath === import.meta.url) {
  runBrowserProductPurityCli().catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  });
}
