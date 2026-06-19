# Agent Instructions

## Specifications

Before implementing features, consult `docs/README.md`:

- **Architecture first** — Read `docs/architecture.md` for system overview
- **Check specs before coding** — Each feature has a dedicated spec file in `specs/`
- **Terminology** — `docs/README.md` has a terminology index

## Building

```bash
nix develop          # Enter devShell
nix build            # Build the loom binary
nix flake check      # Clippy + nextest
```

Inside `nix develop`, the workspace toolchain and `cargo-nextest` are on PATH:

```bash
cargo build
cargo nextest run
```

## Issue Tracking (Beads)

**Use `bd` for ALL issue tracking.** Do NOT use markdown TODOs or external trackers.

```bash
bd ready                          # Show unblocked work
bd show <id>                      # Issue details
bd create --title="..." --description="..." --type=task --priority=2
bd update <id> --status=in_progress   # Claim before starting
bd close <id>                     # Mark complete
bd dep add <issue> <depends-on>   # Add dependency
```

**Priority:** 0-4 (critical to backlog, default 2). **Types:** task, bug, feature, epic.

**Workflow:** `bd ready` → `bd update --status=in_progress` → implement → `bd close`

## Workspace Protection

For bead work, `/workspace` is the operator checkout; do not mutate it.
Work only in the per-bead clone at `.loom/beads/<id>/` on branch
`loom/<id>`, commit there, and stop. When `LOOM_INSIDE` is set, `/workspace`
is already that clone; do not create nested `.loom/beads` clones.

For non-bead work, mutate only the checkout the user explicitly names.
If it has unrelated local changes, stop and ask.

## Session Protocol

### Start

```bash
bd dolt pull
```

### End ("land the plane")

```bash
git add <files>
git commit -m "..."
git push
wrix beads push       # Sync beads branch: bd dolt commit + push + git push origin beads
```

When `LOOM_INSIDE` is set, skip the pushes; the driver publishes.
Work is NOT complete until both `main` and `beads` are pushed.

## Code Style

Read `docs/style-rules.md` before writing or reviewing code — it contains
the authoritative, enforceable rules (prefixed SH-, NX-, DOC-, GIT-, TST-, RS-, COM-).

`nix fmt` runs treefmt (nixfmt + rustfmt + shellcheck) across the tree:

```bash
nix fmt             # Format all files (works outside devShell)
nix fmt flake.nix   # Format specific file
```
