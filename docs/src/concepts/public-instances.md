# Public instances: the threat model

A Rustybin that strangers can reach (a shared demo URL, a self-service mock domain) faces
three kinds of caller. This page says what each can and cannot do since 0.2.2; the
[security](control-plane-security.md) and [sessions](sessions.md#public-mode) pages give
the settings.

## An anonymous caller of the data plane

Anyone may send requests to the echo routes, the mocks, the bins and the demo identity
provider, and on a self-service instance create mocks of their own.

- **Cannot take the process down or stall it.** Specs are parsed and compiled off the
  request threads, four at a time, with byte and check budgets (server variables, path
  templates, generated values, `uniqueItems`); GraphQL fragments are counted once; gRPC
  streams are bounded by the body limit; a visitor gets 30 compiles a minute.
- **Cannot read another client's traffic.** Session keys derived from an address
  (`ip:...`) cannot be claimed through the session header; AI records are filtered by
  session inside the lookup.
- **Cannot host active content on the console's origin.** Bins answer like a visitor's
  mock: HTML, scripts and SVG as `text/plain`, no cookies, a sandbox CSP and `nosniff`.
- **Cannot exhaust shared budgets.** Visits and creations count per address and
  session (at most 20 sessions per address a day), plan limits per address in public
  mode, failed demo sign-ins per address.
- **Cannot use the demo identity provider as an open redirect** or to mint tokens for
  anyone: demo clients send the browser on only to loopback or the instance's own host
  (a page with the link otherwise), and token exchange takes only the provider's own
  RS256 tokens.

## A low-rights caller of the control plane

A console-scope token, a shared-mode API key, a visitor's manage token, an AI agent with
a ceiling.

- **Cannot borrow saved credentials.** A saved MCP connection's credentials go only to
  its own target, and only for an admin.
- **Cannot step around its own limits.** The inspector never targets the management
  server, an agent's revocation and the operator's limits follow its credential
  whatever client name it sends, a control token without the admin scope is never
  admin.
- **Gets third-party content only as data.** Registry entries, MCP answers and check
  reports reach agents under `untrusted_*`, cut and paged.

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
  environment, no secret values in diffs, never the default password back.

## What survives a restart

With a state directory, sign-outs, ended sessions, sign out everywhere, password changes
and agent revocations stay in force (`<state>/sessions.json`). Without one a restart
signs every token out.
