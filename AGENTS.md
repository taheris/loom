# Agent Instructions

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
beads-push # beads branch sync to GitHub
```

Work is NOT complete until both `main` and `beads` are pushed.

## Code Style

Read `docs/style-rules.md` before writing or reviewing code — it contains
the authoritative, enforceable rules (prefixed SH-, NX-, DOC-, GIT-, TST-, RS-, COM-).

`nix fmt` runs treefmt (nixfmt + rustfmt + shellcheck) across the tree:

```bash
nix fmt             # Format all files (works outside devShell)
nix fmt flake.nix   # Format specific file
```
