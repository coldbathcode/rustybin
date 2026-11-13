# Step 20: A pull request opened through the git host

Owner decision first (section 6): which hosts.

## Goal

Step 11's `pull_request` way out pushes the instance's side to a new branch and stops: a person
opens the pull request at the host. With the host's API the instance opens it and answers its
address. It never merges it.

## Design

- Settings, group `git`: `git.host` (`auto`, `none`, or a host kind by name), `git.host_api` (the
  API's base URL for a self-hosted host), and the secret `git.host_token` (falls back to `git.token`
  when that is the auth mode and unset). None is changed by a document from the repository.
- `auto` reads the kind from the remote's host name for the well-known public hosts and is `none`
  for anything else, so a self-hosted host is named on purpose.
- Each host kind is one small module behind one trait: open a pull request (base, head, title,
  body), answer its address and number. Nothing else: no merge, no review, no comments.
- Outbound: through `egress::Egress` under a purpose of its own; `sync_egress` permits exactly the
  API host that the settings resolve to, as it does for the remote. Pinned, no redirects, bounded
  answers.
- The title and body are the instance's own text (what differs, from `configdoc::diff`, capped);
  nothing from the repository's side is copied into them.
- `resolve` with `pull_request` gains `open: true`. It is outward-facing, so the tool asks to
  confirm; `dry_run` says what would be opened and where. A failure to open leaves the pushed branch
  and says so: the person opens it by hand, as today.
- The answer's address comes from the host: `untrusted_pull_request`.

## Hosts (the decision)

Recommended: the two largest public hosts first, behind the trait, with the self-hosted variants of
both through `git.host_api`. A third kind (the lightweight self-hosted family) is one more module
when someone asks. Each kind supported is supported for years, which is why the list is the owner's.

## Surfaces

API (`resolve`), console (a checkbox on the pull request way out, the address shown after), management
MCP (`git_resolve` gains `open`), CLI (`git resolve --with pull-request --open`), the settings.

## Tests that fail first

Against an in-process fake of each host's API on loopback:

- The pull request is opened with the right base and head, and its address is answered.
- A token without the right, a branch the host does not know and a host that answers slowly: the
  branch stays pushed, the answer says the pull request was not opened and why.
- `dry_run` calls nothing.
- A host outside the egress policy is refused before any call.
- The token is in no answer, log line or audit entry.

## Done when

A diverged instance reaches an open pull request in one confirmed call on the chosen hosts, and
`docs/src/source-control.md#a-diverged-branch` shows it.

## Build notes (built 2026-11-12)

The owner chose the recommendation: GitHub and GitLab with their self-hosted variants,
`auto` for the two public host names only. No third kind is built.

