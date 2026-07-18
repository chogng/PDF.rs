import assert from "node:assert/strict";
import { resolve } from "node:path";
import test from "node:test";

import { PdfRsBridge } from "../src/bridge.mjs";

const readablePdf = resolve(
  import.meta.dirname,
  "../../../tests/desktop/readable-preview.pdf",
);

test("persistent bridge opens and renders a readable two-page PDF", async () => {
  const bridge = new PdfRsBridge();
  try {
    const opened = await bridge.open(readablePdf);
    assert.equal(opened.pageCount, 2);
    for (const page of [0, 1]) {
      const surface = await bridge.render(opened.documentId, page, 306);
      assert.equal(surface.page, page);
      assert.equal(surface.renderer, "reference-cpu-v1");
      assert.equal(surface.width, 306);
      assert.equal(surface.height, 396);
      assert.equal(surface.stride, 1_224);
      assert.equal(surface.pixels.byteLength, 306 * 396 * 4);
      assert.equal(
        Array.from(surface.pixels).some(
          (value, index) => index % 4 !== 3 && value < 80,
        ),
        true,
      );
    }
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
