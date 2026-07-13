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
cargo run --quiet --package pdf-rs-quality -- validate-cases tests/cases
cargo run --quiet --package pdf-rs-quality -- check-product-purity .
cargo run --quiet --package pdf-rs-quality -- \
    synthetic-bundle \
    tests/cases/infrastructure/synthetic-failure-bundle-001/case.toml \
    target/ci-artifacts/m0-failure-bundles

if [[ "$lane" == "pr" ]]; then
    RUSTDOCFLAGS="-D warnings -D missing_docs" cargo doc --workspace --no-deps
fi
