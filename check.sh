#!/usr/bin/env bash
# This scripts runs various CI-like checks in a convenient way.
set -eux

cargo check --quiet --workspace --all-targets
cargo check --quiet --workspace --all-features --lib --target wasm32-unknown-unknown
cargo fmt --all -- --check
cargo clippy --quiet --workspace --all-targets --all-features --  -D warnings -W clippy::all
cargo test --quiet --workspace --all-targets --all-features
# `--all-features` enables the `nsi-out-of-domain-trims` reproducer, which
# disables the trim-domain fix and ignores its tests; run them explicitly.
cargo test --quiet --workspace --all-targets --features nsi-render,nsi-export
cargo test --quiet --workspace --doc
trunk build
