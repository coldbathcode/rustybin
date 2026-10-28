# Harness conformance run

`run.sh` starts the compose cluster of `docs/examples/harness/cluster` (a
leader, a standby, two client nodes, two server nodes and a stand-in gateway
in front of the servers), lets the leader start `gateway-load.yaml` once the
two clients and two servers have joined, kills the leader ten seconds into the
run, and asserts the report the standby produces as the new leader.

## Run

```bash
conformance/harness/run.sh                      # builds the image, about 2 minutes plus the build
RUSTYBIN_IMAGE=rustybin:ci conformance/harness/run.sh   # an image built elsewhere (CI)
KEEP=1 conformance/harness/run.sh               # leave the cluster up to look around
```

Needs docker with compose v2, curl and python3. The published ports are
18900 (leader) to 18906 (gateway); `HARNESS_LEADER_PORT` and the other
`HARNESS_*_PORT` variables of the compose file move them. `KILL_AFTER` is
the number of seconds into the run before the leader is killed (10).

## What it asserts

| Check | Why |
|---|---|
| the cluster reaches 2 clients and 2 servers and `failover` is `available` | three eligible nodes (leader, standby, client-1) |
| the headless leader starts the run on quorum | `RUSTYBIN_HARNESS_START=on-quorum` |
| after `docker compose kill leader`, the standby leads a new term | the `prefer` standby is first in the succession (a term above 2 means an election before it got no majority, printed as a note) |
| the report comes from the standby: `state` finished, thresholds pass | the run survived the failover |
| `cluster.history` has two entries, the second `elected` by `standby`; `unreported` 0; `lost_check` complete; `lost` 0 | nothing was dropped or guessed |
| both clients finished with a share of 0.5, both servers saw arrivals | the split and the correlation across nodes |
| `sent` grew past the count at the kill, no empty second in the timeline | the clients never stopped |
| every completed answer is `upstream`, none `gateway`, p99 under a second | the stand-in gateway passed everything through |
| the gateway's `X-Forwarded-For` is listed under headers added | what the gateway changed on the way in is seen (its `Via` is on the answers) |

On a failure the script prints the last 150 lines of the compose logs and
takes the cluster down (unless `KEEP=1`).
