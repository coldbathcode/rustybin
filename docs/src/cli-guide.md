# The command line

The `rustybin` binary is also the command line of a running instance: everything the
console and the control API do, as `rustybin <noun> <verb>`, with tables for people
and `--json` for scripts. The [CLI reference](cli.md) lists every command; this page
is how to use them. The offline commands for specs and load tests
(`rustybin mock ...`, `rustybin harness validate | run | report`) are on their own
pages ([mocks CLI](mocks/cli.md), [load testing](harness.md)).

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
`RUSTYBIN_URL` and `RUSTYBIN_TOKEN`, which win over the profile; with nothing set the
default is `http://localhost:80` (the server's own default, so `docker exec <container>
rustybin status` works inside the image).

`rustybin whoami` says who the credential is; `rustybin logout` revokes the token and
forgets it; `rustybin password change` takes the current and a new password (two lines of
stdin with `--stdin`) and keeps the fresh token the instance answers with.

## Nouns and verbs

```bash
{{#include ../examples/cli/cli.sh:nouns}}
```

Nouns mirror the control API: `mocks`, `requests`, `bins`, `agents`, `ai`, `mcp`,
`registry`, `portal`, `config`, `git`, `harness`, `settings`, `sessions`, `visits`,
`keys`, `egress`, `instance`. `rustybin help <noun>` lists the verbs, `--help` on any
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
thresholds, a document applied with problems), `2` usage, `3` the instance could not
be reached or the credential was refused. `--quiet` drops the progress and confirmation
lines.

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
to it); the agent sign-in is a browser flow; the harness cluster's node-to-node routes
are the nodes' business; a request bin's capture URLs are for the thing under test.
Everything else on the control plane has a verb, and a test keeps it so: a route that
lands without a command fails the build.
