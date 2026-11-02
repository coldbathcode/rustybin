# Step 08: Public demo modules: per-client caps and leftovers

| # | Severity | Finding | Anchor | Fix |
|---|---|---|---|---|
| 1 | medium (verify first) | A2A tasks are bounded by count, not bytes: 2,000 tasks with 1 MiB messages and growing histories | `src/a2a/tasks.rs:360`, `:417`, `src/a2a/wire.rs:309` | byte caps per message, task and in total |
| 2 | medium | Anyone fills the public bin store by rotating `X-Rustybin-Session` (new bins refused for an hour) | `src/request_bin.rs:255` | per-address creation, as visits |
| 3 | medium | Flooding the demo IdP evicts revocations (a revoked token works again), codes and clients | `src/oidc/store.rs:53` | refuse rather than evict revocations; a per-address registration rate |
| 4 | low | `/response-headers` and `/cookies/set` plant cookies (with `Domain`) on a public origin | `src/response_shaping.rs:172`, `src/cookies.rs:78` | in public mode drop `Set-Cookie` and `_domain` |
| 5 | low | Public flaky counters are evictable by session rotation | `src/flaky.rs:97` | a per-address cap |
| 6 | low | The registry's open `/auth/none` mint evicts an admin's publish token at 256 | `src/mcp/registry/mod.rs:292` | a per-address mint rate |
| 7 | low | A gateway scenario run belongs to the session key; without `X-Rustybin-Session` that is the client address, so everyone behind one NAT sees the run and its credential-injection key | `src/scenarios/mod.rs` | show the key to the starting session only once, or bind runs to the credential |
| 8 | low | The CLI test that drives every command occasionally hangs in the full parallel run (not reproduced in 52 runs on 2026-11-02) | `src/cli/tests.rs` | a per-command timeout in the test, so a hang names its command |

## Build notes (2026-11-02)

- **1 (A2A bytes): confirmed, fixed.** Nothing but `MAX_PARTS` and the 1 MiB body limit bounded a
  message, so the verify-first test (`a_message_over_the_byte_cap_is_refused`) failed on today's
  code. `model.rs` gained `approx_bytes` for parts, messages, artifacts and tasks (string lengths
  and a walk of JSON values, no serialising). `wire::parse_message` refuses a message over
  `MAX_MESSAGE_BYTES` (256 KiB) as invalid params, for every binding. A task keeps
  `MAX_TASK_BYTES` (1 MiB): `push_history` and `apply` trim the oldest history entries, the
  newest always stays. `Limits.max_total_bytes` (256 MiB, 64 MiB in public mode) caps the store:
  `Entry.bytes` is kept after every change, `evict` makes room for a new task and `shed` drops
  the least recently touched other tasks after one grows. Tests: the three in `a2a::tasks::tests`.
- **2 (bins per address): fixed.** `BinLimits.max_bins_per_address` (20 in public mode, 200
  otherwise), the creator's address kept on each bin, `RequestBins::create_from`, and a `429`
  naming the address. Test: `public_mode_limits_bins_per_address` (rotating session headers from
  one address stop at 20).
- **3 (demo IdP): fixed.** `BoundedStore::try_insert_until` refuses rather than evicts; the
  revocation denylist uses it and a full list answers `503 temporarily_unavailable` (RFC 7009
  2.2.1), so a flood never makes a revoked token valid again. Dynamic registration counts per
  address per minute (`PerMinute`, 10 in public mode, 100 otherwise, 4096 addresses tracked,
  new addresses refused while full) and answers `429 slow_down` with `Retry-After: 60`. Codes
  and refresh tokens keep evicting: losing one is a failed sign-in, not a security gap. Tests:
  `a_full_denylist_refuses_new_revocations_rather_than_evicting`,
  `dynamic_registration_is_rate_limited_per_address`, `try_insert_refuses_when_full`.
- **4 (cookies on a public origin): fixed by refusing rather than dropping** (a silently
  dropped header would be echoed in the body as if set): on a public instance
  `/response-headers` answers `400 forbidden_header` to `Set-Cookie` in any case, and
  `/cookies/set` and `/cookies/delete` answer `400` to `_domain`; host-only cookies still work.
  Tests: `public_mode_sets_no_cookies_from_response_headers`, `public_mode_refuses_a_cookie_domain`.
- **5 (flaky counters): fixed.** Each counter keeps its creator's address; in public mode an
  address holds at most `MAX_COUNTERS_PER_ADDRESS_PUBLIC` (200) and past that loses its own
  oldest, and a full map evicts the oldest counter of the address holding the most (in every
  mode). Not a shared per-address entry as in visits: gateway scenarios send one session per
  request from the gateway's address, and sharing would break their counts. Test:
  `rotating_sessions_from_one_address_keeps_other_clients_counters`.
- **6 (registry mint): fixed.** `auth_exchange` counts mints per address per minute
  (`mints_per_minute`: 10 in public mode, 60 otherwise, below `MAX_TOKENS`), `429` problem with
  `Retry-After: 60`. Test: `the_open_mint_is_rate_limited_per_address` (the first token still
  passes the publish token check after the flood).
- **7 (scenario NAT key): not in this step's build**; the coordinator does it with the other
  scenario changes.
- **8 (CLI live test hang): done.** `Instance::run_full` runs each command under
  `COMMAND_TIMEOUT` (30 s) and panics with the command line; a watchdog thread per command ends
  the process with the command line after three times that, for a command that blocks the
  test's runtime thread (where the timer cannot fire). The hang itself was not reproduced.
- Docs: `reference/request-bin.md`, `reference/oidc.md`, `reference/http-basics.md`,
  `reference/redirects-cookies.md`, `reference/reliability.md`, `a2a/index.md`,
  `mcp/registry.md`. No Hurl example sends what is now refused (the docs examples' public
  instance runs only the self-service examples; it creates no bins and sets no cookies).
