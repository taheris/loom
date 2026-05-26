# Pre-Commit Discipline

Git-hook plumbing that runs fast checks on commit and slow checks on
push, serialized by a workspace-level flock to avoid prek's
stash/restore race when multiple agents share a worktree.

## Problem Statement

Prek's pre-commit hook stashes unstaged changes before running its
checks, then restores the stash after. Two writers opening that stash
window concurrently in the same worktree can race and silently drop
working-tree edits. Universal worktree isolation
([harness.md — Worktree Dispatch](harness.md)) ensures each loom worker
bead has its own worktree, so workers don't contend with each other or
with the developer's main checkout — but the main checkout itself
remains a contention point: the driver's sequential merge-back step
(which can fire pre-commit on a merge commit), a developer's manual
`git commit`, and editor/tooling auto-commits can all open stash
windows concurrently in the same main checkout. The discipline this
spec defines is: every git hook serializes through a workspace flock
before running prek's `hook-impl`, so only one stash window is open
at a time per workspace; and hook composition is staged into fast
pre-commit checks (~1s) and slow pre-push checks (~10s) so commits
stay cheap and push absorbs the integration cost.

## Architecture

### Hook installation

Hook scripts are versioned in `lib/prek/hooks/` and wired into git via
`core.hooksPath`, set by the `shellHook` in `nix/flake/devshell.nix`
when the developer enters `nix develop`. No per-user `pre-commit
install` step is required.

Other prek hook stages (`prepare-commit-msg`, `post-commit`,
`post-checkout`, `post-merge`) remain under `.git/hooks/` as
prek-managed local hooks — they don't open a stash window and
therefore don't need the flock.

### Lock serialization

The flock implementation lives in `lib/prek/lock.sh`, sourced by both
`pre-commit` and `pre-push` hook shims. Key properties:

- **Path** — `${XDG_STATE_HOME:-$HOME/.local/state}/loom/prek/<workspace-basename>/prek.lock`.
  `<workspace-basename>` is the basename of the main worktree's path
  (the first `worktree` entry in `git worktree list --porcelain`),
  matching harness.md's keying for per-spec locks under
  `loom/locks/<workspace-basename>/`. Deriving from the main worktree
  rather than `git rev-parse --show-toplevel` is load-bearing: a
  linked worktree's `--show-toplevel` returns its own path, whose
  basename differs from the main checkout's and would split the lock
  namespace. The prek lock lives in a sibling
  `prek/<workspace-basename>/` subtree rather than inside harness.md's
  `locks/` namespace, so prek's host-side state stays separate from
  loom-driver's per-spec advisory locks. **All paths
  live on the host filesystem, outside the workspace**, per
  harness.md's lock-placement invariant — bead containers with the
  workspace bind-mounted cannot `rm` the lock out from under the
  host hook.
- **Descriptor** — FD 9, exclusive, inherited across `exec` (so the
  lock is held for the full hook-impl child duration).
- **Timeout** — 600 seconds with 1-second polling. The current
  holder's PID is printed to stderr while waiting.
- **Dead-PID recovery** — if the PID stamped in the lock file is no
  longer alive, the lock file is deleted and re-acquired on a fresh
  inode rather than blocking on a ghost holder.
- **Subprocess discipline** — callers that spawn subprocesses (rather
  than `exec`) must close FD 9 (`9>&-`) on the child so the lock
  doesn't outlive the hook through an orphaned grandchild.

### Hook stages

| Stage      | Wall-time target | Hooks                                                                                                                                                            |
|------------|------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| pre-commit | ~1s              | builtin `trailing-whitespace`, `end-of-file-fixer` (excludes `.beads/config.yaml`), `check-merge-conflict`; `treefmt --fail-on-change`; `shell-reexec-explicit-interpreter` |
| pre-push   | ~10s + smoke     | `nix flake check`; `nix run .#test` (container smoke) when changes touch `^crates/[^/]+/tests/properties\.rs$`                                                     |

The `shell-reexec-explicit-interpreter` hook id is the
`.pre-commit-config.yaml` entry that wraps the
`scripts/check-shell-reexec` script (see *Source-of-truth files*
below).

### Push short-circuit stamp

