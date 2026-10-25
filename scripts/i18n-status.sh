#!/usr/bin/env bash
# Translation coverage per language. `I18N_LIST=1 scripts/i18n-status.sh` also lists the missing strings.
# Fails when a `Complete` language has missing strings (the tests enforce the same).
set -uo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
out=$(cargo test -q -p app --bin codeyard i18n_status_report -- --nocapture 2>&1)
rc=$?
echo "$out" | grep -E "^i18n |^  missing:|panicked|a Complete language"
# The filter above hides everything else: show why the run failed (compile error, test not found).
if [ "$rc" -ne 0 ] && ! echo "$out" | grep -qE "^i18n |panicked"; then echo "$out" | tail -30 >&2; fi
# `cargo test <filter>` exits 0 when the filter matches no test (renamed or deleted): that is not a pass.
if [ "$rc" -eq 0 ] && ! echo "$out" | grep -qE "^i18n "; then
  echo "i18n-status: the i18n_status_report test printed no language lines (renamed, or no non-source language?)" >&2
  exit 1
fi
exit $rc
