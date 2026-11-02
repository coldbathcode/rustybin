# Step 09: Docs and release 0.3.1

The changelog's `0.3.1` entry (behaviour changes first: control-plane CORS, admin-only saved tests,
the new signing key, prune scope), the threat model page updated, the docs pages each step touched,
`0.3.1` in Cargo.toml, and the release (tag, signed image, GitHub release, digest commit) once the
owner says go.

## Build notes (2026-11-02)

- The changelog's `0.3.1` entry puts the behaviour changes first (the signing key, control-plane CORS, the
  sign-in, saved tests, prune scope, untrusted documents, compiles, deploy revisions, `untrusted_*`, public mode,
  the CLI), then the hardening. The threat model (`concepts/public-instances.md`) now says what holds since 0.3.1.
- `0.3.1` in Cargo.toml and the OpenAPI version example.
- Two checks still assumed 0.3.0 behaviour: the A2A docs example asserted the sink's notification token as a
  value (it is a hint now), and the console conformance reset the password with a sign-in token's `DELETE` (it
  takes `current_password` now).
- Checks: fmt, clippy (default and stable toolchains), the full suite, MSRV 1.88, the dash check, the docs
  examples, the includes, the book, and the ui, mcp, oauth and ai conformance suites.
