#!/usr/bin/env bash
# RustSec scan of Cargo.lock. Accepted exceptions live in .cargo/audit.toml, each with a reason.
#   scripts/audit.sh
# A vulnerability, a yanked crate or a new unmaintained/unsound warning exits 1.
# Needs: cargo install cargo-audit --locked
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo-audit >/dev/null || { echo "audit: cargo-audit missing (cargo install cargo-audit --locked)"; exit 2; }
exec cargo audit --deny warnings "$@"
