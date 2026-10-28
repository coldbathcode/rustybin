#!/usr/bin/env bash
# The harness conformance run: start the compose cluster of
# docs/examples/harness/cluster (a leader, a standby, two client nodes, two
# server nodes, a stand-in gateway), let the leader start the scenario once
# the quorum is there, kill the leader mid-run, and assert the report the
# new leader (the standby) produces: the clients never stopped, nothing was
# lost, the failover is in the history, the thresholds pass.
#
#   conformance/harness/run.sh
#
# Environment:
#   RUSTYBIN_IMAGE   an image built elsewhere (CI: rustybin:ci) instead of
#                    `docker compose build`
#   KILL_AFTER       seconds into the run before the leader is killed (10)
#   KEEP=1           leave the cluster running afterwards
#   HARNESS_*_PORT   the published ports (compose.yaml; 18900 to 18906)
# Needs: docker with compose v2, curl, python3.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
COMPOSE="$ROOT/docs/examples/harness/cluster/compose.yaml"
LEADER="http://127.0.0.1:${HARNESS_LEADER_PORT:-18900}"
STANDBY="http://127.0.0.1:${HARNESS_STANDBY_PORT:-18901}"
CLUSTER=perf-1
KILL_AFTER="${KILL_AFTER:-10}"
export HARNESS_JOIN_TOKEN="${HARNESS_JOIN_TOKEN:-conformance-join-token-$(date +%s)}"
export RUSTYBIN_LOG_LEVEL="${RUSTYBIN_LOG_LEVEL:-info}"

compose() { docker compose -f "$COMPOSE" "$@"; }
fail() { echo "FAIL: $*" >&2; exit 1; }
# JSON from a URL through python; `$1` is the URL, `$2` a python expression
# over `d`, printed. Empty on a connection error or a non-JSON answer.
jget() {
  curl -fsS --max-time 5 "$1" 2>/dev/null | python3 -c "
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    sys.exit(0)
v = $2
print('' if v is None else (str(v).lower() if isinstance(v, bool) else v))
" 2>/dev/null || true
}
# Poll until `jget url expr` prints `want`, at most `secs` seconds.
wait_for() {
  local what="$1" url="$2" expr="$3" want="$4" secs="$5" got=""
  for _ in $(seq 1 $((secs * 2))); do
    got="$(jget "$url" "$expr")"
    [[ "$got" == "$want" ]] && return 0
    sleep 0.5
  done
  echo "timed out waiting for $what (last: '$got')" >&2
  return 1
}

cleanup() {
  local rc=$?
  if [[ $rc -ne 0 ]]; then
    echo "--- compose logs (last 150 lines) ---" >&2
    compose logs --no-color --tail 150 >&2 || true
  fi
  if [[ "${KEEP:-0}" != "1" ]]; then
    compose down -v --remove-orphans >/dev/null 2>&1 || true
  fi
  exit $rc
}
trap cleanup EXIT

started=$(date +%s)
if [[ -z "${RUSTYBIN_IMAGE:-}" ]]; then
  echo "### building the image"
  compose build --quiet leader
fi
echo "### starting the cluster"
compose up -d --remove-orphans

echo "### waiting for the quorum (2 clients, 2 servers)"
wait_for "the cluster" "$LEADER/_rustybin/harness/clusters/$CLUSTER" \
  "d['alive']['clients'] == 2 and d['alive']['servers'] == 2" true 120 \
  || fail "the nodes did not join"
wait_for "three eligible nodes" "$LEADER/_rustybin/harness/clusters/$CLUSTER/leader" \
  "str(d['failover']).startswith('available')" true 30 \
  || fail "failover is not available: $(jget "$LEADER/_rustybin/harness/clusters/$CLUSTER/leader" "d['failover']")"
echo "cluster up after $(( $(date +%s) - started )) s; failover available"

echo "### waiting for the headless run to start"
wait_for "the run" "$LEADER/_rustybin/harness/clusters/$CLUSTER/runs" \
  "d['active'] is not None" true 60 || fail "the leader did not start the scenario"
RUN="$(jget "$LEADER/_rustybin/harness/clusters/$CLUSTER/runs" "d['active']")"
wait_for "the run to be running" "$LEADER/_rustybin/harness/clusters/$CLUSTER/runs/$RUN" \
  "d['state'] == 'running'" true 30 || fail "run $RUN did not reach running"
echo "run $RUN is running on the leader"

wait_for "$KILL_AFTER s of the run" "$LEADER/_rustybin/harness/clusters/$CLUSTER/runs/$RUN" \
  "d['elapsed_secs'] >= $KILL_AFTER" true $((KILL_AFTER + 20)) || fail "the run did not reach $KILL_AFTER s"
before="$(jget "$LEADER/_rustybin/harness/clusters/$CLUSTER/runs/$RUN" "d['sent']")"
echo "### killing the leader at $(jget "$LEADER/_rustybin/harness/clusters/$CLUSTER/runs/$RUN" "round(d['elapsed_secs'], 1)") s ($before sent)"
compose kill leader
killed=$(date +%s.%N)

