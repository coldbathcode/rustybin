# Step 23: OAuth sign-in for visitors and key holders

Waits for the owner (section 6). Recommended: not now.

## What it would be

A visitor of a self-service instance, or the holder of an API key in shared mode, connects a coding
agent to `/_rustybin/mcp` and signs it in with the browser, as an operator's agent does, instead of
pasting the manage token or the key into the client's configuration.

## Why it waits

Step 04b of the hosted demo closed this on purpose: on a self-service instance
`control_oauth::enabled` is off, "so no authorization server stands open to strangers". Building
this opens one. Nobody has asked for it, and the demo has no visitors today.

## If it is built, the shape that keeps the rules

- Off by default, its own switch, never on from a document from git.
- The token is the visit, never more: it carries the visit (the manage token's hash) or the key id,
  resolves to exactly the rights that credential has at every call, expires no later than the visit,
  and ends when the visit ends or the key is revoked. It is accepted on `/_rustybin/mcp` only, like
  every control OAuth token.
- Consent is given by the console that already holds the visit's manage token or the key. A browser
  without one is sent to start a visit first; consent never creates a credential.
- Registration stays stateless, so strangers registering clients fill nothing. The authorize and
  token endpoints are throttled per address like the sign-in, which depends on the client address
  being right (`RUSTYBIN_TRUST_FORWARD_HEADER`).
- An agent signed in this way is a visitor's agent: listed to the visitor only, never to another
  visitor, and counted in the visit's limits.
- The threat model (`docs/src/concepts/public-instances.md`) gains the endpoints, and
  `a_caller_without_a_credential_reaches_what_an_open_plane_gives` is extended to walk them.

## What decides it

- Is there a visitor who would use an agent on the demo and finds the pasted token a problem?
- Is an authorization server open to the internet acceptable on the demo, with the limits above?

## Tests that would fail first

A stranger gets no token without a visit; a visitor's token reaches only that visit's mocks; it
stops working when the visit ends; a second visitor never sees the first's agent; the throttle holds
under a forged client address.

## Done when

The owner says build it and the above is on every surface, or this row keeps saying it waits.
