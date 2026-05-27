# Pre-Commit Discipline

Hook composition policy for this project's `.pre-commit-config.yaml`:
which checks fire at commit time vs push time, and what each guarantees.
The hook plumbing (lock, shim shape, devshell wiring, install lifecycle)
is owned upstream by `wrapix.prekHooks`.

## Problem Statement

A project's commit hook is the only check authors run reliably, so the
composition has to satisfy two opposing constraints. Commits stay cheap
(~1s) so authors keep using `git commit` instead of batching, and the
integration cost (workspace-scope clippy + tests + container smoke)
runs at push time (~10s + smoke) where occasional latency is acceptable.
This spec fixes which checks land in which stage, and what each
guarantees.

## Architecture

### Stage composition

| Stage      | Wall-time target | Hooks |
|------------|------------------|-------|
| pre-commit | ~1s              | `repo: builtin` `trailing-whitespace`, `end-of-file-fixer` (excludes `.beads/config.yaml`), `check-merge-conflict`; `treefmt --fail-on-change`; `shell-reexec-explicit-interpreter` |
| pre-push   | ~10s + smoke     | `nix flake check`; container smoke (`nix run .#test`) gated on `^crates/[^/]+/tests/properties\.rs$` |

The pre-commit trio uses `repo: builtin` — prek's native Rust
implementations of the standard pre-commit hooks. The upstream
`pre-commit/pre-commit-hooks` Python repo is rejected because prek
installs Python hooks via `uv`, and `uv` downloads a glibc-linked
binary that fails ld validation on bare NixOS. `repo: builtin`
sidesteps Python entirely.

The `shell-reexec-explicit-interpreter` hook id wraps
`scripts/check-shell-reexec` as a local `language: system` hook.

### Plumbing (owned upstream)

`core.hooksPath`, the hook shim scripts, the flock that serializes
prek's stash/restore window across overlapping commits, and the
`push-verified` short-circuit stamp are all packaged in the
`wrapix.prekHooks` derivation and installed by `wrapixLib.mkDevShell`
when this project's `nix develop` is entered. The downstream project
does not maintain its own hook shims, lock script, or installation
logic.

### Source-of-truth files

This spec owns:

- `.pre-commit-config.yaml`
- `scripts/check-shell-reexec`

## Success Criteria

### Configuration

- `.pre-commit-config.yaml` declares the builtin trio via `repo: builtin`
  [check](grep -q 'repo: builtin' .pre-commit-config.yaml)
- `end-of-file-fixer` excludes `.beads/config.yaml`
  [check](grep -q '\.beads/config\.yaml' .pre-commit-config.yaml)
- `treefmt --fail-on-change` runs at the `pre-commit` stage
  [check](grep -q 'treefmt --fail-on-change' .pre-commit-config.yaml)
- The shell-reexec hook invokes `scripts/check-shell-reexec`
  [check](grep -q 'scripts/check-shell-reexec' .pre-commit-config.yaml)
- The pre-push stage includes a `nix flake check` hook with
  `always_run: true`
  [check](grep -q 'nix flake check' .pre-commit-config.yaml)
- The pre-push stage includes a container-smoke hook gated on
  `crates/*/tests/properties.rs`
  [check](grep -q 'tests/properties.rs' .pre-commit-config.yaml)

### Shell re-exec discipline

- `scripts/check-shell-reexec` exists and is executable
  [check](test -x scripts/check-shell-reexec)

## Requirements

### Functional

1. **Stage composition.** Pre-commit runs the builtin trio
   (`trailing-whitespace`, `end-of-file-fixer` with `.beads/config.yaml`
   excluded, `check-merge-conflict`), `treefmt --fail-on-change`, and
   `shell-reexec-explicit-interpreter`. Pre-push runs `nix flake check`
   unconditionally and the container smoke (`nix run .#test`, owned by
   [tests.md](tests.md)) on changes touching
   `^crates/[^/]+/tests/properties\.rs$`.

2. **Builtin trio over Python.** The trailing-whitespace,
   end-of-file-fixer, and check-merge-conflict hooks use `repo: builtin`
   so prek's native implementations run, not the
   `pre-commit/pre-commit-hooks` Python repo.

### Non-Functional

1. **Cheap commits.** Pre-commit wall-time targets ~1s so authors keep
   using `git commit` frequently rather than batching changes into
   larger commits to avoid the hook.

2. **Integration cost on push.** Slow checks fire at push time so each
   individual commit is not blocked on `nix flake check` or the
   container smoke.

## Out of Scope

- **Hook plumbing.** Locks, shim shape, install lifecycle, and the
  `push-verified` short-circuit are owned upstream by
  `wrapix.prekHooks`. Failures in serialization, prek shim
  regeneration, or `core.hooksPath` wiring belong to that project.

- **Per-user `pre-commit install`.** Installation flows through
  `wrapixLib.mkDevShell` exclusively. The
  `.pre-commit-config.yaml` shape is portable but documenting a
  manual-install path is not this spec's concern.

- **Container smoke runner.** `nix run .#test` is owned by
  [tests.md](tests.md); this spec only specifies when the hook fires
  it.

- **Worker-context hook firing.** Per-bead workspaces are created via
  `git clone --local` (see [harness.md — Worktree Dispatch](harness.md)),
  not linked worktrees, so worker-side commits do not inherit the main
  checkout's `core.hooksPath`. Hooks fire on the driver-side merge-back
  in the main checkout. Whether worker contexts should also run hooks
  is a question for harness.md.

- **Catching "agent forgot to stage" / dirty-tree-after-completion.**
  Pre-commit hooks' stash/restore architecture intentionally hides
  unstaged changes from the hook, so a `treefmt`-style check at commit
  time cannot observe a worker that staged only its own paths and left
  other tracked files dirty. That failure mode is owned by
  [harness.md — Verdict Gate](harness.md) as the `tree-not-clean`
  recovery cause.
