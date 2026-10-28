#!/usr/bin/env bash
# `rustybin harness ...` (docs/src/harness.md): validate a scenario, run it
# through an in-process Rustybin, render the saved report. Run through
# docs/examples/run.sh, which exports RUSTYBIN_BIN.
set -euo pipefail
BIN="${RUSTYBIN_BIN:-$(command -v rustybin || true)}"
[[ -x "$BIN" ]] || { echo "set RUSTYBIN_BIN to a rustybin binary" >&2; exit 1; }
rustybin() { "$BIN" "$@"; }
expect() { grep -q -- "$2" <<<"$1" || { echo "expected '$2' in: $1" >&2; exit 1; }; }
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"

cat > smoke.yaml <<'EOF'
name: smoke
target: http://127.0.0.1:8080
source:
  request:
    path: /status/200
pattern:
  type: constant
  rate: 20
  for: 2s
timeout: 2s
thresholds:
  error_rate: 0
  latency_p99_ms: 500
  saturated: false
EOF

out=$(
# ANCHOR: validate
rustybin harness validate smoke.yaml
# ANCHOR_END: validate
)
expect "$out" 'smoke: 1 layer(s), 1 target(s), 2 s, peak 20 requests/s, about 40 requests'
expect "$out" 'valid'

out=$(
# ANCHOR: run_local
# Loop the scenario through an in-process Rustybin (the target is replaced by
# it and allowed automatically); the JSON report lands in report.json and the
# exit code is the report's: 0 when the thresholds pass, 1 when they fail.
rustybin harness run smoke.yaml --local-server --threads 1 --quiet --report report.json
# ANCHOR_END: run_local
)
expect "$out" 'Requests: 40 requested, 40 sent'
expect "$out" 'Result: PASS (exit code 0)'
[[ -s report.json ]]

out=$(
# ANCHOR: report
rustybin harness report report.json --format junit
# ANCHOR_END: report
)
expect "$out" '<testsuite name="rustybin harness '
expect "$out" 'failures="0"'
echo "ok"
