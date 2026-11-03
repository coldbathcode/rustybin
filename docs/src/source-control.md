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
Nothing more:

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

**Conflicts.** The instance never merges. When the branch has diverged (commits here
and commits there), push and pull answer `409` with `conflict`, `ahead` and `behind`,
and `POST /_rustybin/git/resolve` `{with}` offers the two ways out: `instance`
replaces the repository's copy with this instance's (the local commits are dropped,
the instance's configuration is committed on top of the remote and pushed; `message`
names the commit) and `repository` replaces this instance with the repository's (the
remote branch is taken and applied with prune). The console shows both buttons under
the error.

```bash
{{#include ../examples/config/source-control.sh:diverged}}
```

```bash
{{#include ../examples/config/source-control.sh:resolve}}
```

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
  mock data and are committed only on request.
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
pull request), branches per environment, signed commits, and webhooks that trigger a
pull (set `git.auto: pull` instead).

## On every surface

The same job from the API, the console, the command line, an agent and the configuration document.

| Surface | Where |
|---|---|
| API | `GET /_rustybin/git`, `POST /_rustybin/git/init`, `/commit` (with `checkpoints`), `/push`, `/pull`, `/resolve`, `GET`, `POST /_rustybin/git/key`, `GET /_rustybin/git/history`; the repository's checkpoints: `GET /_rustybin/checkpoints?from=git`, `POST /_rustybin/checkpoints/{name}/restore?from=git` |
| Console | **Management, Source control**: the repository form, status, connect, commit and push (with a checkpoint choice), pull, resolve, the deploy key, history; **Management, Checkpoints, In the repository** |
| CLI | `rustybin git status`, `init`, `commit` (`--checkpoint NAME`), `push`, `pull`, `resolve`, `key`, `history`; `rustybin checkpoint list --from-git`, `checkpoint restore NAME --from-git` ([reference](cli.md#git)) |
| Management MCP | `git_status`, `git_init`, `git_commit` (`checkpoints`), `git_push`, `git_pull`, `git_resolve`, `git_deploy_key`, `list_repository_checkpoints`, `restore_checkpoint` with `from: "git"`; the prompt `backup_to_git` ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | Never: the `git.*` settings (the remote, the token) are not exported, so a document in a repository cannot point the instance elsewhere |
| Environment | `RUSTYBIN_CONFIG_GIT`, `_REF`, `_PATH`, `_AUTH`, `_TOKEN`, `_AUTO`, `_REQUIRED` ([configuration](configuration.md#source-control)) |
