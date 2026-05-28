# Agent Instructions

## Building

```bash
nix develop          # Enter devShell
nix build            # Build the loom binary
nix run -- --help    # CLI overview
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

Skip this entire block when `$LOOM_INSIDE` is set — inside a loom-managed
bead clone, `origin` points at the local driver workdir (not GitHub), and
`.git/beads-worktrees/beads` does not exist in the clone. The driver
publishes `main` + `beads` itself after a Clean review verdict. Manual
sessions in the driver workdir (where `$LOOM_INSIDE` is unset) keep the
existing protocol:

```bash
git add <files>
git commit -m "..."
git push
beads-push                        # bd dolt + beads-branch sync to GitHub
```

Work is NOT complete until both `main` and `beads` are pushed.

> **Known dolt-sync race.** The wrapix-shipped `beads-push` runs
> `bd dolt commit || true` → `bd dolt pull` → `bd dolt push`. Under
> concurrent writes, the interior `pull` can pick the remote's
> pre-write state over a local `bd close` / `bd update` and silently
> revert it. If you observe a local bd write disappearing after
> `beads-push`, re-apply the write and re-run `beads-push` once the
> remote has caught up. Upstream wrapix fix tracked as a sibling
> bead under the harness epic; the workflow does not work around it
> inline because `loom-workflow` may not invoke `bd dolt` directly
> (the bind-mounted Dolt socket is the only authoritative path —
> see `crates/loom-workflow/tests/no_bd_dolt.rs`).

## Code Style

Read `docs/style-rules.md` before writing or reviewing code — it contains
the authoritative, enforceable rules (prefixed SH-, NX-, DOC-, GIT-, TST-, RS-, COM-).

`nix fmt` runs treefmt (nixfmt + rustfmt + shellcheck) across the tree:

```bash
nix fmt             # Format all files (works outside devShell)
nix fmt flake.nix   # Format specific file
```
