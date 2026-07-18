import { app, BrowserWindow, dialog, ipcMain } from "electron";
import { mkdir, writeFile } from "node:fs/promises";
import { basename, dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { PdfRsBridge, PdfRsBridgeError } from "./bridge.mjs";

const moduleDirectory = dirname(fileURLToPath(import.meta.url));
const smokeScreenshot = process.env.PDF_RS_ELECTRON_SMOKE_SCREENSHOT;
let bridge;
let mainWindow;
let currentDocument;
let quitting = false;
let smokeCaptured = false;

const assertMainFrame = (event) => {
  if (!mainWindow || event.sender !== mainWindow.webContents) {
    throw new Error("untrusted-frame");
  }
};

const safeFailure = (error) => ({
  ok: false,
  code: error instanceof PdfRsBridgeError ? error.code : "host",
});

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
  if (!smokeScreenshot || smokeCaptured || !mainWindow) {
    return;
  }
  smokeCaptured = true;
  try {
    const image = await mainWindow.webContents.capturePage();
    await mkdir(dirname(smokeScreenshot), { recursive: true });
    await writeFile(smokeScreenshot, image.toPNG());
    console.log(`PDF_RS_ELECTRON_SMOKE_READY ${smokeScreenshot}`);
    app.quit();
  } catch {
    app.exit(1);
  }
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
