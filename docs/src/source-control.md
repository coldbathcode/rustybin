# Source control

An instance can keep its configuration ([the whole instance as code](configuration-as-code.md))
in a git repository: commit and push it from **Management, Source control**, pull and
apply the repository's version with a dry run first, start from the repository so it
is the source of truth and the backup, and let the instance push every change or pull
at an interval. A branch per environment, signed commits, and a diverged branch
resolved by a person (a side, or a pull request at the git host) are on this page too.

## What it needs

- The `git` binary. The container image includes it (and `ssh-keygen` for deploy
  keys); elsewhere put git on the `PATH`. Without it the status says so and every
  operation answers `503`.
- `RUSTYBIN_STATE_DIR`: the working copy lives in `<state>/repo`, the deploy key in
  `<state>/repo.key`.
- A remote whose host passes the [outbound policy](configuration.md#outbound-connections):
  the host of the configured remote is allowed under the purpose `git` once set, and
  always-refused addresses stay refused. A local repository (a path or a `file://`
  URL, for tests and demos) needs `RUSTYBIN_EGRESS_ALLOW_LOOPBACK=true` or the
  `egress.allow_loopback` setting.

git is held to what the policy checked: it connects to the addresses the policy
pinned (curl's resolve list for https, ssh's `HostName` for ssh, the host key still
checked under the remote's name), follows no redirect, may use only the remote's
transport (`protocol.allow=never` plus https, ssh or file), and runs with a cleared
environment (`PATH` and its own variables, never the instance's secrets). Transport
helpers (`name::address` remotes) are refused, and so is a remote's user name that
starts with `-` (it would reach git as an option).

## What a repository writer can do

Anyone who can push to the repository controls what the document describes: the
mocks, the settings it names, the registry and portal entries, the harness
scenarios, the agents' rules and the health toggle, on the next pull with apply.
(With [verified pulls](#verified-pulls) on, only when a signer the instance allows
signed the commits.) Nothing more:

- **Not the host's files.** A symbolic link in the working copy (`specs/`, the
  document, a spec) is refused when committing, git checks links out as plain files
  (`core.symlinks=false`), and a mock's `spec:` file must be a relative path inside
  the repository (no `..`, no absolute path, no link).
- **Not the process environment.** A pulled document is never expanded as text: a
  `${NAME}` in a mock example stays that text. Under `secrets:` a reference reads only
  the variable that seeds that very secret (`assist.openai_key: ${RUSTYBIN_ASSIST_OPENAI_KEY}`);
  a literal value, or another variable, is refused with a problem. (A file given to
  `RUSTYBIN_CONFIG_FILE` is the operator's own and is still expanded as before.)
- **Not secret values in answers.** A diff names a secret's reference or says "a
  value", never the value, and the same reference on both sides is no change.
- **Not where the instance sends requests.** `egress.allow`, `egress.allow_loopback`,
  `assist.provider`, `assist.base_url` and `proxy.client_ip_header` are never changed
  by a pulled document: a different value is a problem and the setting is kept as it
  is (prune leaves them alone too; the same value is quiet, so an export round-trips).
  A test in the document may name a gateway only when its saved copy already names
  that one, and a test schedule a webhook only when its saved copy already names that
  one.
- **Not who reads the portal.** `portal.auth` (a pull would open every item of a
  `password` portal) and `portal.base_url` (where Try it sends a tester's API key) are
  kept as they are, as above; the portal's items may be closed by a pull, never opened.
- **Not single sign-on.** Every `sso.*` setting (the issuer, the client, the role
  map, the default roles, the groups claim, the allowed domains, provisioning, the
  password sign-in) is kept as it is: a pusher would otherwise repoint the issuer
  or map their own group to `admin` ([Single sign-on](sso.md)). A different value is
  a problem, and prune leaves them alone.
- **Not wider roles.** A pulled `roles:` section never defines, changes or prunes a
  privileged role (one granting `users.manage` or `settings.security`), never gives
  an existing role a permission it did not grant (any permission: its holders would
  gain it), and never creates a role that `sso.role_map` or `sso.default_roles`
  names (the provider's groups would give it at the next sign-in). A role may
  narrow, and a new role nothing names is created (nobody holds it until someone
  with `users.manage` assigns it). Each refusal is a problem, the role kept as it is.
- **Not the audit log's retention.** `audit.retention_days` and `audit.max_bytes`
  are kept as they are (a pusher would otherwise shorten the [audit log](audit-log.md)
  to erase what they did); a different value is a problem.
- **Not the sign-in.** In password mode a document never changes the console's user
  name or password hash (a change is a problem), and pruning never removes them, so a
  document without them cannot bring back the default password; a public instance
  refuses to clear the hash at all.
- **Not who works in a project.** A pulled `projects:` section creates projects and
  changes their descriptions, never their members, and never prunes a project; a
  mock or saved test that exists is never moved to another project (`project:`),
  and a checkpoint from the repository never moves a mock between projects. Each is
  a problem, kept as it is ([Projects](projects.md)).
- **Not other people's mocks.** A prune deletes only the instance's own mocks: never a
  visitor's, an API key's or a running [gateway scenario](gateway-scenarios.md)'s, and
  never a mock the document names but could not read (an entry with a typo included).
- **Not source control itself.** No `git.*` setting or secret is changed by a pulled
  document: not the remote, the branch, the directory, the automatic mode, commit
  signing, the token, the webhook secret or the signing key. A pusher cannot move an
  instance to another branch or repository, turn signing off or stop the webhook; a
  different value is a problem and the setting is kept (the export never carries the
  group, so only a hand-written document names one). A scenario's `file:` and a mock's
  `spec:` are paths inside the repository, read from the commit: no `..`, no absolute
  path, no link.
- **Not the harness's credentials.** Saved scenarios are exported with their API keys
  and OAuth client secrets as `secret:stored`, filled back from the saved scenario of
  the same name when applied, and only when the scenario's targets (scheme, host, port,
  `host` override) are those of the saved one; the files under the harness state
  directory are sealed with the secrets key.

A pull reads the document and every spec it names whole, up to their supported sizes
(8 MiB for `rustybin.yaml`, the instance's spec limit for a spec): a larger file is an
error of the pull, never a shortened file. Each file's size is checked before it is
read.

## Connect a repository

The `git.*` settings (**Management, Source control**, or `PATCH /_rustybin/settings`;
seeded by the `RUSTYBIN_CONFIG_GIT*` variables). Changing one needs `git.write` on
top of `settings.write` (the remote and the automatic mode decide where the whole
instance is pushed and what a pull applies), and `secrets.write` for `git.token`:

| Setting | Variable | Default | Meaning |
|---|---|---|---|
| `git.remote` | `RUSTYBIN_CONFIG_GIT` | unset | `https://host/org/repo.git` (with a token), `ssh://git@host/org/repo.git` or `git@host:org/repo.git` (with the deploy key), or a local path. Never with credentials in the URL. |
| `git.branch` | `RUSTYBIN_CONFIG_GIT_REF` | `main` | The branch the instance commits to and pulls from. |
| `git.path` | `RUSTYBIN_CONFIG_GIT_PATH` | `rustybin` | The directory in the repository that holds `rustybin.yaml` and `specs/` (empty for the root). |
| `git.auth` | `RUSTYBIN_CONFIG_GIT_AUTH` | `token` | `token` for an https remote, `deploy_key` for an ssh remote. |
| `git.token` (secret) | `RUSTYBIN_CONFIG_GIT_TOKEN` | unset | A personal access or deploy token. The instance hands it to git through `GIT_ASKPASS`, which answers the remote's own host only, never on a command line or in a URL, and never shows it again. |
| `git.username` | | `x-access-token` | The user name sent with the token (GitHub accepts any; GitLab deploy tokens have their own; Bitbucket wants the account). |
| `git.author` | | `Rustybin <rustybin@example.invalid>` | `Name <email>` of the commits. |
| `git.auto` | `RUSTYBIN_CONFIG_GIT_AUTO` | `off` | `push_on_change` or `pull`, see [Automatic mode](#automatic-mode). |
| `git.pull_every` | | `5m` | The interval of the pull mode (`1m` to `24h`). |
| `git.webhook_secret` (secret) | | unset | The secret a git host signs push webhooks with, see [A webhook instead of polling](#a-webhook-instead-of-polling). |
| `git.sign` | | `false` | Sign every commit this instance makes, see [Signed commits](#signed-commits). |
| `git.signing_key` (secret) | | unset | The private key commits are signed with: an OpenSSH key without a passphrase. Never the deploy key, never shown again. |
| `git.verify` | | `false` | Take the repository's side only when an allowed signer signed every commit since the last verified head, see [Verified pulls](#verified-pulls). Turning it on trusts the head of that moment. |
| `git.allowed_signers` | | empty | Who may sign what this instance pulls: at most 50 lines, a name and a public key each (`ann ssh-ed25519 AAAA...`, or the line of a `.pub` file). Public keys, not secrets. |

**Deploy key.** For an ssh remote, **Generate the deploy key** (`POST
/_rustybin/git/key`) creates an ed25519 key in `<state>/repo.key` (mode `0600`) and
shows its public half to add to the repository (a deploy key with write access on
GitHub or GitLab). The private half never leaves the instance; `{"replace": true}`
generates a new one. The remote's host key is accepted on first use into
`<state>/repo.known_hosts`.

```bash
{{#include ../examples/config/source-control.sh:deploy_key}}
```

**Connect** (`POST /_rustybin/git/init`) clones the branch into the working copy, or
initialises an empty working copy when the remote has no such branch yet (the first
push creates it). An existing working copy is pointed at the remote. With
`RUSTYBIN_CONFIG_GIT` set the instance connects at startup; otherwise set `git.remote`
and connect when ready:

```bash
{{#include ../examples/config/source-control.sh:connect}}
```

Every operation is admin-only, audited (`rustybin::git` log lines, the timeline in the
status), serialised by one lock (a call that finds it taken answers `409` with
`busy: true`: ask again in a moment, as the console does), and runs git with a timeout,
`GIT_TERMINAL_PROMPT=0` and no global or system git configuration.

## Status

`GET /_rustybin/git` (admin; `?fetch=true` asks the remote) answers the configuration,
whether git is installed and the working copy exists, the last commit, ahead and
behind against the remote, the **drift** (the changes a commit would record: the
instance's export diffed against the working copy's document, as lines such as
`mocks.orders: spec changed (2 operation(s) added)`), the deploy key's public half,
`signing` (whether commits are signed, the signing key's public half and fingerprint),
`divergence` (what differs, when the branch has diverged) and the timeline of
operations. While another git operation holds the working copy the status answers
`busy: true` without the last commit, the counts, the drift or the divergence: that
says nothing about them yet, so ask again in a moment.

```bash
{{#include ../examples/config/source-control.sh:status}}
```

On an instance where nothing is connected yet, `signing` and `divergence` say so:

```hurl
{{#include ../examples/config/git_branches.hurl:status_signing}}
```

## Commit and push

`POST /_rustybin/git/commit` `{message}` exports the instance as a bundle into
`<repo>/<git.path>/` (`rustybin.yaml` and `specs/`, specs as files so a reviewer
diffs them) and commits; nothing to commit is an answer, not an error. `POST
/_rustybin/git/push` pushes the branch. A commit is signed when `git.sign` is on
([Signed commits](#signed-commits)) and its answer says so (`signed`).

```bash
{{#include ../examples/config/source-control.sh:commit_push}}
```

In the console, **Commit and push** does both; **Write the commit message** asks the
AI for one from the drift summary ([Bring your own AI](ai-assist.md)).

**Checkpoints.** `checkpoints: ["demo-start"]` also writes those
[checkpoints](checkpoints.md#in-the-configuration-repository) of the instance to
`<git.path>/checkpoints/<name>.json` in the same commit (the console's commit card
lists them to tick; `rustybin git commit --checkpoint demo-start`). A checkpoint holds
mock data, so only a commit that names it writes one: the automatic mode never does,
and no commit stages anything else in that directory. Another instance lists them
after a pull and restores one with `?from=git`:

```bash
{{#include ../examples/config/source-control.sh:commit_checkpoint}}
```

## Pull and apply

`POST /_rustybin/git/pull` `{apply, dry_run, prune}` fetches and fast-forwards the
working copy; with `apply` it applies the repository's document through [the
configuration document's apply](configuration-as-code.md#apply-with-a-dry-run-first).
The document and its `spec:` files are read from one commit (`HEAD` after the pull),
never from the working copy's files. A dry run reads the commit the pull would move
to (the remote branch) without moving the working copy and answers the diff, so the
preview and the apply read the same files; `prune` makes the document the whole
truth. The apply runs on the compile pool: when it stays busy the pull answers `503`.
The console shows the diff first, then **Pull and apply**. Every pull also answers
`checkpoints`, the names of the repository's checkpoints at the commit it read; a pull
never restores one (`POST /_rustybin/checkpoints/{name}/restore?from=git` does, see
[Checkpoints](checkpoints.md#in-the-configuration-repository)).

```bash
{{#include ../examples/config/source-control.sh:pull_dry_run}}
```

## A diverged branch

The instance never merges. When the branch has diverged (commits here and commits
there), push and pull answer `409` with `conflict`, `ahead`, `behind`, `options` and
`divergence`: **what differs**. The status carries the same `divergence` (null when
nothing has diverged):

- `local_commits` and `remote_commits`: the commits of each side (ten at most);
- `instance`: what taking this instance's copy would change in the repository's
  document, and `repository`: what taking the repository's copy would change here,
  both by section, as the lines of a [diff](configuration-as-code.md#apply-with-a-dry-run-first).

```bash
{{#include ../examples/config/source-control.sh:diverged}}
```

```bash
{{#include ../examples/config/source-control.sh:what_differs}}
```

`POST /_rustybin/git/resolve` `{with, message, branch, dry_run}` is the way out, and a
person chooses it: the [automatic mode](#automatic-mode) and the webhook never resolve a
divergence, they keep reporting it (the log, the timeline, the status and the
`source_control_diverged` check of the [diagnostics](troubleshooting.md)).

| `with` | What happens | `dry_run` answers |
|---|---|---|
| `instance` | The local commits are dropped and this instance's configuration is committed on top of the remote's branch (the new commit's parent is the remote head), then pushed; `message` names the commit. Never a forced push: if the remote moved in between, the push is refused and nothing there changed. | The changes the commit would make to the repository's document |
| `repository` | The working copy takes the remote's branch and its document is applied with prune, through the same checks as a pull. The local commits are dropped. | The apply's own dry run of the remote's document, validated exactly like the apply; the working copy does not move |
| `pull_request` | Neither side wins yet. What the instance holds is committed (when it differs from its last commit) and the local branch is pushed to a new branch of the remote: `branch`, or a made name such as `main-instance-20261109-153012`. Nothing is applied, `git.branch` stays, and the answer names the branch and the base to open the pull request against. Never forced: a branch of that name with other commits is refused. | The branch and the commits it would carry |

A preview of `repository` is the answer of a pull's dry run: `applied.summary` lists
what the document itself changes, and `applied.results` is the full account, with what
prune would unset or delete because the document does not name it.

```hurl
{{#include ../examples/config/git_branches.hurl:resolve_preview}}
```

```bash
{{#include ../examples/config/source-control.sh:resolve_preview}}
```

```bash
{{#include ../examples/config/source-control.sh:resolve}}
```

**Through a pull request.** `pull_request` is for a team that reviews configuration:
the merge happens at the git host, by a person, with the host's own tools for a
conflict in `rustybin.yaml`.

```hurl
{{#include ../examples/config/git_branches.hurl:resolve_pull_request}}
```

```bash
{{#include ../examples/config/source-control.sh:pull_request}}
```

Open a pull request from that branch into the instance's branch. Until it is merged the
instance still reads as diverged, on purpose. After a merge commit the instance's
commits are part of the branch's history, so an ordinary pull fast-forwards and
applies:

```bash
{{#include ../examples/config/source-control.sh:after_merge}}
```

After a squash or a rebase merge the history differs, the branch still reads as
diverged, and `repository` takes the merged result. Commits that named
[checkpoints](checkpoints.md#in-the-configuration-repository) travel with the
`pull_request` branch; `instance` and `repository` drop the local commits, and with
them a checkpoint that was committed and never pushed (commit it again).

In the console the status shows what differs, and **A diverged branch** has the three
ways out, each with **Preview**.

## Branches

A branch per environment: one repository, a `staging` and a `production` branch, an
instance on each. The branch is the `git.branch` setting and nothing else: a
`rustybin.yaml` never names a branch, and a document from the repository never changes
`git.branch` ([what a repository writer can do](#what-a-repository-writer-can-do)).

`GET /_rustybin/git/branches` lists the remote's branches (200 at most, with their
commits) and says which one the instance is on:

```hurl
{{#include ../examples/config/git_branches.hurl:branches}}
```

```bash
{{#include ../examples/config/source-control.sh:branches}}
```

`POST /_rustybin/git/branch` `{branch, create, apply, dry_run, prune, message}` moves
the instance:

- **Switch** to a branch the remote has. With `dry_run` the answer is what applying
  that branch's document would change, read from the commit and validated exactly like
  the apply, and nothing changes: not the setting, not the working copy. Without it
  `git.branch` is stored, the working copy takes the branch, and its document is
  applied (`prune` makes it the whole truth; `apply: false` moves the branch only). A
  branch whose document cannot be read is not switched to, and a switch that fails
  puts the setting and the working copy back.
- **Start** a branch the remote does not have, with `create: true`: the branch is
  created at the working copy's head, `git.branch` is stored, the instance's
  configuration is committed (`message`) and the branch is pushed. `dry_run` answers
  what the commit would record.

```hurl
{{#include ../examples/config/git_branches.hurl:switch_preview}}
```

```bash
{{#include ../examples/config/source-control.sh:switch_preview}}
```

```bash
{{#include ../examples/config/source-control.sh:switch}}
```

```hurl
{{#include ../examples/config/git_branches.hurl:branch_start}}
```

```bash
{{#include ../examples/config/source-control.sh:branch_start}}
```

The old branch keeps its commits, here and on the remote; nothing is deleted. The
webhook and the automatic mode follow the setting, so after a switch they work on the
new branch. With `RUSTYBIN_SETTINGS_LOCK` the branch is the environment's
(`RUSTYBIN_CONFIG_GIT_REF`) and a switch is refused.

## Signed commits

A git host with branch protection may want every commit signed. With `git.sign` on,
the instance signs each commit it makes (yours, the automatic mode's, a resolve's) with
an SSH signature, which needs git 2.34 or later (the image has it).

The key is a key of its own, the secret `git.signing_key`: never the deploy key, since
a host registers a key that pushes and a key that vouches in different places.
`POST /_rustybin/git/signing-key` generates an ed25519 key and answers its **public
half** and its **`SHA256:` fingerprint**; `GET` answers them again. Register the public
key at the git host as a signing key (on GitHub: SSH keys, key type "Signing key"; on
GitLab: SSH keys with usage "Signing"), then turn `git.sign` on:

```hurl
{{#include ../examples/config/git_branches.hurl:signing_key}}
```

```hurl
{{#include ../examples/config/git_branches.hurl:signing_key_generate}}
```

```bash
{{#include ../examples/config/source-control.sh:signing_key}}
```

```bash
{{#include ../examples/config/source-control.sh:signed_commit}}
```

- **A key of yours.** Set the secret instead of generating (Management, Source
  control, or `rustybin settings set-secret git.signing_key < key`): an OpenSSH private
  key without a passphrase, pasted whole. It is kept as one line (the text between the
  `BEGIN` and `END` lines), sealed like every secret, at most 4096 bytes (an ed25519
  or ECDSA key, or RSA up to 3072 bits).
- **Never shown.** Every answer carries the public half and the fingerprint only. When
  a commit is signed the key is written to `<state>/repo.signing.key` (mode `0600`) for
  git, as the deploy key is in `<state>/repo.key`.
- **Never unsigned by accident.** With `git.sign` on and no usable key (none set, one
  with a passphrase, git too old), a commit answers `409` with the reason and is not
  made. The status says so under `signing.problem`, and so does the
  `source_control_signing` check of the [diagnostics](troubleshooting.md).
- **The author** is still `git.author`; a host shows a commit as verified when that
  email belongs to the account the signing key is registered with.

Signing is about what the instance writes. What it reads is the next section.

### Verified pulls

An instance that applies its repository on a pull, a webhook or a timer applies whatever
reaches the branch. With `git.verify` on (off by default), it takes the repository's side
only when a named signer signed it. It needs git 2.38 or later and `ssh-keygen` (the
image has both).

**Who may sign** is the setting `git.allowed_signers`: one line each, a name and a public
key (`ann ssh-ed25519 AAAA...`), or the line of a `.pub` file as a git host shows it (its
comment becomes the name). They are public keys, not secrets, and the status answers
their names and `SHA256:` fingerprints. The instance's own [signing key](#signed-commits)
is allowed without being listed, while it is the current key. A line with an option
(`cert-authority`, `namespaces`) is refused: every signer is good for git signatures only.

```bash
{{#include ../examples/config/source-control.sh:verify_on}}
```

```hurl
{{#include ../examples/config/git_branches.hurl:status_verification}}
```

```hurl
{{#include ../examples/config/git_branches.hurl:signers_refused}}
```

**What is verified** is every commit between the last verified head and the new one,
not only the tip, so a signed commit on top of an unsigned one is refused:

| A commit | Passes when |
|---|---|
| With one parent (or none) | An allowed signer signed it. SSH signatures only: an OpenPGP or X.509 signature, a git host's own web signature included, reads as no signature. |
| A merge of two parents | An allowed signer signed it, or its tree is exactly what merging its two parents gives (`git merge-tree --write-tree`). So a host's merge button works, and a merge that carries a change of its own (a conflict resolved by hand, an edit slipped in) needs a signer. Both parents are verified like any commit. |
| A merge of more than two parents | An allowed signer signed it. |

**Where**: on every path that takes the repository's side. A pull (and its dry run), the
[webhook](#a-webhook-instead-of-polling), the [automatic mode](#automatic-mode), startup
from `RUSTYBIN_CONFIG_GIT`, [`resolve` with `repository`](#a-diverged-branch) and its
preview, a [switch of branch](#branches) and its preview, the first checkout of a new
working copy, and a [checkpoint](checkpoints.md) listed or restored from the repository.
Resolving with the instance's copy, a pull request branch and a push take nothing from
the repository and are not checked.

**A refusal** is `409` with `reason: unverified`, the `commit` and `why`. Nothing is
applied and the working copy does not move. The automatic mode and the webhook record
it in the timeline and apply nothing; the status keeps it under
`verification.last_refusal` and the `source_control_verification` check of the
[diagnostics](troubleshooting.md) warns until a pull passes:

```bash
{{#include ../examples/config/source-control.sh:verify_refused}}
```

```json
{
  "error": "not verified: commit 3f9c2ab has no SSH signature. Nothing was applied and the working copy did not move (...)",
  "reason": "unverified",
  "commit": "3f9c2ab0c7d1e5a4b8f6d2c9e1a7b3f5d8c6e4a2",
  "why": "has no SSH signature"
}
```

**Squash and rebase merges are refused.** A squash merge or a rebase merge made by the
git host is a new commit, signed by the host or by nobody, and no allowed signer vouches
for it. What a team does:

- **Merge pull requests with a merge commit**, and turn squash and rebase merging off for
  the repository (on GitHub: Settings, General, Pull Requests; on GitLab: Settings, Merge
  requests, Merge method "Merge commit"). The commits of the pull request are signed by
  their authors, and the merge commit passes as a clean merge.
- **Or push signed commits**: squash or rebase on your own machine, sign the result with a
  key the instance allows, and push it (a fast-forward of the branch).
- A merge with **conflicts** resolved in the host's editor carries a change of its own:
  resolve it locally and sign the merge commit.

**The last verified head** of each branch is kept in `<state>/repo.verified.json`. A
commit below it is never checked again, so replacing a key (a signer removed from the
list, a new signing key for the instance) does not reopen history: old commits stay
trusted, new ones need a key that is allowed now. A commit the instance makes itself
on a verified head is the verified head from then on, signed or not.

**Turning it on trusts the head of that moment** as it is, whatever is below it, and the
timeline says so (`verification turned on: the head 3f9c2ab is trusted as it is`); the
status answers it as `verification.trusted`. Turning it off forgets the verified heads,
so off and on again is how a person accepts a head as it stands, for example after
resolving a diverged branch with the instance's copy on top of commits nobody signed.
An instance that has `git.verify` on before it has a working copy trusts nothing: every
commit of the branch has to verify before the first checkout, so connect first and turn
verification on after.

A document from the repository never changes `git.verify` or `git.allowed_signers`
(nor any `git.*` setting), so what is pulled cannot turn its own check off. With
verification on and nobody allowed (no signer listed, no signing key), every new commit
is refused, and the status says so under `verification.problem`.

What the instance does not do: it does not trust a git host's web signature (that
would trust every commit the host makes for anyone who can click merge), and it does
not verify the commits it pushed itself at the host. Branch protection at the host is
still worth having: it stops an unsigned commit before it reaches the branch, where
this stops it before it reaches the instance.

## History

`GET /_rustybin/git/history` lists the last 50 commits of the working copy with their
author, time, message and file counts; the console shows them under the actions, with
the timeline of operations and their outcomes.

## Startup from the repository

`RUSTYBIN_CONFIG_GIT=<remote>` makes the instance clone the branch and apply the
document (with prune) before it serves, so the repository is the source of truth and a
fresh instance comes up configured:

```bash
{{#include ../examples/config/source-control.sh:second_instance}}
```

| Variable | Default | Meaning |
|---|---|---|
| `RUSTYBIN_CONFIG_GIT` | unset | The remote (seeds `git.remote`). |
| `RUSTYBIN_CONFIG_GIT_REF`, `_PATH`, `_AUTH`, `_AUTO`, `_TOKEN` | | Seed `git.branch`, `git.path`, `git.auth`, `git.auto` and the secret `git.token`. |
| `RUSTYBIN_CONFIG_GIT_REQUIRED` | `false` | A clone or apply failure at startup is fatal. By default it is a warning and the instance starts without the repository's configuration. |

An empty repository is not an error: the instance starts empty and its first push
creates the branch. The deploy key file, when the remote is ssh, is
`<state>/repo.key` (generate it once from a running instance, or put a key there).

## Automatic mode

`git.auto` keeps the instance and the repository in sync without anyone clicking:

- `push_on_change`: every 30 seconds the instance compares its export with the working
  copy's document and, when they differ, commits with a message naming the changes
  (`Update mocks.orders: added; settings.portal.enabled: changed to on`) and pushes. A
  failure is logged once until it changes. It never writes a checkpoint: those hold
  mock data and are committed only on request. With `git.sign` on its commits are
  signed.
- `pull`: every `git.pull_every` the instance pulls and applies the repository with
  prune: the repository is the truth, console changes last until the next pull.

Neither mode ever resolves a [diverged branch](#a-diverged-branch): the push or the pull
is refused, nothing is applied, and the status and the diagnostics say so until a
person chooses a way out.

Set one per instance and one direction per repository: an instance that pushes and
another that pulls is a deployment pipeline; two that push to one branch end in
conflicts for someone to resolve. One branch per environment keeps them apart
([Branches](#branches)).

The whole flow against a bare repository in a temporary directory:

```bash
{{#include ../examples/config/source-control.sh:bare_repo}}
```

```bash
{{#include ../examples/config/source-control.sh:start_instance}}
```

## A webhook instead of polling

`git.auto: pull` asks the remote every few minutes. To pull the moment someone pushes, set the
secret `git.webhook_secret` (Management, Source control, or `rustybin settings set-secret`) and
point your git host's push webhook at `https://<instance>/_rustybin/git/webhook` with the same
secret. The host signs each call (`X-Hub-Signature-256` for GitHub, Gitea, Forgejo and
Bitbucket, `X-Gitlab-Token` for GitLab), and the signature is the credential, so the instance
has to be reachable from the host but no control token is given out.

A signed push to the configured branch pulls and applies as the automatic mode does, with
everything [a repository writer can do](#what-a-repository-writer-can-do) limited the same way.
A push to another branch is ignored, a burst costs at most two pulls, and a call without
the right signature does nothing. Without the secret the route answers `404`:

```hurl
{{#include ../examples/config/git_webhook.hurl:git_webhook}}
```

## Not in this version

A merge inside the instance: a diverged branch is resolved by naming a side, or in a
pull request at the git host. Trusting a git host's own web signature when [verifying
pulls](#verified-pulls): a squash or rebase merge made by the host is refused, a merge
commit passes as a clean merge.

## On every surface

The same job from the API, the console, the command line, an agent and the configuration document.

| Surface | Where |
|---|---|
| API | `GET /_rustybin/git`, `POST /_rustybin/git/init`, `/commit` (with `checkpoints`), `/push`, `/pull`, `/resolve` (`instance`, `repository`, `pull_request`, with `dry_run`), `GET`, `POST /_rustybin/git/key`, `GET /_rustybin/git/history`, `GET /_rustybin/git/branches`, `POST /_rustybin/git/branch`, `GET`, `POST /_rustybin/git/signing-key`; `verification` in the status and the `409` with `reason: unverified` of [verified pulls](#verified-pulls); the repository's checkpoints: `GET /_rustybin/checkpoints?from=git`, `POST /_rustybin/checkpoints/{name}/restore?from=git` |
| Console | **Management, Source control**: the repository form, status (what differs on a diverged branch), connect, commit and push (with a checkpoint choice), pull, a diverged branch's three ways out with a preview, branches (list, preview, switch, start), the deploy key, commit signing, verified pulls (the switch, the signers, the last verified head, the last refusal), history; **Management, Checkpoints, In the repository** |
| CLI | `rustybin git status`, `init`, `commit` (`--checkpoint NAME`), `push`, `pull`, `resolve` (`--with instance`, `repository` or `pull-request`, `--branch`, `--dry-run`), `key`, `history`, `branches`, `switch`, `branch`, `signing-key`, `signers` (`--add`, `--remove`); `rustybin checkpoint list --from-git`, `checkpoint restore NAME --from-git` ([reference](cli.md#git)) |
| Management MCP | `git_status`, `git_init`, `git_commit` (`checkpoints`), `git_push`, `git_pull`, `git_resolve`, `git_deploy_key`, `git_branches`, `git_switch_branch`, `git_signing_key`, `list_repository_checkpoints`, `restore_checkpoint` with `from: "git"` (`git_status` answers `verification`, and the tools that take the repository's side answer the refusal: `reason`, `commit`, `why`); the prompt `backup_to_git` ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | Never: the `git.*` settings (the remote, the branch, the token, signing) are not exported, and a document from the repository cannot change one, so a document in a repository cannot point the instance elsewhere. The settings themselves: `git.branch`, `git.sign`, the secret `git.signing_key`, `git.verify`, `git.allowed_signers` |
| Environment | `RUSTYBIN_CONFIG_GIT`, `_REF`, `_PATH`, `_AUTH`, `_TOKEN`, `_AUTO`, `_REQUIRED` ([configuration](configuration.md#source-control)) |
