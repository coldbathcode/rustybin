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
