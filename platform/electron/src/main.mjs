import { app, BrowserWindow, dialog, ipcMain } from "electron";
import { mkdir, writeFile } from "node:fs/promises";
import { basename, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { PdfRsBridge, PdfRsBridgeError } from "./bridge.mjs";

const moduleDirectory = dirname(fileURLToPath(import.meta.url));
const smokeScreenshot = process.env.PDF_RS_ELECTRON_SMOKE_SCREENSHOT;
const smokeExpectedRenderer = process.env.PDF_RS_ELECTRON_RENDERER_COHORT
  ? "Fast CPU"
  : "Reference CPU";
const smokeEvidence = smokeScreenshot
  ? Object.freeze({
      firstPage: smokeScreenshot,
      pageTwo: resolve(
        dirname(smokeScreenshot),
        `${basename(smokeScreenshot, ".png")}-page-2.png`,
      ),
      zoom: resolve(
        dirname(smokeScreenshot),
        `${basename(smokeScreenshot, ".png")}-zoom-125.png`,
      ),
      resize: resolve(
        dirname(smokeScreenshot),
        `${basename(smokeScreenshot, ".png")}-resized.png`,
      ),
      closed: resolve(
        dirname(smokeScreenshot),
        `${basename(smokeScreenshot, ".png")}-closed.png`,
      ),
    })
  : undefined;
let bridge;
let mainWindow;
let currentDocument;
let quitting = false;
let smokeCaptured = false;
let smokeHandling = false;
let smokeStage = 0;

const assertMainFrame = (event) => {
  if (!mainWindow || event.sender !== mainWindow.webContents) {
    throw new Error("untrusted-frame");
  }
};

const safeFailure = (error) => ({
  ok: false,
  code: error instanceof PdfRsBridgeError ? error.code : "host",
});

const smokeSnapshot = async () =>
  mainWindow.webContents.executeJavaScript(`(() => {
    const canvas = document.querySelector("#page");
    return {
      pageLabel: document.querySelector("#page-label")?.textContent,
      zoomLabel: document.querySelector("#zoom-label")?.textContent,
      documentName: document.querySelector("#document-name")?.textContent,
      status: document.querySelector("#status")?.textContent,
      pageShellHidden: document.querySelector("#page-shell")?.hidden,
      emptyHidden: document.querySelector("#empty")?.hidden,
      closeDisabled: document.querySelector("#close")?.disabled,
      canvasWidth: canvas?.width,
      canvasHeight: canvas?.height,
      innerWidth: window.innerWidth,
      innerHeight: window.innerHeight,
    };
  })()`);

const assertSmoke = (condition, message) => {
  if (!condition) {
    throw new Error(`smoke-${message}`);
  }
};

const assertRenderedSnapshot = (snapshot, pageLabel, zoomLabel, width) => {
  assertSmoke(snapshot.pageLabel === pageLabel, `page-label-${snapshot.pageLabel}`);
  assertSmoke(snapshot.zoomLabel === zoomLabel, `zoom-label-${snapshot.zoomLabel}`);
  assertSmoke(snapshot.documentName === "readable-preview.pdf", "document-name");
  assertSmoke(snapshot.pageShellHidden === false, "page-hidden");
  assertSmoke(snapshot.emptyHidden === true, "empty-visible");
  assertSmoke(snapshot.closeDisabled === false, "close-disabled");
  assertSmoke(snapshot.canvasWidth === width, `canvas-width-${snapshot.canvasWidth}`);
  assertSmoke(snapshot.canvasHeight > width, `canvas-height-${snapshot.canvasHeight}`);
  assertSmoke(
    snapshot.status.includes(`RGBA8 · ${smokeExpectedRenderer}`),
    "renderer-status",
  );
};

const captureSmoke = async (path) => {
  const image = await mainWindow.webContents.capturePage();
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, image.toPNG());
};

const clickSmokeControl = async (selector) => {
  const clicked = await mainWindow.webContents.executeJavaScript(`(() => {
    const control = document.querySelector(${JSON.stringify(selector)});
    if (!(control instanceof HTMLButtonElement) || control.disabled) {
      return false;
    }
    control.click();
    return true;
  })()`);
  assertSmoke(clicked, `control-${selector}`);
};

