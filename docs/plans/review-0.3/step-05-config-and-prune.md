# Step 05: Config and git: prune scope, untrusted documents

| # | Severity | Finding | Anchor | Fix |
|---|---|---|---|---|
| 1 | medium | Prune deletes every `origin: api` mock: visitors', API keys' and running scenarios' (auto-pull prunes every 5 minutes on a self-service instance) | `src/configdoc/mod.rs:1470`, `:1492`, `src/mocks/store.rs:1246`, `src/mocks/service.rs:1276`, `:1316` | prune only instance-owned mocks without an expiry (`checkpoints::covered`) |
| 2 | medium | A pulled document's mock entry that fails to deserialize is dropped with its name, so prune deletes the live mock (the 0.3 rule holds only for file loads) | `src/mocks/config_file.rs:282`, `src/sourcecontrol/mod.rs:1973`, `src/configdoc/mod.rs:1409` | carry `Failed.name` into `named` and `keep` |
| 3 | medium | A pulled (untrusted) document can set `egress.*`, `assist.provider` and `assist.base_url` (the stored key then goes to that host), tests with a gateway and `{{variables}}`, and harness scenarios that inherit stored secrets for a new target | `src/configdoc/mod.rs:54`, `:1221`, `:1687`, `src/assist/mod.rs:181` | refuse those settings and gateways from untrusted documents; `fill_stored` only for the same target host |
| 4 | medium | `git_bytes` reads a whole blob into memory before checking its size | `src/sourcecontrol/mod.rs:872` | `git cat-file -s` first, or read at most `max + 1` |
| 5 | low | The harness and registry sections prune names that failed validation | `src/configdoc/mod.rs:1717`, `:1518` | the prune set from the document's names |
| 6 | low | A pull's dry run reads `origin/<branch>`, the apply reads the working copy | `src/sourcecontrol/mod.rs:1447` | apply from the same tree the preview read |
| 7 | low | A config apply compiles every spec on the async worker | `src/configdoc/api.rs:124`, `src/sourcecontrol/mod.rs:1512` | the blocking pool behind the compile permits |
| 8 | low (verify first) | A remote's user-info is not checked for option injection (`ssh://-oProxyCommand=...`) | `src/sourcecontrol/mod.rs:328` | refuse user-info starting with `-` |
| 9 | low | The CLI follows redirects, including the sign-in POST, and keeps session and mock-token headers across hosts | `src/cli/client.rs:93` | no redirects for the control client |
| 10 | low | `RUSTYBIN_TOKEN` and `RUSTYBIN_ADMIN_TOKEN` go to whatever `--url` names, over plain http too | `src/cli/mod.rs:65`, `src/cli/profiles.rs:130` | only with `RUSTYBIN_URL` or loopback; warn on http |
| 11 | low | `apply_keeping` updates in three steps without the edit lock: a concurrent edit can be deployed | `src/mocks/store.rs:1221` | one locked edit |

## Build notes (2026-11-02)

- #1 and #11 were built with step 06 (the prune scope is instance-owned, non-scratch mocks; `apply_keeping` is one
  locked edit per definition); see that step's notes.
- #2: `MocksFile.unparsed_mocks` (never serialized) holds the names of `mocks` entries `parse_lenient` could not
  read; `apply_mocks` seeds its kept set with them, so a typo in a pulled document never prunes the live mock.
- #3: `configdoc::REPOSITORY_REFUSED` (`egress.allow`, `egress.allow_loopback`, `assist.provider`,
  `assist.base_url`, `proxy.client_ip_header`): an untrusted document that carries a different value is reported and
  kept as it is, and a prune leaves them alone; the same value is quiet, so an exported document round-trips. A test
  from the repository may name a gateway only when its saved copy already names that one. `secret:stored` in a
  harness scenario fills a saved value only when the scenario's target hosts (scheme, host, port, `host` override)
  are those of the saved scenario, for every document.
- #4: `git cat-file -s` first, then a read capped at `max + 1` bytes.
- #5: the harness and registry prune sets are every name the document carries, valid or not.
- #6: a pull reads one committed tree for the preview and the apply (`HEAD`, or `origin/<branch>` for a dry run
  that would fast-forward), extracted with its specs; the working copy's files are never read.
- #7: `configdoc::apply_on_pool` runs an apply on the blocking pool behind one compile permit (503 with
  `Retry-After` on REST, a tool error on MCP, 503 from a pull).
- #8: verified: `ssh://-oProxyCommand=...@host/` reached git as an option word. A user name starting with `-` (or
  `%2d`) is refused.
- #9: the CLI's control client follows no redirects.
- #10: `RUSTYBIN_TOKEN` and `RUSTYBIN_ADMIN_TOKEN` go to `--url` only when it is `RUSTYBIN_URL` or a loopback
  address; a plain http URL to another host warns.
