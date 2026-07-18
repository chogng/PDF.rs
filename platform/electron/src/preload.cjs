const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld(
  "pdfRs",
  Object.freeze({
    openPdf: () => ipcRenderer.invoke("pdf-rs:open"),
    openStartupPdf: () => ipcRenderer.invoke("pdf-rs:open-startup"),
    renderPage: (request) => ipcRenderer.invoke("pdf-rs:render", request),
    closePdf: () => ipcRenderer.invoke("pdf-rs:close"),
    notifyPreviewReady: () => ipcRenderer.send("pdf-rs:preview-ready"),
  }),
);
