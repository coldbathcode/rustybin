# The command line

The `rustybin` binary is also the command line of a running instance: everything the
console and the control API do, as `rustybin <noun> <verb>`, with tables for people
and `--json` for scripts. The [CLI reference](cli.md) lists every command; this page
is how to use them. The offline commands for specs and load tests
(`rustybin mock ...`, `rustybin harness validate | run | report`) are on their own
pages ([mocks CLI](mocks/cli.md), [load testing](harness.md)).

## Install

The command line is the `rustybin` binary itself: the same file runs a server
(`rustybin` with no command) and talks to one (`rustybin <noun> <verb>`). Install it
on the machine you run commands from; the instance it manages can be anywhere.

**A release binary** (Linux x86_64 and arm64, macOS on Apple silicon and Intel). Each
[GitHub release](https://github.com/coldbathcode/rustybin/releases) carries
`rustybin-<version>-<target>.tar.gz` and its `.sha256`:

```bash
VERSION=v0.4.0
TARGET=aarch64-apple-darwin   # or x86_64-apple-darwin, x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu
curl -fsSLO "https://github.com/coldbathcode/rustybin/releases/download/$VERSION/rustybin-$VERSION-$TARGET.tar.gz"
curl -fsSLO "https://github.com/coldbathcode/rustybin/releases/download/$VERSION/rustybin-$VERSION-$TARGET.tar.gz.sha256"
shasum -a 256 -c "rustybin-$VERSION-$TARGET.tar.gz.sha256"
tar -xzf "rustybin-$VERSION-$TARGET.tar.gz"
sudo install "rustybin-$VERSION-$TARGET/rustybin" /usr/local/bin/   # or any directory on your PATH
rustybin --version
```

On macOS, an archive downloaded with a browser (not `curl`) is quarantined: run
`xattr -d com.apple.quarantine /usr/local/bin/rustybin` once. The Linux binaries need
glibc 2.35 or later (Ubuntu 22.04, Debian 12, RHEL 9 and newer); elsewhere use the image
or build from source. On Windows, use WSL or the image.

**From source** with Rust 1.88 or later (no other tool is needed: `protoc` is vendored):

```bash
cargo install --locked --git https://github.com/coldbathcode/rustybin --tag v0.4.0
```

**The container image**, with nothing installed: the image's entry point is the binary,
so a command is the image's arguments. Keep the profiles on the host with a volume:

```bash
alias rustybin='docker run --rm -it -v "$HOME/.config/rustybin:/config" -e RUSTYBIN_CONFIG_HOME=/config ghcr.io/coldbathcode/rustybin:latest'
rustybin login --url https://mocks.example.com
```

From inside that container, `localhost` is the container itself: an instance on your
machine is `http://host.docker.internal:8080` (Docker Desktop) or needs `--network
host` (Linux). Inside a running instance's container the binary is already there:
`docker exec <container> rustybin status` talks to that instance (see the default URL
below).

Then turn on [completion](#completion) for your shell and [sign in](#sign-in-and-profiles).

## Sign in and profiles

```bash
{{#include ../examples/cli/cli.sh:login}}
```

`rustybin login --url <url>` asks for the console password (`--user` for another user
name, `--password-stdin` to read it from the first line of stdin, as in CI) and keeps the
token in a profile named after the host, in `profiles.json` under
`RUSTYBIN_CONFIG_HOME` (else `$XDG_CONFIG_HOME/rustybin`, else `~/.config/rustybin`;
mode `0600`). The last login is the default profile, so the next commands need no URL.
`rustybin login --token <token>` keeps a token you already have (the admin token, a
control JWT) instead of signing in.

Several instances are several profiles: `--profile staging` on the login names one,
`rustybin profiles list | use | remove` manage them, and `--profile` (or
`RUSTYBIN_PROFILE`) picks one for a call. For a call, `--url` and `--token` win over
`RUSTYBIN_URL` and `RUSTYBIN_TOKEN` (else `RUSTYBIN_ADMIN_TOKEN`), which win over the
profile; with nothing set the default is `http://localhost:80` (the port from
`RUSTYBIN_HTTP_PORT` when set: the server's own default, so `docker exec <container>
rustybin status` works inside the image).

A token from the environment goes to a `--url` only when it is `RUSTYBIN_URL`'s instance
or one on this machine (loopback), so an exported local admin token is never handed to
another host; a profile's token goes only to its own URL. The command line warns when it
sends a token over plain `http://` to another host, and it follows no redirects.

`rustybin whoami` says who the credential is; `rustybin logout` revokes the token and
forgets it; `rustybin passwd` changes your own password (the current and a new one, two
lines of stdin with `--stdin`) and keeps the fresh token the instance answers with
(`rustybin password change` is the 0.3 command, which also renames the first admin).
`rustybin me` shows your account, `me update` your display name or email, `me
sessions` where you are signed in.

## Users and roles

```bash
{{#include ../examples/cli/cli.sh:users}}
```

`rustybin users invite NAME --role R` prints the invitation link once: send it to the
person, who sets their own password ([users and roles](users.md)). `users update`
changes roles (`--role`, repeatable or comma separated, replaces them), the display
name or the email; `users disable` (asks first), `enable`, `delete` (asks first),
`users sessions NAME` (`--end` signs them out everywhere) and `users invite NAME
--reset` (a new link, a password reset). `rustybin roles list | show | save | delete`
manage custom roles (`--permission`, repeatable or comma separated).

## Nouns and verbs

```bash
{{#include ../examples/cli/cli.sh:nouns}}
```

Nouns mirror the control API: `mocks`, `requests`, `bins`, `tests`, `scenario`,
`checkpoint`, `agents`, `ai`, `mcp`, `registry`, `portal`, `config`, `git`, `harness`,
`settings`, `sessions`, `users`, `roles`, `me`, `visits`, `visit`, `keys`, `egress`,
`instance`, `profiles`. `rustybin help <noun>` lists the verbs, `--help` on any
command its flags and an example. Verbs that remove or replace something (`delete`,
`clear`, `revoke`, `end-all`, `apply --prune`, `git resolve`) ask first; `--yes` answers
for a script, and without a terminal they refuse rather than wait.

Files are read by the command line, so `rustybin mocks create orders --spec orders.yaml`
works from anywhere; `-` reads stdin; flags that take JSON (`--settings`, `--params`,
`--input`) take it inline or as a file path. `rustybin mocks apply rustybin.yaml` inlines
the spec files the document references, as `rustybin mock push` does.

## Scripts and CI

```bash
{{#include ../examples/cli/cli.sh:json}}
```

`--json` prints the instance's answer as it is; `jq` does the rest. Exit codes: `0`
done, `1` the instance refused or the operation failed (a 4xx or 5xx, failed
thresholds, a document applied with problems, a failed `doctor` check, saved test or
scenario expectation), `2` usage, `3` the instance could not
be reached, the credential was refused, or it lacks a permission the command needs:
the message starts `permission_required: mocks.write` (the [CLI reference](cli.md)
names each command's permission in its **Needs** column). `--quiet` drops the progress
and confirmation lines.

The tails (`requests tail`, `mocks traffic-tail`, `bins tail`, `agents tail`,
`harness events`) follow a Server-Sent Events feed and print one line per event;
`--max N` stops after N events and `--timeout S` after S seconds, so a script can wait
for the one request it expects:

```bash
{{#include ../examples/cli/cli.sh:tail}}
```

`rustybin config export` and `rustybin config apply --dry-run` are the configuration
as code loop ([the whole instance as code](configuration-as-code.md)); `rustybin git
commit -m "..." && rustybin git push` commits it ([source control](source-control.md));
`rustybin harness run smoke.yaml --remote --wait` starts a load test on the instance,
follows it and exits with the report's result ([load testing](harness.md)).

## Completion

`rustybin completion bash | zsh | fish` prints a completion script for the nouns and
verbs: `source <(rustybin completion bash)` in `.bashrc`, or save the fish one under
`~/.config/fish/completions/rustybin.fish`.

## What the command line does not do

The management MCP endpoint is for agents (`rustybin mcp-stdio` bridges a stdio client
to it); the agent sign-in and an invitation link (`POST /_rustybin/join`) are browser
flows; the harness cluster's node-to-node routes
are the nodes' business; a request bin's capture URLs are for the thing under test.
Everything else on the control plane has a verb, and a test keeps it so: a route that
lands without a command fails the build.

## On every surface

The same job from the API, the console, the command line, an agent and the configuration document.

| Surface | Where |
|---|---|
| API | Every command calls the control plane: the **Calls** column of the [reference](cli.md) names the routes |
| Console | The same sign-in (`rustybin login` takes the console's user name and password) and the same sessions (**Management, Sign-in** lists the CLI's token and can end it) |
| CLI | This page |
| Management MCP | `rustybin mcp-stdio --url <url> --profile <p>` bridges a local agent to a remote instance's management server with the profile's token; the resource `rustybin://cli.md` is this reference ([agent page](agent-mcp.md)) |
| `rustybin.yaml` | `rustybin config export > rustybin.yaml`, `rustybin config apply rustybin.yaml --dry-run` ([the whole instance as code](configuration-as-code.md)) |
| Environment | `RUSTYBIN_URL`, `RUSTYBIN_TOKEN` (else `RUSTYBIN_ADMIN_TOKEN`), `RUSTYBIN_HTTP_PORT` (the default URL's port), `RUSTYBIN_PROFILE`, `RUSTYBIN_MOCK_TOKEN`, `RUSTYBIN_CONFIG_HOME` (where `profiles.json` lives) |