The pre-push shim writes the current HEAD SHA to
`${XDG_STATE_HOME:-$HOME/.local/state}/loom/prek/<workspace-basename>/push-verified`
(same per-workspace subdirectory as the lock) after a successful run. If
the next invocation finds the stamp file with content equal to the
current HEAD SHA, it consumes the stamp (deletes it) and exits 0
without re-running the suite. This covers the SSH-drop case: when the
connection dies after tests pass but before git completes the push,
the user's next `git push` skips the re-run cost because the
verification is still valid for the same commit.

The stamp lives outside the workspace for the same reason as the
lock — a bead container that could write a forged HEAD-matching stamp
into a workspace path would silently bypass the pre-push gate. Keeping
the stamp under `$XDG_STATE_HOME` closes that hole.

The stamp is single-use: any HEAD advance invalidates it.

### Worker-context hook invocation

The hooks installed by the devshell `shellHook` MUST fire on
`git commit` / `git push` invoked from worker contexts, not just
from the developer's interactive shell. Two preconditions and one
prohibition cover this:

- **Worker worktrees MUST be linked, not separate clones.**
  `loom run --parallel N` (see
  [harness.md — Worktree Dispatch](harness.md)) creates worktrees
  under `.wrapix/worktree/<label>/<bead-id>/` via `git worktree add`.
  Linked worktrees share `.git/config` with the main checkout, so
  the devshell-set `core.hooksPath = lib/prek/hooks` applies inside
  the worktree. A separate clone would have its own `.git/config`
  and skip the hooks entirely.
- **Worker shells MUST expose `flock` and `prek` on PATH.** The
  lock script aborts if `flock` is missing (see *Fail-fast on
  missing `flock`* in Non-Functional below); pre-push aborts if
  `prek` is missing (see *Fail-fast on missing `prek`*, ibid.).
  The bead worker container's shell — whichever profile image
  dispatches the bead — MUST resolve both binaries on PATH so the
  hooks succeed rather than abort. Harness owns image composition;
  this spec owns the contract.
- **Workers MUST NOT pass `--no-verify` on `git commit` or
  `git push`.** Bypassing the hooks defeats both the formatter
  check at commit time and the `nix flake check` workspace
  verification at push time — specifically the workspace-scope
  style lints (e.g.
  `crates/loom/tests/style.rs::git_client_encapsulation`) that
  per-bead `cargo test -p <crate>` invocations don't reach.

### Source-of-truth files

This spec owns:

- `.pre-commit-config.yaml`
- `lib/prek/lock.sh`
- `lib/prek/shellcheck-batched.sh`
- `lib/prek/hooks/pre-commit`
- `lib/prek/hooks/pre-push`
- `scripts/check-shell-reexec`
- The `core.hooksPath` line in `nix/flake/devshell.nix`'s `shellHook`

## Success Criteria

### Configuration

- `.pre-commit-config.yaml` exists at repo root and declares
  `trailing-whitespace`, `end-of-file-fixer`, and `check-merge-conflict`
  at the `pre-commit` stage
  [check](grep -q 'check-merge-conflict' .pre-commit-config.yaml)
- `end-of-file-fixer` excludes `.beads/config.yaml`
  [check](grep -q '\.beads/config\.yaml' .pre-commit-config.yaml)
- The treefmt hook runs `treefmt --fail-on-change` at `pre-commit`
  [check](grep -q 'treefmt --fail-on-change' .pre-commit-config.yaml)
- The shell-reexec hook invokes `scripts/check-shell-reexec` at
  `pre-commit` on `types: [shell]`
  [check](grep -q 'scripts/check-shell-reexec' .pre-commit-config.yaml)
- The pre-push stage includes a `nix flake check` hook with
  `always_run: true`
  [check](grep -q 'nix flake check' .pre-commit-config.yaml)
- The pre-push stage includes a hook that runs `nix run .#test`
  and filters on `crates/*/tests/properties.rs`
  [check](grep -q 'tests/properties.rs' .pre-commit-config.yaml)

### Lock implementation

- `lib/prek/lock.sh` resolves the lock under
  `$XDG_STATE_HOME/loom/prek/<workspace-basename>/prek.lock` (with
  the `${XDG_STATE_HOME:-$HOME/.local/state}` default), where
  `<workspace-basename>` is `basename` of the main worktree's path
  (the first `worktree` entry in `git worktree list --porcelain`)
  [check](grep -q 'XDG_STATE_HOME' lib/prek/lock.sh)
