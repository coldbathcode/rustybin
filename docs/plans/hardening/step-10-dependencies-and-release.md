# Step 10: Dependencies, docs, release 0.2.2

## Goal

Clear the advisories that have a path, drop the duplicate axum, and ship 0.2.2 with a changelog
entry that tells operators what changed in behaviour.

## Dependencies (`cargo audit`, 2026-10-30)

| Crate | Advisory | Action |
|---|---|---|
| `rsa` 0.9.10 | RUSTSEC-2023-0071, Marvin timing attack, no fix | keep the CI ignore (review 2027-04-01); confirm RSA private keys are used only for the demo IdP and never decrypt attacker input |
| `serde_yaml` via `unsafe-libyaml` 0.2.11 | RUSTSEC-2024-0449, unmaintained (`serde_yaml` itself is deprecated) | move to a maintained fork (`serde_yml` or `serde_norway`) after snapshot tests pin the YAML output (exports and git diffs must not churn) |
| `rustls-pemfile` 2.2.0 | RUSTSEC-2025-0134, unmaintained | `rustls-pki-types` PEM parsing (`PemObject`) |
| `rand` 0.8.5 | RUSTSEC-2026-0097, unsound with a custom logger | `rand` 0.9 (API renames: `thread_rng` to `rng`, `gen` to `random`) |
| `tokio` 1.52.0 | RUSTSEC-2026-0386, `AsyncFd::new` unsound | update to the patched release |
| `anyhow` 1.0.102 | RUSTSEC-2026-0190, `downcast_mut` unsound | `cargo update -p anyhow` |
| `tonic` 0.12 | pulls axum 0.7 next to the app's axum 0.8 | tonic 0.13 or later (axum 0.8); check MSRV 1.88 and `async-graphql` pins |

## Release

- `docs/src/changelog.md`: a 0.2.2 entry, with a "Behaviour changes" list (saved connections bound
  to their target, the inspector refusing `/_rustybin/mcp`, the console bearer opt-in, the trusted
  forward header setting, bins served inert, pulled documents not expanded, sessions surviving a
  restart).
- `docs/src/security.md` (or the nearest page): a short threat model for public instances that
  names what each finding class now guarantees.
- `docs/src/roadmap.md`: move the plan to "Recently built".
- Version bump, tag `v0.2.2`, the signed image, the digest commit (the 0.2.0 recipe).

## Done when

`cargo audit` reports only the documented `rsa` ignore, `cargo tree -d` shows one axum, every step
above is done, and the release is tagged.
