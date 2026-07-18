import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
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

test("bridge cancels active Rust rendering without losing the document", async () => {
  const bridge = new PdfRsBridge();
  try {
    const opened = await bridge.open(readablePdf);
    const controller = new AbortController();
    const started = Date.now();
    const rendering = bridge.render(opened.documentId, 0, 480, {
      signal: controller.signal,
    });
    controller.abort();
    await assert.rejects(
      rendering,
      (error) => error?.code === "cancelled",
    );
    assert.ok(
      Date.now() - started < 2_000,
      "cooperative cancellation must not wait for a complete Reference render",
    );

    const surface = await bridge.render(opened.documentId, 1, 128);
    assert.equal(surface.page, 1);
    assert.equal(surface.width, 128);
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

test("bridge preserves open-time capability and resource categories", async () => {
  const directory = await mkdtemp(join(tmpdir(), "pdf-rs-electron-open-errors-"));
  const xrefStream = join(directory, "xref-stream.pdf");
  const incremental = join(directory, "incremental.pdf");
  const resourceLimited = join(directory, "resource-limit.pdf");
  await writeFile(xrefStream, xrefStreamPdf());
  await writeFile(incremental, traditionalPdf(0, 9));
  await writeFile(resourceLimited, traditionalPdf(20_000));

  const bridge = new PdfRsBridge();
  try {
    await assert.rejects(
      bridge.open(xrefStream),
      (error) => error?.code === "unsupported",
    );
    await assert.rejects(
      bridge.open(incremental),
      (error) => error?.code === "unsupported",
    );
    await assert.rejects(
      bridge.open(resourceLimited),
      (error) => error?.code === "resource-limit",
    );
  } finally {
    await bridge.shutdown();
    await rm(directory, { recursive: true, force: true });
  }
});

function traditionalPdf(boundaryPadding = 0, previous) {
  const parts = [];
  const offsets = [];
  let length = 0;
  const append = (value) => {
    const bytes = Buffer.isBuffer(value) ? value : Buffer.from(value, "ascii");
    parts.push(bytes);
    length += bytes.byteLength;
  };
  const object = (number, body) => {
    offsets.push(length);
    append(`${number} 0 obj\n${body}\nendobj\n`);
  };

  append(Buffer.from("%PDF-1.7\n%\x80\x81\x82\x83\n", "latin1"));
  object(1, "<< /Type /Catalog /Pages 2 0 R >>");
  object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
  object(
    3,
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources <<>> /Contents 4 0 R >>",
  );
  offsets.push(length);
  append("4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\n");
  append(Buffer.alloc(boundaryPadding, 0x20));
  append("endobj\n");

  const xrefOffset = length;
  append("xref\n0 5\n0000000000 65535 f \n");
  for (const offset of offsets) {
    append(`${String(offset).padStart(10, "0")} 00000 n \n`);
  }
  append(`trailer\n<< /Size 5 /Root 1 0 R`);
  if (previous !== undefined) {
    append(` /Prev ${previous}`);
  }
  append(` >>\nstartxref\n${xrefOffset}\n%%EOF\n`);
  return Buffer.concat(parts);
}

function xrefStreamPdf() {
  const header = Buffer.from("%PDF-1.7\n%\x80\x81\x82\x83\n", "latin1");
  const body = Buffer.from(
    `4 0 obj\n<< /Type /XRef /Size 5 /Root 1 0 R /W [1 2 1] /Length 0 >>\n`
      + `stream\n\nendstream\nendobj\nstartxref\n${header.byteLength}\n%%EOF\n`,
    "ascii",
  );
  return Buffer.concat([header, body]);
}