const waitForClosedSnapshot = async () => {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const snapshot = await smokeSnapshot();
    if (snapshot.pageShellHidden && !snapshot.emptyHidden) {
      await mainWindow.webContents.executeJavaScript(
        "new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)))",
      );
      return smokeSnapshot();
    }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 25));
  }
  throw new Error("smoke-close-timeout");
};

const handleSmokePreview = async () => {
  if (
    !smokeEvidence
    || smokeCaptured
    || smokeHandling
    || !mainWindow
  ) {
    return;
  }
  smokeHandling = true;
  try {
    const snapshot = await smokeSnapshot();
    if (smokeStage === 0) {
      assertRenderedSnapshot(snapshot, "Page 1 / 2", "100%", 384);
      await captureSmoke(smokeEvidence.firstPage);
      console.log(`PDF_RS_ELECTRON_FIRST_PAGE_READY ${smokeEvidence.firstPage}`);
      smokeStage = 1;
      await clickSmokeControl("#next");
      return;
    }
    if (smokeStage === 1) {
      assertRenderedSnapshot(snapshot, "Page 2 / 2", "100%", 384);
      await captureSmoke(smokeEvidence.pageTwo);
      console.log(`PDF_RS_ELECTRON_PAGE_TWO_READY ${smokeEvidence.pageTwo}`);
      smokeStage = 2;
      await clickSmokeControl("#zoom-in");
      return;
    }
    if (smokeStage === 2) {
      assertRenderedSnapshot(snapshot, "Page 2 / 2", "125%", 480);
      await captureSmoke(smokeEvidence.zoom);
      console.log(`PDF_RS_ELECTRON_ZOOM_READY ${smokeEvidence.zoom}`);
      smokeStage = 3;
      mainWindow.setContentSize(900, 700);
      return;
    }
    if (smokeStage === 3) {
      assertRenderedSnapshot(snapshot, "Page 2 / 2", "125%", 480);
      assertSmoke(snapshot.innerWidth === 900, `inner-width-${snapshot.innerWidth}`);
      assertSmoke(snapshot.innerHeight === 700, `inner-height-${snapshot.innerHeight}`);
      await captureSmoke(smokeEvidence.resize);
      console.log(`PDF_RS_ELECTRON_RESIZE_READY ${smokeEvidence.resize}`);
      smokeStage = 4;
      await clickSmokeControl("#close");
      const closed = await waitForClosedSnapshot();
      assertSmoke(closed.pageLabel === "No document", "close-page-label");
      assertSmoke(closed.zoomLabel === "100%", "close-zoom-label");
      assertSmoke(closed.documentName === "Local development preview", "close-document-name");
      assertSmoke(closed.status === "Ready", "close-status");
      assertSmoke(closed.closeDisabled === true, "close-enabled");
      assertSmoke(closed.canvasWidth === 0, "close-canvas-width");
      assertSmoke(closed.canvasHeight === 0, "close-canvas-height");
      await captureSmoke(smokeEvidence.closed);
      console.log(`PDF_RS_ELECTRON_CLOSE_READY ${smokeEvidence.closed}`);
      smokeCaptured = true;
      console.log(`PDF_RS_ELECTRON_SMOKE_READY ${smokeEvidence.firstPage}`);
      app.quit();
    }
  } catch (error) {
    console.error(`PDF_RS_ELECTRON_SMOKE_FAILED ${error?.message ?? "unknown"}`);
    app.exit(1);
  } finally {
    smokeHandling = false;
  }
};

const openPath = async (path) => {
  if (currentDocument) {
    await bridge.close(currentDocument.documentId).catch(() => undefined);
    currentDocument = undefined;
  }
  const opened = await bridge.open(path);
  currentDocument = {
    ...opened,
    name: basename(path),
  };
  return { ok: true, ...currentDocument };
};

