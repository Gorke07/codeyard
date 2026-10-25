#!/usr/bin/env bash
# Translation coverage per language. `I18N_LIST=1 scripts/i18n-status.sh` also lists the missing strings.
# Fails when a `Complete` language has missing strings (the tests enforce the same).
set -uo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
out=$(cargo test -q -p app --bin codeyard i18n_status_report -- --nocapture 2>&1)
rc=$?
echo "$out" | grep -E "^i18n |^  missing:|panicked|a Complete language"
exit $rc
