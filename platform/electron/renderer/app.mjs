const openButton = document.querySelector("#open");
const emptyOpenButton = document.querySelector("#empty-open");
const closeButton = document.querySelector("#close");
const previousButton = document.querySelector("#previous");
const nextButton = document.querySelector("#next");
const zoomOutButton = document.querySelector("#zoom-out");
const zoomInButton = document.querySelector("#zoom-in");
const pageLabel = document.querySelector("#page-label");
const zoomLabel = document.querySelector("#zoom-label");
const documentName = document.querySelector("#document-name");
const status = document.querySelector("#status");
const empty = document.querySelector("#empty");
const pageShell = document.querySelector("#page-shell");
const canvas = document.querySelector("#page");
const viewport = document.querySelector("#viewport");

let activeDocument;
let page = 0;
let zoom = 1;
let generation = 0;
let resizeTimer;

const setStatus = (message, kind = "normal") => {
  status.textContent = message;
  status.dataset.kind = kind;
};

const updateControls = () => {
  const ready = Boolean(activeDocument);
  previousButton.disabled = !ready || page === 0;
  nextButton.disabled = !ready || page + 1 >= (activeDocument?.pageCount ?? 0);
  zoomOutButton.disabled = !ready || zoom <= 0.5;
  zoomInButton.disabled = !ready || zoom >= 2;
  closeButton.disabled = !ready;
  pageLabel.textContent = ready
    ? `Page ${page + 1} / ${activeDocument.pageCount}`
    : "No document";
  zoomLabel.textContent = `${Math.round(zoom * 100)}%`;
};

const render = async () => {
  if (!activeDocument) {
    return;
  }
  const requestGeneration = ++generation;
  setStatus("Rendering with PDF.rs…");
  const available = Math.max(240, Math.min(384, viewport.clientWidth - 96));
  const width = Math.max(160, Math.round(available * zoom));
  const result = await window.pdfRs.renderPage({
    documentId: activeDocument.documentId,
    page,
    width,
  });
  if (requestGeneration !== generation) {
    return;
  }
  if (!result.ok) {
    setStatus(failureLabel(result.code), "error");
    return;
  }
  canvas.width = result.width;
  canvas.height = result.height;
  const context = canvas.getContext("2d", { alpha: false });
  const pixels = new Uint8ClampedArray(result.pixels);
  context.putImageData(new ImageData(pixels, result.width, result.height), 0, 0);
  pageShell.hidden = false;
  empty.hidden = true;
  setStatus(`${result.width} × ${result.height} · RGBA8`);
  requestAnimationFrame(() => window.pdfRs.notifyPreviewReady());
};

const adoptDocument = async (result) => {
  if (!result.ok) {
    if (result.code !== "cancelled" && result.code !== "absent") {
      setStatus(failureLabel(result.code), "error");
    }
    return;
  }
  activeDocument = result;
  page = 0;
  zoom = 1;
  documentName.textContent = result.name;
  updateControls();
  await render();
};

const open = async () => {
  setStatus("Waiting for file selection…");
  await adoptDocument(await window.pdfRs.openPdf());
};

const close = async () => {
  generation += 1;
  await window.pdfRs.closePdf();
  activeDocument = undefined;
  page = 0;
  zoom = 1;
  canvas.width = 0;
  canvas.height = 0;
  pageShell.hidden = true;
  empty.hidden = false;
  documentName.textContent = "Local development preview";
  setStatus("Ready");
  updateControls();
};

const movePage = async (delta) => {
  if (!activeDocument) {
    return;
  }
  const nextPage = Math.max(0, Math.min(activeDocument.pageCount - 1, page + delta));
  if (nextPage === page) {
    return;
  }
  page = nextPage;
  updateControls();
  await render();
};

const changeZoom = async (delta) => {
  if (!activeDocument) {
    return;
  }
  zoom = Math.max(0.5, Math.min(2, Math.round((zoom + delta) * 4) / 4));
  updateControls();
  await render();
};

const failureLabel = (code) => {
  const labels = {
    unsupported: "This page is outside the current PDF.rs Native profile.",
    document: "PDF.rs could not open this document.",
    content: "PDF.rs rejected the page content.",
    source: "The local source could not be read.",
    "resource-limit": "The document exceeded a bounded PDF.rs resource limit.",
    "invalid-input": "The render request was rejected.",
    render: "PDF.rs could not produce this page.",
    "bridge-closed": "The Rust rendering process stopped.",
  };
  return labels[code] ?? `Viewer error: ${code}`;
};

openButton.addEventListener("click", open);
emptyOpenButton.addEventListener("click", open);
closeButton.addEventListener("click", close);
previousButton.addEventListener("click", () => movePage(-1));
nextButton.addEventListener("click", () => movePage(1));
zoomOutButton.addEventListener("click", () => changeZoom(-0.25));
zoomInButton.addEventListener("click", () => changeZoom(0.25));
window.addEventListener("resize", () => {
  clearTimeout(resizeTimer);
  resizeTimer = setTimeout(() => {
    if (activeDocument) {
      void render();
    }
  }, 120);
});

updateControls();
void window.pdfRs.openStartupPdf().then(adoptDocument).catch((error) => {
  setStatus("The Electron host bridge was unavailable.", "error");
  console.error(error);
});
