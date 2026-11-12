#!/usr/bin/env bash
# The performance gate (blueprint section 16 and 21, Gate B). Builds the release shell, measures
# every budget and exits with status 1 if any of them is missed. Everything is measured and
# printed before it exits, so one run shows all that is wrong.
#
#   start to first prompt, idle memory, keystroke to redraw (the real shell on a pty)  omsh-perf
#   `--version` under 10 ms, `-c true` within 5 ms of `/bin/sh -c true`                hyperfine
#   keystroke: highlight + hint under 5 ms p99                                          omsh-editor bench
#   completion menu under 50 ms                                                         omsh-editor bench
#   history explorer (Ctrl+R): a key under 5 ms p99, opening under 20 ms                omsh-ui bench
#   history lookups on 100,000 rows under 20 ms                                         omsh-history bench
#   lexer and parser (a report, no budget)                                              omsh-syntax bench
#
# Needs `hyperfine` (set HYPERFINE=/path to use another), `python3`, `git`, `setsid` and `/proc`.
# When GITHUB_STEP_SUMMARY is set, the results are added to the job summary.

set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

HYPERFINE="${HYPERFINE:-hyperfine}"
BIN=target/release/oh-my-shell
failed=0

command -v "$HYPERFINE" > /dev/null || { echo "perf-gate: hyperfine is not installed" >&2; exit 2; }
command -v python3 > /dev/null || { echo "perf-gate: python3 is not installed" >&2; exit 2; }

out="$(mktemp -d)" || exit 2
[ -n "$out" ] && [ -d "$out" ] || exit 2
trap 'cd / && rm -rf -- "$out"' EXIT

summary() {
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
        cat >> "$GITHUB_STEP_SUMMARY"
    fi
}

# Runs a command, shows its output, adds the output to the job summary and notes a failure.
step() {
    local title="$1"
    shift
    echo
    echo "=== $title"
    "$@" 2>&1 | tee "$out/step.txt"
    local status="${PIPESTATUS[0]}"
    {
        echo "### $title"
        echo '```'
        cat "$out/step.txt"
        echo '```'
    } | summary
    if [ "$status" -ne 0 ]; then
        echo "perf-gate: $title: exit status $status" >&2
        failed=1
    fi
}

echo "=== build (release)"
cargo build --release --locked -p omsh-cli -p omsh-tests || { echo "perf-gate: the build failed" >&2; exit 2; }

echo
echo "=== hyperfine"
"$HYPERFINE" -N --warmup 20 --runs 300 --export-json "$out/hyperfine.json" \
    "$BIN --version" "$BIN -c true" "/bin/sh -c true" || { echo "perf-gate: hyperfine failed" >&2; exit 2; }

hyperfine_check() {
    python3 -I - "$out/hyperfine.json" <<'PY'
import json
import sys

results = json.load(open(sys.argv[1]))["results"]


def median_ms(result):
    # hyperfine 1.x has "median" in seconds; 2.x nests it under "summary".
    if "median" in result:
        return result["median"] * 1000.0
    return result["summary"]["time_wall_clock"]["median"] * 1000.0


version = median_ms(results[0])
c_true = median_ms(results[1])
dash = median_ms(results[2])
overhead = c_true - dash
print(f"oh-my-shell --version        median {version:7.2f} ms   budget 10 ms")
print(f"oh-my-shell -c true          median {c_true:7.2f} ms")
print(f"/bin/sh -c true              median {dash:7.2f} ms")
print(f"-c true overhead vs sh       {overhead:7.2f} ms   budget 5 ms")
bad = []
if version >= 10.0:
    bad.append("--version")
if overhead >= 5.0:
    bad.append("-c true overhead")
if bad:
    print("OVER BUDGET: " + ", ".join(bad))
    sys.exit(1)
PY
}
step "--version and -c true (hyperfine)" hyperfine_check

step "start, memory and keystroke on a terminal (omsh-perf)" target/release/omsh-perf --bin "$BIN"
step "keystroke: highlight and hint" cargo bench --locked -p omsh-editor --bench keystroke
step "completion menu" cargo bench --locked -p omsh-editor --bench completion
step "history explorer (Ctrl+R)" cargo bench --locked -p omsh-ui --bench explorer
step "history lookups on 100,000 rows" cargo bench --locked -p omsh-history --bench query
step "lexer and parser (report only)" cargo bench --locked -p omsh-syntax --bench parse

echo
if [ "$failed" -ne 0 ]; then
    echo "perf-gate: FAILED, a budget was missed or a step did not run (see above)" >&2
    exit 1
fi
echo "perf-gate: every budget is met"
