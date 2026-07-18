import { spawn } from "node:child_process";
import { access, rm, stat } from "node:fs/promises";
import { resolve } from "node:path";

import electron from "electron";

const root = resolve(import.meta.dirname, "../../..");
const fixture = resolve(root, "tests/desktop/readable-preview.pdf");
const screenshot = resolve(root, "target/electron-preview-smoke.png");
const evidence = [
  screenshot,
  resolve(root, "target/electron-preview-smoke-page-2.png"),
  resolve(root, "target/electron-preview-smoke-zoom-125.png"),
  resolve(root, "target/electron-preview-smoke-resized.png"),
  resolve(root, "target/electron-preview-smoke-closed.png"),
];
for (const path of evidence) {
  await rm(path, { force: true });
}
const child = spawn(electron, ["."], {
  cwd: resolve(root, "platform/electron"),
  env: {
    ...process.env,
    PDF_RS_ELECTRON_OPEN: fixture,
    PDF_RS_ELECTRON_SMOKE_SCREENSHOT: screenshot,
  },
  stdio: ["ignore", "pipe", "inherit"],
});

const timeout = setTimeout(() => {
  child.kill();
}, 55_000);

let stdout = "";
child.stdout.setEncoding("utf8");
child.stdout.on("data", (chunk) => {
  stdout += chunk;
  process.stdout.write(chunk);
});

const code = await new Promise((resolveExit) => child.on("exit", resolveExit));
clearTimeout(timeout);
if (code !== 0 || !stdout.includes("PDF_RS_ELECTRON_SMOKE_READY")) {
  throw new Error(`Electron smoke failed with exit ${code}`);
}
const markers = [
  "PDF_RS_ELECTRON_FIRST_PAGE_READY",
  "PDF_RS_ELECTRON_PAGE_TWO_READY",
  "PDF_RS_ELECTRON_ZOOM_READY",
  "PDF_RS_ELECTRON_RESIZE_READY",
  "PDF_RS_ELECTRON_CLOSE_READY",
];
for (const marker of markers) {
  if (!stdout.includes(marker)) {
    throw new Error(`Electron smoke did not report ${marker}`);
  }
}
for (const path of evidence) {
  await access(path);
  const screenshotEvidence = await stat(path);
  if (screenshotEvidence.size < 1_024) {
    throw new Error(`Electron smoke screenshot is unexpectedly empty: ${path}`);
  }
}