wait_for "the standby to lead" "$STANDBY/_rustybin/harness/clusters/$CLUSTER/leader" \
  "d['leader']['node_id'] == 'standby' and d['term'] > 1" true 30 \
  || fail "the standby did not take over"
echo "the standby leads term $(jget "$STANDBY/_rustybin/harness/clusters/$CLUSTER/leader" "d['term']") after $(python3 -c "print(round($(date +%s.%N) - $killed, 1))") s"

echo "### waiting for the report from the standby"
for _ in $(seq 1 240); do
  code="$(curl -s -o "$HERE/.report.json" -w '%{http_code}' "$STANDBY/_rustybin/harness/clusters/$CLUSTER/runs/$RUN/report" || true)"
  [[ "$code" == "200" ]] && break
  sleep 0.5
done
[[ "$code" == "200" ]] || fail "no report from the standby (last status $code)"

echo "### asserting the report"
python3 - "$HERE/.report.json" "$before" <<'EOF'
import json, sys
r = json.load(open(sys.argv[1]))
before = int(sys.argv[2])
c = r["cluster"]
t = r["totals"]
problems = []
def check(cond, msg):
    if not cond:
        problems.append(msg)
check(r["state"] == "finished", f"state {r['state']}")
check(r["passed"] is True, f"thresholds: {[x for x in r['thresholds'] if not x['pass']]}")
check(c["leader"] == "standby" and c["term"] > 1, f"led by {c['leader']} term {c['term']}")
check(len(c["history"]) == 2 and c["history"][1]["reason"] == "elected" and c["history"][1]["node_id"] == "standby",
      f"history {c['history']}")
# A clean failover is one election; a split vote shows as a higher term.
if c["term"] != 2:
    print(f"note: the standby won term {c['term']}, so an election before it did not get a majority")
check(c["unreported"] == 0, f"unreported {c['unreported']}")
check(c["lost_check"] == "complete", f"lost_check {c['lost_check']}")
check(t["lost"] == 0, f"lost {t['lost']}")
check(t["not_sent"] == 0, f"not_sent {t['not_sent']}")
clients = [n for n in c["nodes"] if "client" in n["roles"]]
servers = [n for n in c["nodes"] if "server" in n["roles"]]
check(sorted(n["node_id"] for n in clients) == ["client-1", "client-2"], f"clients {[n['node_id'] for n in clients]}")
check(sorted(n["node_id"] for n in servers) == ["server-1", "server-2"], f"servers {[n['node_id'] for n in servers]}")
check(all(n["state"] == "finished" for n in clients), f"client states {[n['state'] for n in clients]}")
check(all(abs(n["share"] - 0.5) < 1e-9 for n in clients), f"shares {[n['share'] for n in clients]}")
check(all((n.get("arrivals") or 0) > 0 for n in servers), f"server arrivals {[n.get('arrivals') for n in servers]}")
check(t["sent"] > before, f"sent {t['sent']} did not grow past {before} after the failover")
check(t["sent"] >= 6000, f"sent {t['sent']} of about 6730 planned")
check(t["upstream"] >= 0.99 * t["completed"], f"upstream {t['upstream']} of {t['completed']} completed")
check(t["gateway"] == 0, f"gateway-made answers {t['gateway']}: {t['gateway_statuses']}")
check(r["latency"]["p99_ms"] < 1000, f"p99 {r['latency']['p99_ms']} ms")
# The dip while the leader changed: no second of the timeline is empty.
empty = [b["t_secs"] for b in r["timeline"] if b["sent"] == 0 and b["width_secs"] <= 1.0 and b["t_secs"] < r["planned_secs"] - 1]
check(not empty, f"seconds with nothing sent: {empty}")
added = (r.get("server") or {}).get("changes", {}).get("headers_added", {})
check("x-forwarded-for" in {k.lower() for k in added}, f"the gateway's X-Forwarded-For was not seen as added: {added}")
if r["saturation"]["saturated"]:
    print("note: a client flagged saturation (not a threshold here): " + "; ".join(r["saturation"]["reasons"]))
summary = (f"{t['sent']} sent, {t['completed']} completed, {t['ok']} ok, {t['upstream']} upstream, "
           f"{t['gateway']} gateway, {t['transport']} transport, {t['lost']} lost | "
           f"p50 {r['latency']['p50_ms']} ms, p99 {r['latency']['p99_ms']} ms, overhead p99 {r['overhead']['p99_ms']} ms | "
           f"achieved {r['achieved_rate']:.1f}/s of {r['requested_rate']:.1f}/s | "
           f"leader {c['history'][0]['node_id']} -> {c['leader']} (term {c['term']}), unreported {c['unreported']}, lost check {c['lost_check']}")
print(summary)
if problems:
    print("FAIL:\n  " + "\n  ".join(problems))
    sys.exit(1)
print("PASS: the failover left no gap, nothing lost, thresholds pass")
EOF
rm -f "$HERE/.report.json"
echo "done in $(( $(date +%s) - started )) s"
