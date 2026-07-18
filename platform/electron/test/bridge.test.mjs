import assert from "node:assert/strict";
import { resolve } from "node:path";
import test from "node:test";

import {
  FAST_CPU_CANARY_COHORT,
  PdfRsBridge,
  PdfRsBridgeError,
} from "../src/bridge.mjs";

const readablePdf = resolve(
  import.meta.dirname,
  "../../../tests/desktop/readable-preview.pdf",
);
const unsupportedPdf = resolve(
  import.meta.dirname,
  "../../../tests/cases/raster/m3-reference/producer-unsupported-interpolated-image/input.pdf",
);
const invalidPdf = resolve(
  import.meta.dirname,
  "../../../tests/cases/raster/m3-reference/strict-invalid-xref/input.pdf",
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
    await assert.rejects(
      bridge.open(invalidPdf),
      (error) => error?.code === "document",
    );
    const opened = await bridge.open(unsupportedPdf);
    await assert.rejects(
      bridge.render(opened.documentId, 0, 128),
      (error) => error?.code === "unsupported",
    );
    await bridge.close(opened.documentId);
  } finally {
    await bridge.shutdown();
  }
});

test("Fast CPU CANARY rolls back to Reference without changing unsupported", async () => {
  assert.throws(
    () => new PdfRsBridge({ rendererCohort: "unregistered-cohort" }),
    (error) =>
      error instanceof PdfRsBridgeError
      && error.code === "invalid-renderer-cohort",
  );

  const canary = new PdfRsBridge({ rendererCohort: FAST_CPU_CANARY_COHORT });
  try {
    const opened = await canary.open(readablePdf);
    const surface = await canary.render(opened.documentId, 0, 128);
    assert.equal(surface.renderer, "fast-cpu-v1");
    await canary.close(opened.documentId);

    const unsupported = await canary.open(unsupportedPdf);
    await assert.rejects(
      canary.render(unsupported.documentId, 0, 128),
      (error) => error?.code === "unsupported",
    );
    await canary.close(unsupported.documentId);
  } finally {
    await canary.shutdown();
  }

  const rolledBack = new PdfRsBridge();
  try {
    const opened = await rolledBack.open(readablePdf);
    const surface = await rolledBack.render(opened.documentId, 0, 128);
    assert.equal(surface.renderer, "reference-cpu-v1");
    await rolledBack.close(opened.documentId);

    const unsupported = await rolledBack.open(unsupportedPdf);
    await assert.rejects(
      rolledBack.render(unsupported.documentId, 0, 128),
      (error) => error?.code === "unsupported",
    );
    await rolledBack.close(unsupported.documentId);
  } finally {
    await rolledBack.shutdown();
  }
});
