# Loom Harness

Rust workspace, build system, workspace lint config, process architecture,
state store, and command-set platform for the Loom agent driver.

## Problem Statement

Loom is a Rust binary that owns a complete spec-driven workflow:
spec interview (plan), spec-to-beads decomposition (todo), per-bead
agent dispatch (loop), deterministic and LLM-judged review (gate),
and human clarification (msg). The binary holds the workflow's
state in typed domain objects, parses agent protocols against typed
schemas, and renders templates with compile-time variable
validation.

This spec covers the platform: crate structure, Rust conventions,
Nix integration, SQLite state store, beads CLI wrapper, process
architecture, recovery mechanics, the `Session` and `EventSink`
trait surfaces, and the `llm` public LLM-primitives crate.
The Askama template engine, partials inventory, per-phase pinning
policy, the typed context structs Loom exposes for consumer
template composition, and the snapshot-test contract live in
[templates.md](templates.md). The agent abstraction
layer (pi-mono, Claude Code, and Direct backends; container
communication; backend selection) lives in
[agent.md](agent.md). The gate (rubric, invariants,
lanes, stages) lives in [gate.md](gate.md). Workflow
semantics — what each `loom plan` / `loom todo` / `loom loop` /
`loom gate` / `loom msg` command does — are defined in this
spec's Functional section and the Msg Modes / Verdict Gate sections
below.

## Architecture

### Process Architecture

Loom is a host-side orchestrator. Every workflow phase that drives an agent
spawns its own container per bead — no shared long-lived container, no
in-container loom loop. The two motivations:

1. **Per-bead profile selection.** Beads carry `profile:rust` /
   `profile:python` / `profile:base` labels. Each bead must run in a container
   built from the matching profile image. A long-lived parent container can't
   change profile mid-run; per-bead spawn is the only clean way.
2. **Trust boundary.** Loom (orchestrator, on host) is trusted; the agent
   (claude or pi, in container) is the sandboxed execution layer.

**Container spawn is delegated to `wrapix spawn`** — a thin wrapix
subcommand that owns container construction (mounts, env passthrough, krun
runtime selection on aarch64 microVM, network filtering, deploy key, beads
dolt socket). Loom never invokes `podman run` directly. Nix remains the source
of truth for container layout; loom owns only the typed `SpawnConfig` it
hands to the wrapper.

```
loom (host)
    │
    ├─ build SpawnConfig (image_ref, image_source, env allowlist, mounts, scratch_dir)
    ├─ serialize to /tmp/loom-<id>.json
    │
    ├─ spawn: wrapix spawn --spawn-config /tmp/loom-<id>.json --stdio
    │   │
    │   └─ exec podman run [no -t, stdio piped] <image> <entrypoint>
    │       │
    │       └─ entrypoint.sh → agent (claude --print … / pi --mode rpc)
    │           ↑              ↓
    │           └── JSONL over stdin/stdout ─→ loom (parses events)
    │
    └─ on bead completion: container exits, next bead → next spawn
```

`wrapix spawn --stdio` is the non-TTY counterpart of today's interactive
`wrapix run` (which uses `podman run -it`). Both modes share container
construction; they differ only in stdio attachment. The
`--spawn-config <file>` flag accepts a JSON file that mirrors loom's typed
`SpawnConfig` — avoiding a fat argv interface and giving loom a single
serialization boundary.

**`loom plan` is the exception.** It is an interactive spec interview
(human-in-the-loop terminal session), so it shells out to interactive
`wrapix run` rather than driving an JSONL session. Loom prepares the
template-rendered prompt, sets environment, exec's `wrapix run`, and lets
claude attach to the user's terminal. No subprocess capture, no JSONL.

**Trade-off accepted:** parallelism is straightforward (N concurrent
`wrapix spawn` invocations) and per-bead container spawn cost (~1-2s
on podman) is dominated by agent runtime for typical bead sizes
(minutes of agent work). The alternative — one long-lived container
sharing one agent across beads — was rejected because it conflicts
with per-bead profile selection and with the trust-boundary split
between host orchestrator and sandboxed agent.

### Bead Dispatch

Loom owns its own git workspace, separate from the operator's
checkout. The operator's `/workspace` and loom's
`.loom/integration/` are independent clones of the same
origin — they sync through origin, like any pair of dev workstations.
Loom never reads, writes, or rebases the operator's working tree;
the operator never edits loom's. Bead workspaces derive from the
loom workspace, not from `/workspace`.

**Layout.**

```
/workspace/                   operator's checkout, untouched by loom
/workspace/.loom/integration/ loom's clone, integration branch checked out
/workspace/.loom/beads/<id>/  bead workspaces, flat by globally-unique bead id
```

Each bead workspace is a `git clone --local` of the loom workspace.
`<id>` is the bead id (`lm-9ehh.42`-style); ids are globally unique
within the beads database so no spec partition is needed in the path.
The integration branch name is `[loom] integration_branch` in
`loom.toml` (default `main`); the loom workspace has that branch
checked out and never switches. `loom init` materializes the loom
workspace via `git clone <origin> .loom/integration` on a
fresh install; existing operator workspaces run it once after
upgrading to this layout. The lock-file location
(`$XDG_STATE_HOME/loom/locks/<workspace-basename>/<spec>.lock`) is
unchanged.

**Why the layering.**

- **Operator and loom as peer clones of origin.** The operator's
  edits cannot break loom's rebases (a dirty operator working tree
  is irrelevant to loom's machinery); loom's pushes go to origin,
  so the operator's working tree changes only when they `git pull`.
  Sync flows through `git push` / `git pull` against origin.
- **Bead workspace as a child of the loom workspace.** Same fractal
  pattern at every level: bead is to loom-workspace what
  loom-workspace is to origin. The bead workspace has a
  self-contained `.git/` inside the bind-mounted path so the wrapix
  container's `/workspace` mount resolves git operations without
  external `.git/`-mount wiring. Linked worktrees are unsuitable:
  their `.git` pointer file references a host-absolute path
  outside the container's bind mount, so workers inside cannot
  resolve the gitdir.

**Per-bead-close lifecycle.** A bead workspace is created on
first dispatch and persists across attempts, recovery iterations,
and `loom loop` invocations. It is reaped when the bead transitions
to `closed` (via the agent's `bd close` on success, the operator's
`loom msg`-resolve, or a direct `bd close`). Within that lifetime
every fresh dispatch attempt sees a clean working tree: the
dispatch path runs `git reset --hard HEAD` plus
`git clean -fdx --exclude=target --exclude=.git --exclude=.wrapix`
before handing off to the agent. On the first attempt this is a
no-op against a freshly-cloned tree; on retries it discards any
mid-session leftovers and preserves the agent's prior commits on
the bead branch — the agent is responsible for amending or
branch-resetting if `previous_failure` context calls for a
different approach. `target/` survives so cargo + sccache start
warm; `.git/` and `.wrapix/` (extra-mount staging) survive.
`create_worktree` is idempotent at the directory level: directory
exists → reuse; missing → clone fresh from the loom workspace.

**Garbage collection.** At `loom loop` startup, under the spec
advisory lock, loom enumerates `.loom/beads/` and drops
every bead workspace whose bead is `closed`. The sweep is workspace-global
(not spec-scoped) — a closed bead cannot be in flight, so the sweep
is safe regardless of which spec is being loop'd. In-loop, every
bead the current loom dispatches owns its own removal: on the bead
transitioning to `closed` the dispatch path reaps the workspace
inline as part of post-session cleanup. The startup sweep catches
orphans from crashed prior runs, not the happy path. No timer
threshold; no operator-explicit GC. A `loom gc` command is an
additive follow-up if hoarding ever materializes.

**Bead branch flow.** Bead branch `loom/<id>` is created in the
bead workspace at first dispatch via `git checkout -b`. The agent
commits to it; the worker never pushes. On a successful exit
(`LOOM_COMPLETE` clearing the bead-exit verdict-gate signals),
the driver — running on the host in the loom workspace, holding
`index.lock` — fetches the bead branch from the bead workspace
path, verifies signatures, rebases onto the integration branch,
verifies the rewritten commits, fast-forward-merges, and deletes
the transient `loom/<id>` ref:

```
# in loom workspace, inside index.lock
git fetch .loom/beads/<id> loom/<id>:loom/<id>
# signature verification pass 1 (worker commits)
git rebase <integration-branch> loom/<id>
# signature verification pass 2 (rewritten commits)
git checkout <integration-branch>
git merge --ff-only loom/<id>
git branch -D loom/<id>
```

