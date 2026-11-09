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
