# Step 04b: The owner's sign-in on a self-service instance

## Goal

One public instance where visitors stay anonymous and only the owner signs in, with a user name and
password. Self-service worked only with `open` control auth, the sign-in only with `password`.

## Model

- `login::optional`: the password sign-in with self-service on, outside shared mode.
- `control_auth::enforce` lets a caller with no credential past, so `check_route` treats it as a
  visitor, exactly as `open` does. A credential that fails is still `401`, never a visitor.
- `Need::Authenticated` means "got past `enforce`", so `check_route` holds that route to a credential
  here, but for a visit's own routes (`.self_service()`) and the management MCP server, where each
  tool checks its own need.
- `control_oauth::enabled` is off here, as on `open`: a stranger's agent reaches `/_rustybin/mcp` as a
  visitor, so nothing would ask it to sign in, and no authorization server stands open to strangers.
- `Resolved::anonymous` counts the sign-in as a way to hold more, so a refusal is `401`.

## Surfaces

No new route, tool or setting: `RUSTYBIN_MOCKS_SELF_SERVICE` now also works with `password`. The console
learns it from `<meta name="rustybin-sign-in" content="optional">`: `auth.required()` is false without a
token, the top bar has **Sign in**, the screen has **Continue as a visitor**, and signing out leaves a
visitor. `rustybin login` and `POST /_rustybin/login` work as ever.

## Tests

`mocks::selfservice::tests::sign_in::*`, above all
`a_caller_without_a_credential_reaches_what_an_open_plane_gives`, which walks every control route and
method and compares the two modes; the browser steps `sign-in optional: *` on a seventh instance of
`conformance/ui`; the Hurl example `mocks/self_service_sign_in.hurl` on a tenth instance of
`docs/examples/run.sh`.

## Build notes (built 2026-11-07)

- **The walk found one difference** in over 200 route and method pairs: `GET /_rustybin/projects` is an
  empty list for a visitor on `open` and `401` here. Kept as the stricter answer, named in the test.
- **Mutation check**: with the guard in `check_route` removed, the walk fails on `POST /logout`,
  `/me`, `/me/password` and `/me/sessions`, which reach their handlers with no caller.
- **An upgrade can open an instance**: `RUSTYBIN_MOCKS_SELF_SERVICE=true` next to `password` used to be
  ignored with a warning, and now takes effect. The changelog says so first.
- **The unexplained test crash of step 04 explained**: `src/cli/tests.rs` has a watchdog that ends the
  process with exit 101 when a command blocks the runtime, and its `eprintln!` went to the test's
  capture, which `exit` throws away, so nothing was printed. It writes to stderr itself now, and named
  `password change --stdin`: the test gave no canned stdin, so the command read the process's own, which
  blocks when that is an open pipe with no data and returns at once on `/dev/null`. No product bug. With
  stdin held open it hung every time; with canned stdin it passes in 7 seconds.
- **The conformance failure of `78a48f3` was a real bug, not the script's**: position `k` stood at
  `E + k x E/6` after its own last answer, but answers come up to `E/3` apart, so a later node could
  stand first and `RUSTYBIN_HARNESS_LEADER=prefer` meant nothing. The slots are `E/2` now
  (`member::stand_after`); the first in line still waits `E`.
- **Widening the retry with the slots was a regression**: reusing `stand_after` for the next try made
  `the_old_leaders_own_load_is_reported_or_bounded` fail 9 of 30 runs of the harness tests, 0 of 10 before.
  A busy node stands early, is refused by a voter that still hears the leader, and then waited about
  600 ms longer. The retry is `E/2 x (line + 1)` from now, the old length for three eligible nodes: 0 of
  20. Measure a timing change under the parallel suite, not alone, where that test passes every time.
- **`rustybin password hash`** closes a gap: the docs preferred `RUSTYBIN_CONSOLE_PASSWORD_HASH` and
  nothing made one.
