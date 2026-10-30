# Source control

An instance can keep its configuration ([the whole instance as code](configuration-as-code.md))
in a git repository: commit and push it from **Management, Source control**, pull and
apply the repository's version with a dry run first, start from the repository so it
is the source of truth and the backup, and let the instance push every change or pull
at an interval.

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

## Connect a repository

The `git.*` settings (**Management, Source control**, or `PATCH /_rustybin/settings`;
seeded by the `RUSTYBIN_CONFIG_GIT*` variables):

| Setting | Variable | Default | Meaning |
|---|---|---|---|
| `git.remote` | `RUSTYBIN_CONFIG_GIT` | unset | `https://host/org/repo.git` (with a token), `ssh://git@host/org/repo.git` or `git@host:org/repo.git` (with the deploy key), or a local path. Never with credentials in the URL. |
| `git.branch` | `RUSTYBIN_CONFIG_GIT_REF` | `main` | The branch the instance commits to and pulls from. |
| `git.path` | `RUSTYBIN_CONFIG_GIT_PATH` | `rustybin` | The directory in the repository that holds `rustybin.yaml` and `specs/` (empty for the root). |
| `git.auth` | `RUSTYBIN_CONFIG_GIT_AUTH` | `token` | `token` for an https remote, `deploy_key` for an ssh remote. |
| `git.token` (secret) | `RUSTYBIN_CONFIG_GIT_TOKEN` | unset | A personal access or deploy token. The instance hands it to git through `GIT_ASKPASS`, never on a command line or in a URL, and never shows it again. |
| `git.username` | | `x-access-token` | The user name sent with the token (GitHub accepts any; GitLab deploy tokens have their own; Bitbucket wants the account). |
| `git.author` | | `Rustybin <rustybin@example.invalid>` | `Name <email>` of the commits. |
| `git.auto` | `RUSTYBIN_CONFIG_GIT_AUTO` | `off` | `push_on_change` or `pull`, see [Automatic mode](#automatic-mode). |
| `git.pull_every` | | `5m` | The interval of the pull mode (`1m` to `24h`). |

**Deploy key.** For an ssh remote, **Generate the deploy key** (`POST
/_rustybin/git/key`) creates an ed25519 key in `<state>/repo.key` (mode `0600`) and
shows its public half to add to the repository (a deploy key with write access on
GitHub or GitLab). The private half never leaves the instance; `{"replace": true}`
generates a new one. The remote's host key is accepted on first use into
`<state>/repo.known_hosts`.

**Connect** (`POST /_rustybin/git/init`) clones the branch into the working copy, or
initialises an empty working copy when the remote has no such branch yet (the first
push creates it). An existing working copy is pointed at the remote.

Every operation is admin-only, audited (`rustybin::git` log lines, the timeline in the
status), serialised by one lock, and runs git with a timeout, `GIT_TERMINAL_PROMPT=0`
and no global or system git configuration.

## Status

`GET /_rustybin/git` (admin; `?fetch=true` asks the remote) answers the configuration,
whether git is installed and the working copy exists, the last commit, ahead and
behind against the remote, the **drift** (the changes a commit would record: the
instance's export diffed against the working copy's document, as lines such as
`mocks.orders: spec changed (2 operation(s) added)`), the deploy key's public half and
the timeline of operations.

```bash
{{#include ../examples/config/source-control.sh:status}}
```

## Commit and push

`POST /_rustybin/git/commit` `{message}` exports the instance as a bundle into
`<repo>/<git.path>/` (`rustybin.yaml` and `specs/`, specs as files so a reviewer
diffs them) and commits; nothing to commit is an answer, not an error. `POST
/_rustybin/git/push` pushes the branch.

```bash
{{#include ../examples/config/source-control.sh:commit_push}}
```

In the console, **Commit and push** does both; **Write the commit message** asks the
AI for one from the drift summary ([Bring your own AI](ai-assist.md)).

## Pull and apply

`POST /_rustybin/git/pull` `{apply, dry_run, prune}` fetches and fast-forwards the
working copy; with `apply` it applies the repository's document through [the
configuration document's apply](configuration-as-code.md#apply-with-a-dry-run-first)
(`spec:` files resolve in the working copy). A dry run reads the document from the
remote branch without moving the working copy and answers the diff; `prune` makes the
document the whole truth. The console shows the diff first, then **Pull and apply**.

```bash
{{#include ../examples/config/source-control.sh:pull_dry_run}}
```

**Conflicts.** The instance never merges. When the branch has diverged (commits here
and commits there), push and pull answer `409` with `conflict`, `ahead` and `behind`,
and `POST /_rustybin/git/resolve` `{with}` offers the two ways out: `instance`
replaces the repository's copy with this instance's (the local commits are dropped,
the instance's configuration is committed on top of the remote and pushed; `message`
names the commit) and `repository` replaces this instance with the repository's (the
remote branch is taken and applied with prune). The console shows both buttons under
the error.

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
  failure is logged once until it changes.
- `pull`: every `git.pull_every` the instance pulls and applies the repository with
  prune: the repository is the truth, console changes last until the next pull.

Set one per instance and one direction per repository: an instance that pushes and
another that pulls is a deployment pipeline; two that push to one branch end in
conflicts for someone to resolve.

The whole flow against a bare repository in a temporary directory:

```bash
{{#include ../examples/config/source-control.sh:bare_repo}}
```

```bash
{{#include ../examples/config/source-control.sh:start_instance}}
```

## Not in this version

Merges and conflict resolution inside the instance (resolve with either side, or in a
pull request), branches per environment, signed commits, webhooks that trigger a pull
(set `git.auto: pull` instead), and the `rustybin config` and `rustybin git` commands
of the CLI (the next step of the plan).
