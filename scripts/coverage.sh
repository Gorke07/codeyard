#!/usr/bin/env bash
# Code coverage (cargo-llvm-cov): lcov for the workspace plus a per-crate summary.
#   scripts/coverage.sh                    # coverage/lcov.info, coverage/summary.json, crate table
#   COVERAGE_CLEAN=1 scripts/coverage.sh   # also delete target/llvm-cov-target afterwards (disk)
# Needs: cargo install cargo-llvm-cov --locked && rustup component add llvm-tools
# Excluded: vendor/ (patched GPUI), test files, testdata and embedded grammars.
# Debug info is off: coverage does not need it and the build directory stays smaller. The app
# tests take ~75 s; the instrumented build is much larger than a normal one.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo-llvm-cov >/dev/null || { echo "coverage: cargo-llvm-cov missing (cargo install cargo-llvm-cov --locked)"; exit 2; }
out=coverage
mkdir -p "$out"
ignore='(^|/)(vendor|target)/|/\.rustup/|/tests?\.rs$|_tests?\.rs$|/tests/|/testdata/|/grammars/|/test_[a-z_]+\.rs$'
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
cargo llvm-cov clean --workspace
# qa_debug_real_cargo_project_without_config debugs a real cargo project; with
# CARGO_PROFILE_DEV_DEBUG=0 it cannot find a breakpoint, so only this test is skipped here
# (plain `cargo test` runs it). A failing test does not block the report (--no-fail-fast);
# the exit code is kept until the report is written.
rc=0
cargo llvm-cov --workspace --no-report --no-fail-fast -- --skip qa_debug_real_cargo_project_without_config || rc=$?
cargo llvm-cov report --ignore-filename-regex "$ignore" --lcov --output-path "$out/lcov.info"
cargo llvm-cov report --ignore-filename-regex "$ignore" --json --summary-only --output-path "$out/summary.json"
python3 -I - "$out/summary.json" <<'PY'
import json, re, sys, collections
data = json.load(open(sys.argv[1]))["data"][0]
agg = collections.defaultdict(lambda: collections.defaultdict(lambda: [0, 0]))
for f in data["files"]:
    m = re.search(r"/crates/([^/]+)/", f["filename"])
    crate = m.group(1) if m else "(other)"
    for k in ("lines", "functions", "regions"):
        agg[crate][k][0] += f["summary"][k]["covered"]
        agg[crate][k][1] += f["summary"][k]["count"]
tot = {k: [0, 0] for k in ("lines", "functions", "regions")}
print(f"{'crate':<10} {'lines':>8} {'functions':>10} {'regions':>9}")
def row(name, d):
    cells = [f"{100*d[k][0]/d[k][1]:.1f}%" if d[k][1] else "-" for k in ("lines", "functions", "regions")]
    print(f"{name:<10} {cells[0]:>8} {cells[1]:>10} {cells[2]:>9}")
for c in sorted(agg):
    row(c, agg[c])
    for k in tot:
        tot[k][0] += agg[c][k][0]; tot[k][1] += agg[c][k][1]
row("total", tot)
PY
if [ "${COVERAGE_CLEAN:-0}" = 1 ]; then rm -rf target/llvm-cov-target; echo "coverage: removed target/llvm-cov-target"; fi
[ "$rc" = 0 ] || echo "coverage: some tests FAILED (exit $rc); the figures include them as failed runs"
exit "$rc"
