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

if [[ "$lane" == "pr" ]]; then
    cargo doc --workspace --no-deps
fi
