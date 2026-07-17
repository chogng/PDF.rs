#!/usr/bin/env bash

set -euo pipefail

usage() {
    echo "usage: $0 <local|pr>" >&2
}

lane="${1:-local}"

case "$lane" in
    local | pr) ;;
    *)
        usage
        exit 2
        ;;
esac

if (($# > 1)); then
    usage
    exit 2
fi

cargo run --quiet --package pdf-rs-quality -- "$lane"

cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo run --quiet --package pdf-rs-generate -- \
    tests/cases/infrastructure/synthetic-failure-bundle-001/source.dsl \
    tests/cases/infrastructure/synthetic-failure-bundle-001/input.pdf
cargo run --quiet --package pdf-rs-corpus -- \
    validate tests/corpus/manifests/t0-bootstrap-v1.toml .
cargo run --quiet --package pdf-rs-benchmark -- \
    validate tests/performance/m0-synthetic-benchmark-replay-v1.toml \
    tests/corpus/manifests/t0-bootstrap-v1.toml
cargo run --quiet --package pdf-rs-quality -- validate-cases tests/cases
cargo test --locked --package pdf-rs-quality --test m3_raster_oracle_contract
cargo test --locked --package pdf-rs-quality --test m3_content_graphics_trace
cargo test --locked --package pdf-rs-quality --test m3_reference_geometry_trace
cargo test --locked --package pdf-rs-quality --test m3_reference_color_trace
cargo test --locked --package pdf-rs-quality --test m3_basic_image_trace
cargo test --locked --package pdf-rs-quality --test m3_basic_text_trace

m2_scene_gate_root="target/ci-artifacts/m2-scene-gate"
if [[ "$m2_scene_gate_root" != "target/ci-artifacts/m2-scene-gate" ]]; then
    echo "refusing to clean unexpected M2 Scene gate root: $m2_scene_gate_root" >&2
    exit 1
fi
if [[ -L "target" || -L "target/ci-artifacts" || -L "$m2_scene_gate_root" ]]; then
    echo "refusing to clean M2 Scene gate root through a symbolic link" >&2
    exit 1
fi
rm -rf -- "$m2_scene_gate_root"
mkdir -p -- \
    "$m2_scene_gate_root/debug-1" \
    "$m2_scene_gate_root/debug-2" \
    "$m2_scene_gate_root/release-1" \
    "$m2_scene_gate_root/release-2"

PDF_RS_M2_SCENE_GATE_OUTPUT="$m2_scene_gate_root/debug-1" \
    cargo test --locked --package pdf-rs-quality --test m2_scene_gate
PDF_RS_M2_SCENE_GATE_OUTPUT="$m2_scene_gate_root/debug-2" \
    cargo test --locked --package pdf-rs-quality --test m2_scene_gate
PDF_RS_M2_SCENE_GATE_OUTPUT="$m2_scene_gate_root/release-1" \
    cargo test --locked --release --package pdf-rs-quality --test m2_scene_gate
PDF_RS_M2_SCENE_GATE_OUTPUT="$m2_scene_gate_root/release-2" \
    cargo test --locked --release --package pdf-rs-quality --test m2_scene_gate

diff --recursive --brief \
    "$m2_scene_gate_root/debug-1" \
    "$m2_scene_gate_root/debug-2"
diff --recursive --brief \
    "$m2_scene_gate_root/release-1" \
    "$m2_scene_gate_root/release-2"
diff --recursive --brief \
    "$m2_scene_gate_root/debug-1" \
    "$m2_scene_gate_root/release-1"

cargo test --locked -p pdf-rs-quality --test m2_exit

cargo test --locked --package pdf-rs-quality --test m3_reference_oracle_model

m3_reference_gate_root="target/ci-artifacts/m3-reference-gate"
if [[ "$m3_reference_gate_root" != "target/ci-artifacts/m3-reference-gate" ]]; then
    echo "refusing to clean unexpected M3 Reference gate root: $m3_reference_gate_root" >&2
    exit 1
fi
if [[ -L "target" || -L "target/ci-artifacts" || -L "$m3_reference_gate_root" ]]; then
    echo "refusing to clean M3 Reference gate root through a symbolic link" >&2
    exit 1
fi
rm -rf -- "$m3_reference_gate_root"
mkdir -p -- \
    "$m3_reference_gate_root/debug-1" \
    "$m3_reference_gate_root/debug-2" \
    "$m3_reference_gate_root/release-1" \
    "$m3_reference_gate_root/release-2"

PDF_RS_M3_REFERENCE_GATE_OUTPUT="$m3_reference_gate_root/debug-1" \
    cargo test --locked --package pdf-rs-quality --test m3_reference_gate
PDF_RS_M3_REFERENCE_GATE_OUTPUT="$m3_reference_gate_root/debug-2" \
    cargo test --locked --package pdf-rs-quality --test m3_reference_gate
PDF_RS_M3_REFERENCE_GATE_OUTPUT="$m3_reference_gate_root/release-1" \
    cargo test --locked --release --package pdf-rs-quality --test m3_reference_gate
PDF_RS_M3_REFERENCE_GATE_OUTPUT="$m3_reference_gate_root/release-2" \
    cargo test --locked --release --package pdf-rs-quality --test m3_reference_gate

diff --recursive --brief \
    "$m3_reference_gate_root/debug-1" \
    "$m3_reference_gate_root/debug-2"
diff --recursive --brief \
    "$m3_reference_gate_root/release-1" \
    "$m3_reference_gate_root/release-2"
diff --recursive --brief \
    "$m3_reference_gate_root/debug-1" \
    "$m3_reference_gate_root/release-1"

cargo test --locked --package pdf-rs-quality --test m3_reference_raster_trace

cargo run --quiet --package pdf-rs-quality -- \
    validate-m1-maturity docs/traceability/capability-profiles.toml
cargo run --quiet --package pdf-rs-quality -- check-product-purity .

product_build_root="$(mktemp -d "${TMPDIR:-/tmp}/pdf-rs-product-build.XXXXXX")"
product_target="$product_build_root/target"
product_proof_id="${product_build_root##*/}"
cleanup_product_build() {
    rm -rf -- "$product_build_root"
}
trap cleanup_product_build EXIT

cargo run --quiet --package pdf-rs-quality -- \
    prepare-product-build-proof . "$product_target" "$product_proof_id"
CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$product_target" cargo build \
    --locked \
    --release \
    --lib \
    --package pdf-rs-bytes \
    --package pdf-rs-filters \
    --package pdf-rs-syntax \
    --package pdf-rs-content \
    --package pdf-rs-xref \
    --package pdf-rs-object \
    --package pdf-rs-document \
    --package pdf-rs-font \
    --package pdf-rs-raster \
    --package pdf-rs-scene \
    --package pdf-rs-cache \
    --package pdf-rs-session
cargo run --quiet --package pdf-rs-quality -- \
    check-product-build-closure . "$product_target" "$product_proof_id"

cleanup_product_build
trap - EXIT

cargo run --quiet --package pdf-rs-quality -- \
    synthetic-bundle \
    tests/cases/infrastructure/synthetic-failure-bundle-001/case.toml \
    target/ci-artifacts/m0-failure-bundles

if [[ "$lane" == "pr" ]]; then
    RUSTDOCFLAGS="-D warnings -D missing_docs" cargo doc --workspace --no-deps
fi
