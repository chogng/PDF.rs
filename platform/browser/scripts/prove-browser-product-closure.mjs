import { spawnSync } from "node:child_process";
import {
  copyFile,
  lstat,
  mkdir,
  mkdtemp,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = fileURLToPath(new URL("../../../", import.meta.url));
const MAX_CLOSURE_FILES = 4_096;
const MAX_CLOSURE_BYTES = 128 * 1024 * 1024;
const EXCLUDED_PREFIXES = Object.freeze([
  ".git/",
  "platform/browser/.test-dist/",
  "platform/browser/dist/",
  "platform/browser/node_modules/",
  "target/",
  "tools/baseline/",
]);
const ISOLATED_BROWSER_WORKSPACE = `[workspace]
resolver = "3"
members = [
    "core/*",
    "platform/browser/worker",
    "runtime/cache",
    "runtime/engine",
    "runtime/policy",
    "runtime/protocol",
    "runtime/scheduler",
    "runtime/surface",
]

[workspace.package]
edition = "2024"
rust-version = "1.93"
version = "0.1.0"
`;

const run = (command, commandArguments, options) => {
  const result = spawnSync(command, commandArguments, {
    encoding: "utf8",
    ...options,
  });
  if (result.status !== 0) {
    process.stderr.write(result.stdout);
    process.stderr.write(result.stderr);
    throw new Error(
      `${command} failed during isolated browser product closure`,
    );
  }
  return result.stdout;
};

const repositoryFiles = () => {
  const output = run(
    "git",
    ["ls-files", "-z", "--cached", "--others", "--exclude-standard"],
    { cwd: repositoryRoot, encoding: "buffer" },
  );
  const files = Buffer.from(output)
    .toString("utf8")
    .split("\u0000")
    .filter((path) => path.length > 0)
    .filter((path) =>
      !EXCLUDED_PREFIXES.some((prefix) => path.startsWith(prefix))
      && !path.split("/").includes("target")
      && !path.split("/").includes("node_modules")
    )
    .sort();
  if (
    files.length === 0
    || files.length > MAX_CLOSURE_FILES
    || new Set(files).size !== files.length
  ) {
    throw new Error("isolated browser product closure file bound failed");
  }
  return files;
};

const copyClosure = async (destination, files) => {
  let totalBytes = 0;
  for (const path of files) {
    if (
      path.startsWith("/")
      || path.includes("\\")
      || path.split("/").some((part) => part === "" || part === "." || part === "..")
    ) {
      throw new Error("isolated browser product closure contains an unsafe path");
    }
    const source = resolve(repositoryRoot, path);
    const sourceRelative = relative(repositoryRoot, source);
    if (sourceRelative.startsWith(`..${sep}`) || sourceRelative === "..") {
      throw new Error("isolated browser product closure escaped the repository");
    }
    const metadata = await lstat(source);
    if (!metadata.isFile() || metadata.isSymbolicLink()) {
      throw new Error("isolated browser product closure contains a non-file");
    }
    totalBytes += metadata.size;
    if (totalBytes > MAX_CLOSURE_BYTES) {
      throw new Error("isolated browser product closure byte bound failed");
    }
    const target = join(destination, path);
    await mkdir(dirname(target), { recursive: true });
    await copyFile(source, target);
  }
  return totalBytes;
};

const isolatedEnvironment = (temporary, cargoHome, temporaryDirectory) => {
  const inherited = {};
  for (const key of [
    "ComSpec",
    "HOME",
    "PATH",
    "PATHEXT",
    "RUSTUP_HOME",
    "SystemRoot",
    "USERPROFILE",
    "WINDIR",
  ]) {
    if (typeof process.env[key] === "string") {
      inherited[key] = process.env[key];
    }
  }
  return {
    ...inherited,
    CARGO_HOME: cargoHome,
    CARGO_NET_OFFLINE: "true",
    CARGO_TERM_COLOR: "never",
    TMPDIR: temporaryDirectory,
    TMP: temporaryDirectory,
    TEMP: temporaryDirectory,
    PDF_RS_ISOLATED_CLOSURE_ROOT: temporary,
  };
};

const temporary = await mkdtemp(join(tmpdir(), "pdf-rs-browser-closure-"));
try {
  const isolatedRepository = join(temporary, "repository");
  const isolatedCargoHome = join(temporary, "cargo-home");
  const isolatedTemporaryDirectory = join(temporary, "tmp");
  await mkdir(isolatedRepository);
  await mkdir(isolatedCargoHome);
  await mkdir(isolatedTemporaryDirectory);
  const files = repositoryFiles();
  const copiedBytes = await copyClosure(isolatedRepository, files);
  await writeFile(
    join(isolatedRepository, "Cargo.toml"),
    ISOLATED_BROWSER_WORKSPACE,
    { encoding: "utf8", flag: "w" },
  );
  const forbiddenSibling = join(temporary, "pdfium");
  const baselineGraph = join(isolatedRepository, "tools/baseline");
  if (
    await lstat(forbiddenSibling).catch(() => undefined) !== undefined
    || await lstat(baselineGraph).catch(() => undefined) !== undefined
  ) {
    throw new Error("isolated browser product closure copied an external engine graph");
  }

  const isolatedBrowser = join(isolatedRepository, "platform/browser");
  const environment = isolatedEnvironment(
    temporary,
    isolatedCargoHome,
    isolatedTemporaryDirectory,
  );
  await rm(join(isolatedRepository, "Cargo.lock"), { force: true });
  const lockOutput = run(
    "cargo",
    ["generate-lockfile", "--offline"],
    { cwd: isolatedRepository, env: environment },
  );
  const buildOutput = run(
    process.execPath,
    ["scripts/build-native-worker.mjs"],
    { cwd: isolatedBrowser, env: environment },
  );
  const purityOutput = run(
    process.execPath,
    ["scripts/check-browser-product-purity.mjs"],
    { cwd: isolatedBrowser, env: environment },
  );
  process.stdout.write(lockOutput);
  process.stdout.write(buildOutput);
  process.stdout.write(purityOutput);
  process.stdout.write(`isolatedClosureBytes=${copiedBytes}\n`);
  process.stdout.write(`isolatedClosureFiles=${files.length}\n`);
  process.stdout.write("isolatedCargoHome=private-empty\n");
  process.stdout.write("isolatedCargoLock=browser-product-only\n");
  process.stdout.write("isolatedCargoNetwork=offline\n");
  process.stdout.write("isolatedPdfiumSiblingPresent=false\n");
  process.stdout.write("isolatedRustcWrapperInherited=false\n");
  process.stdout.write("isolatedToolsBaselinePresent=false\n");
  process.stdout.write("isolatedWorkspace=browser-product-only\n");
  process.stdout.write("scope=isolated-browser-product-build-closure\n");
} finally {
  await rm(temporary, { force: true, recursive: true });
}
