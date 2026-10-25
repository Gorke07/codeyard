#!/usr/bin/env bash
# Cross-compile check from macOS or Linux: does it build for Windows? Produces no package, runs nothing.
#   scripts/check-windows.sh
# Needs: rustup target add x86_64-pc-windows-gnu, cargo-zigbuild, zig.
# Check only, because GPUI's Windows renderer reads the HLSL from the source path baked in at
# compile time (env!("CARGO_MANIFEST_DIR")) in debug and needs fxc.exe (Windows SDK) in release,
# so an .exe built here would not run on another Windows machine. Packaging: scripts/bundle-windows.ps1.
# Catches changes that break the Windows-specific code (cfg(unix) / cfg(windows) branches).
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/windows-check}"
cargo zigbuild -q --target x86_64-pc-windows-gnu -p app --bin codeyard
echo "ok: builds for x86_64-pc-windows-gnu (not run)"
