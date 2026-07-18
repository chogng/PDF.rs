import { spawn } from "node:child_process";
import { access, rm, stat } from "node:fs/promises";
import { resolve } from "node:path";

import electron from "electron";

const root = resolve(import.meta.dirname, "../../..");
const fixture = resolve(root, "tests/desktop/readable-preview.pdf");
const screenshot = resolve(root, "target/electron-preview-smoke.png");
await rm(screenshot, { force: true });
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
}, 45_000);

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
await access(screenshot);
const evidence = await stat(screenshot);
if (evidence.size < 1_024) {
  throw new Error("Electron smoke screenshot is unexpectedly empty");
}