- `lib/prek/lock.sh` never writes the lock under any path inside the
  workspace (no `.wrapix/`, no repo-relative path)
  [check](cargo run -p loom-walk -- prek_lock_path_outside_workspace)
- `lib/prek/lock.sh` uses `flock -x` on FD 9 with a 600-second timeout
  [check](grep -qE 'flock.*-x.*9' lib/prek/lock.sh)
- Dead-PID recovery: a lock file whose stamped PID is not alive is
  removed and re-acquired, rather than blocked on
  [test](crates/loom-driver/tests/prek_lock.rs::dead_pid_lock_is_reclaimed)
- Concurrent acquisition: two processes contending on the same lock
  serialize; the second waits for the first to release before
  proceeding
  [test](crates/loom-driver/tests/prek_lock.rs::concurrent_acquisitions_serialize)
- Shared lock across linked worktrees: a hook running inside
  `.wrapix/worktree/<label>/<bead-id>/` and a hook running in the
  main checkout resolve to the same lock file (same
  `<workspace-basename>`)
  [test](crates/loom-driver/tests/prek_lock.rs::linked_worktrees_share_lock)
- Subprocess discipline: callers spawning non-`exec` subprocesses
  close FD 9 on the child (`9>&-`)
  [check](grep -q '9>&-' lib/prek/hooks/pre-push)

### Hook shims

- `lib/prek/hooks/pre-commit` is executable
  [check](test -x lib/prek/hooks/pre-commit)
- `lib/prek/hooks/pre-commit` sources `lib/prek/lock.sh`
  [check](grep -q 'lock.sh' lib/prek/hooks/pre-commit)
- `lib/prek/hooks/pre-commit` calls `_prek_acquire_lock` before
  invoking prek
  [check](grep -q '_prek_acquire_lock' lib/prek/hooks/pre-commit)
- `lib/prek/hooks/pre-commit` `exec`s `prek hook-impl
  --hook-type=pre-commit` with `--no-progress`
  [check](grep -q -- '--no-progress' lib/prek/hooks/pre-commit)
- `lib/prek/hooks/pre-push` is executable
  [check](test -x lib/prek/hooks/pre-push)
- `lib/prek/hooks/pre-push` sources `lib/prek/lock.sh` and calls
  `_prek_acquire_lock`
  [check](grep -q '_prek_acquire_lock' lib/prek/hooks/pre-push)
- `lib/prek/hooks/pre-push` invokes `prek hook-impl
  --hook-type=pre-push` with `--no-progress` (the `9>&-` discipline
  for the prek child is covered by *Subprocess discipline* in the
  Lock implementation block above)
  [check](grep -q -- '--no-progress' lib/prek/hooks/pre-push)
- `lib/prek/hooks/pre-push` short-circuits on a HEAD-matching
  `$XDG_STATE_HOME/loom/prek/<workspace-basename>/push-verified`
  stamp and consumes the stamp on hit
  [check](grep -q 'XDG_STATE_HOME.*push-verified\|push-verified.*XDG_STATE_HOME' lib/prek/hooks/pre-push)
- Stamp + lock share the same `<workspace-basename>` so linked
  worktrees see the same stamp
  [test](crates/loom-driver/tests/prek_lock.rs::stamp_shared_across_worktrees)

### Devshell integration

- `nix/flake/devshell.nix` sets `git config --local core.hooksPath
  lib/prek/hooks` from its `shellHook` when `.git` exists
  [check](grep -q 'core.hooksPath' nix/flake/devshell.nix)
- The devShell exposes `flock` and `prek` on PATH
  [check](grep -q 'flock' nix/flake/devshell.nix)

### Worker-context coverage

- A `git commit` from inside a linked worktree under
  `.wrapix/worktree/<label>/<bead-id>/` fires the same `lib/prek/hooks/pre-commit`
  shim as a commit from the main checkout (worktrees inherit
  `core.hooksPath` via shared `.git/config`)
  [test](crates/loom-driver/tests/prek_lock.rs::worker_worktree_commit_fires_pre_commit_hook)
- A `git push` from inside a linked worktree fires the same
  `lib/prek/hooks/pre-push` shim and runs `nix flake check`
  [test](crates/loom-driver/tests/prek_lock.rs::worker_worktree_push_fires_pre_push_hook)