const createWindow = async () => {
  mainWindow = new BrowserWindow({
    width: 1180,
    height: 840,
    minWidth: 760,
    minHeight: 560,
    show: !smokeScreenshot,
    backgroundColor: "#e9e7e1",
    title: "PDF.rs",
    webPreferences: {
      preload: resolve(moduleDirectory, "preload.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      webSecurity: true,
    },
  });
  if (smokeScreenshot) {
    mainWindow.webContents.on("did-fail-load", (_event, code) => {
      console.error(`PDF_RS_ELECTRON_LOAD_FAILED ${code}`);
    });
    mainWindow.webContents.on("preload-error", (_event, _path, error) => {
      console.error(`PDF_RS_ELECTRON_PRELOAD_FAILED ${error?.message ?? "unknown"}`);
    });
    mainWindow.webContents.on("render-process-gone", (_event, details) => {
      console.error(`PDF_RS_ELECTRON_RENDERER_GONE ${details.reason}`);
    });
    mainWindow.webContents.on("console-message", (_event, details) => {
      console.error(
        `PDF_RS_ELECTRON_CONSOLE ${details.level ?? "unknown"} ${details.message ?? "unknown"}`,
      );
    });
  }
  mainWindow.removeMenu();
  if (smokeScreenshot) {
    await mainWindow.webContents.session.clearCache();
  }
  await mainWindow.loadFile(resolve(moduleDirectory, "../renderer/index.html"));
  mainWindow.on("closed", () => {
    mainWindow = undefined;
  });
};

ipcMain.handle("pdf-rs:open", async (event) => {
  assertMainFrame(event);
  const selection = await dialog.showOpenDialog(mainWindow, {
    title: "Open a PDF with PDF.rs",
    properties: ["openFile"],
    filters: [{ name: "PDF documents", extensions: ["pdf"] }],
  });
  if (selection.canceled || selection.filePaths.length !== 1) {
    return { ok: false, code: "cancelled" };
  }
  try {
    return await openPath(selection.filePaths[0]);
  } catch (error) {
    return safeFailure(error);
  }
});

ipcMain.handle("pdf-rs:open-startup", async (event) => {
  assertMainFrame(event);
  const path = process.env.PDF_RS_ELECTRON_OPEN;
  if (!path) {
    return { ok: false, code: "absent" };
  }
  try {
    const opened = await openPath(path);
    if (smokeScreenshot) {
      console.log(`PDF_RS_ELECTRON_OPENED ${opened.pageCount}`);
    }
    return opened;
  } catch (error) {
    return safeFailure(error);
  }
});

ipcMain.handle("pdf-rs:render", async (event, request) => {
  assertMainFrame(event);
  if (
    !currentDocument
    || request?.documentId !== currentDocument.documentId
    || !Number.isSafeInteger(request?.page)
    || request.page < 0
    || !Number.isSafeInteger(request?.width)
  ) {
    return { ok: false, code: "invalid-input" };
  }
  try {
    const surface = await bridge.render(request.documentId, request.page, request.width);
    if (smokeScreenshot) {
      console.log(
        `PDF_RS_ELECTRON_RENDERED ${surface.page} ${surface.width} ${surface.height}`,
      );
    }
    return { ok: true, ...surface };
  } catch (error) {
    return safeFailure(error);
  }
});

ipcMain.handle("pdf-rs:close", async (event) => {
  assertMainFrame(event);
  if (!currentDocument) {
    return { ok: true };
  }
  const document = currentDocument;
  currentDocument = undefined;
  try {
    await bridge.close(document.documentId);
    return { ok: true };
  } catch (error) {
    return safeFailure(error);
  }
});

ipcMain.on("pdf-rs:preview-ready", async (event) => {
  assertMainFrame(event);
  if (smokeScreenshot) {
    console.log("PDF_RS_ELECTRON_PREVIEW_READY");
  }
  await handleSmokePreview();
});

app.whenReady().then(async () => {
  bridge = new PdfRsBridge();
  await createWindow();
});

app.on("window-all-closed", () => {
  app.quit();
});

app.on("before-quit", (event) => {
  if (quitting || !bridge) {
    return;
  }
  event.preventDefault();
  quitting = true;
  void bridge.shutdown().finally(() => app.exit(0));
});

app.on("will-quit", () => {
  bridge?.terminate();
});
