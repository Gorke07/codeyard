#!/usr/bin/env bash
# Architecture budget: files and the `Workbench` struct must not grow (limits only go down).
#   scripts/arch-check.sh
# Exit 1 when a limit is exceeded. For new state, add a field group in `workbench_state.rs`
# (see docs/architecture.md); split the file instead of raising the limit.
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0
check() { # file limit
  local n; n=$(wc -l < "$1")
  if [ "$n" -gt "$2" ]; then echo "FAIL $1: $n lines (limit $2)"; fail=1; else echo "ok   $1: $n / $2"; fi
}
# Known large files: limits sit just above today's size; lower them as files get split.
check crates/app/src/workbench.rs 2750
check crates/app/src/editor.rs 1800
check crates/buffer/src/lib.rs 1950
# Every other source file (vendor and generated code excluded).
while IFS= read -r f; do
  case "$f" in
    */workbench.rs|*/editor.rs|crates/buffer/src/lib.rs) continue ;;
    */tests.rs|*/qa_*_tests.rs) check "$f" 6000 ;;
    *) check "$f" 2500 ;;
  esac
done < <(git ls-files 'crates/*.rs' | grep -v -e '/grammars/' -e '/testdata/')
# Length of the `Workbench` struct (one line per field plus doc lines).
n=$(awk '/^pub struct Workbench/{f=1} f{c++} f&&/^}/{print c; exit}' crates/app/src/workbench.rs)
if [ "$n" -gt 80 ]; then echo "FAIL Workbench struct $n lines (limit 80): put the new field in a state group"; fail=1; else echo "ok   Workbench struct: $n / 80"; fi
# vendor/ patches still in place (vendor/gpui/PATCHES.md).
scripts/check-gpui-patches.sh || fail=1
exit $fail
