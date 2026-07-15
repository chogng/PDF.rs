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
    --package pdf-rs-xref \
    --package pdf-rs-object \
    --package pdf-rs-document \
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
