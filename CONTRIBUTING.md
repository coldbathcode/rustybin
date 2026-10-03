# Contributing to Rustybin

Thanks for helping! Bug reports, gateway recipes, docs fixes and new protocol
mocks are all welcome.

## Before your first pull request

Rustybin is dual licensed (AGPL-3.0 and commercial, see
[LICENSING.md](LICENSING.md)). To keep that possible, every contributor signs
the [Contributor License Agreement](CLA.md) once. The CLA bot comments on your
first pull request with instructions; signing is a one-line comment. You keep
the copyright in your work.

If you contribute as part of your job, check that your employer allows it
(CLA section 4).

## How to contribute

1. Open an issue first for anything larger than a small fix, so we can agree
   on the approach.
2. Read [CLAUDE.md](CLAUDE.md) for the architecture and house rules: the route
   catalogue, bounded state, no panics on request paths, no em or en dash
   characters.
3. Run before pushing:

   ```bash
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```

4. If you change protocol behaviour, run the matching suite under
   `conformance/` (each has a README).
5. Keep pull requests focused; describe what changed and how you tested it.

## Reporting security issues

Please do not open a public issue for a vulnerability. Contact the maintainer,
[@oliwynn](https://github.com/oliwynn) on GitHub, privately first.
