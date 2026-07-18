import assert from "node:assert/strict";
import { resolve } from "node:path";
import test from "node:test";

import { PdfRsBridge } from "../src/bridge.mjs";

const mixedPdf = resolve(
  import.meta.dirname,
  "../../../tests/cases/raster/m3-reference/valid-mixed/input.pdf",
);

test("persistent bridge opens and renders real PDF.rs pixels", async () => {
  const bridge = new PdfRsBridge();
  try {
    const opened = await bridge.open(mixedPdf);
    assert.equal(opened.pageCount, 1);
    const surface = await bridge.render(opened.documentId, 0, 128);
    assert.equal(surface.width, 128);
    assert.equal(surface.height, 128);
    assert.equal(surface.stride, 512);
    assert.equal(surface.pixels.byteLength, 128 * 128 * 4);
    assert.equal(
      Array.from(surface.pixels).some(
        (value, index) => index % 4 !== 3 && value !== 255,
      ),
      true,
    );
    await bridge.close(opened.documentId);
  } finally {
    await bridge.shutdown();
  }
});

test("bridge returns structured errors for unsupported ownership", async () => {
  const bridge = new PdfRsBridge();
  try {
    await assert.rejects(
      bridge.render(999, 0, 128),
      (error) => error?.code === "unknown-document",
    );
  } finally {
    await bridge.shutdown();
  }
});
