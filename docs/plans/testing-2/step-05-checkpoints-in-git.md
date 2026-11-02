# Step 05: Checkpoints in the configuration repository

## Goal

A team that keeps its instance in git (0.2, step 09) keeps its demo checkpoints there too: a
checkpoint is written next to `rustybin.yaml`, committed with it, and restored on another instance
after a pull.

## Model

- `<repo>/<path>/checkpoints/<name>.json`, the same format as `<state>/checkpoints/`.
- `POST /_rustybin/git/commit` takes `checkpoints: [names]` (default none); a pull lists the
  repository's checkpoints and `POST /_rustybin/checkpoints/{name}/restore?from=git` restores one.
- Never written by the automatic mode; never a secret (a checkpoint has none; mock demo
  credentials are part of mocks and follow the export's `secrets` mode).
- A checkpoint read from git is untrusted input: size-capped, validated, compiled on the pool.

## Surfaces

API, console (Checkpoints: "in the repository"; Source control: the commit dialog's checkpoint
choice), management MCP (`git_commit` gains `checkpoints`; `restore_checkpoint` gains `from`), CLI
(`git commit --checkpoint NAME`, `checkpoint restore NAME --from-git`), `rustybin.yaml`: nothing.

## Tests

Commit with a checkpoint, pull on a second instance, restore: the same mocks and data; a
checkpoint over the size cap from git is refused; the automatic mode never writes one.