- **Where it lives**: `src/sourcecontrol/host.rs` (the settings' resolution, the trait
  `Host`, the one call, the text of a pull request, the status' `host`), one module per
  kind in `src/sourcecontrol/host/` (`github.rs`, `gitlab.rs`), the pull request path of
  `branches.rs`, three settings in `specs()`, `Purpose::GitHost` in `src/egress.rs`. No
  new `AppState` field, no new route, no new tool. Tests:
  `src/sourcecontrol/tests/host.rs` and the fakes in `src/sourcecontrol/fake_host.rs`
  (test builds only), plus a tool test, a CLI test, the docs examples and the browser
  check.
- **The order of work, as it was.** The tests were written with the code, not before
  it. Then each protection was removed once and the tests run; every removal failed at
  least one test: the purpose's rule permitting any host; no check of the outbound
  policy before the push; the remote's token sent to any API host; the host's words not
  scrubbed of a credential; the host's address passed on as it came; the body naming
  what only the repository has; `auto` recognising a name that only looks like a public
  host; an API over plain http; `open` with another way out; the settings' refusal
  ignored; the timeout. No test was seen failing before its code existed. Not removed:
  "no redirect followed" (that is `Egress::client`'s, and the test for it here passes
  through the fake's redirecting path) and the tool's confirmation (it was there before
  the step: every `git_resolve` that is not a dry run asks).
- **The trait is the request and the reading of the answer, not the call.** A kind says
  where its API is, how it names a repository, what the call carries and how its
  answer reads (`Host`: seven small functions). The call itself (the outbound policy,
  the pinned client, the timeout, the bounded read, the judgement of a status) is made
  once in `host.rs`, so no kind can reach out on its own.
- **The API's base when `git.host_api` is empty.** The step file names `git.host_api`
  for a self-hosted host. Built: with a kind named and no `git.host_api`, the API is the
  kind's usual path on the repository's own host (`/api/v3`, `/api/v4`), so naming the
  kind is enough for a host installed the usual way. `auto` never does this.
- **The fallback to `git.token` is narrower than the step file says.** It applies when
  `git.auth` is `token` and the API is on the remote's own host, or is the public API
  of the public host the repository is on. With a `git.host_api` on another host the
  remote's token is not sent there: `git.host_token` is needed. A token goes only where
  it was set for.
- **`git.host_api` is `https` only**, except `http` to a loopback address (the tests'
  fakes and a demo on one machine), because the token is sent to it.
- **What is refused before the push, and what is not.** No host (`host_not_set`), no
  token (`host_token_missing`) and an API the outbound policy refuses on a dry check
  (`egress_refused`) are a `409` before anything is pushed, for a preview too. A name
  that resolves to a refused address is found only when the call is made, after the
  push: that is `not_opened` with `egress_refused`, the branch pushed.
- **A failure to open is `200`**, with `opened: false` and `not_opened: {reason, why,
  status, untrusted_host_message}`: the push went through. The timeline records the
  operation as done. The call is made once and never retried (a retry of a slow host
  could open two); a timeout says that what the host did is not known.
- **The text.** The title is the first line of the caller's `message` (120 characters)
  or a default; the body lists, by `section.key` and action, what the instance adds or
  holds differently (40 at most, 4000 characters). Entries only the repository has are
  counted, not named, and the `detail` of a change (which quotes values of both sides)
  is never written.
- **The answer's address** is kept only when it parses as an `http` or `https` address
  without credentials, 500 characters at most; otherwise it is `null` and the number
  stands. The console makes it a link under the same condition.
- **Settings have no variable** (`git.host`, `git.host_api`, `git.host_token`), like
  `git.sign`. The repository's side cannot change them: `git.` was already in
  `REPOSITORY_REFUSED_PREFIXES` and the group check of `apply_secrets` (step 11).
- **Surfaces**: API (`open` on `resolve`, `host` in the status, OpenAPI and catalogue
  descriptions); console (the box under the pull request way out, off with the reason
  until a host and a token are set, a "Git host" row in the status, the address shown
  under the answer; the three settings are in the repository form from the schema);
  MCP (`git_resolve` gains `open`; it already asked for every call that is not a dry
  run, and the confirmation now says the host is asked; `git_status` answers `host`);
  CLI (`git resolve --with pull-request --open`, which prints the address and, on a
  refusal, the host's words with control characters removed); `rustybin.yaml`: nothing,
  the `git` group is never exported.
- **Also changed**: the `purpose` enum of the `check_egress` tool lacked `record` and
  `events`; both are added with `git_host`. `SourceControl::scrub` also removes
  `git.host_token`.
- **Not built**: a diagnostics check for a host that is named and not ready (the status
  and the `409` say why); a third host kind; anything at the host beyond opening the
  pull request; finding an already open pull request for the branch (the host refuses a
  second one: `refused`).
- **How faithful the fakes are.** No real host was called. Each fake was written from
  what is known of the host's public API, not from a recording:
  - `github`: `POST /repos/{owner}/{repo}/pulls` with `title`, `head`, `base`, `body`,
    a bearer token, `Accept: application/vnd.github+json` and `X-GitHub-Api-Version:
    2022-11-28`; `201` with `number` and `html_url`; `401 Bad credentials`; `403
    Resource not accessible by personal access token`; `404 Not Found`; `422 Validation
    Failed` with `errors[]`. These are the documented shapes. Not modelled: a `403` or
    `429` of the rate limiter with its headers, a `404` for a token that may not see a
    private repository versus one that lacks the permission (the real host answers
    either), `head` in the `owner:branch` form of a fork.
  - `gitlab`: `POST /projects/{url-encoded path}/merge_requests` with `source_branch`,
    `target_branch`, `title`, `description` and a bearer token; `201` with `iid` and
    `web_url`; `401`, `403`, `404 Project Not Found` with `message`. **The answer for
    a branch the host does not know is a guess** (`400` with `message` as a map of
    field to reasons); the real host may answer `409` or `422` or another body. The
    instance treats `400`, `409` and `422` alike and reads `message` as a string, a
    list or a map, so the guess decides only the words under
    `untrusted_host_message`.
  A first run against each real host is worth doing before the release.

## For the merge

Changelog, under "Unreleased":

**New**

- **A pull request opened by the git host.** `POST /_rustybin/git/resolve` with
  `pull_request` takes `open: true`: after the push the git host's API is asked to open
  the pull request from the new branch into the instance's branch, and the answer
  carries its address (`untrusted_pull_request`: the host's own answer). GitHub and
  GitLab, and their self-hosted variants: the settings `git.host` (`auto` recognises
  `github.com` and `gitlab.com` only, `none`, `github`, `gitlab`), `git.host_api` and
  the secret `git.host_token` (else `git.token` on the remote's own host). The instance
  only ever opens a pull request: it never merges one. When the host does not open it
  the branch stays pushed and `not_opened` says why; what the settings cannot serve is
  a `409` before anything is pushed; `dry_run` says what would be opened and calls
  nothing. On every surface: the console's **Open the pull request at the git host**
  box, `git_resolve` with `open`, `rustybin git resolve --with pull-request --open`.
  See [source control](source-control.md#opened-by-the-git-host).
- The outbound policy has a purpose `git_host`: exactly the git host's API once a host
  is set up.
- `GET /_rustybin/git` answers `host`: where a pull request would be opened, and what
  is missing.

**Changed**

- The `check_egress` tool's `purpose` accepts `record`, `events` and `git_host`.

**Fixed**

- Nothing.

Upgrade note: none. `git.host` is `auto`, and nothing calls a host until a caller asks
with `open: true`. An instance whose repository is on `github.com` or `gitlab.com` with
a token as its authentication can open pull requests with that token at once, if the
token has the right.

Roadmap: the line "a pull request opened through the git host's API" is done; a third
host kind (the lightweight self-hosted family) stays under consideration.

Rules for CLAUDE.md (step 20, source control; `src/sourcecontrol/host.rs`,
`host/github.rs`, `host/gitlab.rs`, the fakes in `fake_host.rs`): a git host is asked
for one thing, to open a pull request, never to merge, review or comment; a new kind is
a module behind `host::Host` (where the API is, the repository's name, the call, the
reading of the answer) and never makes a call itself: `SourceControl::open_at_host`
does, once, under `Purpose::GitHost`, whose rule is exactly the API's host
(`sync_host_egress`); `auto` recognises a public host by its exact name and is `none`
for anything else; the token is `git.host_token`, and `git.token` goes only to the
remote's own host; what the host answers is `untrusted_pull_request` and
`untrusted_host_message` (cleaned, cut, scrubbed of the token, never in `detail`, a log
line or an audit entry); what is sent is the instance's own text (`pull_request_text`:
no name only the repository has, no value of either side); what the settings cannot
serve is refused before the push, a host that does not open it leaves the branch
pushed and is a `200`; the call is never retried.