The verification and rebase-conflict mechanics are defined in
[Verdict Gate](#verdict-gate). Linear history, no merge commits.
The `loom/<id>` ref in the loom workspace is transient — deleted
unconditionally at end of the critical section, whether the path
exited cleanly, rolled back from audit-fail, or aborted from
rebase conflict. The bead clone retains its branch until the
bead transitions to `closed` and the clone is reaped; the operator
can `cd .loom/beads/<id>` to inspect.

The bead clone's `origin` remote remains pointing at the loom
workspace path; the worker never invokes it but this preserves
host-side ahead/behind tracking when the operator `cd`s into the
bead clone (e.g., starship prompt). The bead container has no path
mount to the loom workspace and cannot push from inside; manual
host-side pushes are harmless because the integration step still
owns rebase + verify + ff. The driver's fetch is against a
filesystem path (always present through the bead's lifetime), not
over a network or daemon.

After every bead in the molecule has integrated, the
molecule-completion push gate pushes the integration branch to
`origin/<integration-branch>`. A non-fast-forward error at origin
(operator pushed first, or another loom landed cross-spec work)
retries by fetching + rebasing the integration branch onto
`origin/<integration-branch>` + re-pushing.

**Preserve-on-failure.** Rebase-conflict abort, audit-fail
rollback, signature-verification failure, and post-integration push
failure all route the bead to `Blocked` or `Clarify` per the
verdict gate; the bead workspace persists on disk. Under the
per-bead-close lifecycle "preserve" is the default — the workspace
persists until `bd close` regardless of why it stopped progressing.
The operator unblocks via `loom msg`-resolve; the bd-close-driven
reap drops the bead workspace.

**Concurrency.** Concurrent `loom loop --parallel N` and concurrent
loops across specs share one loom workspace. Serialization points:

- N concurrent `git clone --local` from the loom workspace into
  bead workspaces: git's atomic-rename + per-ref lock semantics
  make concurrent clone-from-loom safe.
- Concurrent driver-side fetches from N bead workspaces into the
  loom workspace: each fetch lands a uniquely-named ref
  (`loom/<bead-id>`), so the fetches themselves don't contend on
  the same ref. The fetch nevertheless runs inside the per-bead
  critical section's `index.lock` because the subsequent rebase
  and ff-merge mutate the integration branch.
- Cross-spec rebase + ff into the integration branch in the loom
  workspace: serialized by git's `index.lock`. The losing process
  surfaces the error and retries the rebase + ff from its current
  view of the integration branch.
- Origin push: serialized by origin's non-fast-forward + retry-with-
  fetch loop.

The per-spec advisory lock (see [Concurrency & Locking](#concurrency--locking))
continues to serialize plan / todo / loop / gate / msg on the same
spec.

**Mounts.** Bead containers see two mandatory bind mounts plus an
optional sccache mount, all via `SpawnConfig`:

- **Mandatory: the bead workspace** at `/workspace`.
- **Mandatory: the host `wrapix-beads` dolt socket** at
  `/workspace/.wrapix/dolt.sock` via `SpawnConfig.mounts`
  (see [agent.md § SpawnConfig](agent.md#spawnconfig)). This
  replaces the historical host-side hardlink shim and survives
  changes to the bead-workspace path. Linux passes the socket
  through directly; on Darwin the wrapix sandbox rejects
  Unix-socket `host_path` entries at launch (VirtioFS cannot
  forward socket operations across the VM boundary), so dolt-over-
  socket on Darwin requires a TCP fallback rather than this mount —
  out of scope for the present Linux-only loom target.
- **Optional: a shared sccache directory** at the configured
  container path (default `/sccache`; see
  [Configuration](#configuration)), gated on `[loom] sccache_dir`
  being set. When configured, bead and plan/todo container spawns
  receive the directory as a bind mount with `SCCACHE_DIR` and
  `RUSTC_WRAPPER=sccache` set in container env. Host-side cargo
  invocations (loom's `cargo nextest` against the loom workspace
  during gate verify; the operator's `nix develop` cargo if opted
  in via devshell config) read the same host directory directly
  with the same env vars exported — no bind mount, because there
  is no container boundary to cross. One cache, all clients;
  sccache's content-addressed scheme makes the cache safe to
  share across workspaces whose absolute paths differ.

**Darwin compatibility.** Clones on a single host filesystem
hardlink objects within that filesystem — no VM-boundary crossing.
Bind-mounting plain directories and the dolt socket file through
virtiofs is the standard wrapix pattern. The clone-over-worktree
choice eliminates the host-absolute `.git`-pointer failure mode
that would also bite on Darwin. Outstanding Darwin work for the
broader runtime layer is tracked in
[agent.md § Out of Scope](agent.md#out-of-scope).

**Git operations: hybrid `gix` + `git` CLI.** Workspace, branch,
status, and integration operations go through a typed `GitClient`
in `loom-driver`. The implementation is hybrid, encapsulated inside
the module; callers see only typed Rust methods.

| Operation | Backend | Reason |
|-----------|---------|--------|
| `status` (working tree vs HEAD) | [`gix`](https://docs.rs/gix) `Repository::status()` | mature |
| `diff` (HEAD vs HEAD~) | `gix::diff` (`blob-diff` feature) | mature |
| List refs / branches | `gix::Repository::references()` | mature |
| Read commit graph / HEAD | `gix` | mature |
| **Create loom workspace** (`loom init`) | `git clone <origin> .loom/integration` (CLI) | one-shot, infrequent; `gix-clone` is unchecked |
| **Create bead clone** | `git clone --local .loom/integration .loom/beads/<id>` then `git checkout -b loom/<id>` (CLI) | hardlinks loom workspace's `.git/objects`; self-contained `.git/` inside bind mount |
| **Pre-attempt reset of bead clone** | `git reset --hard HEAD` + `git clean -fdx --exclude=target --exclude=.git --exclude=.wrapix` (CLI) | clean working tree at bead-branch HEAD while preserving `target/`, `.git/`, and `.wrapix/` |
| **Fetch bead branch from bead workspace** | `git fetch <bead-workspace-path> loom/<id>:loom/<id>` (CLI) | filesystem path as ad-hoc URL; runs in loom workspace inside `index.lock` |
| **Verify commit signatures** | `git verify-commit <commits>` (CLI) | gates integration on signed-by-wrapix-key; conditional on signing key resolving (see [Commit signing](#commit-signing)) |
| **Rebase + ff into integration branch** | `git rebase` + `git merge --ff-only` (CLI) | `gix-merge` cannot persist `MERGE_HEAD`/`MERGE_MSG`; avoids index dance |
| **Delete bead-branch ref in loom workspace** | `git branch -D loom/<id>` (CLI) | end of per-bead critical section, unconditional |
| **Push integration to origin** | `git push origin <integration-branch>` (CLI) | with non-ff retry |
| **Remove bead clone** | `std::fs::remove_dir_all` | standalone tree; not a registered worktree |

`gix` 0.83+ is pinned with features `["status", "blob-diff",
"revision", "parallel", "sha1"]` (the `sha1` feature is required
for gix-hash to compile; without it the `Kind` enum has no
variants). `gix::Repository` is `!Sync`; loom holds a
`ThreadSafeRepository` and clones a thread-local handle inside
`spawn_blocking` per call. CLI shell-outs use
`tokio::process::Command` with arguments passed via `.arg()` —
never shell interpolation — and a 60-second timeout (10 minutes
for pushes that drive pre-push CI hooks).

The hybrid line is reviewed each loom release; gix operations
migrate inward as the corresponding `crate-status.md` items become
checked.

### Commit signing

Loom's host-side git operations must sign without prompting for
the operator's GPG passphrase. Two host-side workspaces sign: the
loom workspace, where driver-side rebase produces new commits, and
bead clones, where operator-run git commands during debug or
host-side tests may commit. Both receive a local `.git/config`
pointing at the same wrapix-injected SSH signing key the bead
container uses, making signing non-interactive end-to-end.

**Key resolution mirrors wrapix's host-side rule** (see
`lib/sandbox/linux/default.nix` and `scripts/setup-deploy-key` in
the wrapix flake — same two-tier precedence, set-but-missing fails
loud):

1. `$WRAPIX_SIGNING_KEY` pointing at an existing file. Set-but-
   missing exits non-zero at startup naming the path; silent
   fallback would mask a parent-process misconfiguration.
2. `$HOME/.ssh/deploy_keys/<repo>-<host>-signing` if the env var
   is unset and the file exists. `<repo>` is the repo segment of
   the loom workspace's origin URL (parsed as
   `github.com[:/]<user>/<repo>`); `<host>` is `hostname -s`
   (short form, fallback to `hostname`). Same derivation as
   wrapix's `setup-deploy-key` script uses to choose the keyname
   at provisioning time, so the two ends stay in sync without
   shared config. If the origin URL doesn't match the GitHub
   pattern, the fallback is skipped.
3. If neither resolves, loom writes no signing block; the
   operator's global `~/.gitconfig` governs (and may prompt). This
   is the "wrapix isn't set up on this host" path — intentionally
   noisy rather than silently degraded.

Auth is handled by the `GIT_SSH_COMMAND` env var wrapix already
sets; loom inherits it and does not duplicate auth configuration.
Deploy-key resolution (same precedence with the suffix dropped:
`<repo>-<host>` instead of `<repo>-<host>-signing`) is needed only
to confirm presence on the host — the key path itself is consumed
by wrapix, not by loom's git invocations.

**Workspace gitconfig writes.** When `loom init` materializes the
loom workspace and when `GitClient::create_worktree` materializes a
bead clone, the driver writes a local `.git/config` block:

```ini
[gpg]
    format = ssh
[user]
    signingkey = <resolved signing-key host path>
[gpg "ssh"]
    allowedSignersFile = <workspace>/.git/loom-allowed-signers
[commit]
    gpgsign = true
```

Local config beats the operator's `~/.gitconfig` in git's hierarchy,
so this block is the sole authority on signing behavior inside the
loom workspace and every bead clone — operator GPG/passphrase setup
is bypassed without modification.

**allowed_signers derivation.** Wrapix derives the allowed_signers
file inside the container via `ssh-keygen -y -f $SIGNING_KEY` against
the public-key half of the same pair. Loom mirrors this on the host:
at the same moment it writes the gitconfig block, it runs
`ssh-keygen -y -f <signing-key>` and writes the result with the
identity prefix (`$GIT_AUTHOR_EMAIL` or `sandbox@wrapix.dev` per
wrapix convention) to `<workspace>/.git/loom-allowed-signers`. The
signing key is passphrase-less, so the derivation is non-interactive.
The file lives under `.git/` so workspace removal cleans it up
automatically.

**Scope.** This subsection covers commit signing only.
SSH-over-git auth (deploy key for github.com push) flows through
wrapix's existing `GIT_SSH_COMMAND` pathway, inherited via the
operator's shell environment; loom does not reconfigure it. Container
gitconfig is unchanged — bead-container commits are already signed
by wrapix's `git-ssh-setup.sh` entrypoint and need no host-side
intervention.

**rerere configuration.** Alongside the signing block, `loom init`
writes `[rerere] enabled = true` and `[rerere] autoupdate = true`
into the loom workspace's local `.git/config`. The driver-side
rebase in the per-bead integration step relies on rerere to replay
previously-recorded conflict resolutions before falling through to
`integration-conflict` recovery (see
[Verdict Gate](#verdict-gate)). Recorded resolutions accumulate
only in the loom workspace's `.git/rr-cache/`, where the driver-
side rebase consults them across bead lifetimes. Bead clones don't
need rerere — even when the integration-conflict retry path asks
an agent to rebase its bead-workspace branch onto the new tip, the
bead clone is reaped on `bd close` and any rerere cache it
accumulated wouldn't transfer to the loom workspace.

### Profile-Image Manifest

The *profile-image manifest* is a JSON file produced by Nix at flake-build
time that maps each profile name to the podman ref and Nix store path
needed to spawn its image. Loom reads it at startup and, for each bead,
looks up the profile label to populate `SpawnConfig.image_ref` (the podman
ref) and `SpawnConfig.image_source` (the store path handed to
`podman load`).

The file is a JSON object keyed by profile name, with two string fields
per entry:

```json
{
  "base":   { "ref": "localhost/wrapix-base:abc123",   "source": "/nix/store/...-image-base" },
  "rust":   { "ref": "localhost/wrapix-rust:def456",   "source": "/nix/store/...-image-rust" },
  "python": { "ref": "localhost/wrapix-python:ghi789", "source": "/nix/store/...-image-python" }
}
```

Built by `wrapix.lib.${system}.mkProfileImages` (defined in
[profiles.md](profiles.md)); the bundled flake output is
`packages.profile-images`. External flakes that add custom profiles call
`mkProfileImages` themselves to produce a manifest covering their full
profile set.

Loom reads the manifest path from the `LOOM_PROFILES_MANIFEST` environment
variable. The bundled devshell sets it to `${self'.packages.profile-images}`;
consumers integrating loom into their own flake set it the same way. If the
variable is unset or the file is missing, loom errors at startup before any
bead spawn — there is no implicit search path or fallback default. The
manifest is parsed once at startup and held as a
`BTreeMap<ProfileName, ImageEntry>` in `loom-driver`.

Per-bead dispatch is:

1. Parse the bead's labels; pick the highest-precedence `profile:X` (or the
   value of `--profile` if set on the CLI).
2. Look up `X` in the parsed manifest. Missing key → exit immediately as
   `loom:blocked` with cause `unknown-profile`; no retry. The note on the
   bead names the requested profile and the manifest's declared set so the
   operator can relabel (`bd update`-shaped fix, not a `loom msg` chat
   reply — the chat session does not retag beads). Same routing as
   `infra-preflight` (see *Verdict gate* below).
3. Build `SpawnConfig` with `image_ref = entry.ref` and `image_source =
   entry.source`. Hand it to `wrapix spawn`.

Agent (claude vs pi) is selected at container start via the `WRAPIX_AGENT`
env-allowlist entry the entrypoint switches on — see
[agent.md — Entrypoint Agent Selection](agent.md#entrypoint-agent-selection).
The manifest stays one-dimensional; each per-profile image carries both
runtimes, and `mkSandbox` no longer takes an `agent` parameter at Nix-eval
time.

`loom plan` and `loom msg --chat` are interactive, so they shell out to
`wrapix run` (TTY-attached) rather than `wrapix spawn`. To keep one
resolution path, both commands look up their profile (per
[Configuration](#configuration); default `base`) in the manifest and
export `WRAPIX_DEFAULT_IMAGE_REF=<entry.ref>` plus
`WRAPIX_DEFAULT_IMAGE_SOURCE=<entry.source>` into the child environment
before exec'ing `wrapix run`. The launcher reads those env vars when no
`--spawn-config` is supplied — see
[sandbox.md — Launcher Subcommands](sandbox.md#launcher-subcommands).
`wrapix run` has no `--profile` argv parser; any extra tokens between
the workspace positional and the in-container command (`claude
--dangerously-skip-permissions …`) are forwarded into the container as
the command vector, so the env-var hand-off is the sole
profile-selection contract on this path.

### Concurrency & Locking

Multiple `loom` invocations on the same workspace are explicitly allowed.
The lock model is **per-spec advisory locks** plus a single workspace
exclusive lock used only during destructive state rebuild.

**Lock files** live **outside the workspace**, under
`$XDG_STATE_HOME/loom/locks/<workspace-basename>/` (default
`~/.local/state/loom/locks/<workspace-basename>/`):

- `<label>.lock` — one per spec
- `workspace.lock` — held by `loom init` and `loom init --rebuild`
  (`workspace` is reserved as a spec label to avoid collision)

`<workspace-basename>` is the final path component of the canonicalized
workspace root (e.g. `/workspace` → `workspace`, `~/work/myrepo` →
`myrepo`). Two workspaces with the same basename share a lock namespace;
this is accepted as a known limitation in exchange for human-readable,
greppable lock paths.

Lock files **must not** live inside any bind-mounted workspace
(operator's `/workspace`, the loom workspace, or any bead
workspace): the bead container has its workspace bind-mounted
read-write and could `rm` a lock file inside it out from under the
host driver, silently breaking mutual exclusion. Putting locks
under `$XDG_STATE_HOME` keeps them on the host filesystem only,
outside every bind mount.

All locks are POSIX advisory locks acquired via `flock(2)` through the
`fd-lock` crate. The kernel releases them on process exit or crash, so
there are no stale locks to clean up.

**Lock matrix:**

| Class | Commands | Lock acquired |
|-------|----------|---------------|
| Read-only | `status`, `logs` (incl. `-f` follow), `spec` | none |
| Spec-scoped mutating | `plan`, `todo`, `loop`, `gate`, `msg`, `use` | exclusive on `<label>.lock` |
| Workspace-exclusive | `init`, `init --rebuild` | exclusive on `workspace.lock` |

A spec-scoped command on label `X` waits up to 5 seconds for `<X>.lock`,
then errors with `another loom command is operating on <X>` (no busy-loop,
no silent stalls). `init` and `init --rebuild` error immediately if any
spec lock is held.

**Why git is the second-order serialization point.** Two `loom loop`
invocations on *different* specs share the loom workspace's
integration branch. They collide briefly at integration and at
origin push:

- Concurrent rebase + ff into the integration branch is serialized
  by git's own `index.lock` in the loom workspace; the losing
  process surfaces a clear error and retries.
- Concurrent `git push` from `loom gate verify` to
  `origin/<integration-branch>` produces non-fast-forward on the
  second push; the gate's push gate re-fetches and retries.

These are accepted, recoverable failure modes — not silent corruption —
which is why a workspace-wide lock is *not* required for `loop` /
`gate verify`.

### Nested-Loom Guard

The driver sets `LOOM_INSIDE=1` in every bead container's environment
(passed through the `SpawnConfig.env` allowlist — see
[agent.md](agent.md)). On startup, `loom` checks this env var
and, if set, refuses to run **container-spawning or workspace-mutating**
subcommands with a clear error:

```
error: loom cannot run inside a loom-managed container
  this command spawns containers or mutates workspace state, which
  would create a nested driver. read-only commands (status, logs,
  spec) are still available.
```

**Refused inside container:** `loop`, `init`, `plan`, `gate`, `todo`,
`msg`, `use`.

**Allowed inside container:** `status`, `logs`, `spec` (read-only;
useful for an agent inspecting bead history during its own task).

The guard is mechanical, not advisory: a single env-var check at CLI
entry, before any subcommand dispatch.

### Loop UX & Logging

`loom loop` is the long-running command users watch live. Its terminal
output is shaped for a human reading along; machine consumers (CI
harnesses, SSE bridges, log analyzers) consume the JSONL stream
directly.

**Renderer architecture.** A single `Renderer` trait in `loom-render`
consumes `AgentEvent` values; one impl is selected at startup based
on flags + TTY detection. Four modes:

| Mode | Selected when | Output shape |
|------|---------------|--------------|
| `Pretty` (default) | TTY, no `--plain` / `--json` / `--raw` | Colored, glyphs, indented tool bodies, diffs for `Edit` / `Write`, OSC 8 hyperlinks where supported. |
| `Plain` | Non-TTY (pipe / redirect), `NO_COLOR`, or `--plain` | ASCII-only, no color, no OSC 8 — same content shape as `Pretty` minus decoration. |
| `Json` | `--json` | One pretty-printed JSON object per line. Pure data, zero ANSI. |
| `Raw` | `--raw` | Pass-through of the original JSONL bytes. No parsing, no formatting. |

`loom logs` reuses the same trait + impls — replay and live render
share one code path. The same Renderer takes a `live: bool` so the
in-place running indicator is suppressed on replay; durations come
from `ts_ms` deltas between paired `tool_call` / `tool_result`
events.

**Verbosity** is one flag: `-v` / `--verbose` disables tool-body
truncation, streams `text_delta` / `thinking_delta` live, and shows
`thinking` blocks. No finer-grained sub-flags in v1.

**Driver events** ride the same channel as agent events with
`source: "driver"`. The renderer marks them so the eye separates
"what loom did" from "what the agent did". Variant set defined in
*Event Schema*.

**Cancellation.** Ctrl-C / SIGINT produces a clean closing block;
the in-place running indicator is collapsed, the partial diff is
captured, the closing line names `⚠ interrupted`. A panic hook +
`tokio::signal` handler ensure the in-place region is cleared on
every exit path.

**Parallel runs.** Under `--parallel N > 1`, every line carries a
`[bead-id]` prefix with a stable hash-derived hue so interleaved
output stays attributable. Bead headers and closing lines print
atomically. The in-place running indicator is disabled (multiple
`\r`-updating regions on one terminal don't compose).

**Log persistence.** Loom always writes the full raw JSONL event
stream for every bead spawn to disk via a tee-style sink,
regardless of terminal verbosity:

```
.loom/logs/<spec-label>/<bead-id>-<utc-timestamp>.jsonl
```

One file per bead spawn — parallel batches never interleave inside
a single file. Per-event flush is mandatory so downstream consumers
(`tail -f`, SSE bridges, CI ingest) see events at emit time, not at
OS-buffer cadence. The path is logged at `info!` when the spawn
starts.

**Retention.** Logs older than `[logs] retention_days` (default 14)
are deleted on `loom loop` startup. `retention_days = 0` disables
sweeping. The sweep is best-effort; deletion failures are logged at
`debug!` but never abort the run.

The terminal renderer and the disk writer subscribe to the same
`AgentEvent` stream — one channel, two subscribers, never two
parallel pipelines.

### Event Schema

`AgentEvent` is loom's typed event union — the public contract
between producers (loom + agent backends) and downstream consumers
(terminal renderer, disk log, `--json` pipelines, SSE bridges, log
analyzers). It lives in the `loom-events` crate. Field-level
shapes are defined by the Rust types with serde derives; this
section names the *shape* of the contract.

**Wire shape.** Flat tagged JSON — one top-level `kind`
discriminator, no nested envelopes. A consumer dispatches with one
`match` (Rust) or one `switch (event.kind)` (TypeScript).

**Common envelope.** Every event carries seven structural fields
plus its variant-specific payload, all flat at the top level:

| Field | Type | Purpose |
|-------|------|---------|
| `kind` | `string` | Discriminator — variant name, snake_case |
| `bead_id` | `string` | Per-bead routing |
| `molecule_id` | `string` | Per-molecule grouping for push-gate / multi-bead UIs |
| `iteration` | `u32` | Bead's iteration counter (1-based) |
| `source` | `"agent" \| "driver"` | Distinguishes agent activity from driver-emitted events |
| `ts_ms` | `i64` | Unix milliseconds UTC |
| `seq` | `u64` | Monotonic per-bead-spawn counter — SSE resume key (`Last-Event-ID: <bead_id>:<seq>`) |

**Variant set.** Eighteen variants, flat tagged enum, snake_case on
the wire and in Rust:

- **Lifecycle** — `agent_start`, `agent_end`, `turn_start`,
  `turn_end`, `session_complete`
- **Streaming** — `text_delta`, `text_end`, `thinking_delta`,
  `thinking_end`, `toolcall_delta`
- **Tools** — `tool_call`, `tool_result`, `tool_progress`
- **Operational** — `compaction_start`, `compaction_end`,
  `auto_retry`, `error`
- **Driver catch-all** — `driver_event`

Field-level payload shapes per variant are defined by the Rust
types in `loom-events`; the crate's API docs are the source of
truth for per-variant fields.

**Architecture-bearing types.** Four load-bearing patterns from
this schema and the surrounding session contract, each enforcing
an invariant structurally:

- **`Session` trait** — the public agent-driver contract, defined
  in `loom-events`. Workflow code holds backends as
  `Box<dyn Session>` so per-phase backend selection is a runtime
  choice rather than a compile-time one. The trait exposes
  `prompt(msg) -> EventStream`, `steer(msg)`, `cancel()`, and
  `set_mode(mode)`; its `Events` associated type is concretized
  to `Pin<Box<dyn Stream<Item = AgentEvent> + Send>>` so
  `dyn Session` is dyn-compatible without trait-variant
  gymnastics. Backends pick their own stream type internally; the
  box happens at the trait boundary. Subprocess-driving backends
  (Pi, Claude) keep a typestate (`AgentSession<Idle|Active>`) as
  an *internal mechanic* — handshake completed, stdin attached,
  etc. — but that typestate does not leak through `Session`.
  Backends that don't drive a subprocess (Direct, future
  ACP-exposed sessions) carry no typestate at all; the
  asymmetry is *why* the trait belongs on top.
- **ID newtypes** (`BeadId`, `MoleculeId`, `ToolCallId`, etc.) —
  `#[serde(transparent)]` wrappers over `String`. Construction
  validates at the parse boundary; downstream code receives the
  typed form, never raw `String`.
- **Parser-to-stamper split** — the parser layer cannot see the
  live envelope (bead id, molecule id, iteration), so it emits
  `ParsedAgentEvent` carrying only payload + parser-derived fields.
  The session layer is the only constructor of `AgentEvent`,
  combining a `ParsedAgentEvent` with an `Envelope`. The compiler
  makes "unstamped event reaches a consumer" unrepresentable.
- **`DriverKind` typed enum with `Other(String)` fallback** — on
  the wire `driver_kind` is a string for forward compatibility; in
  Rust it deserializes to an enum with an `Other` arm for unknown
  values. Producers cannot typo a kind; consumers get exhaustive
  `match` plus graceful unknown-handling.

**Driver events.** `driver_event` carries a `driver_kind` string
discriminator (`verdict_gate`, `retry_dispatch`, `push_gate_walk`,
`push_gate_refuse`, `push_gate_clean`, `container_spawn`,
`container_oom`, `infra_failure`, `doom_loop_tripped`,
`duplicate_tool_result`, `token_usage`, …) plus a free-form `summary`
and structured `payload`. Adding a new producing variant is additive
on the wire — older consumers fall through to a generic render via
`DriverKind::Other`. The observer-emitted variants
(`doom_loop_tripped`, `duplicate_tool_result`, `token_usage`)
originate in `llm` rather than `loom-driver`, so they fire on
both Loom-binary runs and external consumer-driven `Conversation`
runs.

**Schema versioning.** `agent_start` carries `schema_version: u32`
(currently `1`). Adding new variants, new fields on existing
variants, or new `driver_kind` values is minor (consumers ignore
unknown variants / fields). Renaming, removing, or repurposing
fields requires a major bump. Consumers version-gate on the major.
The Rust API tracks the same surface — non-additive enum changes
are a `loom-events` crate major bump. Consumers must accept unknown
`kind` values gracefully (drop or render as `<unknown>`); unknown
variants are the contract working across versions.

**Backend adapters.** Per-backend wire schemas (Pi-mono RPC,
Claude Code stream-json, the `loom-direct-runner` JSONL stream)
are flattened into the same `AgentEvent` variant set at the parser
layer. See [agent.md](agent.md) for each backend's
adapter contract.

**SSE integration.** A pipeline runner that wants to broadcast a
bead's event stream over SSE pulls `loom-events`, tails the bead's
JSONL log, deserializes each line as `AgentEvent`, and emits
`id: <bead_id>:<seq>\nevent: <kind>\ndata: <json>\n\n`. SSE clients
resume on disconnect via `Last-Event-ID`. Loom does not ship an SSE
server — `loom-events` is the integration boundary; the pipeline
runner owns the rest.

**Disk writer contract.** `LogSink` writes the same `AgentEvent`
stream the renderer consumes, with per-event flush. The flush is
the contract — downstream `tail -f` and file-watcher SSE bridges
see each event at emit time, not at OS-buffer cadence. The agent's
IO is bound by the disk write+flush — measured at <100µs per event
on local SSD, well below per-token agent latency, so no
async channel or backpressure machinery is justified. `LogSink`
implements the `EventSink` trait (below); it is the persistence
impl of a general consumer interface.

### EventSink and SessionCommand

`EventSink` is the universal `AgentEvent` consumer interface,
defined in `loom-events` alongside the event type:

```rust
pub trait EventSink: Send {
    fn emit(&mut self, event: &AgentEvent);
    fn react(&mut self) -> Vec<SessionCommand> { Vec::new() }
}

pub enum SessionCommand {
    Steer(String),   // inject a system message into the next turn
    Abort(String),   // terminate the session with this reason
}
```

**Contract:**

- `emit` is **sync** — sinks push to channels, write to disk, or
  mutate counters without awaiting. Sinks that need async work
  (e.g. network broadcast) own a channel internally.
- `emit` takes `&AgentEvent` — the driver owns the event;
  multiple sinks read it without cloning.
- `Send` bound supports multi-runtime deployments (SaaS).
- `react()` is **pull-based**, default empty. The driver invokes
  it after every **non-streaming** event (lifecycle, tool, driver,
  operational) and applies the returned commands to the live
  `Session`. Streaming variants (`text_delta`, `thinking_delta`,
  `toolcall_delta`) do not trigger `react()` — observer state
  doesn't change on text bytes, and polling them would be pure
  overhead.

**Composition.** Sinks compose via a chainable `.tee(other)` method
producing `TeeSink<Self, Other>`. The driver builds a static-typed
chain at session start; registration order is the `react()`
invocation order:

```rust
let sink = LogSink::new(path)
    .tee(DoomLoopObserver::new(config))
    .tee(DuplicateResultObserver::new(config));
```

`react()` priority: any returned `Abort` is terminal — the driver
cancels the session immediately and ignores subsequent commands in
the same batch. `Steer` commands process in registration order
before the next event is read.

`SessionCommand`'s variant set is deliberately narrower than
`Session`'s own surface (`steer` / `cancel` / `set_mode`) — observers
only have two levers, both safety-relevant. Direct callers of
`Session` have the full surface.

### Logs UX

`loom logs` replays or tails a saved log file via the same renderer used
by `loom loop`. Reusing the renderer (rather than shipping a second
formatter) keeps live and replay output identical and prevents drift.

| Flag | Behavior |
|------|----------|
| (default) | Pretty-render the most recent bead's full log; exit at EOF |
| `-f` / `--follow` | Same renderer, tail-mode (block on EOF, like `tail -f`) |
| `-b` / `--bead <id>` | Select a specific bead instead of the most recent |
| `-v` / `--verbose` | Stream assistant text deltas (parity with `loom loop -v`) |
| `--raw` | Emit raw JSONL bytes from the file, unparsed (for `jq` pipelines) |
| `--path` | Print the resolved log file path and exit; preserves today's `tail -f $(loom logs --path)` recipe |

`-f` and `--raw` compose: `loom logs -f --raw` tails raw JSONL, the
spiritual successor to today's `tail -f $(loom logs)` shorthand.
`--path` is mutually exclusive with `-f`, `-v`, and `--raw` — it
short-circuits to a path-only output before any rendering happens.
`-b` combines with everything (it just changes which file is selected).

**Empty-logs case.** Bare `loom logs` against an empty
`.loom/logs/` prints a one-line message
(`No bead logs yet. Run 'loom loop' to generate one.`) and exits 0 —
this is normal post-`loom init`, not an error. `--path` against an
empty logs directory exits non-zero with a clear error so scripts
relying on `$(loom logs --path)` fail loudly rather than expanding to
an empty string.

**No auto-follow.** Bare `loom logs` does **not** detect a still-running
bead and switch to follow mode automatically. Auto-detection (file
mtime, fd introspection) is brittle and surprising. Users who want
live tailing pass `-f` explicitly — matches the `tail` vs `tail -f`
mental model already in muscle memory.

### Verdict Gate

After every agent phase ends, the verdict gate evaluates the result
before the bead's state can advance. The gate runs in two passes:
`loom gate verify` (deterministic — mechanical signals, `[check]` /
`[test]` / `[system]` verifiers, style linters) followed by `loom
gate review` (LLM-judged rubric). The review rubric, inputs, and
concerns are defined in [gate.md](gate.md); this section
retains the execution layer — the decision table, recovery mechanics,
markers, labels, and infra-failure handling.

**Where the gate runs — two stages at the loom workspace.**

Per-bead integration runs at the **loom workspace**, inside the
`index.lock` critical section. The step has six phases, all
atomic under the lock:

1. **Fetch** the bead branch from the bead workspace path into the
   loom workspace as `loom/<id>`. The fetch is against a filesystem
   path (`git fetch .loom/beads/<id> loom/<id>:loom/<id>`), not the
   network — workers never push (see [Bead Dispatch § Bead branch
   flow](#bead-dispatch)).
2. **Verify signatures (pass 1)** on the fetched commits using
   `git verify-commit` against
   `<workspace>/.git/loom-allowed-signers` (see
   [Commit signing](#commit-signing)). Conditional on the signing
   key resolving; skipped if no key is configured. Verification
   failure routes the bead to `loom:blocked` with cause
   `signature-verification-failed` — agent-retry cannot fix a
   signature on existing commits, so this is operator
   investigation territory (likely a misconfigured wrapix
   container or missing key bind-mount).
3. **Rebase** the bead branch onto the integration branch. On
   textual conflict, `git rerere` replays any previously-recorded
   resolution (rerere is enabled in the loom workspace's gitconfig
   at `loom init`; see [Commit signing](#commit-signing)); if
   conflicts remain, `git rebase --abort` returns the loom
   workspace to its pre-rebase state and routes the bead to
   recovery with cause `integration-conflict` carrying
   `{ files, new_base_sha }`. The
   recovery budget is **one** agent-retry pass (not the full
   `[loop] max_retries`); the agent's next dispatch sees the
   conflict files in `previous_failure` and is expected to rebase
   its work onto the new tip in its bead workspace, resolve, and
   re-commit. A second rebase-conflict on the retry escalates to
   `loom:clarify` — same `integration-conflict` cause, human-
   resolution required.
4. **Verify signatures (pass 2)** on the rewritten commits the
   rebase produced. Conditional on the same signing-key
   resolution. Failure here routes to `loom:blocked` with cause
   `signature-verification-failed` — meaning loom's own signing
   setup is broken (gitconfig-write skipped, key path wrong,
   allowed_signers missing). The recovery detail distinguishes
   "pass 1" (worker-side signing broken) from "pass 2"
   (driver-side signing broken) so the operator knows where to
   look.
5. **FF-merge** the bead branch into the integration branch and
   run `prek run --hook-stage pre-push --all-files` against the
   integrated tree (cargo + clippy + nextest + `loom gate
   verify`). On verify-pass, walk the LLM rubric at per-bead
   scope and consume any findings via `loom gate mint --bead <id>`
   (see [gate.md § Stages](gate.md#stages)); this is the rubric-
   walking action that produces fix-up beads, distinct from the
   inspection-only `loom gate review` subcommand. No `loom gate
   review` here, no marker mint, no push.
6. **Delete the transient `loom/<id>` ref** with `git branch -D`,
   unconditionally — whether the path exited cleanly, rolled
   back from audit-fail, or aborted from rebase conflict. The
   bead clone keeps its copy of the branch until the bead's
   workspace is reaped on `bd close`.

On audit-fail, the integration is rolled back via
`git reset --hard HEAD~1` and the bead routes to recovery with
cause `post-integrate-fail`. On audit-pass, the bead's integration
is durable and the lock is released — any findings mint emits
become bonded fix-up beads in the molecule and are picked up by
the next `loom loop` iteration; they do not trigger rollback. A
structural mint failure (dispatch error, duplicate fingerprint
label, conflicting epic state) refuses the mint and surfaces the
conflicting bead ids; integration stays durable, the operator
clears the conflict before re-running.

Molecule-completion push gate runs at the **loom workspace** after
all beads in the molecule have integrated: full audit
(`prek run --hook-stage pre-push --all-files` + `loom gate review
--diff <molecule.base_commit>..HEAD`), mint `MarkerProof`, then
`git push origin <integration-branch>`. The critical section spans
**audit + mint + push** atomically — releasing the lock between
mint and push would let another verdict gate's rebase mutate HEAD,
invalidating the just-minted marker. prek's pre-push hook chain
fires on the push; the `pre-push-checks` wrapper around each
slow-tier hook reads the just-minted marker (via `loom gate
verify-marker` or equivalent marker-validation logic) and
short-circuits the slow tier on a valid marker.

Both stages at the loom workspace are load-bearing for
parallel-agent correctness: the marker bound to HEAD's tree OID
must match the state being pushed, and rebasing bead A onto
integration that already includes bead B mutates A's commit SHA
(and tree). Auditing at the bead workspace pre-integration would
mint a marker that becomes stale the moment another bead lands
first. The two stages also separate concerns: per-bead catches
cross-bead deterministic interactions (compile/lint/test
breakage) at the bead that introduced them; per-molecule catches
review-level concerns once over the cumulative diff.

**Cost and queue depth.** Parallel beads complete in their bead
containers concurrently, but their loom-workspace integration
steps serialize. Each per-bead pass is ~30–60s (cargo warm +
verify); the per-molecule push gate adds ~30–90s (review LLM
call). At high parallelism (N beads landing simultaneously),
queue depth × per-pass time is the integration tail. For typical
workloads (N ≤ 4) this is sub-5min; for higher N, the queue is
the bottleneck and the throughput gain from per-bead-container
parallelism caps.

**Bead-container self-verify is feedback only.** The bead workspace
inherits `core.hooksPath` from the loom workspace's `wrapix.prekHooks`
installation (see [pre-commit.md § Agent self-verify in the bead
container](pre-commit.md)), so prek fires on the agent's commits,
catching treefmt drift, integrity findings, and obvious cargo
failures in-session. It is **not** the
trust source for the marker. The driver's verdict gate at the loom
workspace runs its own independent audit; the agent cannot mint a
`MarkerProof` and cannot bypass driver verification by emitting a
structured "I verified" report. The agent's hook chain is a
feedback layer that reduces wasted recovery iterations, not an
authorization mechanism.

**Marker mint at the molecule-completion push gate.** Per
[gate.md § Marker](gate.md#marker), the mint trigger is the
molecule-completion push gate's audit-pass: the gate constructs
`GateSuccess`, calls
`MarkerProof::from_gate_success(success, loom_workspace)`, writes
the sealed marker atomically to `.loom/marker.json`, then
runs the integration push — all inside the same critical section.
prek's pre-push hook chain fires on the push; the `pre-push-checks`
wrapper around each slow-tier hook reads the just-minted marker and
short-circuits the underlying command (per
[pre-commit.md § Marker integration](pre-commit.md#marker-integration)).
There is no standalone `loom gate verify-marker` hook gating the
chain; the wrapper is the marker's only push-time consumer, and
marker absence is a fall-through condition (operator-manual push),
not a push failure. Per-bead integration steps do not mint markers
(they do not push); the marker is one per molecule, covering the
cumulative integrated state. The marker's content-addressed
validation is what makes the push fast in the warm-driver-loop
case without trusting an unverified stamp.

Driver-detected failures enter a bounded recovery loop; agent
self-reports go straight to human resolution via `loom msg`.

**Interactive vs worker sessions.** The verdict gate's reconciliation
applies to **worker sessions only** — single-shot agent dispatches
against a bead or molecule (`loom loop`'s per-bead worker,
`loom todo_new` / `loom todo_update`, `loom gate review`).
**Interactive sessions** —
multi-turn chats with a human in the loop (`loom plan -n` /
`loom plan -u`, `loom msg -c`; identifiable in the template layer
by inclusion of `chat_marker_final_turn_only.md`) — are
agent-and-human authoritative:
the driver does **not** mutate bd state as a consequence of an
interactive session. Whatever bd state exists at session end IS the
state. The chat agent has full bd-write authority on the beads it
operates against (close, status change, label add/remove, notes
write); the human authorizes each turn, so the chokepoint reasoning
that justifies driver-side mint for worker sessions (replay-safety,
cross-finding dedup, deterministic per-spec routing) does not apply.

Concretely, the driver's only post-session action after an
interactive session is parsing the terminal marker for log +
exit-code. Interactive sessions emit `LOOM_COMPLETE` only —
`LOOM_RETRY` / `LOOM_BLOCKED` / `LOOM_CLARIFY` / `LOOM_CONCERN`
are wrong-phase-marker errors and exit non-zero.

The driver-side reconciliation paths that fire for worker sessions
are uniformly suppressed: no `loom:blocked` / `loom:clarify` label
application, no bead-status changes, no `loom gate verify` /
`loom gate review` against the session's effects, no fix-up bead
minting. On mid-session failure (container OOM, observer abort,
marker swallowed) the driver exits non-zero with a diagnostic
without auto-retry — the one-free-retry infra-failure path applies
to expensive worker dispatches; an interactive session re-invocation
is cheap and the user is right there to redispatch.

The decision table below therefore documents **worker-session**
outcomes only. Interactive sessions short-circuit before the table
is consulted.

**Decision table** (worker sessions only — interactive sessions
short-circuit before this table is consulted, per *Interactive vs
worker sessions* above). The gate inspects five signals — the
agent's exit marker, whether the bead was bd-closed, whether the
bead-branch diff is empty (no commits since dispatch), whether the
working tree is clean (`git status --porcelain` empty), and the
review verdict — and produces one of four outcomes (`done`,
`blocked`, `clarify`, or `recovery` with a cause):

| Marker | bd-closed | Diff | Tree clean | Review | Outcome |
|--------|-----------|------|------------|--------|---------|
| `LOOM_BLOCKED` | — | — | — | — | `blocked` |
| `LOOM_CLARIFY` | — | — | — | — | `clarify` |
| `LOOM_RETRY` | — | — | — | — | recovery (`agent-retry`) |
| (none) | — | — | — | — | recovery (`swallowed-marker` OR `observer-abort`; see below) |
| `LOOM_COMPLETE` | no | — | — | — | recovery (`incomplete-signaling`) |
| `LOOM_COMPLETE` | yes | empty | — | — | recovery (`zero-progress`) |
| `LOOM_COMPLETE` | yes | non-empty | no | — | recovery (`tree-not-clean`) |
| `LOOM_COMPLETE` | yes | non-empty | yes | verify-fail (review may also raise a concern) | recovery (`verify-fail`; review notes appended if any) |
| `LOOM_COMPLETE` | yes | non-empty | yes | verify-pass + review-concern | recovery (`review-concern`) |
| `LOOM_COMPLETE` | yes | non-empty | yes | verify-pass + review-bad-walk | recovery (`bad-walk`) |
| `LOOM_COMPLETE` | yes | non-empty | yes | verify-pass + review-pass | `done` |
| `LOOM_NOOP` | yes | * | no | — | recovery (`tree-not-clean`) |
| `LOOM_NOOP` | yes | * | yes | verify-fail (review may also raise a concern) | recovery (`verify-fail`; review notes appended if any) |
| `LOOM_NOOP` | yes | * | yes | verify-pass + review-concern | recovery (`review-concern`) |
| `LOOM_NOOP` | yes | * | yes | verify-pass + review-bad-walk | recovery (`bad-walk`) |
| `LOOM_NOOP` | yes | * | yes | verify-pass + review-pass | `done` |

In the table above, `—` means the signal isn't inspected because an
earlier signal already determined the outcome (e.g. an agent self-report
short-circuits before review runs); `*` means any value is accepted.

**Loom-workspace integration outcomes.** The decision table covers
the per-bead exit signals. Independently, the per-bead integration
step in the loom workspace can fail in four distinct ways even
when the bead's own exit signals were clean:

| Failure phase | Cause | Detail | Recovery |
|---------------|-------|--------|----------|
| Signature verification pass 1 (fetched commits) | `signature-verification-failed` | side = `worker` | `loom:blocked` — operator investigates wrapix container signing setup |
| Rebase | `integration-conflict` | `{ files, new_base_sha }` | one agent-retry pass; second failure escalates to `loom:clarify` (same cause) |
| Signature verification pass 2 (rewritten commits) | `signature-verification-failed` | side = `driver` | `loom:blocked` — operator investigates loom-workspace gitconfig + key resolution |
| Audit (`prek run --hook-stage pre-push` + verify) | `post-integrate-fail` | `Vec<VerifierFailure>` | rollback via `git reset --hard HEAD~1`; route to recovery so next iteration sees the cross-bead breakage |

`post-integrate-fail` covers cross-bead interactions (bead A's API
change breaks tests bead B introduced earlier in the molecule),
rebase-induced breakage, and integration-tree state that no
bead-workspace verify could anticipate. The recovery prompt
includes the audit's specific failures (verify failures, review
concern, or both) so the next iteration can address the cross-bead
interaction. Other beads in the molecule that haven't integrated
yet continue to be dispatched; their integrations queue at
`index.lock` after recovery resolves.

`integration-conflict` carries the conflict file list and the new
integration tip SHA so the agent's next dispatch can rebase its
bead-workspace branch onto the new tip and resolve. The single-
retry cap reflects the empirical reality that agents are uneven
at textual git conflict resolution; one honest attempt is more
useful than burning the full `[loop] max_retries` budget on a
structural problem. Permanent escalation lands as `loom:clarify`
with the same cause so the operator sees the conflict files and
the SHAs in the bead's notes.

`signature-verification-failed` cannot be addressed by agent retry
— signatures are bound to existing commits and the cause is
environmental (wrapix container misconfiguration on pass 1,
loom-workspace gitconfig drift or missing allowed_signers on
pass 2). The bead routes to `loom:blocked` immediately; the
operator investigates per the `side` discriminator in the cause
detail (`worker` → check the bead container's
`git-ssh-setup.sh`-rendered `~/.gitconfig`; `driver` → check
loom's workspace gitconfig writes and the resolver's signing-key
resolution).

**Tree-clean check.** After the agent emits `LOOM_COMPLETE` or
`LOOM_NOOP` and the bead is bd-closed, the driver runs `git status
--porcelain` against the bead's workspace. The pre-attempt reset
described in [Bead Dispatch](#bead-dispatch) — `git reset --hard
HEAD` plus `git clean -fdx` (with `target/`, `.git/`, `.wrapix/`
excluded) — is the load-bearing source of the empty-starting-tree
guarantee. The bead workspace persists across attempts under the
per-bead-close lifecycle, so freshness from `create_worktree` is
*not* what the gate relies on; the reset re-establishes the
invariant on every dispatch. A non-empty porcelain output post-bead
is therefore unambiguously either the agent's own intra-session
leftover *or* a reset-step bug — both are surfaced as
`tree-not-clean` so the failure mode lands in recovery rather than
masquerading as dispatch success. The operator's `/workspace` is a
separate clone and cannot leak edits into a bead workspace. A
non-empty result — tracked files modified-but-not-staged,
staged-but-not-committed, or untracked files outside the ignore
set — routes to `recovery` with cause `tree-not-clean`, taking
precedence over the verify / review steps (running verifiers
against a half-staged tree would conflate the agent's intended diff
with its leftover scratch). The recovery detail names the dirty
paths so the next iteration's agent can stage them into a follow-up
commit or revert them. This catches the failure mode where a worker
runs a formatter (or other tree-touching tool), stages only its own
paths, and leaves unrelated tracked files dirty — pre-commit hooks
structurally cannot see this because their stash/restore
architecture hides unstaged changes (see
[pre-commit.md — Out of Scope](pre-commit.md)).

**Disambiguating "no marker"** (`observer-abort` vs
`swallowed-marker`): when an `EventSink`'s `react()` returns
`SessionCommand::Abort` and the driver terminates the session
before the agent emits any marker, the cause is
`observer-abort` — not `swallowed-marker`. The driver knows
because it issued the cancel; the recovery cause's detail names
the responsible observer + the reason it gave. Without this
disambiguation, doom-loop kills would mis-classify as agent
sloppiness instead of legitimate driver-detected failure.

**Closure is the agent's responsibility.** The driver never calls
`bd close` on a bead it dispatched. The `bd-closed` column is an
*observable* — the agent invokes `bd close <id>` itself per the loop-phase
prompt contract — not a driver action. A driver that auto-closes on
`exit_code == 0` collapses every marker into `done` and silently masks
`LOOM_BLOCKED` / `LOOM_CLARIFY` self-reports, which is why marker
parsing (not exit_code) must be the primary outcome signal for every
phase that spawns an agent.

`recovery` resolves to `retry` if the molecule's iteration counter is
below `[loop] max_iterations` (default 10), otherwise `blocked` with
the cause preserved in `bd update --notes`. The iteration counter is
**molecule-level** state — stored in `molecules.iteration_count` (see
the schema in *SQLite State Store* below) — and survives
`retry → [running]` round-trips. Per FR1, the same counter bounds
`loom loop`'s outer loop on fix-up beads: every full molecule pass
(initial pass + each fix-up pass produced by the verdict gate)
consumes one slot. This is the same knob as the per-bead recovery
loop because a fix-up bead getting picked up *is* a molecule pass —
the two concepts collapse onto one molecule-level counter, with
in-session retry left to `[loop] max_retries` (default 2).

**Mechanical vs review.** Marker parsing, bd-closed lookup, and diff
inspection are deterministic. The gate then runs **every**
`[check]` / `[test]` / `[system]` verifier attached to the bead's
success criteria (see [gate.md](gate.md)) — none
short-circuit each other. Per verifier, the gate captures pass/fail
+ stderr.

**Review always runs**, regardless of `loom gate verify` results.
If verify failed, review still runs so the agent gets verify failures
*and* live-path / scope / `[judge]` feedback in one `previous_failure`
round trip — otherwise the agent might "fix" a failing test by
mocking harder and reach `done` on the next iteration before review
catches it.

When verify fails, the recovery cause is `verify-fail` (mechanical
trumps semantic), and review's concern reasoning, if any, is appended
to the `previous_failure` detail under a `Review notes:` heading.

A `LOOM_CONCERN` marker from the review phase produces `recovery`
with cause `review-concern`; the detail carries the parsed
`{"summary": "..."}` payload plus the buffered `LOOM_FINDING:`
records the walk streamed. Per-finding routing (which spec, which
fix-up bead, clarify vs. fix-up vs. blocked) is decided by `loom
gate mint` on each `LOOM_FINDING:` line; the terminal marker carries
only the verdict-log summary. **Clarify-bound findings** (any token
routing to `loom:clarify`, currently `invariant-clash`) raise
`loom:clarify` on the minted fix-up bead with the `## Options — …`
block extracted from the finding's `evidence` payload, per
[gate.md § Findings and Minting](gate.md#findings-and-minting). A
clarify-bound finding whose evidence lacks a well-formed options
block falls back to `loom:blocked` with cause
`clarify-without-options` so no stranded clarify bead is created.

A malformed `LOOM_CONCERN` payload, or a stream/terminator
mismatch (findings streamed with `LOOM_COMPLETE`, or `LOOM_CONCERN`
emitted with zero findings), surfaces as `recovery` with cause
`bad-walk`, carrying the typed `BadWalk` variant per the recovery
table below. These are distinct from `swallowed-marker`: the agent
*did* attempt a terminal signal but the shape was wrong, and the
recovery prompt can quote the malformed payload back to the agent
on the next iteration.

**Self-reports route by intent.** The three worker-phase self-report
markers route distinctly:

- `LOOM_BLOCKED` and `LOOM_CLARIFY` are agent self-reports that re-
  running the same prompt cannot resolve — the agent has already
  judged that the obstacle is human-resolvable, not retry-resolvable.
  The gate exits straight to `[blocked]` / `[clarify]` for human
  resolution, skipping the recovery loop. (For `LOOM_CLARIFY`, the
  gate first validates the bead's options block per the marker
  definition above; a missing or malformed block downgrades to
  `[blocked]` with cause `clarify-without-options` rather than
  stranding a clarify the chat-drafter cannot resolve.)
- `LOOM_RETRY` is an agent self-report that re-running *can* resolve
  — the obstacle is environmental or approach-specific, not
  judgment-requiring. The gate enters the recovery loop with cause
  `agent-retry`, consuming one `[loop] max_retries` slot. Exhaustion
  routes to `loom:blocked` with cause `retry-exhausted`; the agent
  is instructed in the recovery prompt to escalate to `LOOM_BLOCKED`
  / `LOOM_CLARIFY` directly if the same problem persists rather than
  re-emitting `LOOM_RETRY`.

**Driver-detected causes flow through recovery.** Swallowed marker,
incomplete signaling, zero-progress, tree-not-clean, verify-fail,
review-concern, and bad-walk all enter the recovery loop. Each recovery iteration
either retries the bead in place with prior failure context, or — when
the failure shape calls for a discrete follow-up unit of work — spawns
a **fix-up bead**.

**Fix-up beads bond to the originating molecule.** Every fix-up bead
created during recovery is bonded to the failing bead's molecule via
`bd mol bond <molecule-id> <fix-up-bead-id>` **before dispatch** (i.e.,
before the bead becomes eligible for `loom loop` to pick up). The bond
is mandatory and atomic with creation — a fix-up bead that is not
bonded to a molecule by the time it leaves the verdict gate is a bug.

Bonding is load-bearing in two places:

1. The **push gate** refuses to push while any bead in the molecule
   carries `loom:blocked` or `loom:clarify`. Orphan fix-up beads are
   invisible to that check, so a molecule could push with unresolved
   work attached to a shadow bead the gate never saw.
2. **Auto-iteration** (Push gate, Functional #9) walks `bd mol
   progress <id>` to decide whether the molecule is clean. Orphan
   fix-up beads are absent from that walk; the molecule looks done
   even when its remediation work is pending.

The originating molecule is resolved by reading the failing bead's
existing molecule bond — `bd show <id> --json` returns the molecule
ID. If the failing bead is itself unbonded (which is itself a bug
upstream), the verdict gate refuses to spawn a fix-up bead and
escalates to `loom:blocked` with cause `unbonded-origin` so the
inconsistency surfaces immediately rather than propagating.

**Recovery context (`previous_failure`).** On `retry → [running]`, the next
session's prompt is rendered with a **typed** `PreviousFailure` value
plus optional `review_notes` and an `attempt` counter — the shape lives
in [templates.md](templates.md). The template renders each
variant with distinct framing. Detail content per cause (each variant
capped, total truncated to `PREVIOUS_FAILURE_MAX_LEN = 4000` chars):

| Cause | `PreviousFailure` variant | Detail content |
|-------|----|----|
| `swallowed-marker` | `DriverNotice` | "Last phase ended without a `LOOM_*` exit marker." |
| `incomplete-signaling` | `DriverNotice` | "Marker `LOOM_COMPLETE` emitted but bead `<id>` was not bd-closed." |
| `zero-progress` | `DriverNotice` | "Marker `LOOM_COMPLETE` emitted with empty diff. Use `LOOM_NOOP` if no work was needed." |
| `tree-not-clean` | `TreeNotClean { dirty_paths: Vec<String> }` | "Working tree was not clean after the bead committed: <N> uncommitted path(s). Stage them into a follow-up commit or revert them." Path list is capped at 30 entries; the truncation suffix names the overflow count. |
| `observer-abort` | `DriverNotice` | "Session aborted by `<observer name>`: `<reason>`." |
| `verify-fail` | `VerifyFailures(Vec<VerifierFailure>)` | One `VerifierFailure { target, exit_code, stderr_tail }` per failing `[check]` / `[test]` / `[system]` verifier. All failing verifiers are included; the budget is split across them with later failures truncated first; each `stderr_tail` is capped at ~1500 chars before split. If `review` also raised a concern, its reasoning is set as `review_notes` (separate ~1000-char budget) rendered under a `Review notes:` heading. |
| `review-concern` | `ReviewConcern { summary: String, findings: Vec<Finding> }` | Summary is the parsed `summary` field from the terminal `LOOM_CONCERN: {"summary": "..."}` marker. `findings` is the buffered list of `LOOM_FINDING:` records the walk streamed before the terminator (per the typed `Finding` record in [gate.md § Findings and Minting](gate.md#findings-and-minting)). Per-finding tokens drive `mint`'s routing: clarify-bound findings mint as single-finding clarify beads (one per finding), all other findings bundle into per-spec fix-up batches (one batch per lead-spec). The recovery prompt renders the summary plus a one-line-per-finding `evidence` digest. The in-code recovery cause is `RecoveryCause::ReviewConcern`. |
| `bad-walk` (concern-malformed) | `BadWalk(BadWalk::Concern { payload: String, parsed_findings: Vec<Finding> })` | "Your `LOOM_CONCERN:` payload did not parse as `{"summary": "<non-empty>"}`. Literal payload after the marker: `<payload>`." When `parsed_findings` is non-empty, append a per-finding digest so the agent's diagnosis from the well-formed streamed findings is not lost. Wrapped-enum pattern mirrors `RecoveryCause::ReviewConcern(ReviewFlag)`. |
| `bad-walk` (concern-without-findings) | `BadWalk(BadWalk::ConcernWithoutFindings { summary: String })` | "You emitted `LOOM_CONCERN` with summary `<summary>` but no `LOOM_FINDING:` lines streamed. Either emit findings before the terminator or terminate with `LOOM_COMPLETE`." |
| `bad-walk` (findings-without-concern) | `BadWalk(BadWalk::FindingsWithoutConcern { finding_count: usize, findings: Vec<Finding> })` | "You streamed `<finding_count>` `LOOM_FINDING:` line(s) but terminated with `LOOM_COMPLETE`. Use `LOOM_CONCERN: {"summary": "..."}` when findings are emitted." Per-finding digest of `findings` is appended so the agent's next iteration sees the diagnosis it just emitted. |
| `bad-walk` (malformed-finding) | `BadWalk(BadWalk::MalformedFinding { errors: Vec<FindingParseError>, terminal: TerminalSurface })` | "One or more `LOOM_FINDING:` lines failed parse." Per-line errors are enumerated; the well-formed terminal is rendered alongside so the agent fixes the malformation (typically: drop the surrounding markdown fence) without losing the surrounding well-formed context. This is the variant that fires on backtick-wrapped finding lines whose JSON otherwise would have parsed. |
| `integration-conflict` | `IntegrationConflict { files: Vec<PathBuf>, new_base_sha: GitOid }` | "Your bead branch could not be rebased onto integration — files conflict: <files>. The new integration tip is <new_base_sha>. Rebase your bead workspace onto the new tip, resolve, and re-commit." Single-retry cap (not full `[loop] max_retries`); a second rebase-conflict escalates the bead to `loom:clarify` with the same cause. The `signature-verification-failed` cause does **not** appear in this table because it routes to `loom:blocked` immediately without an agent-retry pass — there is no next dispatch and thus no `PreviousFailure` context. |
| `post-integrate-fail` | `PostIntegrateFail { failures: Vec<VerifierFailure> }` | "After your bead was rebased onto the integration branch and ff'd, the post-integration verify failed at the loom workspace. The integration was rolled back. Specific failure: <verifier-failure blocks>." Used for cross-bead interaction breakage where the bead-workspace verify passed but the integrated tree's verify failed. Per-bead does not run `loom gate review` (per the per-bead step composition described above), so review-style concerns are not a `post-integrate-fail` cause — they fire at the molecule-completion push gate via `GateFailReason`. Capped at the shared `PREVIOUS_FAILURE_MAX_LEN` budget. |
| `agent-retry` | `AgentRetry { reason: String }` | "Previous attempt requested retry: <reason>. A fresh dispatch was scheduled." `reason` is the verbatim prose the agent wrote on the line preceding `LOOM_RETRY` (environmental detail or stuck-on-approach summary). Consumes one `[loop] max_retries` slot; on exhaustion the molecule escalates to `loom:blocked` with cause `retry-exhausted`. The recovery prompt instructs the retry attempt to escalate to `LOOM_BLOCKED` (no candidate resolutions) or `LOOM_CLARIFY` (with `## Options — …`) if the same problem persists rather than emitting `LOOM_RETRY` again. |

When `previous_failure.is_some() && attempt > 0`, the `loop.md`
template prepends a first-instruction reframe: *"Re-read the
previous failure block above and address its specific concern
before re-implementing."* The `attempt` counter is per-bead
in-session (bounded by `[loop] max_retries`), resetting when a
fresh bead is dispatched; molecule-level iteration is opaque to the
agent because each fix-up bead is a different prompt context.

Transcript excerpts are deliberately not included — the agent can re-read
its own session log if it needs prior tool-call context.

**Labels.**

- `loom:blocked` is applied by either: (a) the `LOOM_BLOCKED` agent marker, or
  (b) driver-detected gate failure with recovery exhausted, or (c)
  `loom gate mint` refusing to apply `loom:clarify` to a
  clarify-bound finding whose `evidence` lacks a well-formed
  `## Options — …` block (cause `clarify-without-options` — the
  agent should have emitted `LOOM_BLOCKED` directly, but the driver
  falls back to blocked rather than minting a stranded clarify bead
  the chat-drafter cannot resolve). All meanings are uniform from
  the human's perspective — the bead is blocked and `loom msg` is
  the resolution channel.
- `loom:clarify` is applied by either: (a) the `LOOM_CLARIFY` agent
  marker, (b) `loom gate mint` lifting a clarify-bound finding
  whose `evidence` carries a well-formed `## Options — …` block —
  the agent has a specific question with structured options for the
  human, persisted to bead state per the Options Format Contract,
  or (c) the per-bead integration step escalating
  `integration-conflict` after one agent-retry pass failed to
  resolve the rebase conflict. Driver-applied `integration-conflict`
  clarify beads carry the conflict files and the new integration
  tip SHA in the bead's notes, plus a synthesized `## Options — …`
  block satisfying the Options Format Contract with two
  `### Option N — …` subsections (resolve-in-bead-clone and
  abandon-the-bead). Synthesizing the block keeps the
  Options-Format invariant universal — no exemption case for
  driver-applied clarify.
- `LOOM_RETRY` does NOT apply a terminal label; it routes the bead
  into the recovery loop with cause `agent-retry`. On recovery
  exhaustion the cause becomes `retry-exhausted` and `loom:blocked`
  is applied at that point.
- The cause of a driver-applied `loom:blocked` (`swallowed-marker`,
  `incomplete-signaling`, `zero-progress`, `tree-not-clean`, `verify-fail`,
  `review-concern`, `bad-walk`, `observer-abort`, `retry-exhausted`,
  `post-integrate-fail`, `signature-verification-failed`,
  `clarify-without-options`) is preserved in the bead's notes.
  Per-cause sub-labels can be stacked on top later if filtering
  becomes important; the gate's terminal label stays `loom:blocked`.

**Marker definitions.** The agent ends every phase by emitting exactly
**one** marker on its own line, as the final output of the session.
Markers are **mutually exclusive** — a session emits one and only one.
Six markers are defined:

- `LOOM_COMPLETE` — the work succeeded. The agent has implemented the
  bead's criteria and `bd close`d the bead. The diff is non-empty
  (real changes); see `LOOM_NOOP` below for the zero-diff variant.
  Valid in every phase.
- `LOOM_NOOP` — the work was already done in tree; the phase
  intentionally produced an empty diff. Without `LOOM_NOOP`, an empty
  diff is treated as `zero-progress` (a recovery cause). The agent
  emits `LOOM_NOOP` to distinguish "no work needed" from "work
  attempted but produced no diff." Valid in worker phases (`loop`,
  `todo`); not valid in the review phase.
- `LOOM_RETRY` — the agent self-reports that this attempt cannot
  finish but a fresh dispatch is likely to succeed. Two failure
  shapes warrant `LOOM_RETRY`:
  - **Environmental.** Tools failing mid-session (cwd unlinked,
    sandbox/IO errors, dependency missing), where the failure is
    bound to this attempt's container/process and not to the work
    itself.
  - **Agent self-reset.** Prompt-context exhausted, the agent is
    stuck-but-not-blocked on its current approach and judges a
    fresh dispatch with prior-failure context will fare better.

  Write the reason on the line preceding the marker; the gate
  routes to recovery with cause `agent-retry`, populating
  `PreviousFailure::AgentRetry { reason }` (per [templates.md § Typed
  `PreviousFailure`](templates.md#typed-previousfailure)) for the
  next attempt. Consumes one `[loop] max_retries` slot; exhaustion
  routes to `loom:blocked` with cause `retry-exhausted` (same as the
  existing retry-exhaustion path). Valid in worker phases only
  (`loop`, `todo_*`, `review`); invalid in interactive sessions
  (`plan_*`, `msg`).
- `LOOM_BLOCKED` — the agent cannot proceed and is self-reporting a
  **genuine dead end** — no candidate resolutions to enumerate, no
  retry path the agent expects to succeed. Use `LOOM_RETRY` for
  environmental failures or stuck-on-this-approach cases; use
  `LOOM_CLARIFY` when the agent can frame the decision-point as a
  structured `## Options — …` block. Write the reason on prior lines
  before the marker; the gate applies `loom:blocked` to *this bead*
  and exits the verdict evaluation without entering recovery. Other
  beads in the molecule continue running; the labelled bead waits
  for human resolution via `loom msg` (where `msg -c` walks the
  human through candidate enumeration in-session). Valid in worker
  phases only — invalid in interactive sessions (`plan_*`, `msg`).
- `LOOM_CLARIFY` — the agent has a specific question with structured
  options for the human (per the [Options Format
  Contract](gate.md#options-format-contract)). The discriminator
  against `LOOM_BLOCKED`: clarify means *I can enumerate the
  candidate resolutions*; blocked means *I cannot*. The target bead
  is **the bead under dispatch** for `loop` / `review`, and
  **the molecule epic** for `todo_*` (per [templates.md —
  Decomposition Discipline](templates.md#decomposition-discipline)).
  Write the question + options block to the target bead's state
  (notes or description) before emitting the marker. **The gate
  validates the options block before applying the label**: it
  inspects the target bead's notes ∪ description for a well-formed
  `## Options — <summary>` heading with at least one
  `### Option <N> — <title>` subsection (same shape mint validates
  on a finding's `evidence`). If absent or malformed, the gate
  falls back to `loom:blocked` with cause `clarify-without-options`
  rather than applying `loom:clarify` — symmetric with the mint
  path's enforcement for finding-routed clarifies, so a forgetful
  agent cannot produce a stranded clarify bead the chat-drafter
  cannot resolve. On a well-formed options block the gate applies
  `loom:clarify` to the target bead and exits the verdict evaluation
  without entering recovery. Other beads in the molecule continue
  running; the labelled bead waits for `loom msg` resolution. Valid
  in worker phases only — invalid in interactive sessions (`plan_*`,
  `msg`).
- `LOOM_CONCERN` — the review phase found a quality issue with the
  molecule's work; push must not fire. Carries a JSON payload:
  `LOOM_CONCERN: {"summary": "<one-sentence summary>"}`. The
  payload is **terminator-shaped**, not routing-shaped — `summary`
  is a verdict-log entry, nothing else. Per-finding routing
  (concern token → fix-up bead, `invariant-clash` → clarify,
  per-spec bonding) is decided by `loom gate mint` on each
  `LOOM_FINDING:` line the walk streamed before the terminator.
  The walk must satisfy the streaming + terminator **pairing
  rule** defined in [gate.md § Findings and
  Minting](gate.md#findings-and-minting): `LOOM_CONCERN` iff
  ≥1 findings streamed, `LOOM_COMPLETE` iff zero findings. A
  mismatch routes to `RecoveryCause::BadWalk(BadWalk)` with the
  specific variant matching the malformation. **Review-phase-only**
  — emitting `LOOM_CONCERN` from any other phase is a
  `wrong-phase-marker` error in the verdict gate.

**Choosing a marker in the review phase.** Five markers are valid:

- `LOOM_COMPLETE` — clean review, no concerns.
- `LOOM_CONCERN: {"summary": "..."}` — review found one or more
  quality issues (each emitted as a streaming `LOOM_FINDING:` line
  during the walk); push refused, molecule re-enters recovery.
- `LOOM_RETRY` — review *itself* cannot run for an environmental
  reason (logs corrupt, workspace inaccessible, transient IO,
  missing prerequisite that should be present); a fresh dispatch
  should retry the walk. Consumes one `[loop] max_retries` slot.
  This is the appropriate marker for "I couldn't review for
  environmental reasons" — distinct from `LOOM_BLOCKED` (genuine
  dead end) and from `LOOM_CONCERN` (I reviewed and found a problem).
- `LOOM_BLOCKED` — review cannot run and the reviewer has no
  candidate resolution to enumerate. Reserve for genuine dead ends;
  prefer `LOOM_RETRY` for transient infrastructure failures.
- `LOOM_CLARIFY` — rare: review surfaces a spec ambiguity the
  reviewer can frame as a `## Options — …` block, requiring human
  resolution before the verdict can be rendered.

The five are mutually exclusive — exactly one per session. The
common case is `LOOM_COMPLETE` xor `LOOM_CONCERN`. Multiple
concerns are emitted as multiple `LOOM_FINDING:` lines during the
walk — each carries its own structured detail; the terminal
`LOOM_CONCERN` summary names the strongest only as a verdict-log
entry. The streamed findings are buffered into `previous_failure`
for recovery in addition to the summary.

The gate distinguishes markers by parsing **the final line of the
agent's final assistant message**. Because markers are mutually
exclusive, exactly one valid marker is expected on that line.
`exit_code` alone is insufficient because backend errors,
swallowed-marker turns, and successful self-reports all exit 0.

**Infra failures bypass the gate.** Pre-flight failures (image load, container
start) exit immediately as `blocked` with cause `infra-preflight` — there is
no agent output to evaluate. Mid-session failures (agent process exit
non-zero, container OOM, IO errors) get one free retry per `loom loop`,
tracked in driver memory; a second mid-session failure exits as `blocked`
with cause `infra-repeated`. This counter is separate from
`[loop] max_iterations` and does not persist across `loom loop` invocations.

### Loop Outcome Types

Architecture-bearing types per
[spec-conventions.md](../docs/spec-conventions.md) *In scope #4* —
the shape of these types is how Invariant 4 of [gate.md](gate.md)
("a divergence sits in the working tree undetected") becomes
structurally unrepresentable. A code path that yields a clean
`loom loop` exit *without* invoking the gate cannot compile.

**`LoopOutcome`** — the typed return of every successful `loom loop`
invocation, sequential and parallel. `LoopError` (`Result::Err`)
covers paths that never reached a clean outcome. `LoopOutcome` has
no `Default` and carries `#[must_use]` so the binary cannot drop
it on the floor:

```rust
#[must_use = "every loom loop produces a gate outcome — the binary must \
              inspect it before exiting"]
pub struct LoopOutcome {
    pub beads_processed: u32,
    pub beads_clarified: u32,
    pub beads_blocked: u32,
    pub outer_iterations: u32,
    pub gate: GateOutcome,   // not Option, no default
}
```

**`GateOutcome`** — three terminal variants. `Success` and `Fail`
both require a receipt minted only by gate-invocation code; `NoGate`
is the only legitimate "gate did not fire" terminal and carries the
reason so the human-readable summary names it:

```rust
#[must_use]
pub enum GateOutcome {
    Success(GateSuccess),                              // clean ship
    Fail(GateFail),                                    // gate ran, found problems
    NoGate { beads_processed: u32, reason: NoGateReason },
}

pub enum NoGateReason {
    NoBeadsReady,    // queue empty at start; nothing to gate
    OncePartial,     // `--once` processed beads, molecule not complete
}
```

**`GateSuccess`** — the bulletproof variant. Construction asserts
every condition the FR9 four-condition AND covers *plus* on-disk
evidence that the gate's child processes actually ran. Field shapes
are non-`Option`: absence of any value is a failure path that
constructs `GateFail` instead. The constructor lives in `loom-gate`
(alongside `MarkerProof::from_gate_success`, the mint authority that
consumes a sealed `GateSuccess`); the `_private: ()` field is the
structural seal that prevents struct-literal construction outside the
crate, so `GateSuccess::new` is the sole minting path regardless of
its `pub` visibility.

```rust
pub struct GateSuccess {
    pub verify_exit: i32,            // always 0 by construction
    pub review_exit: i32,            // always 0
    pub review_marker: ExitSignal,   // always ExitSignal::Complete
    pub review_log_path: PathBuf,    // file exists, non-empty, last line
                                     //   is a terminal AgentEvent matching marker
    pub total_handoffs: u32,         // >= 1
    _private: (),                    // structural seal — no struct-literal path
}

impl GateSuccess {
    /// Asserts: verify_exit == 0, review_exit == 0,
    /// review_marker == ExitSignal::Complete, review_log_path exists,
    /// file size > 0, last line parses as a terminal AgentEvent whose
    /// marker matches review_marker, total_handoffs >= 1.
    /// Any failure returns Err(GateFail::new(...)).
    pub fn new(...) -> Result<Self, GateFail> { ... }
}
```

**`GateFail`** — carries the failure reason explicitly so CLI/log
summaries and the next outer-loop iteration consume it directly,
without reverse-engineering from exit codes:

```rust
pub struct GateFail {
    pub reason: GateFailReason,
    pub verify_exit: Option<i32>,
    pub review_exit: Option<i32>,
    pub review_marker: Option<ExitSignal>,
    pub review_log_path: Option<PathBuf>,
    pub total_handoffs: u32,
    pub stalled_at_max_iterations: bool,
    _private: (),
}

pub enum GateFailReason {
    VerifierFailed,                          // verify_exit != 0
    ReviewConcern { summary, finding_count },// marker is LOOM_CONCERN; per-finding detail in mint output
    BadWalk(BadWalk),                        // review walk terminator malformed or mismatched
    EmptyDiffNoop,                           // marker is LOOM_NOOP — no reviewable work
    StalledMaxIterations,                    // outer-loop counter exhausted
    SignalKilled,                            // child terminated by signal
    ReviewEvidenceMissing,                   // log file absent / empty / mismatched marker
    IntegrityFinding,                        // unresolved annotation / stub test / unneeded pending marker — cap-exhausted; recoverable cases land via mint pipeline, not as a GateFail
}

pub enum BadWalk {
    Concern { payload: String, parsed_findings: Vec<Finding> },          // terminal malformed; well-formed findings preserved
    ConcernWithoutFindings { summary: String },                          // LOOM_CONCERN emitted with zero LOOM_FINDING streamed
    FindingsWithoutConcern { finding_count: usize, findings: Vec<Finding> }, // findings streamed but LOOM_COMPLETE emitted; findings preserved
    MalformedFinding { errors: Vec<FindingParseError>, terminal: TerminalSurface }, // >=1 finding-line failed parse; terminal preserved
}
```

**CLI exit-code mapping.** The binary's exit code is a pure
function of `outcome.gate`:

| `GateOutcome` variant | Exit |
|---|---|
| `Success(_)` | 0 |
| `Fail(_)` | non-zero (1) |
| `NoGate { .. }` | 0 |

Plus `Err(LoopError)` paths exit non-zero. This is a behaviour
contract: any `loom loop` invocation whose outcome was not
`GateOutcome::Success(_)` or `NoGate { .. }` surfaces a non-zero
exit. Wrapper scripts must consume this signal.

### Msg Modes

`loom msg` is the human resolution channel for outstanding `loom:blocked`
and `loom:clarify` beads. Clarify beads carry their options in the
*Options Format Contract* defined in
[gate.md](gate.md#options-format-contract); `loom msg`
consumes that format for list / view / fast-reply / dismiss. The
flag table below documents `loom msg`'s own surface.

**Five modes plus a filter:**

| Mode | Invocation | Where it runs |
|------|-----------|---------------|
| List (default) | `loom msg` | host, no container |
| View | `loom msg -n <N>` / `loom msg -b <id>` | host, no container |
| Fast-reply (option) | `loom msg -n <N> -o <int>` | host, no container |
| Fast-reply (verbatim) | `loom msg -n <N> -r <text>` | host, no container |
| Dismiss | `loom msg -n <N> -d` | host, no container |
| Chat | `loom msg -c` | container, Claude (`msg.md` template) |
| Filter | `-s <label>` (combines with any mode) | scope to `spec:<label>` |

**Flag table.** Both short and long forms are accepted; the long form is
what `loom msg --help` documents.

| Short | Long | Argument | Purpose |
|-------|------|----------|---------|
| `-c` | `--chat` | — | Launch interactive Drafter session in a container |
| `-s` | `--spec` | `<label>` | Filter to clarifies labeled `spec:<label>` |
| `-n` | `--number` | `<int>` | Address a clarify by 1-based list index |
| `-b` | `--bead` | `<bead-id>` | Address a clarify by bead ID |
| `-o` | `--option` | `<int>` | Fast-reply with the bead's `### Option <int>` body; **validated** — errors `option <int> not found in bead <id>` if no matching subsection exists |
| `-r` | `--reply` | `<text>` | Fast-reply with verbatim free-form text; works on any bead regardless of whether it has an Options section |
| `-d` | `--dismiss` | — | Clear the label with a work-around note |

**Mutually exclusive flags.** `-o` and `-r` cannot both be supplied —
passing both errors before any side effects. `-d` cannot combine with
`-o` or `-r`. `-n` and `-b` cannot both be supplied (they're alternative
addressing schemes for the same target). `-c` is mutually exclusive with
all other action flags except `-s`.

**Cross-spec by default.** Bare `loom msg` lists every outstanding
`loom:blocked` and `loom:clarify` bead across all specs, regardless of
the `current_spec` meta value. `-s <label>` is the only narrowing path.
The `current_spec` is not consulted for any msg mode.

**Chat session shape.** `loom msg -c` (optionally with `-s <label>`)
launches the base profile via `wrapix spawn`, runs Claude with the
`msg.md` template, and walks the user through outstanding beads
interactively. The session has **full bd-write authority** on the
beads in its queue: notes via `bd update --notes`, label add/remove
via `bd update --add-label` / `--remove-label`, status changes, and
bead closure via `bd close`. The chokepoint reasoning that gates
worker-session bd writes (replay-safety, cross-finding dedup,
deterministic per-spec routing) does not apply to chat because the
human is present and authorizes each turn.

Per [Verdict Gate § Interactive vs worker
sessions](#verdict-gate), the driver does **not** mutate bd state as
a consequence of an interactive session. Whatever bd state the chat
agent (with human authorization) established at session end IS the
state — the driver does not reconcile, revert, or re-classify.
Unresolved beads remain visible in the next `loom msg` list; mis-
applied label changes are corrected in the next chat session by the
human, not by driver auto-fixup.

Mid-walk exit is a clean `LOOM_COMPLETE`; the chat session emits
`LOOM_COMPLETE` only — `LOOM_RETRY`, `LOOM_BLOCKED`, `LOOM_CLARIFY`,
and `LOOM_CONCERN` are wrong-phase-marker errors from `msg` (the
session itself is the resolution channel, not a producer of new
clarifies; the human is present to resolve friction in-turn).

**Chat queue — clarify vs blocked framing.** The chat session queue
includes both `loom:clarify` and `loom:blocked` beads, but the two
flows differ in the rendered prompt:

- **`loom:clarify`** beads carry options under the *Options Format
  Contract*. The drafter helps the user **pick among existing
  options**; it does not re-generate them.
- **`loom:blocked`** beads do not carry options (the `LOOM_BLOCKED`
  marker is the no-options variant). The drafter walks the user
  through **enumerating candidate resolutions first**, then helps
  them pick. This is equivalent to promoting the bead from
  `loom:blocked` to `loom:clarify` in-session and immediately
  resolving the promoted clarify.
- **Options in notes.** Per [gate.md](gate.md#options-format-contract),
  a reviewer that promotes a previously-blocked bead writes the
  `## Options` block into the bead's `--notes`. The msg queue reads
  options from notes ∪ description so notes-carried options are
  surfaced alongside description-carried ones.
- **Epic exclusion.** Epic beads (`issue_type == "epic"`) are filtered
  out of the chat queue: workers target leaf beads, and an epic
  carrying `loom:blocked` would surface as a non-actionable
  container.

### Crate Layout

The workspace has eight member crates. Three are **public-contract**
crates (downstream consumers import them as Rust dependencies);
the other five are internal organization.

| Crate | Tier | Role |
|-------|------|------|
| `loom` | internal | CLI binary — arg parsing, entry point, dispatch. |
| `loom-events` | **public** | `AgentEvent` enum, ID newtypes (`BeadId`, `MoleculeId`, `ToolCallId`, `SpecLabel`, `ProfileName`, `SessionId`, `RequestId`), `DriverKind`, `Session` trait, `EventSink` trait, `SessionCommand`. Frontends, SSE bridges, and external log tools depend only on this. |
| `llm` | **public** | Typed wrapper over a multi-provider LLM crate. `LlmClient` trait, `Conversation` with built-in tool-use loop, `ModelId`, `CacheControl`, `complete_structured::<T>` (provider-agnostic), `TokenUsage`. Hosts the agent-loop observers (`DoomLoopObserver`, `DuplicateResultObserver`) so consumers driving via `Conversation` get the same safety nets Loom's binary uses. See [llm.md](llm.md). |
| `templates` | **public** | Askama templates + typed context structs. Consumers compose their own templates from the exposed typed building blocks (`PinnedContext`, `PreviousFailure`, `LoopContext`, partial strings). Loom's workflow templates themselves stay internal. See [templates.md](templates.md). |
| `loom-driver` | internal | Host-side runtime — `AgentBackend` trait, `StateDb`, `Config`, `BdClient`, `Clock`, profile manifest, lock files, scratch dir, git ops, workflow-layer driver-event emission (verdict-gate, push-gate, container-spawn). |
| `loom-render` | internal | `Renderer` trait + `Pretty` / `Plain` / `Json` / `Raw` impls; `LogSink` (impl `EventSink`) driving disk JSONL from the same event stream the renderer consumes. |
| `agent` | internal | `AgentBackend` implementations (pi, claude, direct). Pi/Claude drive subprocess agents; `direct` composes `llm` with Loom's six sandbox-aware tools and exposes a `Session`. Adapters flatten backend wire schemas into `loom-events` variants. |
| `loom-workflow` | internal | Workflow engine — plan, todo, run, gate, msg. Holds backends behind `Box<dyn Session>`. Owns orchestration loop, bead lifecycle, retry logic, push gate, verdict gate. |

### Dependency Graph

Load-bearing constraints on the dep graph:

- `loom-events` is a **leaf** — no internal-crate imports. The
  contract crate's dep footprint is `serde + serde_json +
  thiserror + futures-core` only (`futures-core` carries the
  `Stream` trait referenced by `Session::Events`).
- `llm` depends on `loom-events` only (no `loom-driver`,
  `agent`, or `loom-workflow` import). Its dep footprint is
  the public-contract floor plus the underlying multi-provider
  LLM crate and `schemars`. The crate is independently versionable
  for the same reason `loom-events` is.
- `templates` depends on `loom-events` only (typed contexts
  reference `BeadId` / `SpecLabel` / etc.). The Askama compile
  machinery is a build-time concern, not a runtime dep.
- `loom-render` depends on `loom-events` only — no `loom-driver`
  import. A renderer regression must be local to `loom-render`.
- `agent` depends on `llm` (its `direct` backend wraps
  `Conversation`) and `loom-events` (the `Session` trait, `AgentEvent`).
- `loom-workflow` depends on all the internal crates because it is
  the orchestration layer; `loom-events` is the bottom of the
  internal-crate stack and `loom-workflow` is the top.

`loom-events`'s, `llm`'s, and `templates`'s leaf-or-near-leaf
status is what makes each contract version-able in isolation — a
public-API change shows up as a single-crate bump, not as accidental
coupling through a deeper crate.

### Workspace Dependencies

All third-party crates are pinned once under
`[workspace.dependencies]`; every member crate uses
`foo = { workspace = true }`. Specific version pins live in
`Cargo.toml`; the workspace-deps-pattern is a team-wide convention
per [`docs/style-rules.md`](../docs/style-rules.md) RS-3.

`loom-events` is the contract-crate dependency-floor: its dep
footprint is `serde + serde_json + thiserror + futures-core` only —
no internal crates, no timestamps crate, no `ulid`, no `uuid`. The
contract stays small. `llm` and `templates` carry their own
small public-surface dep sets (LLM crate + `schemars` for `llm`;
Askama for `templates`).

### Workspace Lints

Lints are declared at workspace scope (`[workspace.lints.*]` in the
root `Cargo.toml`); every member crate carries `[lints] workspace =
true`. No crate-root `#![warn(...)]` / `#![deny(...)]`. Test
exemptions live in `clippy.toml`'s native `allow-*-in-tests` flags.
The specific lint denials and per-site override rules are defined
in [`docs/style-rules.md`](../docs/style-rules.md) (RS-3 et seq.);
this spec only commits to the workspace-scope enforcement
architecture, not the rule list.

### Parse, Don't Validate

Raw data enters typed domain representations at the boundary and stays typed
everywhere downstream. No internal function re-checks or re-parses.

**Boundary layers (outside → inside):**

1. **JSONL framing** — `BufReader::read_line` splits the byte stream into
   lines. Each line is one JSON object.
2. **Protocol parsing** — `serde_json::from_str` deserializes each line into a
   backend-specific message type (`PiMessage` or `ClaudeMessage`).
3. **Event normalization** — backend-specific messages map to `AgentEvent`.
   After this point, no code knows which backend is running.
4. **Domain newtypes** — identifiers (`BeadId`, `SpecLabel`, etc.) are parsed
   from strings at construction. Downstream code receives `BeadId`, never
   `String`.
5. **State queries** — SQLite rows map to typed Rust structs via `rusqlite`.
   No intermediate untyped step.
6. **CLI output parsing** — `bd --json` output deserializes into typed structs
   (`Bead`, `Molecule`).
7. **Profile-image manifest** — the JSON produced by `mkProfileImages`
   deserializes into `BTreeMap<ProfileName, ImageEntry { ref, source }>` once
   at loom startup. Downstream code receives `&ImageEntry`, never raw JSON.

**Newtype IDs:**

Each identifier in `loom-events::identifier` is hand-written (no shared macro)
so per-type parse rules can be enforced at construction. Every newtype wraps
a single `String`, exposes `as_str() -> &str`, implements `Display` as the
inner string, and derives the standard value traits (`Debug`, `Clone`,
`PartialEq`, `Eq`, `Hash`) plus `#[serde(transparent)]` so it serializes as
a plain string — no wrapper object.

`BeadId` additionally validates the canonical
`<prefix>-<base32>(.<digits>)?` shape at every construction path: `new`
returns `Result<Self, ParseBeadIdError>`, and `Deserialize` is hand-written
to reject malformed input rather than constructing an invalid wrapper.
Other newtypes (`SessionId`, `ToolCallId`, `RequestId`, `SpecLabel`,
`MoleculeId`, `ProfileName`) keep a permissive `new(impl Into<String>)`.

`derive(From)` and `derive(Into)` are banned (RS-8) to prevent accidental
bypass of the newtype boundary.

### Askama Template System

See [templates.md](templates.md) — engine choice,
per-template typed context structs, partials inventory, per-phase
pinning policy, typed `PreviousFailure`, attempt counter,
agent-output markers, public-contract building blocks for consumer
template composition, and the snapshot-test contract all live there.
`templates` is the crate (public-contract);
[templates.md](templates.md) is the spec.

### Beads CLI Wrapper

`loom-driver` provides `BdClient`, a typed wrapper around the `bd` CLI:

- Invokes `bd` via `tokio::process::Command` with each argument passed via
  `.arg()`. No shell interpolation — values from agent output (bead titles,
  error messages, labels) must never be passed through `sh -c` or string
  interpolation into a shell command. This prevents injection of shell
  metacharacters from agent-controlled content.
- Uses `--json` flag where available
- Parses output into typed structs (`Bead`, `Molecule`, `MolProgress`).
  Bead labels deserialize into a `Label` newtype that pre-parses the
  `spec:`/`profile:`/`loom:clarify`/`loom:blocked` prefix families once at
  the boundary, so call sites read through typed accessors
  (`spec_label()`, `profile_name()`, `is_clarify()`, `is_blocked()`)
  rather than re-doing `strip_prefix` walks. The `loom:active` family
  is **removed** — disambiguation isn't needed once "at most one open
  epic per spec" is invariant.
- Maps CLI errors to typed error variants
- All subprocess calls have a 60-second timeout (configurable). Prevents
  unbounded hangs from a stuck `bd` process.
- Key operations: `show`, `create`, `close`, `update`, `list`, `dep_add`,
  `mol_bond`, `mol_progress`. No `dolt_push` / `dolt_pull` wrappers — loom
  relies on the bind-mounted Dolt socket so every `bd` call is already
  authoritative.

### SQLite State Store

Workflow state lives in `.loom/state.db`. The schema is owned by
`loom-driver` and migrated on open (embed migrations via `rusqlite`'s
`execute_batch`).

**Sources of truth.** Git (code + specs) and Beads (tasks + molecules
+ metadata) are the durable, shared sources of truth. The state DB is
a **per-machine cache** of values derived from those sources, plus
session-bound transient data (notes). Every state-DB value is either
rebuildable from Git/Beads or session-bound by design; nothing in the
state DB is load-bearing-and-unrecoverable.

```sql
CREATE TABLE specs (
    label TEXT PRIMARY KEY
);

CREATE TABLE molecules (
    id              TEXT PRIMARY KEY,
    spec_label      TEXT NOT NULL REFERENCES specs(label),
    base_commit     TEXT,                       -- cache of bead metadata `loom.base_commit`
    iteration_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE companions (
    spec_label     TEXT NOT NULL REFERENCES specs(label),
    companion_path TEXT NOT NULL,
    PRIMARY KEY (spec_label, companion_path)
);

CREATE TABLE notes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    spec_label TEXT NOT NULL REFERENCES specs(label) ON DELETE CASCADE,
    kind       TEXT NOT NULL,
    text       TEXT NOT NULL,
    created_at INTEGER NOT NULL  -- unix millis
);
CREATE INDEX idx_notes_spec_kind ON notes(spec_label, kind);

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- meta rows: current_spec, schema_version
```

**No `current_molecule` pointer table.** At most one epic per spec is
open at any time (see *Molecule lifecycle* below), so resolution is a
single `bd find --type=epic --label=spec:<X> --status=open` query —
no pointer, no tiers, no README parse at resolution time.

**`molecules.base_commit` is a cache of bead metadata.** The
authoritative diff base for a molecule lives on the epic bead in
Beads as the `loom.base_commit` metadata field, written when `loom
todo` mints the molecule (see *Molecule lifecycle* above). The
state-DB column is a per-machine cache populated at rebuild time and
kept in sync when `loom todo` advances the diff base. A wiped state
DB recovers the value from Beads.

**No `todo_cursor:<label>` meta key.** The molecule's
`loom.base_commit` (in bead metadata) is the sole diff base for
`loom todo` fan-out — rebuildable across machines and state-DB
wipes — and advances atomically when fan-out completes.

Typed Rust API — no raw SQL outside `loom-driver`. `loom-driver` owns a single
`StateDb` handle that wraps the SQLite connection and exposes typed
operations: open the DB at a path, fetch a spec row by label, get/set the
`current_spec` meta key, increment a molecule's iteration counter, manage
notes (`set`/`add`/`clear`/`list` scoped by `(spec_label, kind)`; `rm` by
note id), and run the rebuild described below. Every operation returns
`Result<_, StateError>`; row shapes are `SpecRow` (label), `MoleculeRow`
(id, spec_label, base_commit, iteration_count), and `NoteRow` (id,
spec_label, kind, text, created_at) — the columns of the `specs`,
`molecules`, and `notes` tables. Notes are one row per note, so
`loom note add` is a single `INSERT`, `loom note rm <id>` is a single
`DELETE`, and `ORDER BY id` yields chronological order without a parse
step.

**Rebuild (`loom init --rebuild`):** Drops and recreates all tables, then
repopulates from three sources:

1. Glob `specs/*.md` → one `specs` row per file (label from filename).
   ~10-20 files.
2. `bd list --status=open --type=epic` → one `molecules` row per open
   epic carrying a `spec:<label>` label. The "at most one open epic
   per spec" invariant means each spec contributes 0 or 1 row;
   discovering more than one is a structural-invariant error that
   surfaces with the conflicting epic IDs (operator closes one before
   re-running). For each row, `bd show <id> --json` reads
   `loom.base_commit` metadata and populates the row's `base_commit`.
3. Each spec markdown is parsed for a canonical `## Companions` section
   (see *Companion declaration in specs* below); each listed path becomes
   one `companions` row. Specs without the section contribute zero
   companions, not an error.

Iteration counters reset to 0 on rebuild. **Notes are lost on rebuild** —
they live only in SQLite and have no filesystem source to reconstruct
from. Rebuild is a clean-slate operation; running it discards all
transient hints, and `loom init --rebuild --help` says so explicitly.
Recovering notes after a rebuild means re-running `loom plan` for the
affected spec. (`loom todo`'s routine consumption is *not* a loss path —
notes are rendered into bead bodies before the rows are deleted; see
*Notes lifecycle* below.)

Total cost: a glob + ~5 `bd` CLI calls + N markdown reads (already loaded
for source #1). Runs in under a second.

**Companion declaration in specs.** Specs declare their companion paths in
a single, parseable section so rebuild is lossless:

```markdown
## Companions

- `lib/sandbox/`
- `crates/loom-templates/`
```

Parser rules:

- Heading must be exactly `## Companions` (case-sensitive, level 2).
- Body is a flat bullet list of `- ` lines. Each path is a single
  backtick-delimited token; anything outside the backticks is ignored.
- Paths are normalized to repo-relative POSIX form (no leading `/`,
  trailing slash preserved if present).
- Missing section → zero companions for this spec (not an error).
- Malformed lines (no backticks, multiple paths) are skipped with a
  `warn!`, not an abort.

This is the only contract between spec authors and the state DB on
companions. The `loom plan` interview enforces the format when adding
companion paths to a spec.

**Molecule lifecycle.** A molecule is the unit of cross-cutting
work for a plan session: one plan session that touches N specs
produces one molecule containing N epics (one per touched spec)
plus the task beads fanned out from each spec's diff.

**Invariant: at most one open epic per spec, ever.** This is the
single load-bearing rule for molecule identity. It collapses a
prior four-tier resolution machinery (state DB pointer, bd
auto-pick, README parse, fresh decomposition) into one bd query,
makes the "which molecule is current?" question structurally
unrepresentable as a misclassification, and renders the
`loom:active` label and `current_molecule` pointer table
unnecessary — both removed. Recovery work and follow-ups extend the
spec's open epic (or re-open a closed one explicitly via
`bd update <id> --status=open`); a fresh epic is only minted when
none is open.

**Single-tier resolution.** "Which molecule is active for spec X?"
is answered by:

```
open_epic = bd find --type=epic --label=spec:<X> --status=open
```

- **One result** → that epic's bonded molecule is the active
  molecule for X.
- **Zero results** → no active molecule; the next operation that
  needs one (`loom todo`, `loom gate mint --tree`) mints it.
- **More than one** → structural invariant violation. Loom refuses
  to proceed and surfaces the conflicting epic IDs; the operator
  closes one before re-running.

`loom init --rebuild` populates the state DB's `molecules` cache from
bd (one row per open epic), but resolution itself never reads the
cache — it always asks bd directly. The cache exists for `loom
status` and other read-only listings, not for the hot resolution
path.

**Lifecycle events:**

- **`loom plan -n <label>` / `loom plan -u <label>`** — edits the
  anchor spec markdown and any cross-cutting siblings under
  `specs/`. No bd writes. No epic creation. Plan sessions run
  repeatedly against a spec without minting anything.
- **`loom todo`** — fans out beads across **every spec touched in
  the working tree** (anchor + siblings whose markdown differs from
  `HEAD`). For each touched spec, single-tier resolution decides:
  - All touched specs return **zero** open epics → mint one
    molecule, N epics (one per touched spec via `bd create
    --type=epic --title="<X>" --labels="spec:<X>" --metadata
    "loom.base_commit=<HEAD>"`, bonded to the new molecule via
    `bd mol bond`), fan out task beads bonded to the molecule and
    depending on their per-spec epic.
  - All touched specs share the **same** existing molecule → bond
    fan-out beads to that molecule under the right per-spec epic.
  - Touched specs span **different** molecules, or some have open
    epics and others don't → **multi-spec collision**. Loom mints
    nothing and writes a `## Options — …` block to a new
    `loom:clarify` bead per the
    [Options Format Contract](gate.md#options-format-contract).
    The options enumerate the bonding alternatives (join one of
    the pre-existing molecules) plus the fresh-mint alternative
    (close the pre-existing epic(s), mint one cross-cutting
    molecule covering all touched specs); the operator resolves
    via `loom msg`.
- **`loom gate mint --tree`** — for each finding about spec X,
  applies single-tier resolution: bonds fix-up beads to X's open
  epic, or mints molecule + epic if none exists. `loom gate audit
  --tree` performs the same walk for inspection but produces no bd
  writes. See
  [gate.md — Findings and Minting](gate.md#findings-and-minting) and
  [gate.md — Standing-safety-net checks](gate.md#standing-safety-net-checks).

**Epic close is gated structurally on `GateSuccess`** via the
composition of three facts: (1) the worker queue filter excludes
`type=epic` beads from worker dispatch (FR1), so the agent's
`bd close <bead-id>` on `LOOM_COMPLETE` can never target an epic;
(2) the workflow runtime does not call `bd close` from any other
non-review path; (3) `auto_close_completed_epics` runs only inside
`apply_verdict`'s `ReviewVerdict::Clean` branch, which is reachable
only when `GateSuccess` is constructed (see *Loop Outcome Types*
above). Manual `bd close` by the user remains permitted — that is
outside loom's scope to police.

**`loom.base_commit` inheritance for out-of-band beads.** Beads
created via `bd create` (outside `loom todo`) that bond to an
existing molecule's epic as `--parent` may legitimately ship
without their own `loom.base_commit`. The `loom init --rebuild` /
`loom loop` init entry point self-heals:

1. Read the bead's own `loom.base_commit` metadata. If present, use
   it.
2. Otherwise, follow the bead's `parent` to its parent bead's
   `loom.base_commit`. On a hit, write the value back to the child
   via `bd update <id> --set-metadata loom.base_commit=<sha>`.
3. If neither the bead nor its parent carries the metadata, the
   init returns `InitError::MoleculeMissingBaseCommit` whose
   `Display` text names the exact `bd update` fix command.

The single-step lookup is sufficient because Beads enforces at most
one parent per bead and `loom todo` writes the metadata
unconditionally on every molecule it creates.

**Cycle close.** When the push gate constructs `GateSuccess`, the
existing `auto_close_completed_epics` walk closes every epic whose
direct children are all closed. After closure, single-tier
resolution for that spec returns zero open epics — the molecule
is "done" by construction. Continuing work against the same
molecule means **re-opening** the closed epic explicitly (`bd
update <id> --status=open`); starting a fresh cycle means leaving
the closed epic alone and letting the next `loom todo` /
`loom gate mint --tree` mint a new molecule because no open epic
exists. Closed epics persist as a queryable history; they're never
auto-revived.

**Notes lifecycle.** Notes are *transient hints* attached to a spec —
bug-or-gotcha context, file paths to touch, design trade-offs left to
the implementer's judgement, decisions captured during a review, etc.
They are never canonical: the spec markdown holds the durable design,
the `notes` table holds the in-flight scratch around it. Notes are
discriminated by `kind`, with `implementation` being the kind consumed
by `loom todo` to seed bead bodies. Other kinds (`decision`, `review`,
`interview-context`, …) can be added additively without a schema change.

The agent never writes notes by editing markdown. It calls a CLI:

```
loom note set   <label> [--kind implementation] --json '["note 1", …]'
loom note add   <label> [--kind implementation] --text "single note"
loom note clear <label> [--kind implementation | --all-kinds]
loom note list  [<label>] [--kind implementation | --all-kinds]
loom note rm    <id>
```

`--kind` defaults to `implementation` so the common case stays terse.
`set` is atomic: `DELETE WHERE spec_label=? AND kind=?` plus N `INSERT`s
in a single transaction. The agent thinks in arrays; storage stays
per-row. Each invocation surfaces in the `AgentEvent` stream as a
regular `tool_call`/`tool_result` pair — visible in `loom logs`,
reproducible in replay, same shape as how the agent already calls
`bd update` and `bd close`.

Lifecycle for `kind = implementation`:

| Event | Effect on `notes` rows where `kind = 'implementation'` |
|-------|--------------------------------------------------------|
| `loom plan -n <label>` | Interview ends by calling `loom note set <label> --json '[…]'`. Plan does NOT create a molecule epic; epic creation is owned by `loom todo` — see *Molecule lifecycle* above. |
| `loom plan -u <label>` | Interview reads existing notes via `loom note list <label>`, then writes a **merged** array back via `loom note set` — agent's judgement, keeping what still applies, dropping what new decisions invalidate, adding what's fresh. Not a blind append or replace. Plan does NOT touch bd. |
| `loom todo` (productive completion: `(LOOM_COMPLETE or LOOM_NOOP)` AND `exit_code == 0`) | Renders the notes into each new bead body, then atomically: deletes the notes, advances `loom.base_commit` on the molecule's epic (via `bd update --metadata`), and refreshes the `molecules.base_commit` cache. Single SQLite transaction wraps the local writes; the bead-metadata write is the durable source of truth. |
| `loom todo` (any other terminal state) | Notes untouched; `loom.base_commit` untouched; next invocation reprocesses the same diff with the same notes. |
| `loom init --rebuild` | All notes drop with the table — no filesystem source to reconstruct from. |
| Spec file deleted from `specs/` | The `specs` row is orphaned but stays; cleanup is deferred to the next `--rebuild`. |

The `ON DELETE CASCADE` on `notes.spec_label` is dormant — no routine
command `DELETE`s from `specs`, and rebuild drops the table outright.
The clause exists only to keep the FK honest if a future code path ever
takes the explicit-delete route.

**Productive-completion gate.** `loom todo` advances
`loom.base_commit` on the molecule's epic and deletes the implementation
notes only when the session demonstrates productive completion:

- The agent emitted a `LOOM_COMPLETE` or `LOOM_NOOP` marker on its
  final line (the completion shapes the verdict gate recognises).
- The session's `exit_code == 0`.

A zero exit alone is not enough — backend errors (529 overload,
network drop, watchdog timeout) and swallowed-marker turns also exit
zero, and treating those as success would skip the spec's diff
range on the next `loom todo` run. On any other terminal state
(`LOOM_BLOCKED`, `LOOM_CLARIFY`, missing marker, nonzero exit) both
the metadata and the notes stay put so the next invocation
reprocesses the same range.

Three writes happen against the same productive-completion gate.
The ordering is load-bearing:

1. `BEGIN` SQLite transaction.
2. `DELETE` notes rows for `(spec_label, kind='implementation')`.
3. `UPDATE molecules SET base_commit = <HEAD>` (local cache).
4. `bd update <epic-id> --metadata loom.base_commit=<HEAD>` (Beads —
   the durable source of truth).
5. If step 4 succeeded → `COMMIT` SQLite transaction. If step 4
   failed → `ROLLBACK`; local state stays unchanged and matches
   pre-write Beads state.

The bead-metadata write happens **before** the SQLite `COMMIT` so
that Beads becomes the leading edge of the state — if anything
crashes between step 4 and step 5, the local cache lags Beads on
next open; `loom init --rebuild` recovers cleanly. The inverse
ordering (commit SQLite first, then update Beads) would risk a
local cache ahead of the durable source after a crash, which is
the harder inconsistency to detect.

The API exposing this gate
(`StateDb::consume_notes_and_refresh_base_commit(&label, &molecule_id,
new_base_commit)` plus an injected bead-update closure) wraps the
sequence so calling code cannot perform one without the others.

**Container exposure:** The state DB is inside the workspace bind-mounted
into containers. A malicious agent could modify it directly. This is an
accepted risk — the DB is reconstructable via `loom init --rebuild`, the
blast radius is limited to local-cache values (iteration counters,
`current_spec`, the `molecules.base_commit` cache) all of which recover
from Beads + Git on rebuild, and the durable sources of truth (spec
files on disk, beads in Dolt with `loom.base_commit` metadata) are
independently verifiable.

### Compaction Recovery

Compaction summarizes conversation history; anything that lived only in
conversation is lost. Recovery uses two pieces — the original phase prompt
(re-pinned verbatim) and a live scratchpad the agent writes to during the
session — joined by a hook script that re-injects both after compaction.

**Per-session scratch directory.** At session start the driver creates
`.loom/scratch/<key>/`:

- `prompt.txt` — the initial prompt sent to the agent at session start.
- `scratch.md` — empty scratchpad. The agent appends decisions, open
  questions, and TODOs as the session progresses, per
  `partial/scratchpad.md`.
- `repin.sh` — small bash that emits the `SessionStart[compact]` JSON
  envelope: a short fixed preamble identifying this as a post-compaction
  re-pin, then `cat prompt.txt`, then `cat scratch.md`.

`<key>` is the session concurrency unit, matching the existing locks:
spec **label** for `loom plan -n` / `loom plan -u` / `loom todo`; **bead
ID** for `loom loop` / `loom gate` / `loom msg`. Two parallel run
workers on different beads of the same molecule get independent scratch
directories.

**Per-backend delivery.** How each backend re-pins the recovery
content at compaction time — including any hook fragments the
driver writes alongside the scratch directory at session start —
is owned by [agent.md § Compaction Handling](agent.md#compaction-handling).

**Cleanup.** The driver removes the per-key scratch directory at session
end on every exit path. A new session for the same key starts
empty — no carry-over from a prior crashed session.

### Loom-LLM

See [llm.md](llm.md) — the `LlmClient` trait, typed
`CompletionRequest` / `ModelId` / `CacheControl`, structured
output, `TokenUsage`, `Conversation` with built-in tool-use loop,
the `Tool` trait, and the two agent-loop observers
(`DoomLoopObserver`, `DuplicateResultObserver`) all live there.
`llm` is the crate (public-contract);
[llm.md](llm.md) is the spec.

The observers are configured CLI-side via the
`[agent.doom_loop]` and `[agent.duplicate_result]` blocks under
*Configuration* below; their behaviour and the `observer-abort`
recovery-cause flow into the verdict gate are owned by
[llm.md](llm.md) and [Verdict Gate](#verdict-gate)
respectively.

## Configuration

Loom reads `<workspace>/loom.toml` by default; setting the
`LOOM_CONFIG` env var overrides the path (absolute or cwd-relative).
TOML, parsed natively via the `toml` crate into a typed `LoomConfig`
struct with `#[serde(default)]` on all fields so the file can be
empty or absent (defaults apply).

```toml
# Project overview — pinned in every phase via partial/context_pinning.md
pinned_context = "docs/README.md"

# Rust / project style rules — pinned in run + check via partial/style_rules.md
# (see templates.md for the partial inventory and pinning policy)
style_rules = "docs/style-rules.md"

[beads]
priority = 2
default_type = "task"

[loom]
# Name of the branch the loom workspace (`.loom/integration/`)
# has checked out and into which bead branches rebase + fast-forward.
# Loom pushes this branch to `origin/<integration_branch>` from the
# gate. Default `main`.
integration_branch = "main"
# Setting `sccache_dir` enables shared sccache. For bead and
# plan/todo container spawns, the directory is bind-mounted at
# `sccache_container_path` and `SCCACHE_DIR` + `RUSTC_WRAPPER=sccache`
# are set in container env. For host-side cargo invocations (loom's
# gate-verify against the loom workspace; optionally the operator's
# `nix develop`), the same env vars are exported and cargo reads
# the host directory directly — no mount, because there is no
# container boundary. Leaving `sccache_dir` unset disables the
# feature entirely; every cargo invocation pays full cold-build
# cost. `sccache_container_path` defaults to `/sccache` and is
# consulted only when `sccache_dir` is set.
# sccache_dir = "~/.cache/loom-sccache"
# sccache_container_path = "/sccache"

[loop]
# Molecule-level: bounds `loom loop`'s outer loop on fix-up beads (each
# full molecule pass — initial pass + every verdict-gate-produced
# fix-up pass — consumes one slot). Recorded as
# `molecules.iteration_count` in the state DB and surfaced in
# `previous_failure` context on each retry.
max_iterations = 10
# In-session: bounds the per-bead retry-with-`previous_failure` budget
# inside one `process_one_bead` call. Independent of
# `max_iterations`; the two counters never share slots.
max_retries = 2

[logs]
# Delete log files under .loom/logs/ older than this many days on
# `loom loop` startup. 0 disables sweeping (keep forever).
retention_days = 14

# Per-phase config. Resolution for any field: [phase.<name>] →
# [phase.default] → built-in. `loom loop` reads its profile from the
# bead's `profile:X` label first, then [phase.run] / [phase.default];
# the `--profile` CLI flag overrides everything.
[phase.default]
profile = "base"
agent.backend = "claude"

# [phase.todo]
# profile = "rust"
# agent.backend = "pi"
# agent.provider = "deepseek"
# agent.model_id = "deepseek-v3"
#
# [phase.check]
# agent.backend = "claude"

[claude]
# Agent-runtime settings, applied wherever claude is selected. Seconds to
# wait for clean exit after `result` before SIGTERM (shutdown watchdog).
post_result_grace_secs = 5

# Backend-agnostic liveness knobs. `handshake_timeout_secs` bounds the pi
# startup probe + optional set_model response — a non-responsive launcher
# fails fast with `HandshakeTimeout` instead of hanging. `stall_warn_secs`
# emits a `warn!` every N seconds of agent silence on the run loop without
# aborting; claude can legitimately think for minutes, so this is a
# heartbeat, not a deadline. Defaults: 30s / 60s.
# handshake_timeout_secs = 30
# stall_warn_secs = 60

[security]
# Tool names to deny when claude sends control_request. Claude-only —
# pi has no host-side permission flow (tools execute internally, no
# control_request analog). Empty by default; the container sandbox is
# the trust boundary.
# denied_tools = ["SomeNewHostTool"]

[agent.doom_loop]
# Detects same-(call, result) repetition. Enabled by default — safety
# net for a known agent failure mode, not an experimental feature.
# Consumer-driven `Conversation` runs can override via the builder.
enabled = true
# Sliding-window size for trip detection.
window = 5
# Identical pairs in the window required to trigger stage 1.
threshold = 3
# Additional identical pairs (same CallKey) after stage 1 before stage 2
# emits Abort. Provides the structural escape hatch — the agent has a
# chance to reconsider, escalate, or demonstrate intent.
stage_2_after_stage_1 = 3

[agent.duplicate_result]
# Pure-observability dedup signal. Enabled by default.
enabled = true
# Skip result payloads smaller than this — short outputs ("ok",
# single-line booleans) would dominate the map with noise.
min_bytes = 256

# Gate runners — per-tier batched dispatch with per-runner cwd. Full
# schema (match patterns, target templates, parsers) is owned by
# gate.md. The blocks below are loom-the-repo's own values; other
# consumers declare their own.
[runner.test]
command = "cargo nextest run --manifest-path loom/Cargo.toml -E '{filter}' --message-format=libtest-json"
target  = "test({name})"
join    = " + "
parse   = "libtest-json"
cwd     = "."  # nextest's --manifest-path makes this cwd-agnostic

[runner.check]
# Per-tier default cwd for [check] annotations whose specific runner
# does not override. Loom-the-repo's Rust workspace lives at `loom/`.
cwd = "loom"

[runner.system.nix]
match   = '^nix (build|run) \.#(\S+)$'
command = "nix build {targets}"
target  = ".#{capture_2}"
join    = " "
parse   = "nix-build-status"
cwd     = "."  # nix commands always run from repo root
```

Defaults are chosen so the file can be absent on a fresh install and
loom still works. Concerns that don't appear as config fields (output
display, hook integration, watch behaviour, failure-pattern handling)
are handled in Rust code rather than exposed as user-tunable
parameters.

**Single config file.** All loom configuration — including every
`[runner.<tier>.<name>]` block — lives in `<workspace>/loom.toml` at
the workspace root. There is no auxiliary config file inside
`.loom/`, so the entire `.loom/` state tree can be gitignored
without carve-outs. Set `LOOM_CONFIG` to relocate.

## Success Criteria

### Crate structure

- Workspace builds with `cargo build` from `loom/` root
  [check](cargo build --workspace)
- All eight crates present: loom, loom-events, loom-llm, loom-templates, loom-driver, loom-render, loom-agent, loom-workflow
  [check](cargo run -p loom-walk -- crate_structure)
- Three public-contract crates declared in workspace manifest metadata: loom-events, loom-llm, loom-templates
  [check](cargo run -p loom-walk -- public_contract_crates)
- Workspace uses edition 2024 and resolver "3"
  [check](cargo run -p loom-walk -- workspace_edition)
- All dependencies pinned under `[workspace.dependencies]`
  [check](cargo run -p loom-walk -- workspace_deps_pinned)
- All crates declare `[lints] workspace = true`
  [check](cargo run -p loom-walk -- workspace_lints)
- No `types.rs` or `error.rs` files at crate roots
  [check](cargo run -p loom-walk -- no_types_or_error_files)
- Domain identifiers use newtypes (BeadId, SpecLabel, MoleculeId, etc.)
  [check](cargo run -p loom-walk -- newtype_identifiers)
- No `unwrap()`, `todo!()`, `panic!()`, `unimplemented!()`, `unreachable!()` in non-test code
  [check](cargo run -p loom-walk -- no_panics_in_production)
- No `#[allow(dead_code)]` in non-test code
  [check](cargo run -p loom-walk -- no_allow_dead_code)
- No `derive(From)` or `derive(Into)` on newtype structs
  [check](cargo run -p loom-walk -- no_derive_from_on_newtypes)

### Templates

Owned by [templates.md](templates.md); see that spec's Success
Criteria.

### Process architecture

- Loom never invokes `podman run` directly (grep `crates/` for
      `podman` finds only documentation references)
  [check](cargo run -p loom-walk -- loom_does_not_invoke_podman)
- `wrapix spawn --spawn-config <file> --stdio` accepts a JSON config,
      reuses container construction from existing `wrapix run`, omits TTY
  [test](wrapix_spawn_invocation_records_correct_argv)
- `SpawnConfig` JSON shape is stable: serialization round-trip preserves
      all fields and key names, including the `image_ref` and `image_source`
      fields
  [test](spawn_config_with_model_some_round_trips_both_fields)
- `wrapix spawn` runs `podman load` from `image_source` (a Nix store
      path) before invoking podman with `image_ref` as the ref; the load is
      idempotent on the image's hash tag
  [system](nix run .#test)
- Per-bead profile selection: two beads with different profile labels
      result in two `wrapix spawn` invocations with different `image_ref`
      and `image_source`
  [test](per_bead_profile_dispatch_produces_distinct_image_refs)
- Loom reads `LOOM_PROFILES_MANIFEST` at startup and parses it into
      `BTreeMap<ProfileName, ImageEntry>`; missing env var or missing file
      errors before any bead spawn
  [test](from_path_missing_file_returns_manifest_not_found)
- A bead with `profile:X` where `X` is not in the manifest fails with a
      typed `ProfileError::UnknownProfile` naming the missing profile
  [test](lookup_unknown_profile_carries_manifest_path)
- `--profile` CLI override takes precedence over bead labels
  [test](cli_override_swaps_resolved_image)
- `loom plan` shells out to interactive `wrapix run` (TTY attached); does
      not capture stdio for JSONL
  [check](cargo test -p loom-workflow --lib argv_starts_with_run_subcommand)

### Concurrency & locking

- Spec-scoped mutating commands acquire `<label>.lock` and release on
      process exit
  [test](acquire_spec_creates_lock_file)
- Two mutating commands on the same spec serialize: the second waits up
      to 5s, then errors clearly
  [test](second_acquire_times_out_with_spec_busy)
- Two mutating commands on *different* specs run concurrently (no
      blocking)
  [test](cross_spec_locks_do_not_block)
- Read-only commands (`status`, `logs`, `spec`) acquire no lock and run
      during an active `loom loop`
  [test](readonly_paths_unaffected_by_spec_lock)
- `loom init` and `loom init --rebuild` acquire the workspace lock
      and error immediately if any per-spec lock is held
  [test](acquire_workspace_errors_when_spec_lock_held)
- Crashed loom process leaves no stale lock (kernel releases flock on
      exit; new invocation acquires immediately)
  [test](crash_releases_spec_lock)
- Lock files live under `$XDG_STATE_HOME/loom/locks/<workspace-
      basename>/` (default `~/.local/state/loom/locks/<basename>/`); no
      lock files are created inside the workspace bind-mount
  [test](locks_outside_workspace)
- Removing the lock file from inside the bead container does not
      break mutual exclusion on the host (locks live outside the
      bind-mount; agent has no path to them)
  [check](cargo test -p loom-driver --test lock_manager container_cannot_rm_host_lock)
- Driver sets `LOOM_INSIDE=1` in every bead container's env via the
      `SpawnConfig.env` allowlist
  [test](spawn_config_env_includes_loom_inside_marker)
- With `LOOM_INSIDE=1`, mutating subcommands (`loop`, `init`, `plan`,
      `check`, `todo`, `msg`, `use`) refuse with a clear error
  [test](mutating_subcommands_refuse_with_loom_inside_set)
- With `LOOM_INSIDE=1`, read-only subcommands (`status`, `logs`,
      `spec`) still run normally
  [test](readonly_subcommands_run_under_loom_inside_set)

### Loop UX & logging

**Renderer modes**

- Four renderer modes implemented: `Pretty`, `Plain`, `Json`, `Raw`
  [test](renderer_modes_present)
- `Pretty` is selected when stdout is a TTY and no `--plain`/`--json`/`--raw` flag is set
  [test](run_default_output_shape)
- `Plain` is auto-selected on non-TTY stdout (pipe/redirect), `NO_COLOR=1`, or `--plain`
  [test](plain_selected_on_non_tty)
- `Json` mode emits one pretty-printed JSON object per line; colorized when TTY, plain when piped
  [test](json_mode_pretty_prints)
- `Raw` mode passes through the original JSONL bytes unparsed
  [test](raw_mode_passthrough)

**Per-tool rendering**

- Each builtin (`Read`, `Edit`, `Write`, `Grep`, `Glob`, `Bash`, `WebFetch`, `WebSearch`, `Task`) renders its tailored summary cell
  [test](every_spec_variant_present)
- Unknown tools fall through to a generic `<name>  <truncated args>` row
  [test](unknown_tool_falls_through_to_name)
- Tool body is capped at 10 lines or 2 KB (whichever first); cap line names recovery `[N more lines — loom logs -b <id> --tool <id>]`
  [test](cap_body_keeps_short_bodies_unchanged)
- `Edit` and `Write` render unified diffs via `imara-diff`; `+<add> -<del>` counts on the summary cell
  [test](edit_summary_includes_added_removed_counts)
- Subagent (`Task`) tool nests inner events under the parent at deeper indent via `parent_tool_call_id`
  [test](task_subagent_nesting_threads_parent_tool_call_id)
- `tool_call` and `tool_result` collapse into one rendered block; duration computed from `ts_ms` delta
  [test](tool_call_result_pairing_collapses_with_ts_ms_duration)

**Driver events**

- `driver_event` variants emit with `source: "driver"` discriminator and render with `→` glyph
  [test](driver_event_renders_arrow_glyph)
- Verdict gate, retry dispatch, push gate walk/refuse/clean, container spawn/oom all emit `driver_event`
  [test](driver_kinds_present_for_spec_emission_sites)
- Unknown `driver_kind` values render as generic `→ <kind>: <summary>` (additive without schema bump)
  [test](driver_event_accepts_unknown_driver_kind)

**Live UX**

- In-place running indicator updates duration via `\r` + clear-to-EOL while a tool is in flight
  [test](second_tick_overwrites_with_carriage_return_and_clear)
- In-place running indicator is auto-disabled in non-TTY modes and with `--parallel N > 1`
  [test](disabled_indicator_writes_nothing)
- `-v` / `--verbose` disables tool-body truncation, streams `text_delta`/`thinking_delta` live, and shows `thinking` blocks (`◆`)
  [test](run_verbose_streams_text)
- Cancellation (Ctrl-C / SIGINT) collapses the in-place indicator and emits a `⚠ interrupted` closing block with partial-diff size
  [test](run_finish_finalizes_dangling_running_indicator)
- OSC 8 hyperlinks emitted for paths/URLs when terminal supports it (iTerm2, Kitty, WezTerm, recent VS Code, Alacritty, GNOME Terminal); auto-degrades silently on unsupported terminals
  [test](wrap_emits_osc8_escape_when_supported)
- Path normalization: absolute `/workspace/...` paths render repo-relative in tool summary cells
  [test](normalize_for_display_strips_workspace_prefix)

**Replay**

- `loom logs` reuses the same `Renderer` trait + impls as `loom loop` (no second formatter)
  [check](cargo test -p loom-render --lib logs_reuses_renderer_via_jsonl_round_trip)
- Live-vs-replay distinction: `Pretty` renderer takes a `live: bool` parameter; replay suppresses the in-place running indicator and computes durations from `ts_ms` deltas
  [test](live_vs_replay_distinction_pretty_renderer)
- `AgentEvent` derives `Deserialize` so `loom logs` reads its own JSONL files back through the same enum it writes
  [test](agent_event_deserialize_round_trip)

**Event schema**

- Every event carries common envelope fields: `kind`, `bead_id`, `molecule_id`, `iteration`, `source`, `ts_ms` (i64 unix millis), `seq` (u64 monotonic per-bead-spawn)
  [test](common_envelope_fields_present_on_every_variant)
- `agent_start` carries `schema_version: u32` (currently `1`), `title`, `profile`, `spec_label`, `started_at_ms`
  [test](agent_start_fields_present)
- `seq` is monotonic per bead spawn, starting at `0`
  [test](seq_advances_monotonically)
- Variant set is flat (no nested `message_update { delta: ... }`) — top-level `text_delta` / `thinking_delta` / `toolcall_delta` are siblings of `tool_call` / `tool_result`
  [check](cargo test -p loom-events --lib flat_variant_shape_has_no_nested_envelopes)
- `loom-events` crate has exactly four deps: `futures-core`, `serde`, `serde_json`, `thiserror` (no `chrono`, no `ulid`, no `uuid`)
  [check](cargo run -p loom-walk -- loom_events_minimal_deps)
- Unknown event variants are accepted gracefully (deserialized as a fallback or skipped, never error)
  [test](unknown_variants_fail_with_a_loud_error)
- `Session` trait defined in `loom-events` with methods `prompt`, `steer`, `cancel`, `set_mode`; `Events` associated type concretized to `Pin<Box<dyn Stream<Item = AgentEvent> + Send>>` so `Box<dyn Session>` is dyn-compatible
  [check](cargo run -p loom-walk -- session_trait_in_loom_events)
- `EventSink` trait defined in `loom-events` with sync `emit(&AgentEvent)` and default `react() -> Vec<SessionCommand>`; `SessionCommand` enum has `Steer(String)` and `Abort(String)` variants
  [check](cargo run -p loom-walk -- event_sink_in_loom_events)
- `EventSink` composition via `.tee(other) -> TeeSink<Self, Other>`; registration order equals `react()` invocation order
  [test](tee_chain_preserves_registration_order_for_react)
- Driver applies `react()` after every non-streaming event (not after `text_delta` / `thinking_delta` / `toolcall_delta`)
  [test](react_invoked_after_non_streaming_events_only)
- Driver treats any `SessionCommand::Abort` returned from `react()` as terminal: subsequent commands in the same batch are not applied, session is cancelled, recovery cause is `observer-abort`
  [test](abort_command_short_circuits_remaining_commands_and_classifies_observer_abort)
- `LogSink` implements `EventSink`; it is the persistence sink in the trait's first implementor
  [test](log_sink_implements_event_sink)

**Disk log**

- Full raw JSONL event stream is written to
      `.loom/logs/<spec-label>/<bead-id>-<timestamp>.jsonl` for every
      bead spawn, regardless of terminal verbosity
  [test](run_writes_per_bead_jsonl_log)
- Per-event flush: every `LogSink::emit` call calls `flush()` so `tail -f` and SSE-via-file-watcher consumers see events at emit time
  [test](log_sink_per_event_flush)
- Log path is logged at `info!` when the spawn starts
  [test](run_logs_log_path)
- With `--parallel N > 1`, each bead writes to its own file (no
      interleaving in a single log)
  [test](parallel_logs_are_per_bead)
- Terminal renderer and log writer consume the same `AgentEvent` stream
      (single tee-style sink, not two parallel pipelines)
  [check](cargo run -p loom-walk -- single_event_channel)
- On `loom loop` startup, log files older than `[logs] retention_days`
      (default 14) are deleted; recent logs are preserved
  [test](log_retention_sweep)
- `[logs] retention_days = 0` disables sweeping (no files deleted)
  [test](log_retention_disabled)
- Sweep failures (permission denied, in-use file) do not abort the run
  [test](log_retention_failure_tolerance)

**Crate boundary**

- `loom-events` is a leaf crate — no internal deps on `loom-driver` / `loom-render` / `loom-workflow` / `templates` / `llm` / `agent`
  [check](cargo run -p loom-walk -- loom_events_is_leaf)
- `llm` depends on `loom-events` only (no `loom-driver` / `agent` / `loom-workflow` import)
  [check](cargo run -p loom-walk -- loom_llm_deps)
- `templates` depends on `loom-events` only (no `loom-driver` / `llm` / `agent` / `loom-workflow` import)
  [check](cargo run -p loom-walk -- loom_templates_deps)
- `loom-render` depends on `loom-events` only (no `loom-driver`)
  [check](cargo run -p loom-walk -- loom_render_deps)
- `agent` depends on `llm` and `loom-events`; its `direct` backend wraps `loom-llm::Conversation`
  [check](cargo run -p loom-walk -- loom_agent_deps)

### Bead dispatch

- `loom init` materializes the loom workspace at
      `.loom/integration/` (one-shot clone from origin) — the
      workspace is separate from the operator's `/workspace`
  [test](loom_init_materializes_loom_workspace)
- `loom loop` never touches the operator's working tree at
      `/workspace`; all dispatch runs against the loom workspace
  [test?](loom_loop_does_not_touch_operator_workspace)
- The integration branch is settable via `[loom] integration_branch`
      in `loom.toml` (default `main`); the loom workspace has that
      branch checked out and never switches
  [test](integration_branch_setting_honored_by_loop)
- `loom loop --parallel N` creates one bead workspace per dispatched
      bead under `.loom/beads/<id>/`, derived from the loom
      workspace via `git clone --local`; bead ids are globally unique
      so no spec partition appears in the path
  [test](bead_dispatch_creates_clone_under_loom_beads)
- Bead workspaces persist across attempts, recovery iterations,
      and `loom loop` invocations until the bead's first attempt
      after `bd close`
  [test?](bead_workspace_survives_retry_until_close)
- A bead workspace is reaped on the first `loom loop` iteration that
      observes the bead in `closed` status
  [test?](bead_workspace_reaped_on_bd_close)
- Every dispatch attempt sees a clean working tree against the bead
      workspace's current HEAD; `target/`, `.git/`, and `.wrapix/`
      survive the pre-attempt reset
  [test](bead_workspace_reset_preserves_target_and_dotwrapix)
- `loom loop` startup drops every bead workspace under
      `.loom/beads/` whose bead is `closed`, under the spec
      advisory lock
  [test](loop_startup_gc_drops_closed_bead_workspaces)
- `loom loop` startup leaves bead workspaces alone whose bead is in
      any non-closed state
  [test](loop_startup_gc_skips_open_bead_workspaces)
- Each bead workspace's dispatch spawns its own `wrapix spawn`;
      spawns overlap in wall-clock under `--parallel N > 1`
  [test](concurrent_spawns_overlap_in_wall_clock)
- Successful bead branches are fetched by the driver from the bead
      workspace path into the loom workspace, then rebased + fast-
      forwarded into the integration branch (linear history, no
      merge commits); the worker never invokes `git push`
  [test?](driver_fetches_bead_branch_from_workspace_path)
- The bead-branch ref `loom/<id>` in the loom workspace is deleted
      unconditionally at the end of the per-bead critical section —
      clean exit, audit-fail rollback, and rebase-conflict abort
      all delete the ref
  [test?](bead_branch_ref_deleted_on_every_exit_path)
- The bead clone's `origin` remote remains pointing at the loom
      workspace path after `create_worktree` so host-side
      ahead/behind tracking works; the bead container has no path
      mount to the loom workspace and cannot push from inside
  [test?](bead_clone_origin_unchanged_under_a3)
- Parallel dispatch's second-and-later beads rebase onto the moved
      integration-branch HEAD before fast-forwarding
  [test](merge_branch_rebases_bead_branch_onto_head_before_ff)
- Driver-side rebase that conflicts textually aborts (`git rebase
      --abort`) and routes the bead to recovery with cause
      `integration-conflict` carrying the conflict files and the
      new integration tip SHA
  [test?](rebase_conflict_routes_to_integration_conflict)
- `integration-conflict` recovery dispatches the agent at most
      once; a second rebase-conflict on the retry escalates to
      `loom:clarify` with the same cause
  [test?](integration_conflict_one_retry_then_clarify)
- Driver-applied `integration-conflict` clarify beads carry a
      synthesized `## Options — …` block satisfying the Options
      Format Contract with two `### Option N — …` subsections
      (resolve-in-bead-clone and abandon-the-bead)
  [test?](driver_applied_integration_conflict_clarify_carries_synthesized_options)
- `loom init` writes `[rerere] enabled = true` and `[rerere]
      autoupdate = true` into the loom workspace's local
      `.git/config` so the driver-side rebase replays previously-
      recorded conflict resolutions before falling through to
      `integration-conflict` recovery
  [test?](loom_init_enables_rerere_in_loom_workspace_gitconfig)
- Origin push of the integration branch retries non-fast-forward
      errors by fetching and re-rebasing onto
      `origin/<integration-branch>`
  [test](origin_push_retries_non_fast_forward)
- On rebase abort, audit-fail rollback, signature-verification
      failure, or post-integration push failure, the bead workspace
      persists (the default per-bead-close behavior) and the bead is
      routed to `Blocked` or `Clarify` per the verdict gate
  [test?](workspace_persists_on_all_failure_paths)
- Bead containers receive the host `wrapix-beads` dolt socket as a
      single-file bind mount at `/workspace/.wrapix/dolt.sock` via
      `SpawnConfig.mounts`, replacing the host-side hardlink shim
      previously used in `GitClient::create_worktree`
  [test](bead_container_dolt_socket_via_mounts)
- When `[loom] sccache_dir` is configured, the directory is
      bind-mounted into the loom workspace and every bead container
      at the configured container path
  [test](sccache_mount_present_when_configured)
- When `[loom] sccache_dir` is unset, no sccache mount appears on
      the bead container spawn args
  [test](sccache_mount_omitted_when_unset)
- Cache hits are observable across beads in a multi-bead loop when
      `[loom] sccache_dir` is configured
  [judge?](sccache_hits_visible_across_beads)
- `GitClient` is the only module that imports `gix` or invokes the
      `git` CLI; callers see typed Rust methods
  [check](cargo run -p loom-walk -- git_client_encapsulation)
- `loom init` writes a local `.git/config` block in the loom
      workspace declaring `gpg.format=ssh`, `user.signingkey`
      pointing at the resolved wrapix signing key,
      `commit.gpgsign=true`, and `gpg.ssh.allowedSignersFile`
      pointing at `<workspace>/.git/loom-allowed-signers`, when
      `$WRAPIX_SIGNING_KEY` or the
      `$HOME/.ssh/deploy_keys/<repo>-<host>-signing` fallback
      resolves
  [test?](loom_init_writes_signing_gitconfig)
- `GitClient::create_worktree` writes the same signing block into
      each bead clone's local `.git/config` at materialization time,
      using the same resolution rule
  [test?](create_worktree_writes_signing_gitconfig)
- The fallback keyname is derived as `<repo>-<host>` where `<repo>`
      is parsed from the origin URL (`github.com[:/]<user>/<repo>`)
      and `<host>` is `hostname -s`, matching wrapix's
      `setup-deploy-key` derivation rule
  [test?](signing_key_fallback_uses_wrapix_repo_host_derivation)
- The allowed_signers file at
      `<workspace>/.git/loom-allowed-signers` is derived via
      `ssh-keygen -y -f <signing-key>` at gitconfig-write time and
      contains the wrapix signing identity
  [test?](allowed_signers_derived_from_signing_key)
- Driver-side rebase in the loom workspace produces signed commits
      whose `gpgsig` header is present in the commit object, without
      prompting for a passphrase
  [test?](driver_rebase_signs_with_wrapix_key)
- `git log --show-signature` against a driver-rebased commit in the
      loom workspace prints `Good "git" signature` using the derived
      allowed_signers file
  [test?](rebased_commits_verify_via_derived_allowed_signers)
- `$WRAPIX_SIGNING_KEY` set to a non-existent file aborts loom
      startup with a non-zero exit and an error naming the missing
      path
  [test?](wrapix_signing_key_missing_file_fails_loud)
- When neither `$WRAPIX_SIGNING_KEY` nor the
      `$HOME/.ssh/deploy_keys/<repo>-<host>-signing` fallback
      resolves, no signing block is written and the operator's
      global gitconfig governs signing in loom-materialized
      workspaces
  [test?](no_wrapix_keys_leaves_global_gitconfig_governing)
- When the signing key resolves, the per-bead integration step
      runs `git verify-commit` against the fetched commits
      (pass 1) and against the rebased commits (pass 2); pass-1
      failure routes the bead to `loom:blocked` with cause
      `signature-verification-failed` (worker-side) and pass-2
      failure routes to `loom:blocked` with the same cause but the
      detail naming "driver-side"
  [test?](integration_step_verifies_signatures_in_two_passes)
- When the signing key does not resolve, signature verification is
      skipped at both passes and the integration step proceeds
  [test?](signature_verification_skipped_when_no_key)

### Workflow commands

- `loom plan -n <label>` spawns container with base profile, runs
      spec interview; edits the spec markdown only — no bd writes
  [test](plan_new_invokes_wrapix_run_and_records_companions)
- `loom plan -u <label>` updates existing spec with anchor/sibling
      support; edits the spec markdown(s) only — same isolation as
      `-n`. Sibling specs that the interview touches are detected
      by `loom todo`'s working-tree diff walk; plan does not write
      a touched-set manifest
  [test](plan_update_threads_existing_companions_into_prompt)
- `loom use <label>` sets the `current_spec` meta key only. There
      is no `--epic` flag (the prior pointer-write surface is
      removed under the "at most one open epic per spec" invariant)
  [test?](loom_use_sets_current_spec_only)
- `loom todo` resolves "the active molecule for spec X" via a
      single `bd find --type=epic --label=spec:<X> --status=open`
      query — no tier walk, no pointer table, no README parse at
      resolution time. Zero results → mint molecule + epic; one
      result → bond to its molecule; more than one → structural
      invariant violation, refuse with conflicting IDs surfaced
  [test](todo_single_query_resolution_with_invariant_violation_refusal)
- `loom todo` fans out across **every** spec whose markdown differs
      from `HEAD` in the working tree (anchor + siblings), not just
      the anchor. For each touched spec it applies single-tier
      resolution; multi-spec collision (touched specs span different
      molecules, or mix has/has-not open epics) is surfaced as a
      `loom:clarify` bead carrying a structured `## Options — …`
      block per the Options Format Contract — loom does not mint
      anything in that case
  [test](todo_fans_out_across_all_touched_specs_and_clarifies_on_collision)
- `loom todo` reads [gate.md](gate.md)'s sqlite status cache before
      rendering the prompt and populates `criterion_status:
      Vec<CriterionStatus>` on the rendered context (struct shape owned by
      [templates.md](templates.md)); empty cache yields rows with
      `CriterionResult::NoResult`, never an inline verifier run
  [test](todo_populates_criterion_status_from_gate_cache)
- The driver computes each `CriterionStatus.commits_since` via
      `git rev-list --count <last_commit>..HEAD` at render time;
      `last_commit = None` ⇒ `commits_since = None`
  [test](criterion_status_commits_since_computed_from_git_rev_list)
- `loom todo_new` creates the molecule epic before the agent's
      gap-analysis pass, so the clarify-on-epic fallback has a
      valid target mid-decomposition; an emitted `LOOM_CLARIFY`
      with no epic in scope is a verdict-gate dispatch error
  [check](cargo run -p loom-walk -- todo_new_creates_epic_before_decomposition)
- `loom loop` continuous mode processes beads until molecule complete
  [test](continuous_loops_until_molecule_complete)
- `loom loop --once` processes a single bead then fires the gate if
      that bead closed the molecule (queue subsequently empty); if
      the bead did NOT close the molecule, exits with
      `GateOutcome::NoGate { reason: OncePartial }`. `--once` never
      silently skips the gate when the molecule is complete
  [test?](once_mode_fires_gate_when_molecule_closes_else_no_gate_partial)
- `loom loop --parallel N` (alias `-p N`) accepts a positive integer; non-
      positive or non-integer values fail with a clear error
  [test](default_is_one)
- `loom loop --all-specs` iterates every spec with an open epic
      (resolved via single bd query per spec in the index), ordered
      by bd's `Updated` timestamp ascending (oldest first), running
      the per-spec outer loop to completion for each; per-spec
      `GateOutcome::Fail` does not stop the iteration
  [test?](all_specs_iterates_by_bd_updated_asc)
- `loom loop --all-specs` aggregate exit code is non-zero iff any
      spec ended in `GateOutcome::Fail`; `Success` and `NoGate`
      across all specs → exit 0
  [test?](all_specs_exit_code_reflects_aggregate)
- `loom loop`'s worker queue resolution skips any bead with
      `issue_type == "epic"`, emitting an info-level log line naming
      the skipped epic. Sequential and parallel codepaths share the
      chokepoint
  [test](worker_queue_skips_epic_type_beads_with_info_log)
- Every successful `loom loop` invocation returns
      `LoopOutcome { gate: GateOutcome, .. }`; the binary's exit code
      is a pure function of the `GateOutcome` variant
      (`Success` → 0, `Fail` → non-zero, `NoGate` → 0)
  [test](loom_loop_exit_code_is_function_of_gate_outcome_variant)
- `GateSuccess` is constructible only by the gate-invocation code
      (`pub(crate)` constructor, no struct-literal path) and only
      when verify_exit == 0, review_exit == 0, review_marker ==
      ExitSignal::Complete, review_log_path exists, file is
      non-empty, and last line is a terminal AgentEvent matching
      review_marker; any condition failing returns `GateFail`
      instead
  [test](gate_success_constructor_asserts_every_evidence_condition)
- Every `loom loop` returning `LoopOutcome { gate: Success(r), .. }`
      produces a non-empty `review-*.jsonl` at `r.review_log_path`,
      ending in a terminal `AgentEvent` whose marker equals
      `r.review_marker`. Holds for all execution modes (`--once`,
      `--parallel`, default continuous, `--all-specs`)
  [test](every_successful_loom_loop_writes_a_review_log_with_terminal_marker)
- `run_parallel_loop` returns `Result<LoopOutcome, LoopError>` —
      identical type to the sequential codepath; parallel mode
      invokes the same `exec_review` chokepoint after the batch
      drains, constructs `GateOutcome` from the receipt, and
      returns. There is no parallel-specific summary type
  [test](parallel_codepath_returns_loop_outcome_with_gate_field)
- `loom loop` reads profile from bead label and spawns correct container
  [test](resolve_profile_reads_label)
- `loom loop` retries failed beads with previous error context
  [test](default_policy_is_two_retries)
- On molecule completion `loom loop` invokes `loom gate verify --diff
      <molecule.base_commit>..HEAD` followed by `loom gate review
      --diff <molecule.base_commit>..HEAD` (scope = molecule's own
      diff, proportional to its work — not `--tree`)
  [test](exec_review_invokes_gate_verify_then_gate_review_with_molecule_diff)
- After each per-bead agent run signals Success and the bead's branch
      is rebased onto the integration branch + ff'd at the loom
      workspace (inside `index.lock`), the loop invokes `loom gate
      verify --bead <id>` followed by `loom gate mint --bead <id>`.
      The mint step's `Vec<Finding>` comes from the production
      `MintWalker` impl (per [gate.md § Production walker
      wiring](gate.md#production-walker-wiring)), not from a
      `Vec::new()` shortcut
  [test?](per_bead_path_invokes_verify_then_mint_after_run_phase_success)
- The molecule-completion handoff's `HandoffEvidence` is populated
      from the reviewer subprocess's actual outputs: `verify_exit` and
      `review_exit` from the child exit codes, `review_marker` from
      `ExitSignal` parsing the agent's stdout, `review_log_path` from
      the LogSink. No field is left at default `None` when the child
      process produced a parseable run; absence surfaces as a
      `GateFail` variant per [Loop Outcome
      Types](#loop-outcome-types)
  [test](handoff_evidence_populates_marker_and_log_path)
- When the molecule-completion review produces ≥1 streamed
      `LOOM_FINDING:` line and a `LOOM_CONCERN:` terminator, the
      parsed `Vec<Finding>` rides through `PreviousFailure::ReviewConcern
      { summary, findings }` into the next bead-attempt's recovery
      prompt. Mint does NOT fire at molecule completion (per
      [gate.md § Stages](gate.md#stages) — push is `audit`,
      inspection-only); fix-ups are minted at the per-bead step only
  [test](molecule_completion_review_threads_findings_into_previous_failure_review_concern)
- A per-bead `loom gate mint --bead <id>` exit with `refused > 0`
      causes the loop to route the bead to `loom:blocked` with cause
      `mint-structural-violation` and the conflicting `bd` ids in the
      cause detail; the bead's loop-phase commit is not unwound (the
      integration is already durable)
  [test](loop_per_bead_routes_mint_refused_to_loom_blocked_with_structural_cause)
- A per-bead `loom gate mint --bead <id>` exit with `errors > 0`
      threads the mint summary's error detail into `PreviousFailure`
      and re-runs through the existing per-bead recovery loop bounded
      by `[loop] max_retries`. After exhaustion the bead routes to
      `loom:blocked` with cause `retry-exhausted` and the accumulated
      error context in notes (mint errors are not options-shaped, so
      per the Options Format Contract the correct terminal label is
      `loom:blocked`; `loom msg -c` walks the human through candidate
      resolutions)
  [test](loop_per_bead_routes_mint_errors_through_recovery_loop_bounded_by_max_retries)
- `loom loop`'s outer loop, after the molecule-completion handoff
      returns and the push gate has not yet fired clean, re-polls
      `bd ready` and continues processing any newly-ready fix-up
      beads (i.e., fix-ups not labelled `loom:blocked` /
      `loom:clarify`). The outer loop is bounded by
      `[loop] max_iterations` (default 10) and exits cleanly on push
      success, a fully-stuck molecule, or counter exhaustion
  [test](continuous_outer_loop_processes_fix_up_bead_then_exits_on_stall)
- Push gate is a **four-condition AND**: bead labels, verify exit,
      review exit, integrity findings. Failure on any one input
      refuses the push. The integrity-findings input is **recoverable
      within the molecule's iteration cap** (per [gate.md § Integrity
      gate](gate.md#integrity-gate)); the other three are terminal. The
      verdict is encoded as `GateOutcome` (`Success` when all four
      hold, `Fail { reason }` otherwise); the constructor for
      `GateSuccess` asserts each condition structurally — see
      [Loop Outcome Types](#loop-outcome-types)
  [test](push_gate_evaluates_all_four_conditions)
- Push gate refuses when `loom gate review`'s `--diff`-scoped
      invocation emits `LOOM_CONCERN`; molecule routes to recovery
      with cause `review-concern`
  [test](push_blocked_on_review_concern_with_id_payload)
- Push gate handles integrity-gate findings
      (`UnresolvedAnnotation`, `StubTestFunction`,
      `UnneededPendingMarker`) within the molecule's diff scope by
      **recovery-first then escalate**: while the molecule's
      iteration counter is below cap, the gate normalizes findings
      to typed `Finding`s and dispatches them through the standard
      `loom gate mint` pipeline (per [gate.md § Findings and
      Minting](gate.md#findings-and-minting)). Findings bundle into
      one fix-up batch per lead-spec, the push is refused, the
      counter is incremented, and the outer loop re-enters so the
      worker can address the batch. On cap exhaustion, the gate
      falls back to the terminal escalation: `loom:clarify` on the
      molecule's epic with one composed auto-generated `## Options
      — …` block (kind-grouped resolutions per [gate.md § Integrity
      gate](gate.md#integrity-gate))
  [test?](push_gate_recovers_integrity_findings_until_cap_then_clarifies)
- Push gate refuses on any verify-tier dispatch error (exit code
      2 = unknown verifier, command not found); dispatch errors
      count as fails, not skips
  [test](push_blocked_on_verify_dispatch_error)
- `loom loop` auto-iterates on fix-up beads (up to max iterations)
  [test](default_cap_matches_spec)
- The surface-conformance walk hard-fails when the binary's surface
      drifts from FR1 (command set, flag set, removed surface,
      grouping order) and exits 0 when spec and binary agree.
      Wired as a `[check]`-tier verifier under `loom gate check`
  [check](cargo run -p loom-walk -- surface_conformance)
- Bare `loom` (no args) renders the same Workflow / Inspection /
      State grouped sections (in spec order) as `loom --help`,
      `loom -h`, and `loom help` — clap's flat default-help fallback
      is not produced for any top-level invocation
  [test](loom_help_groups_workflow_inspection_state_in_order)
- Bare `loom msg` lists every outstanding `loom:blocked` and
      `loom:clarify` bead across all specs (cross-spec default); the
      `current_spec` meta value is not consulted
  [test](filter_keeps_only_clarify_labelled_beads)
- `loom msg -s <label>` (alias `--spec`) filters the list to
      clarifies carrying the `spec:<label>` bead label
  [test](msg_spec_filter_narrows_list_to_matching_spec)
- `loom msg -n <N>` / `loom msg -b <id>` (long forms `--number` /
      `--bead`) views a clarify host-side without launching a container
  [test](msg_view_modes_render_bead_host_side)
- `loom msg -n <N> -o <int>` (long form `--option`) writes the bead's
      `### Option <int>` body to notes and clears the label; errors
      `option <int> not found in bead <id>` and exits non-zero if the
      subsection is missing
  [test](msg_option_fast_reply_persists_note_via_bd_show)
- `loom msg -n <N> -r <text>` (long form `--reply`) writes verbatim
      text to notes and clears the label, regardless of whether the bead
      has an Options section
  [test](options_em_dash_summary_and_three_options)
- `loom msg -n <N> -d` (long form `--dismiss`) clears the label with
      a work-around note, host-side
  [test](msg_dismiss_writes_canonical_note_and_clears_label)
- `-o` and `-r` are mutually exclusive; `-d` is mutually exclusive
      with both; `-n` and `-b` are mutually exclusive; passing
      conflicting flags errors before any side effects
  [test](msg_flag_exclusivity_enforced_at_parse_time)
- `loom msg -c` (long form `--chat`) launches an interactive
      Drafter session in a container with the base profile, using the
      `msg.md` template; bare `loom msg` stays host-side
  [test](loom_msg_chat_launches_container)
- The chat session has full bd-write authority on the beads in its
      queue: notes via `bd update --notes`, label add/remove via
      `bd update --add-label` / `--remove-label`, status changes via
      `bd update --status`, and bead closure via `bd close`. The
      `msg.md` template instructs the agent on when to use each
      (resolved → close; needs further work → unblock without close;
      misclassified → re-label) and lets the human authorize each turn
  [test?](msg_template_documents_full_bd_write_authority_for_chat_agent)
- The driver does **not** reconcile bd state after an interactive
      session — no canonical unblock, no status reversion, no label
      re-application. Whatever bd state the chat agent (with human
      authorization) established at session end IS the state. The
      previously-mandated "driver reverses agent-applied bd close"
      behavior is removed
  [test?](msg_chat_driver_does_not_reconcile_bd_state_after_session)
- The bd state the chat session leaves in place persists across
      `loom msg` invocations: a bead the chat closed stays closed; a
      bead the chat unblocked stays unblocked; a bead the chat
      relabelled stays relabelled
  [test?](msg_chat_bd_state_persists_across_invocations)
- Clearing the `loom:clarify` label via any `loom msg` path
      (`-o`, `-r`, `-d`, chat session) removes the originating
      `## Options — …` block from the bead's notes in the same
      transaction that records the resolution note, per
      [gate.md](gate.md)'s *Resolution lifecycle*; only the
      resolution note remains on the bead afterwards
  [test](msg_resolution_removes_originating_options_block_from_notes)
- The chat session ending mid-walk is a clean `LOOM_COMPLETE`;
      unresolved clarifies remain visible in the next session
  [test](loom_msg_chat_partial_progress_leaves_unresolved_clarifies_open)
- The chat session's only valid exit signal is `LOOM_COMPLETE`
      (no `LOOM_RETRY`, `LOOM_BLOCKED`, `LOOM_CLARIFY`, `LOOM_NOOP`,
      or `LOOM_CONCERN` — interactive sessions don't self-report
      failures because the human is present)
  [test](loom_msg_chat_rejects_non_complete_exit_signal)
- Interactive-session crashes (container OOM, observer abort,
      swallowed marker) exit non-zero with a diagnostic; the driver
      does NOT auto-retry (the one-free-retry infra-failure path is
      worker-session only — interactive sessions are cheap to re-
      invoke and the user is present to redispatch)
  [test?](msg_chat_crash_exits_nonzero_without_auto_retry)
- `loom msg -c` with `-s <label>` scopes the chat session to
      clarifies labeled `spec:<label>`; without `-s`, the session sees
      every outstanding clarify regardless of `current_spec`
  [test](loom_msg_chat_scope_filters_to_spec)
- `loom spec` queries spec annotations (`[check]` / `[test]` /
      `[system]` / `[judge]`) parsed via `loom-gate`'s annotation parser
  [test](list_for_label_reads_all_four_tiers)
- `loom spec --deps` walks file-shaped `[test]`/`[judge]` targets and
      `[check]`/`[system]` command strings in the active spec, printing
      the required nixpkgs
  [test](deps_for_label_walks_file_targets_and_command_strings)

### Verdict gate

- After every agent phase, `loom gate verify` evaluates the result
      against the verdict-gate decision table; mechanical signals
      (marker, bd-closed, diff) make no LLM call
  [test](recovery_cause_labels_match_spec_strings)
- `phase_verdict::decide()` is invoked from `loom loop`'s per-bead
      exit AND from `loom gate review`'s phase-end; no production
      site inlines ad-hoc marker → outcome classification (FR12)
  [check](cargo run -p loom-walk -- phase_verdict_decide_called_from_production)
- `loom loop` never invokes `bd close` on a bead it dispatched;
      closure is the agent's responsibility and the `bd-closed` column
      is observed post-hoc. Verified by stubbing an agent that emits
      `LOOM_BLOCKED` / `LOOM_CLARIFY` without calling `bd close` and
      asserting the bead remains open after the run finishes.
  [test](loom_loop_never_invokes_bd_close_on_dispatched_bead_across_all_markers)
- `LOOM_BLOCKED` agent marker → bead transitions to `[blocked]`,
      recovery loop is skipped
  [test](blocked_marker_routes_to_blocked_with_reason)
- `LOOM_CLARIFY` agent marker → bead transitions to `[clarify]`,
      recovery loop is skipped
  [test](clarify_marker_routes_to_clarify_with_question)
- Direct-emit `LOOM_CLARIFY`: the gate validates the target bead's
      notes ∪ description for a well-formed `## Options — <summary>`
      heading with at least one `### Option <N> — <title>`
      subsection before applying `loom:clarify`. Same shape mint
      validates on a clarify-bound finding's evidence. Forgetful-
      agent case (marker emitted, options block absent or malformed)
      falls back to `loom:blocked` with cause `clarify-without-options`
      — no stranded clarify bead reaches `loom msg`
  [test?](direct_emit_clarify_without_options_block_falls_back_to_blocked)
- `LOOM_RETRY` agent marker → recovery with cause `agent-retry`,
      `previous_failure` populated with `AgentRetry { reason }` from
      the prose preceding the marker; one `[loop] max_retries` slot
      consumed
  [test?](retry_marker_routes_to_agent_retry_recovery_with_reason_carried)
- `LOOM_RETRY` recovery exhaustion → `loom:blocked` with cause
      `retry-exhausted` (the same exhaustion path as other
      driver-detected recoveries)
  [test?](retry_marker_exhaustion_routes_to_retry_exhausted_blocked)
- `LOOM_RETRY` from an interactive session (`plan_*`, `msg`) is a
      wrong-phase-marker error; the driver exits non-zero with a
      diagnostic and does not apply any label
  [test?](retry_marker_from_interactive_session_is_wrong_phase_error)
- `LOOM_CLARIFY` from a `loom todo_new` / `loom todo_update` session
      targets the **molecule epic** (rationale per
      [templates.md — Decomposition Discipline](templates.md));
      the agent's `## Options — …` block is persisted to the
      epic's notes per [gate.md](gate.md)'s Options Format
      Contract before the label is applied
  [test](todo_clarify_marks_molecule_epic)
- No marker emitted → recovery with cause `swallowed-marker`
  [test](missing_marker_routes_to_swallowed_marker_recovery)
- `LOOM_COMPLETE` + bead not bd-closed → recovery with cause
      `incomplete-signaling`
  [test](complete_without_bd_closed_routes_to_incomplete_signaling)
- `LOOM_COMPLETE` + closed + empty diff → recovery with cause
      `zero-progress`
  [test](complete_with_empty_diff_routes_to_zero_progress)
- `LOOM_NOOP` + closed + empty diff → review runs (legitimate no-op
      proceeds to semantic review rather than zero-progress)
  [test](noop_with_empty_diff_and_clean_review_is_done_not_zero_progress)
- `LOOM_COMPLETE` + closed + non-empty diff + dirty working tree
      (`git status --porcelain` non-empty) → recovery with cause
      `tree-not-clean`; verify and review are NOT run (recovery
      precedes them so verifiers don't execute against a half-staged
      tree); `previous_failure` lists the dirty paths capped at 30
  [test](complete_with_dirty_tree_routes_to_tree_not_clean_before_verify)
- `LOOM_NOOP` + closed + dirty working tree → recovery with cause
      `tree-not-clean` (NOOP claims "no work needed" but the tree
      disagrees; surfacing the discrepancy is more useful than
      letting the bead close on a false negative)
  [test](noop_with_dirty_tree_routes_to_tree_not_clean)
- `tree-not-clean` detail enumerates the dirty paths (modified,
      staged-but-uncommitted, and untracked outside the gitignore set)
      capped at 30 entries with a "+N more" suffix when truncated
  [test](tree_not_clean_detail_enumerates_and_caps_dirty_paths)
- All `[check]` / `[test]` / `[system]` verifiers on the bead's
      success criteria run; none short-circuit each other; per-verifier
      pass/fail + stderr is captured
  [test](complete_with_verify_fail_routes_to_verify_fail)
- One or more `loom gate verify` failures → recovery with cause
      `verify-fail`; `previous_failure` carries every failure (not just
      the first), with a 4000-char budget split across them
  [test](verify_fail_carries_every_failure_block_for_previous_failure)
- Review (LLM step) runs regardless of `loom gate verify` result; on
      verify-fail, review's concern reasoning is appended to
      `previous_failure` under `Review notes:`
  [test](complete_with_verify_fail_routes_to_verify_fail)
- Review's primary concern is live-path coverage: at least one
      `[check]` / `[test]` / `[system]` verifier on the bead must
      exercise the live path (same binary, same argv shape, same env).
      All-mock verifier sets raise a `LOOM_CONCERN`
  [judge](../tests/judges/loom.sh#judge_live_path_coverage)
- Review raises a `LOOM_CONCERN` on mocks that stand in for the very
      thing the test claims to test (e.g. mocking the agent backend in
      an agent-integration test)
  [judge](../tests/judges/loom.sh#judge_mock_discipline)
- Review's secondary concerns are scope appropriateness and
      `[judge]` rubric satisfaction
  [test](review_renders_review_context_fields)
- Review walks the pinned `{{ style_rules }}` document rule by
      rule, discovering rule families from the document itself
      (no fixed prefix enumeration in the prompt — the partial
      adapts to whatever conventions the consuming project uses).
      Each violation cites the rule id (whatever shape the project
      uses) and the offending file/line range. The prompt pins
      `{{ style_rules }}` so the LLM has the rules in its context.
  [test](build_review_prompt_includes_style_rule_conformance_walkthrough)
- `LOOM_CONCERN` → recovery with cause `review-concern`; the
      detail names which concern triggered (live-path / mock / scope /
      judge / style-rule)
  [test](concern_marker_with_streamed_findings_routes_to_review_concern_recovery)
- Production wiring obligation: every production caller that
      constructs `GateInputs` for the review-phase verdict gate must
      populate `streamed_findings_count` from the parsed walk output
      rather than relying on `..GateInputs::default()` (which leaves
      it at 0). At minimum: `classify_review_phase` at
      `crates/loom-workflow/src/review/production.rs` and
      `neutral_gate_inputs` at `crates/loom-workflow/src/loop/production.rs`
      both invoke `parse_walk_output` against the agent's combined
      stdout before constructing `GateInputs`. A well-formed
      `LOOM_CONCERN` with `≥1` streamed `LOOM_FINDING:` lines routes
      to `RecoveryCause::ReviewConcern { summary, findings }`, never
      collapses to `BadWalk::ConcernWithoutFindings` because the count
      was left at default
  [test](classify_review_phase_invokes_parse_walk_output_and_threads_findings_through_gate_inputs)
- Wire-format dead-code excision: no production code path
      constructs `ReviewError::ConcernWithoutBeadDeltas`; the variant
      is removed from `review/error.rs` and its raise site at
      `review/runner.rs` is deleted. Concern handling routes through
      `decide_concern` + `RecoveryCause::ReviewConcern` exclusively
  [test](no_path_constructs_concern_without_bead_deltas_in_production_harness_lane)
- Recovery iter < `[loop] max_iterations` (default 10) → spawns
      fix-up bead OR retries the bead with prior failure context
  [test](under_max_recovers_with_previous_failure)
- Every fix-up bead spawned by the verdict gate is bonded to the
      originating bead's molecule via `bd mol bond` before becoming
      eligible for `loom loop` dispatch; the bond is atomic with bead
      creation (no transient orphan window)
  [test](spawned_outcome_bonds_to_origins_parent_molecule)
- If the originating bead is unbonded (no molecule), the verdict
      gate refuses to spawn a fix-up bead and instead applies
      `loom:blocked` with cause `unbonded-origin` to surface the
      upstream inconsistency
  [test](refused_outcome_applies_unbonded_origin_blocked_to_origin)
- `loom gate verify` push gate walks `bd mol progress <id>` and
      refuses to push when any bead in the molecule — including bonded
      fix-up beads — carries `loom:blocked` or `loom:clarify`; an
      orphan fix-up bead would slip past this check, so the bond
      invariant is what makes the gate sound
  [test](fix_up_beads_under_cap_auto_iterate)
- Recovery iter ≥ max_iterations → applies `loom:blocked` with cause
      in `bd update --notes`
  [test](at_or_above_max_applies_blocked_with_retry_exhausted_cause)
- Iteration count is **molecule-level** state (stored in
      `molecules.iteration_count`, not on individual beads) and
      survives `retry → [running]` round-trips; every fix-up pass
      consumes one slot of `[loop] max_iterations`
  [test](iteration_counter_round_trips_through_state_db)
- Pre-flight infra failures (image load, container start) exit
      immediately as `loom:blocked` with cause `infra-preflight`; no retry
  [test](infra_preflight_routes_to_blocked_without_retry)
- Per-bead dispatch with an undeclared `profile:X` label exits
      immediately as `loom:blocked` with cause `unknown-profile`;
      the bead's notes name the requested profile + the manifest's
      declared set; the loop continues with the next ready bead
      rather than aborting the workflow
  [test](unknown_profile_routes_to_blocked_without_retry_then_continues)
- Mid-session infra failures (agent process exit non-zero, container
      OOM, IO errors) get one free retry per `loom loop`; second mid-
      session failure → `loom:blocked` with cause `infra-repeated`
  [test](infra_midsession_one_retry_then_blocks_on_repeat)
- Infra-retry counter is driver-memory only; resets on a fresh
      `loom loop` invocation; does not consume `[loop] max_iterations`
  [test](infra_retry_counter_does_not_consume_max_retries)
- `loom gate verify` push gate refuses to push while any bead in
      the molecule carries `loom:blocked` or `loom:clarify`
  [test](clarify_present_stops_without_pushing)
- Observer-driven abort (`EventSink::react()` returning
      `SessionCommand::Abort`) classifies as recovery cause
      `observer-abort` with detail naming the responsible observer +
      the reason it gave; distinct from `swallowed-marker` (which
      means the agent ended without a marker on its own, not under
      driver cancel)
  [test](observer_abort_routes_to_observer_abort_distinct_from_swallowed_marker)
- After the push-gate `Clean` branch's `git push` + `beads-push`
      both succeed, the driver walks the molecule's spec-bead
      parents and closes every ancestor epic whose direct children
      are all `status == "closed"` via `bd close --reason="all
      children complete; auto-closed by review gate"`. Each close
      emits one `DriverKind::EpicAutoClosed` driver event carrying
      the epic id.
  [test](epic_auto_closes_when_all_children_closed_and_review_passes)
- Epic auto-close does not fire while any direct child of the
      candidate epic carries `status != "closed"` (`open`,
      `in_progress`, or `deferred`).
  [test](epic_does_not_auto_close_when_any_child_non_closed)
- Epic auto-close does not fire on any non-Clean push-gate verdict
      (`LOOM_CONCERN`, any bead carrying `loom:blocked` or
      `loom:clarify`); only the `Clean` arm reaches the walk.
  [test](epic_does_not_auto_close_on_non_clean_review_verdict)
- Nested epics close inside-out in a single review-phase pass:
      closing an inner epic re-enqueues its parent so a fully-
      resolved grandparent retires in the same `Clean` walk.
  [test](nested_epics_close_inside_out_in_one_pass)
- Epic auto-close runs strictly **after** `git push` + `beads-
      push` succeed; a push failure returns early through the
      `Clean` arm and skips the walk, so no closed-locally / open-
      on-remote split arises.
  [test](auto_close_skipped_when_git_push_fails)

### Loom-LLM crate

Owned by [llm.md](llm.md); see that spec's Success
Criteria for the `LlmClient` public surface, `CacheControl`,
`Conversation` + tool-use loop, wrapper-boundary checks, and the
two agent-loop observers.

### Auxiliary commands

- `loom init` creates `<workspace>/loom.toml` (or `$LOOM_CONFIG` when
      set) and `.loom/state.db` with the default schema
  [test](run_creates_config_and_state_db)
- `loom init --rebuild` drops and repopulates the state DB from
      three sources: `specs/*.md` (one row per file), `bd list
      --status=open --type=epic` (one `molecules` row per open epic;
      finding more than one open epic for the same spec aborts the
      rebuild with a structural-invariant error), and each spec's
      `## Companions` section
  [test](rebuild_drops_and_repopulates_state_db)
- `loom status` prints the active spec (`current_spec` meta key),
      the open epic ID resolved via single bd query for that spec
      (or "(none)" when no open epic exists), and the iteration
      count for the resolved molecule
  [test](empty_state_reports_unset_spec)
- `loom use <label>` sets `current_spec` in the state DB; round-trips
      with `loom status`. No `--epic` flag (single-tier resolution
      derives the active molecule from bd directly)
  [test](use_round_trips_with_status_load)
- Bare `loom logs` pretty-renders the most recent bead's full log
      via the same `AgentEvent` renderer used by `loom loop`, then
      exits at EOF (no implicit follow); `-b <id>` (long form
      `--bead`) selects a specific bead's log
  [test](empty_root_returns_no_logs)
- `loom logs -f` (long form `--follow`) tails the selected log,
      blocking on EOF until the file grows or the user interrupts
  [test](follow_blocks_past_eof_until_budget_expires)
- `loom logs --raw` emits raw JSONL bytes from the file, unparsed;
      `loom logs -f --raw` tails raw JSONL (composes with follow)
  [test](replay_raw_copies_bytes_verbatim)
- `loom logs --path` prints the resolved log file path and exits;
      mutually exclusive with `-f`, `-v`, and `--raw` (passing any of
      those alongside `--path` errors before opening the file)
  [test](loom_logs_help_snapshot)
- `loom logs -v` (long form `--verbose`) streams assistant text
      deltas during render, matching `loom loop -v` output
  [test](replay_verbose_streams_text_deltas)
- Bare `loom logs` against an empty `.loom/logs/` exits 0
      with a one-line "No bead logs yet" message; `loom logs --path`
      in the same state exits non-zero with a clear error
  [test](empty_root_returns_no_logs)
- `loom logs` and `loom loop` share a single renderer; the
      `AgentEvent` consumer used to format live output is the same
      module used to replay saved logs (no second formatter)
  [check](cargo test -p loom-workflow --lib replay_renders_via_shared_renderer)
- No `loom sync` / `loom tune` commands exist (compiled templates make
      them unnecessary)
  [check](cargo run -p loom-walk -- no_sync_or_tune_command)

### State database

- `StateDb::open` creates tables on first open (the `specs`,
      `molecules`, `companions`, `notes`, and `meta` tables)
  [test](state_db_init_creates_tables)
- `StateDb::rebuild` populates `specs` from spec files and
      `molecules` from open `type=epic` beads carrying `spec:<label>`;
      discovering more than one open epic for the same spec aborts
      with `RebuildError::MultipleOpenEpicsForSpec` naming the
      conflicting IDs
  [test](state_db_rebuild_populates_specs_and_molecules)
- `StateDb::rebuild` parses each spec's `## Companions` section and
      writes one `companions` row per listed path; specs without the
      section contribute zero rows (not an error)
  [test](state_db_rebuild_companions)
- `StateDb::rebuild` resets iteration counters to 0
  [test](state_db_rebuild_resets_counters)
- `current_spec` / `set_current_spec` round-trips correctly
  [test](state_current_spec_round_trips)
- `increment_iteration` returns updated count
  [test](state_increment_iteration_returns_updated_count)
- Corrupted DB file → `loom init --rebuild` recovers
  [test](state_corruption_recovery)
- `loom plan -n/-u` does NOT create a molecule epic and does NOT
      write to bd; plan sessions edit specs only
  [test?](plan_does_not_create_epic_or_touch_bd)
- `loom init --rebuild` populates `molecules.base_commit` from
      `bd show <id> --json` reading `loom.base_commit` metadata for
      every open `type=epic` bead carrying `spec:<label>` (with the
      at-most-one-open-epic-per-spec invariant enforced)
  [test](rebuild_reads_base_commit_from_bead_metadata)
- `fetch_active_molecules` inherits `loom.base_commit` from a
      bead's parent when the bead is missing it of its own, writes
      the inherited value back via `bd update --set-metadata`,
      and surfaces the inherited value to the rebuilt row
  [test](rebuild_inherits_base_commit_from_parent_when_missing)
- An open epic with neither own `loom.base_commit` nor a parent
      carrying it fails loudly with
      `InitError::MoleculeMissingBaseCommit`, and the error's
      `Display` text names the exact
      `bd update <id> --set-metadata loom.base_commit=<sha>`
      fix command
  [test](rebuild_errors_when_active_molecule_lacks_base_commit_metadata)
- `loom todo` advances `loom.base_commit` on the molecule's epic
      AND the local `molecules.base_commit` cache only when the
      session emitted `LOOM_COMPLETE` or `LOOM_NOOP` **and**
      `exit_code == 0`; any other terminal state leaves both
      untouched
  [test](base_commit_advances_only_on_complete_or_noop_with_clean_exit)
- The implementation-notes delete, the local `molecules.base_commit`
      cache refresh, and the `bd update --metadata` write happen
      atomically under productive completion; failure of the
      bead-metadata write aborts the SQLite transaction
  [test](consume_notes_and_advance_base_commit_is_atomic)
- No `meta.todo_cursor:<label>` keys exist in the state DB schema;
      the cursor concept is replaced by the molecule's `loom.base_commit`
      bead metadata
  [check](cargo run -p loom-walk -- no_todo_cursor_meta_key)
- `loom plan -n <label>` inserts a `specs` row and seeds
      implementation notes via `loom note set` from the interview
  [test](new_prompt_instructs_agent_to_call_loom_note_set)
- `loom plan -u <label>` reads the existing implementation notes
      via `loom note list`, and writes back a merged array via
      `loom note set` (interview-driven keep/drop/add — not blind
      append, not blind replace)
  [judge](../tests/judges/loom.sh#judge_plan_update_merges_notes)
- `loom todo` reads implementation notes from the anchor's `notes`
      rows and renders each note's text into every new bead body
      created during the run
  [test](build_spawn_config_renders_implementation_notes_from_db)
- `loom note set <label> --kind <k> --json '[…]'` is atomic —
      `DELETE WHERE spec_label=? AND kind=?` plus N `INSERT`s in one
      transaction; partial failure leaves the prior set intact
  [test](notes_set_replaces_atomically)
- `loom note add <label> --kind <k> --text "…"` appends a single
      row to `notes`
  [test](notes_add_then_list_chronological)
- `loom note rm <id>` deletes by primary key
  [test](notes_rm_removes_one_row_by_id)
- `loom note list [<label>]` returns rows for the spec/kind pair
      (default kind: `implementation`) ordered by `id` ascending
      (chronological); `--all-kinds` widens to every kind and includes
      the `kind` column in output
  [test](notes_add_then_list_chronological)
- `loom note clear <label>` deletes rows for the spec/kind pair
      (default kind: `implementation`); `--all-kinds` wipes every kind
      for the spec in one statement
  [test](notes_clear_kind_only_or_all_kinds)
- `--kind` defaults to `implementation` on every subcommand that
      accepts it, so `loom note add my-spec --text "…"` is the
      common-case shorthand
  [test](notes_kind_defaults_implementation)
- `loom init --rebuild` drops and recreates the `notes` table —
      no notes survive a rebuild, regardless of `kind`
  [test](rebuild_drops_all_notes)
- `notes.spec_label` is declared with `ON DELETE CASCADE`; an
      explicit `DELETE FROM specs WHERE label = ?` removes the notes in
      the same statement. No routine command takes that path today —
      this verifies the FK clause itself
  [test](notes_cascade_on_spec_delete)
- Routine commands never DELETE a `specs` row; row removal happens
      only via `loom init --rebuild`
  [check](cargo test -p loom-driver --test state_db routine_commands_never_delete_spec_row)

### Compaction recovery

- At session start, `.loom/scratch/<key>/` contains
      `prompt.txt`, `scratch.md`, `repin.sh` for every phase command
      (plan, todo, run, check, msg)
  [test](open_creates_layout_and_drop_removes_it)
- `<key>` is the spec label for plan/todo phases and the bead ID for
      run/check/msg phases
  [test](resolve_scratch_key_picks_label_for_spec_scoped_phases)
- Running `repin.sh` emits a valid `SessionStart[compact]` JSON
      envelope containing banner + `prompt.txt` + `scratch.md` contents
  [test](repin_script_runs_jq_envelope_against_files)
- `claude-settings.json` registers `repin.sh` under
      `SessionStart[matcher: compact]`
  [test](claude_settings_registers_repin_under_session_start_compact)
- On session end (success or failure), the per-key scratch directory
      is removed
  [test](close_removes_dir_and_is_idempotent_with_drop)
- Two parallel `loom loop` workers on different beads use independent
      scratch directories and do not collide
  [test](parallel_keys_get_independent_dirs)
- `partial/scratchpad.md` instructs the agent that the scratchpad is
      agent-lifecycle-only and points at durable destinations for
      long-term records
  [judge](../tests/judges/loom.sh#test_scratchpad_partial_clarity)

### Beads CLI wrapper

- `bd show` output parsed into typed `Bead` struct
  [test](show_parses_first_row_into_bead)
- `bd list` output parsed with label and status filtering
  [test](list_parses_array_of_beads)
- `bd create` returns created bead ID
  [test](create_returns_id_from_silent_output)
- CLI errors mapped to typed error variants
  [test](cli_failure_maps_to_typed_error)

### Nix integration

- Loom binary builds via `nix build`
  [system](nix build .#loom)
- Loom binary is available in the devShell
  [system](nix develop -c loom --version)
- `cargo clippy --workspace` and `cargo test --workspace` are
      covered by the `loom-clippy` and `loom-nextest` flake checks
      (shared cargoArtifacts cache); see [profiles.md](profiles.md)

## Requirements

### Functional

1. **Command set** — commands fall into three groups that MUST be
   rendered as separate sections under those headings in
   `loom --help` output (in this order). Order within each group is
   as listed.

   **Workflow** — the loom loop, in execution order:
   - `loom plan` — spec interview (interactive agent session); flags
     `-n <label>` for a new spec and `-u <label>` for updating an existing
     one. Plan sessions edit specs only — they do **not** create molecule
     epics or write to bd. Epic creation is owned by `loom todo` and by
     `loom gate mint --tree`'s mint-if-missing branch (see
     [gate.md](gate.md)). No hidden-spec flag: scratch / private specs
     are kept out of git via `.git/info/exclude`.
   - `loom todo` — spec-to-beads decomposition. Fans out across **every**
     spec whose markdown differs from `HEAD` in the working tree
     (anchor + siblings). For each touched spec, resolution is one
     `bd find --type=epic --label=spec:<X> --status=open` query: zero
     results → mint molecule + epic and bond fan-out; one result →
     bond fan-out to that epic's molecule; more than one → structural
     invariant violation, refuse. A multi-spec collision (touched
     specs span different molecules or mix has/has-not open epics)
     writes a structured `## Options — …` block to a `loom:clarify`
     bead and exits without minting anything.
   - `loom loop` — execute beads in loop (continuous or `--once`).
     The loop pulls beads via `bd ready` filtered to exclude
     `loom:blocked` / `loom:clarify` beads. **Worker queue excludes
     epics structurally:** `loom loop`'s ready-queue resolution skips
     any bead with `issue_type == "epic"`, emitting an info-level log
     line naming the skipped epic. The agent never receives an epic
     as a worker task. Under `--parallel N`, a clarify or block on one
     of the N concurrent beads does not cancel the others. **On
     molecule completion** (the spec's open epic has no remaining
     ready children), the driver invokes `loom gate verify --diff
     <molecule.base_commit>..HEAD` then `loom gate review --diff
     <molecule.base_commit>..HEAD` (scope is the molecule's own diff —
     not `--tree` — so push-gate cost is proportional to the molecule's
     work), then evaluates the push gate per FR9. The
     outer loop iterates over molecule passes (initial pass + each
     verdict-gate-produced fix-up pass) bounded by `[loop]
     max_iterations`. **`loom loop` returns a typed
     [`LoopOutcome`](#loop-outcome-types) whose `gate: GateOutcome`
     field is non-optional; the binary's exit code is a pure
     function of the `GateOutcome` variant.** `--all-specs` iterates every spec with
     an open epic (single-query resolution per spec in the index)
     ordered by bd's `Updated` field ascending (oldest first),
     running the per-spec outer loop to completion for each;
     per-spec failures continue to the next spec; aggregate exit
     code is non-zero iff any spec's outcome was `GateOutcome::Fail`.
   - `loom gate` — quality gate (annotation-dispatched verifiers +
     LLM rubric). Subcommands per [gate.md](gate.md)
     Commands table: bare `loom gate` reads the status cache;
     `loom gate audit` runs verify then review; `loom gate verify`
     runs every `[check]` / `[test]` / `[system]` verifier; per-tier
     subcommands (`loom gate check`, `loom gate test`,
     `loom gate system`) run one tier in isolation;
     `loom gate review` runs the LLM rubric;
     `loom gate judge` / `loom gate rubric` run one lane each. All
     subcommands accept `--spec <label>`, a positional `<selector>`,
     and one of the four scope flags `--bead <id>` / `--diff
     <range>` / `--files <paths>` / `--tree` (mutually exclusive;
     bare invocation defaults to `--diff <molecule.base_commit>..HEAD`
     when the active spec has an open epic, else `--diff HEAD` —
     see [gate.md](gate.md) for the scope-flag contract).
     The surface-conformance walk (FR13) ships as a `[check]`-tier
     verifier dispatched by `loom gate check`.
   - `loom msg` — clarify resolution

   **Inspection** — read-only views over state and logs:
   - `loom status` — print active spec, current molecule, iteration count
     (trivial state DB query)
   - `loom logs` — pretty-render a bead's JSONL log under
     `.loom/logs/` via the same `AgentEvent` renderer used by
     `loom loop`. Full flag set in [Logs UX](#logs-ux).
   - `loom spec` — query spec annotations; supports `--deps` to print
     nixpkgs required by the spec's `[check]` / `[test]` / `[system]`
     / `[judge]` verifier targets

   **State** — workspace lifecycle and persisted state:
   - `loom init` — create `.loom/` config + state DB. `--rebuild`
     drops and repopulates the state DB from `specs/*.md`, bd state
     (one `molecules` row per open epic; finding more than one open
     epic for the same spec aborts with a structural-invariant error),
     and each spec's `## Companions` section. The orchestrator's hot
     path never parses markdown.
   - `loom use <label>` — set `current_spec` in the state DB. No
     `--epic` flag — under single-tier resolution, the active
     molecule is derived directly from bd (one query, no pointer
     table to write to).
   - `loom note` — manage spec notes

   The single-line help text for every command follows CLI-1: one
   short sentence describing current behavior, no implementation
   details / migration history / decision references / bead ids.
   The binary has no `loom doctor` subcommand; its absence is part
   of the surface contract (the surface audit flags reintroduction).

   **Removed surface.** The table below lists user-facing surface
   explicitly removed from the binary — both top-level subcommands
   and flags on retained commands. The surface-conformance walk
   (registered under `loom gate check`) parses it and hard-fails if
   any listed surface element resurfaces.

   | Surface | Removed because |
   |---------|-----------------|
   | `loom doctor` | replaced by `loom gate <subcommand>` per-tier dispatch |
   | `loom check` | renamed to `loom gate <subcommand>` per [gate.md](gate.md) |
   | `loom run` | renamed to `loom loop` (current name describes the iteration shape) |
   | `loom sync` | Askama-compiled templates make per-project sync unnecessary |
   | `loom tune` | Askama-compiled templates make per-project tune unnecessary |
   | `loom use --epic <id>` | pointer-write surface removed under the at-most-one-open-epic-per-spec invariant — single-tier resolution derives the active molecule from bd directly (see *Molecule lifecycle*) |
   | `loom todo --spec <label>` | selectively narrowing fan-out is the failure mode the at-most-one-open-epic-per-spec invariant was rebuilt to prevent; `loom todo` always fans out across every spec whose markdown differs from `HEAD` |

2. **Compiled templates with consumer-composable typed building blocks** —
   Askama engine, per-phase templates, partials, and per-phase pinning
   policy live in [templates.md](templates.md). The crate that
   builds them (`templates`) is one of the eight enumerated below.
   `templates` is **public-contract**: it exposes its typed context
   structs (`PinnedContext`, `PreviousFailure`, `LoopContext`, etc.) and
   partial-string constants so external Rust consumers can compose their
   own templates from the same building blocks Loom's workflow uses.
   Loom's workflow templates themselves remain compile-time Askama and
   internal — consumers do not override them.
3. **SQLite state store** — workflow state persisted in a SQLite database
   (`.loom/state.db`). Tracks active specs, per-molecule
   iteration counts, companions, and implementation notes.
   Reconstructable from spec files on disk and bd state via
   `loom init --rebuild`. **"Which molecule is active for spec X?"
   is not stored** — it's derived on demand via a single
   `bd find --type=epic --label=spec:<X> --status=open` query
   under the at-most-one-open-epic-per-spec invariant (see
   *Molecule lifecycle*). The `loom:active` label is not used.
4. **Beads integration** — interacts with beads via the `bd` CLI (subprocess
   calls). Bead operations: create, show, close, update, list, dep add, mol
   bond, mol progress. CLI output parsed into typed Rust structs.
5. **Profile selection** — reads `profile:X` labels from beads and resolves
   each label to a profile image via the
   [Profile-Image Manifest](#profile-image-manifest). Unknown labels fail
   at dispatch (no silent default). `--profile` overrides bead labels.
6. **Bead dispatch** — `loom loop --parallel N` (alias `-p N`) dispatches
   up to N ready beads, each in its own clone of the loom workspace
   under `.loom/beads/<id>/` on a per-bead branch. The operator's
   `/workspace` is never the bead's workdir. `--parallel 1` (default)
   runs one bead at a time; `--parallel N > 1` runs N concurrently.
   After workers finish, the driver fetches each bead branch from its
   bead workspace path into the loom workspace, then rebases +
   fast-forwards into the integration branch sequentially (per
   [Verdict Gate § Loom-workspace integration outcomes](#verdict-gate)).
   Workers never push.
7. **Retry with context** — on in-session worker failure (or explicit
   agent self-report via `LOOM_RETRY`), retries with the prior error
   output injected as the `previous_failure` template variable.
   Configurable max retries per bead (default 2; `LOOM_RETRY` consumes
   one slot per emission). After in-session retries exhaust, the phase
   ends; the verdict is delegated to the [Verdict Gate](#verdict-gate).
8. **Verdict gate per phase** — `loom gate verify` (deterministic)
   followed by `loom gate review` (LLM) evaluates each phase's result
   before the bead's state can advance. See [Verdict Gate](#verdict-gate)
   for the execution layer (decision table, recovery mechanics,
   markers, labels) and [gate.md](gate.md) for the review
   rubric. Driver-detected gate failures and `LOOM_RETRY` self-reports
   enter a bounded recovery loop; agent self-reports `LOOM_BLOCKED` /
   `LOOM_CLARIFY` escalate directly to the human via `loom msg`. The
   verdict gate applies to **worker sessions only** (`loop`, `todo_*`,
   `review`); interactive sessions (`plan_*`, `msg`) are agent-
   and-human authoritative — the driver does not mutate bd state as
   a consequence of an interactive session. See [Verdict Gate §
   Interactive vs worker sessions](#verdict-gate) for the full
   no-reconciliation contract.
9. **Push gate — four-condition AND, structurally enforced.** Push
   fires only when **all four** of the following hold; failure on any
   one refuses push. The driver computes each input explicitly — no
   implicit short-circuit, no `&&` chaining that could mask a failure.
   The push verdict is encoded in the typed
   [`GateOutcome`](#loop-outcome-types) variant: `Success(GateSuccess)`
   when all four hold, `Fail(GateFail { reason, .. })` on any
   failure. **`GateSuccess` is constructible only inside the
   gate-invocation code and only when every condition below is
   satisfied** — the constructor (`pub(crate)`, no struct-literal
   path) asserts each condition before returning. There is no
   code path that yields `GateSuccess` without the gate actually
   firing clean. Combined with FR1's worker-queue filter (epics never
   reach worker dispatch) and the existing close-on-Clean walk
   (`auto_close_completed_epics` runs only inside `apply_verdict`'s
   `ReviewVerdict::Clean` branch), this composes "epic close is
   reachable only via a `GateSuccess`" as a structural invariant —
   no separate enforcement code required.

   1. **Bead labels.** Every bead in the molecule has reached
      `[done]` — no `loom:blocked` and no `loom:clarify` outstanding.
   2. **Verify exit.** `loom gate verify --diff <molecule.base_commit>..HEAD`
      reports zero failing verifiers across `[check]` / `[test]` /
      `[system]` tiers. **Dispatch errors (exit code 2: unknown
      verifier, command not found, etc.) count as fails, not skips.**
   3. **Review exit.** `loom gate review --diff <molecule.base_commit>..HEAD`
      ends with `LOOM_COMPLETE`. Any other marker refuses the push,
      routed per the marker's semantics: `LOOM_CONCERN` → recovery
      with cause `review-concern` (per [Verdict Gate](#verdict-gate));
      `LOOM_BLOCKED` → `loom:blocked` on the molecule's epic, human
      resolution via `loom msg`; `LOOM_CLARIFY` → `loom:clarify` on
      the molecule's epic, structured-options resolution via `loom msg`.
   4. **Integrity gate.** Zero `UnresolvedAnnotation`, zero
      `StubTestFunction`, and zero `UnneededPendingMarker` findings
      across the molecule's diff scope. Integrity findings are
      **recoverable within the molecule's iteration cap** (per
      [gate.md § Integrity gate](gate.md#integrity-gate)): while
      below cap, the verdict gate normalizes findings to typed
      `Finding`s, dispatches them through the standard mint pipeline
      (bundling into one fix-up batch per lead-spec), refuses the
      push, increments the counter, and re-enters the loop. On cap
      exhaustion, the gate falls back to terminal escalation —
      `loom:clarify` on the molecule's epic with the integrity
      gate's auto-generated `## Options — …` block (Options Format
      Contract — see [gate.md](gate.md)).

   **Production wiring requirement.** The push-gate verdict MUST
   consume the exit codes of the verify and review invocations and
   the integrity gate's findings — not just bead labels. An earlier
   revision of the driver computed the verdict from labels alone
   and discarded the verify/review exit codes; a molecule pushed
   despite the reviewer raising `LOOM_CONCERN: spec-conventions-violation`
   (then named `LOOM_REVIEW_FLAG`). The four-condition AND is the
   load-bearing contract: any path that pushes without evaluating
   all four inputs is a bug.

   Per FR1, auto-iteration on fix-up beads is owned by `loom loop`'s
   outer loop, bounded by `[loop] max_iterations`; this requirement
   is the molecule-final condition the outer loop drives toward, not
   a separate iteration mechanism.

   **Epic auto-close on Clean push.** After the `Clean` branch of
   the push gate completes (verify pass + review `LOOM_COMPLETE` +
   integrity clean + every bead in scope `[done]`) **and both
   `git push` and `beads-push` succeed**, the driver walks up from
   the molecule's spec beads to find every ancestor epic whose
   direct children are all `status == "closed"` and closes them via
   `bd close <epic-id> --reason="all children complete; auto-closed
   by review gate"`. The walk is **inside-out in one pass**: each
   newly-closed epic is enqueued so its own parent is re-evaluated,
   so an epic-of-epics collapses to a single closed root without
   needing a second review cycle. Each close emits one
   `DriverKind::EpicAutoClosed` driver event carrying the epic id
   in its payload — visible in the JSONL log alongside the push-
   gate trace. The walk is **strictly post-push**: a `git push` or
   `beads-push` failure returns early through the `Clean` arm and
   skips the walk, so a closed-locally / open-on-remote split
   cannot arise. The walk does **not** fire on any non-Clean
   verdict (`LOOM_CONCERN`, `LOOM_BLOCKED`, `LOOM_CLARIFY`,
   `verify-fail`, `integrity-finding`, or any bead carrying
   `loom:blocked` / `loom:clarify`) — those paths leave the gate
   before the `Clean` arm runs.
10. **Beads via shared Dolt socket** — every container has the host's
    `wrapix-beads` Dolt server bind-mounted at
    `/workspace/.wrapix/dolt.sock` via `SpawnConfig.mounts`
    (see [Bead Dispatch](#bead-dispatch)); in-container `bd` writes
    go straight to the authoritative state. No per-bead `bd dolt
    push/pull` handoff. Loom on the host reads the same state
    through the same socket. The legacy `.beads/issues.jsonl` path
    is not used — beads no longer supports it.
11. **Spec resolution** — `--spec <name>` flag or fallback to the
    `current_spec` key in the state database.
12. **Verdict-gate production wiring** — the verdict-gate decision
    function is the single source of truth for marker → outcome
    routing. Production MUST invoke it from `loom loop`'s per-bead
    exit and `loom gate review`'s phase-end; no site may inline
    ad-hoc marker classification. The function is unit-tested in
    isolation and also exercised through its production callers
    (live-path coverage), per the trust-tier rules in
    [docs/spec-conventions.md](../docs/spec-conventions.md).
13. **Surface conformance** — the surface-conformance walk
    (registered as a `[check]`-tier verifier dispatched by `loom gate
    check`) audits the binary's user-facing surface against this
    spec, hard-failing on any drift across four dimensions:
    (1) **Command set** — FR1's commands ↔ the `Command` enum's
    variants; (2) **Flag set** — flags documented in the spec's
    per-command tables (e.g. *Msg Modes*, *Logs UX*, FR1 scope-flag
    lines) ↔ declared `#[arg(...)]`; (3) **Removed surface** — the
    `Removed` table is absent from the binary; (4) **Grouping
    order** — both `loom --help` AND bare `loom` render `Workflow:`
    / `Inspection:` / `State:` in FR1's declared order. Help-text
    wording is *not* a dimension — CLI-1 style is enforced by
    `loom gate review`'s style-rule walk. The audit exists because
    an earlier multi-bead molecule closed despite cross-component
    drift that the success-criteria walk did not catch.
14. **Verifier-driven status; no checkboxes in spec markdown.**
    Success Criteria bullets carry their `[check]` / `[test]` /
    `[system]` / `[judge]` annotation but **no `[ ]` / `[x]`
    prefix**. Status is a property of running the verifier against
    the current code-spec pair, not a value stored in the spec.
    `loom gate verify` enumerates every annotation in scope and
    reports per-criterion `pass | fail | skipped` from running the
    annotated verifier; output is live, not cached. Past passes do
    not grant immunity from re-evaluation. This rules out the
    failure class where a checkbox is `[x]` while the verifier
    points to a stub, or where production behaviour diverges from
    the unit-tested function the verifier exercises — the gate runs
    the verifier each time and reports current truth.
15. **`llm` public-contract crate** — typed multi-provider
    LLM primitives + `Conversation` with built-in tool-use loop +
    agent-loop observers. Surface, dependency graph constraints,
    and observer behavior owned by [llm.md](llm.md).
    Loom-harness's role is the crate-graph placement
    (public-contract leaf, dep floor) — see *Crate Layout* and
    *Dependency Graph* above.
16. **`EventSink` trait and composition** — per *EventSink and
    SessionCommand* above. Sinks compose via chainable
    `.tee(other)`; the driver applies `react()` after every
    non-streaming event and processes returned
    `SessionCommand`s with `Abort` as terminal priority. The
    `EventSink` trait lives in `loom-events` so any AgentEvent
    consumer (Loom binary, external `llm` `Conversation`
    consumer, SSE bridge, log analyzer) can implement and compose
    it.
17. **Observer-abort verdict-gate routing** — when an
    `EventSink::react()` returns `SessionCommand::Abort`, the
    driver cancels the session and classifies the outcome as
    recovery cause `observer-abort` with detail naming the
    responsible observer + the reason. This is the verdict-gate
    landing path for the loom-llm observer behavior owned by
    [llm.md](llm.md) (notably `DoomLoopObserver`'s
    stage 2). Without this routing, observer kills would
    mis-classify as `swallowed-marker`.
18. **Decomposition-phase wiring.** `loom todo` reads
    [gate.md](gate.md#status-cache)'s sqlite status cache before
    rendering the prompt and surfaces a per-criterion
    `CriterionStatus` row (shape owned by
    [templates.md](templates.md)) so the decomposition agent
    decides gaps from evidence rather than spec text alone.
    `loom todo_new` creates the molecule epic before any path that
    can emit `LOOM_CLARIFY`; clarify from a `todo_*` session
    targets the epic (per-bead clarify is not appropriate when
    the child beads under negotiation don't yet exist or are
    exactly the set being authored). Empty cache surfaces as
    `CriterionResult::NoResult` rows — staleness is exposed, not
    papered over.

### Non-Functional

1. **Style.** All loom crates follow
   [`docs/style-rules.md`](../docs/style-rules.md). The
   architectural commitments specific to loom — newtype IDs at
   parse boundaries, parser-to-stamper split, `Session` trait as
   public surface (with subprocess-driving backends keeping their
   typestate as internal mechanic), workspace-scope lints,
   single-source-of-truth verdict gate function — are described in
   the *Architecture* sections above; this NFR commits to the
   team-wide style rules as a whole.
2. **Required newtypes** — `BeadId`, `SpecLabel`, `MoleculeId`,
   `ProfileName` for domain identifiers; `SessionId`, `ToolCallId`,
   `RequestId` for protocol identifiers. No bare `String` for typed IDs.
   `AgentKind` is an enum (`Pi`, `Claude`), not a newtype.
3. **Nix integration** — built via `wrapix.profiles.rust.buildPackage`
   (crane-backed; see [profiles.md — Rust package builder](profiles.md#rust-profile)).
   `packages.loom` consumes `.bin` so devshell rebuilds skip the clippy/nextest
   passes; those land as separate `loom-clippy` / `loom-nextest` entries in
   `nix flake check`. Binary is included in the devShell.

## Out of Scope

- **Agent backend implementations** — defined in [agent.md](agent.md).
- **Parallelism beyond clone-per-bead** — `loom loop --parallel N`
  dispatches one bead clone per bead in parallel. New parallelism
  strategies (cross-spec, distributed, scheduler-aware) are future
  work.
- **Hidden specs (`-h` flag)** — scratch / private specs are not a
  first-class concept. The use case — keeping a spec out of git — is
  covered by `.git/info/exclude` on `specs/<label>.md`. Eliminating
  the flag keeps `plan` / `todo` / `loop` path-resolution
  single-shaped. Reintroducing it later is a non-breaking additive
  change if the workflow asks for it.
- **Override of Loom's workflow templates** — Loom's `plan` / `todo`
  / `loop` / `review` / `msg` templates are Askama, compiled
  into the binary. There is no per-project template-fetch /
  template-tune mechanism for overriding *Loom's own* templates;
  template updates ship via a new loom release. Project-specific
  prompt tweaks to Loom's workflow happen via `pinned_context` /
  `style_rules` config and per-spec implementation notes.
  Consumers writing their *own* templates (for their own LLM
  calls via `llm`) compose them from `templates`'
  exposed typed building blocks — that path is supported and
  is *not* what this exclusion covers.
- **Runtime template engine for consumer overrides of Loom's
  workflow templates** — adding a runtime engine (e.g. `minijinja`)
  to allow consumers to drop in replacements for Loom's compiled
  Askama templates is bolt-on-able after the typed-context public
  surface lands and is deferred until a concrete consumer asks.
- **Observation daemon** — a polling monitor that spawns short-lived
  agent sessions to observe tmux / browser logs and create beads for
  detected issues. Independent of the workflow phase set; deferred to
  a follow-up spec if and when the use case re-emerges.
- **Session persistence across container restarts** — each container starts a
  fresh agent session.