- `nix flake check` exercises the workspace-level style lints
  (`crates/loom/tests/style.rs`) that per-bead `cargo test -p <crate>`
  invocations don't reach — confirmed by deliberately introducing a
  `git_client_encapsulation` violation and asserting pre-push fails
  [test](crates/loom-driver/tests/prek_lock.rs::pre_push_catches_workspace_style_violation)

### Shellcheck batching

- `lib/prek/shellcheck-batched.sh` exists, is executable, and chunks
  file arguments in groups of 25
  [check](grep -q 'i+=25' lib/prek/shellcheck-batched.sh)

### Shell re-exec discipline

- `scripts/check-shell-reexec` exists and is executable
  [check](test -x scripts/check-shell-reexec)

## Requirements

### Functional

1. **Stage composition** — pre-commit runs builtin
   `trailing-whitespace`, `end-of-file-fixer` (with
   `.beads/config.yaml` excluded), `check-merge-conflict`; `treefmt
   --fail-on-change`; and `shell-reexec-explicit-interpreter`.
   Pre-push runs `nix flake check` unconditionally and a hook that
   invokes `nix run .#test` (the container smoke owned by
   [tests.md](tests.md)) when changes touch
   `^crates/[^/]+/tests/properties\.rs$`.

2. **Flock serialization** — `lib/prek/lock.sh` exposes
   `_prek_acquire_lock`, which holds an exclusive flock on FD 9
   with a 600-second timeout and 1-second polling. The lock path is
   `${XDG_STATE_HOME:-$HOME/.local/state}/loom/prek/<workspace-basename>/prek.lock`,
   where `<workspace-basename>` is `basename` of the main worktree's
   path (the first `worktree` entry in `git worktree list
   --porcelain`), mirroring harness.md's keying scheme. The lock
   lives on the host filesystem, never inside the workspace, so a
   bead container with the workspace bind-mounted cannot delete or
   forge it (see [harness.md — Lock matrix](harness.md)).
   Both hook shims source `lib/prek/lock.sh` and call
   `_prek_acquire_lock` before invoking `prek hook-impl`.
   Subprocesses spawned without `exec` close FD 9 on the child.

3. **Dead-PID recovery** — when the PID stamped in the lock file is
   not a live process, the lock is reclaimed on a fresh inode rather
   than blocked on indefinitely. Recovery is silent on the success
   path; a stderr line names the reclaimed PID so a stuck worktree
   leaves a trace.

4. **Push short-circuit stamp** — pre-push writes the current HEAD
   SHA to
   `${XDG_STATE_HOME:-$HOME/.local/state}/loom/prek/<workspace-basename>/push-verified`
   after a successful test run; a subsequent pre-push invocation
   matching the same HEAD consumes the stamp and exits 0 without
   re-running. Any HEAD advance invalidates the stamp. The stamp
   lives outside the workspace for the same reason as the lock: a
   bead container that could write a forged HEAD-matching stamp into
   a workspace path would silently bypass the pre-push gate.

5. **Hook installation** — `nix/flake/devshell.nix`'s `shellHook`
   runs `git config --local core.hooksPath lib/prek/hooks` when
   `.git` exists. The hook directory is versioned, so a clean
   checkout that enters `nix develop` has working hooks without a
   separate `pre-commit install` step.

6. **Shellcheck batching** — `lib/prek/shellcheck-batched.sh` wraps
   `shellcheck`, chunking files in groups of 25 to avoid OOM on
   large changesets. The `treefmt` configuration owned by the
   project's formatter setup invokes the wrapper rather than
   `shellcheck` directly.

7. **`--no-progress`** — both shims pass `--no-progress` to `prek
   hook-impl` to work around a prek 0.3.x crash where the
   `indicatif` progress bar's `Drop` hits a poisoned mutex from a
   parallel worker and aborts before the hook verdict propagates.

