# Step 10: Docs and release

## Goal

Release whatever steps 01 to 09 built, with the book current, as the release after core-next's
0.8.0 (0.9.0), or folded into 0.8.0 if the owner says so (master plan section 6).

## Work

- The book: the new pages in `docs/src/SUMMARY.md` under the jobs they serve (AI and MCP gateway
  proof, mocking a customer's API, presenting), each with its "On every surface" table; the
  introduction and the AI and MCP index pages point to them.
- Every new route has a tested example in `docs/examples/run.sh`; `python3 docs/tools/check_includes.py`
  passes; `mdbook build docs`.
- The CLI reference regenerated (`RUSTYBIN_UPDATE_CLI_DOCS=1 cargo test cli_docs`) and the README
  tables (`RUSTYBIN_UPDATE_README=1 cargo test readme_endpoints`).
- The SDK and console conformance green (`conformance/ui`), the harness conformance if step 07
  touched the harness.
- `docs/src/roadmap.md`: the built steps move to "Recently built"; a step answered no stays under
  consideration with its reason.
- The changelog's "Unreleased" section becomes the version; the tag; the signed image and its digest
  in the changelog; the binaries workflow.
- CLAUDE.md gains a section for this milestone with the rules the steps left.

## Done when

The tag and the signed image exist, the digest is in the changelog, and the master plan's section 4
says done with the date.
