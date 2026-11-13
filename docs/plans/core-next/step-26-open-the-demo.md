# Step 26: The demo on the current release, and open to visitors

The owner's to start: it changes live infrastructure on the owner's VPS.

## Goal

`demo.rustybin.dev` runs 0.6.0, locked to the owner's sign-in since 2026-11-07, with no visitors.
Section 1 of the hosted demo plan, an open demo where visitors make their own mocks, is still
waiting. This step moves the demo to the current release and, on the owner's word, opens it.

## Part A: the current release (after step 14)

- `deploy/demo` is pinned to the new digest; the compose project is brought up on the VPS and the
  diagnostics read.
- What steps 11 to 13 added is off there and stays off: `agents.secret_requests`, `git.sign`, the
  harness. Nothing new needs a variable.
- The walk of 2026-11-07 is repeated over the public URLs for a locked instance: every control
  route refuses a caller without a credential, a wrong password is refused, the owner's mocks answer
  on their hosts.

## Part B: open to visitors (the owner's word)

- The anonymous surface is walked again on the new release before anything is opened, as steps 01
  to 03 of the hosted demo plan did on 0.5.0: every control route and method called without a
  credential, the answers of the open ones read in full. The routes new since then
  (`/_rustybin/secret-requests*`, `/_rustybin/git/branches`, `/git/branch`, `/git/signing-key`, the
  import) must all refuse a visitor: check that
  `a_caller_without_a_credential_reaches_what_an_open_plane_gives` covers them in the suite, and the
  walk says so on the image.
- The image is run at the demo's limits again (read-only root, tmpfs, 512 MiB) with a visitor's
  worst spec: step 02 of the hosted demo plan measured 0.5.0, and nothing has measured this release.
- `RUSTYBIN_MOCKS_SELF_SERVICE`, `_HOST` and `_HOME` are set as `deploy/demo/README.md` says, with
  the owner still signing in (`login::optional`, step 04b). `RUSTYBIN_TRUST_FORWARD_HEADER` stays
  `x-real-ip`.
- Stateful visitor mocks stay off (the decision of 2026-10-18) unless the owner turns them on.

## Decisions for the owner in this step

- **Email leads.** No webhook or file is set, so a visit ends after its anonymous hours and the
  email step is off. Opening with leads needs a destination for addresses, and the privacy page
  already describes it.
- **A date.** Opening is a public act; the step does not pick the day.

## Not in this step

More than one replica (step 24). OAuth for visitors' agents (step 23).

## Done when

The demo runs the current release, and it is either open with the walk recorded here or this row
says it stays locked by the owner's choice.

## Build notes: part A (2026-11-13)

The owner said "get the demo to the latest release" on 2026-11-13; part B (opening to visitors)
was not asked for and the demo stays locked to the owner's sign-in.

- `deploy/demo/compose.yaml` is pinned to the 0.7.0 digest
  (`sha256:84663e342a16caf8a9fea08a9288cffc14673a708db6b33af3b3ed2da35f229b`, verified with cosign
  before the deploy). Nothing else in the file changed and nothing new needed a variable.
- On the VPS, before the change: the old compose file kept as `compose.yaml.0.6.0` and the data
  volume archived as `data-backup-0.6.0-2026-11-13.tgz`, both in `~/rustybin-demo`. Going back is
  the old file, the archive restored into the volume, and `docker compose up -d`.
- After `docker compose up -d`: the container reports 0.7.0, no warning or error line in its log,
  about 30 MiB of its 512 MiB in use.
- The walk over the public URLs, without a credential: 256 control route calls (every method the
  catalogue lists, path parameters filled with `x`), 243 answered `401`. The other 13 are the open
  ones and answered as an open route should with no input: the readiness check `200`, the
  inspector's client document `200` (new in 0.7.0, open by design, nothing secret), the single
  sign-on status `200` and start `302`, `400` from the OAuth registration and authorization, the
  join and the single sign-on callback, and `404` from the harness node routes and the git webhook
  (the harness is off and no webhook secret is set).
- A wrong password is refused (`401`); the console and the landing page load; the sample mock
  answers on its host (`petstore.rustybin-mock.dev/v1/pets/1`) and by path.
- Not done: the diagnostics were not read, since that needs the owner's sign-in and the password is
  written nowhere. The owner reads Management, Diagnostics after signing in.

