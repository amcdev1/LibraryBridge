#!/usr/bin/env bash
#
# Builds the locked, release-mode binaries for LibraryBridge's distributable
# assets. Run from the repository root. Produces:
#   target/release/librarybridge       the CLI
#   target/release/librarybridge-gui   the desktop window
#
# Uses the Rust toolchain pinned in rust-toolchain.toml and the locked
# dependency versions from Cargo.lock. Refuses to build if the lockfile and
# manifest disagree ("--locked").

set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --locked --release -p librarybridge -p librarybridge-gui

echo "Built:"
ls -lh target/release/librarybridge target/release/librarybridge-gui