#!/usr/bin/env bash

set -euo pipefail

if (($# != 1)); then
    echo "usage: $0 <new-output-directory>" >&2
    exit 2
fi

output_directory="$1"
if [[ -e "$output_directory" ]]; then
    echo "refusing to populate an existing output directory: $output_directory" >&2
    exit 2
fi

upstream="https://pdfium.googlesource.com/pdfium_tests"
revision="a0cdeeeac46f1b2272094ee498cd59a30ce1c073"

git init --quiet "$output_directory"
git -C "$output_directory" remote add origin "$upstream"
git -C "$output_directory" sparse-checkout init --no-cone
git -C "$output_directory" sparse-checkout set --no-cone \
    LICENSE \
    pdfium/bug_493126_endobj_bug_weirdWS.pdf \
    pdfium/bug_493126_endobj_bug_weirdWS_expected.pdf.0.png \
    pdfium/bug_880920.pdf \
    pdfium/bug_880920_expected.pdf.0.png \
    pdfium/bug_883026.pdf \
    pdfium/bug_883026_expected.pdf.0.png \
    pdfium/bug_583804.pdf \
    pdfium/bug_583804_expected.pdf.0.png
git -C "$output_directory" fetch --depth 1 origin "$revision"
git -C "$output_directory" checkout --detach --quiet FETCH_HEAD

actual_revision="$(git -C "$output_directory" rev-parse HEAD)"
if [[ "$actual_revision" != "$revision" ]]; then
    echo "fetched unexpected pdfium_tests revision: $actual_revision" >&2
    exit 1
fi

for required in \
    LICENSE \
    pdfium/bug_493126_endobj_bug_weirdWS.pdf \
    pdfium/bug_493126_endobj_bug_weirdWS_expected.pdf.0.png \
    pdfium/bug_880920.pdf \
    pdfium/bug_880920_expected.pdf.0.png \
    pdfium/bug_883026.pdf \
    pdfium/bug_883026_expected.pdf.0.png \
    pdfium/bug_583804.pdf \
    pdfium/bug_583804_expected.pdf.0.png; do
    if [[ ! -f "$output_directory/$required" ]]; then
        echo "missing required PDFium corpus object: $required" >&2
        exit 1
    fi
done