8. **Worker-context hook invocation** — hooks MUST fire on
   `git commit` / `git push` from worker contexts, not only from
   the developer's interactive shell. Worker worktrees MUST be
   linked worktrees (created by `loom run --parallel N` via
   `git worktree add`, per
   [harness.md — Worktree Dispatch](harness.md)) so they inherit
   `core.hooksPath` via shared `.git/config`; the worker container's
   shell MUST expose `flock` and `prek` on PATH so the lock script
   and pre-push shim succeed rather than abort. Workers MUST NOT
   pass `--no-verify` on commit or push — bypassing the hooks
   defeats both the pre-commit formatter check and the pre-push
   `nix flake check`, the latter being the only path that runs the
   workspace-scope style lints that per-bead
   `cargo test -p <crate>` invocations don't reach.

### Non-Functional

1. **Concurrency-safe** — the flock guarantees only one `prek
   hook-impl` operates on the working-tree stash at a time, even
   when multiple loom workers, plan sessions, and a developer's
   `git commit` overlap on the same worktree.

2. **Worktree-aware** — linked worktrees share one lock and one
   stamp file via `<workspace-basename>`, the basename of the main
   worktree's path (the first `worktree` entry in `git worktree list
   --porcelain`). A hook running in
   `.wrapix/worktree/<label>/<bead-id>/` serializes against a hook
   running in the main checkout because both resolve to the same
   `$XDG_STATE_HOME/loom/prek/<workspace-basename>/` directory.
   Deriving from the main worktree rather than `git rev-parse
   --show-toplevel` is what makes this property hold — a linked
   worktree's own toplevel basename would key a separate directory.

3. **Single-use stamp** — the `push-verified` stamp is consumed on
   hit; no stale stamp persists across a HEAD advance.

4. **Fail-fast on missing `flock`** — the lock script aborts with a
   message naming `flock` as the missing dependency when it isn't on
   PATH. The Nix devShell guarantees `flock` is available; the abort
   path covers contributors who bypass the devShell.

5. **Fail-fast on missing `prek`** — the hook shims abort with a
   message naming `prek` when it isn't on PATH.

6. **Cross-platform** — the lock script and hook shims are
   POSIX-flock-dependent. The devShell ensures `flock` is on PATH on
   Linux (util-linux) and macOS (nixpkgs' Darwin flock package). The
   pre-push container smoke remains Linux-only; on Darwin, pre-push
   still runs `nix flake check` but the `nix run .#test` hook exits 0
   with the "not available on Darwin" message owned by
   [tests.md](tests.md).

## Out of Scope

- **Per-user `pre-commit install`** — installation flows through
  `nix develop`'s `shellHook` exclusively. The
  `.pre-commit-config.yaml` shape happens to be portable, but
  documenting a manual-install path is not this spec's concern.
- **A second mechanism for `[system]`-tier verifiers** — the smoke
  app `nix run .#test` is owned by [tests.md](tests.md); this spec
  only specifies when the hook fires it.
- **Hooks beyond pre-commit/pre-push** —
  `prepare-commit-msg`, `post-commit`, `post-checkout`,
  `post-merge` and similar remain prek defaults under `.git/hooks/`
  without the flock wrapper. The race the lock protects against is
  the stash/restore window, not the cheap message-shaping hooks.
  `post-commit` specifically was considered for catching the
  "agent forgot to stage" failure mode and rejected — see the
  `tree-not-clean` bullet below for where that lives.
- **Lock-script behavioural tests beyond acquisition / dead-PID
  recovery** — timeout-exhaustion, holder-PID printout shape, and
  partial-write recovery are valuable but not load-bearing for
  correctness; if they prove necessary, they get added as
  `[test]`-tier criteria in a future revision.
- **Replacing prek with a native Rust hook runner.** prek is a
  third-party dependency pinned at the workspace level; rewriting it
  would be a separate spec.
- **Catching "agent forgot to stage" / dirty-tree-after-completion.**
  Pre-commit hooks' stash/restore architecture intentionally hides
  unstaged changes from the hook (the very property the flock here
  protects), so a `treefmt`-style check at commit time cannot observe
  a worker that staged only its own paths and left other tracked
  files dirty. That failure mode is owned by
  [harness.md — Verdict Gate](harness.md) as the `tree-not-clean`
  recovery cause: the driver runs `git status --porcelain` after the
  worker emits `LOOM_COMPLETE` / `LOOM_NOOP` and routes a non-empty
  result to recovery. The split is deliberate — pre-commit hooks
  govern `index ∩ working-tree`; the driver-side check governs
  `working-tree − index`.
