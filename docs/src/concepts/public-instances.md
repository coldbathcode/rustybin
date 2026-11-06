# Public instances: the threat model

A Rustybin that strangers can reach (a shared demo URL, a self-service mock domain) faces
three kinds of caller. This page says what each can and cannot do since 0.3.1; the
[security](control-plane-security.md) and [sessions](sessions.md#public-mode) pages give
the settings.

## An anonymous caller of the data plane

Anyone may send requests to the echo routes, the mocks, the bins and the demo identity
provider, and on a self-service instance create mocks of their own.

- **Cannot take the process down or stall it.** Specs are parsed and compiled off the
  request threads, four at a time, with byte and check budgets (server variables, path
  templates, generated values, `uniqueItems`); GraphQL fragments are counted once; gRPC
  streams are bounded by the body limit and end after a minute idle or ten minutes; every
  caller without admin rights gets 30 compiles a minute (per visit, key or address), and
  a creation past a visitor's caps is refused before its spec is compiled. Schema walks,
  `$ref` expansion and generated strings have node and size budgets, a request head must
  arrive within 30 seconds, and at most 16,384 connections are open at once.
- **Cannot read another client's traffic.** Session keys derived from an address
  (`ip:...`) cannot be claimed through the session header; AI records are filtered by
  session inside the lookup; the captures of an instance mock show a caller without a
  credential its own session's calls and an API key its own, on every route and tool
  that reads them ([who reads what](../mocks/self-service.md#whose-traffic-a-caller-reads)).
- **Cannot host active content on the console's origin.** Bins answer like a visitor's
  mock: HTML, scripts and SVG as `text/plain`, no cookies, a sandbox CSP and `nosniff`.
- **Cannot exhaust shared budgets.** Visits and creations count per address and
  session (at most 20 sessions per address a day), plan limits per address in public
  mode, failed demo sign-ins per address. Bins (20), flaky counters (200), A2A task
  bytes, demo client registrations, registry token mints and portal renders are capped
  per address or in all; captures are cut to 16 KiB of headers and the ring to 64 MiB.
- **Cannot set cookies for other hosts.** On a public instance `Set-Cookie` through
  `/response-headers` and a cookie `_domain` are refused.
- **Cannot use the demo identity provider as an open redirect** or to mint tokens for
  anyone: demo clients send the browser on only to loopback or the instance's own host
  (a page with the link otherwise), and token exchange takes only the provider's own
  RS256 tokens.

## A low-rights caller of the control plane

A console-scope token, a shared-mode API key, a visitor's manage token, an AI agent with
a ceiling.

- **Cannot borrow saved credentials.** A saved MCP connection's credentials go only to
  its own target, and only for an admin. Saved tests (which may name a gateway) are read
  by an admin only, and hold `{{variables}}`, never a credential.
- **Cannot drive the control plane from another origin.** `/_rustybin/*` answers
  cross-origin only to the origins in `RUSTYBIN_CORS_ORIGINS`; sign-in attempts are
  throttled per address (an IPv6 /64) and user name, and in all.
- **Cannot change the sign-in without the password.** `console.user` and the password
  hash change only with the current password or the admin token, never through the
  management MCP or a document. The signing key is random per instance, never the admin
  token.
- **Cannot step around its own limits.** The inspector never targets the management
  server, an agent's revocation and the operator's limits follow its credential
  whatever client name it sends, an agent cannot lift the limits of its own credential,
  a control token without the admin scope is never admin, and without admin rights an
  MCP server on another host gets only read-only methods.
- **Gets third-party content only as data.** Registry entries, MCP answers, check
  reports, captured paths, methods and validation messages, and diagnostics that quote
  files or remotes reach agents under `untrusted_*`, cut and paged.
- **Cannot delete other people's mocks with a prune.** An apply or a pull prunes only the
  instance's own mocks: never a visitor's, an API key's or a running scenario's.

## Content the instance did not write

What an MCP server answers, a discovery document, an AI answer, a pulled repository.

- **The console sends its token only where it chose to.** The bearer is opt-in per call;
  a legacy MCP endpoint on another origin, OAuth metadata pointing at the control plane
  and an AI action aimed at `/_rustybin` are refused.
- **Outbound connections go where the policy checked.** Every client resolves the
  pinned name only (a trailing dot included), never through a system proxy; addresses
  carried inside IPv6 are checked as their IPv4 destination; git connects only to the
  pinned addresses, follows no redirect and runs with a cleared environment.
- **A configuration repository controls what the document describes, nothing more.**
  No symbolic links, no spec file outside the working copy, no expansion from the
  environment, no secret values in diffs, never the default password back. A pulled
  document cannot change the egress policy, the AI provider or its URL, or the trusted
  client address header, point a saved test at a new gateway, or send a stored scenario
  secret to new targets; a mock entry it cannot read is never pruned; blobs are sized
  before they are read, and the preview and the apply read the same commit.

## What survives a restart

With a state directory, sign-outs, ended sessions, sign out everywhere, password changes
and agent revocations stay in force (`<state>/sessions.json`). Without one a restart
signs every token out.
