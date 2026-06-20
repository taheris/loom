# Agent Instructions

## Start

- Run `bd dolt pull`.
- For feature/spec work, read `docs/README.md`, then the linked architecture,
  specs, terminology, and `docs/style-rules.md`.

## Beads

Use `bd` only; no markdown TODOs.

```bash
bd ready
bd show <id>
bd create --title="..." --description="..." --type=task --priority=2
bd update <id> --status=in_progress
bd close <id>
bd dep add <issue> <depends-on>
```

Flow: ready → claim → implement → close. Priorities: 0-4. Types: `task`,
`bug`, `feature`, `epic`.

## Workspaces

- Bead work: use `.loom/beads/<id>/` on branch `loom/<id>`.
- If `LOOM_INSIDE` is set, `/workspace` is already the bead clone; do not nest.
- Non-bead work: edit only the named checkout; stop on unrelated local changes.

## Land

If `LOOM_INSIDE` is set, skip pushes; the driver publishes. Otherwise:

```bash
# in .loom/beads/<id>/
git add <files>
git commit -m "..."

# in /workspace/.loom/integration
git checkout main
git pull --ff-only origin main
git fetch /workspace/.loom/beads/<id> HEAD
git merge --ff-only FETCH_HEAD
git push origin main

cd /workspace
wrix beads push
```

Do not push `loom/<id>` branches. Work is complete only after main and beads are
pushed.

## Verify

```bash
nix fmt
nix build
nix flake check
cargo build
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run
```
