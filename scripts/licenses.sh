#!/usr/bin/env bash
# Licence texts that every release artifact must ship, and the helpers that copy them.
#   scripts/licenses.sh                 fail if a listed file is missing
#   scripts/licenses.sh copy <dir>      check, then copy them into <dir> (created if needed)
# The bundle scripts (macOS, Windows) call `copy`; installer/windows/codeyard.iss lists the same files.
set -euo pipefail
here=$PWD
cd "$(dirname "$0")/.."

# source path -> name inside the licences folder (directories keep their contents)
FILES=(
  "LICENSE-MIT LICENSE-MIT"
  "LICENSE-APACHE LICENSE-APACHE"
  "THIRD_PARTY_NOTICES.md THIRD_PARTY_NOTICES.md"
  "THIRD_PARTY_CRATES.md THIRD_PARTY_CRATES.md"
  "crates/app/assets/icons/LICENSE icons-LICENSE"
  "crates/syntax/grammars/dockerfile/LICENSE dockerfile-grammar-LICENSE"
  "vendor/gpui/LICENSE-APACHE gpui-LICENSE-APACHE"
  "vendor/xattr/LICENSE-MIT xattr-LICENSE-MIT"
  "vendor/xattr/LICENSE-APACHE xattr-LICENSE-APACHE"
  "crates/spell/dicts/en/license spell-dict-en-license"
  "crates/syntax/LICENSES syntax-LICENSES"
)

missing=0
for entry in "${FILES[@]}"; do
  src=${entry%% *}
  if [ ! -e "$src" ]; then echo "licenses: missing $src" >&2; missing=1; fi
done
[ -d crates/syntax/LICENSES ] && [ -z "$(ls -A crates/syntax/LICENSES)" ] && { echo "licenses: crates/syntax/LICENSES is empty" >&2; missing=1; }
# The Windows script and the installer keep their own copy of the list; each source must appear in both.
for entry in "${FILES[@]}"; do
  src=${entry%% *}; win=${src//\//\\}
  [ "${src%%/*}" = "$src" ] && win=$src
  for f in scripts/bundle-windows.ps1 installer/windows/codeyard.iss; do
    grep -qF -- "$win" "$f" || { echo "licenses: $f does not list $src" >&2; missing=1; }
  done
done
[ "$missing" = 0 ] || exit 1

case "${1:-}" in
  "") echo "licenses: ok" ;;
  copy)
    dest=${2:?usage: licenses.sh copy <dir>}
    case "$dest" in /*) ;; *) dest=$here/$dest ;; esac
    mkdir -p "$dest"
    for entry in "${FILES[@]}"; do
      src=${entry%% *} name=${entry#* }
      rm -rf "${dest:?}/$name"
      cp -R "$src" "$dest/$name"
    done
    ;;
  *) echo "usage: licenses.sh [copy <dir>]" >&2; exit 2 ;;
esac
