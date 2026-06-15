# Loom Harness

Rust workspace, build system, workspace lint config, process architecture,
cache store, and command-set platform for the Loom agent driver.

## Problem Statement

Loom is a Rust binary that owns a complete spec-driven workflow:
spec interview (plan), spec-to-beads decomposition (todo), per-bead
agent dispatch (loop), deterministic and LLM-judged review (gate),
and human clarification (msg). The binary holds the workflow's
state in typed domain objects, parses agent protocols against typed
schemas, and renders templates with compile-time variable
validation.

This spec covers the platform: crate structure, Rust conventions,
Nix integration, SQLite cache store, beads CLI wrapper, process
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

1. **Per-bead profile/runtime selection.** Beads carry `profile:rust` /
   `profile:python` / `profile:base` labels, and each phase resolves an
   agent runtime. Each bead must run in the matching composed image. A
   long-lived parent container can't change profile/runtime mid-run;
   per-bead spawn is the only clean way.
2. **Trust boundary.** Loom (orchestrator, on host) is trusted; the agent
   (claude, pi, or direct, in container) is the sandboxed execution layer.

**Container spawn is delegated to `wrix spawn`** — a thin wrix
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
    ├─ spawn: wrix spawn --spawn-config /tmp/loom-<id>.json --stdio
    │   │
    │   └─ exec podman run [no -t, stdio piped] <image> <entrypoint>
    │       │
    │       └─ entrypoint.sh → agent (claude / pi --mode rpc / direct)
    │           ↑              ↓
    │           └── JSONL over stdin/stdout ─→ loom (parses events)
    │
    └─ on bead completion: container exits, next bead → next spawn
```

`wrix spawn --stdio` is the non-TTY counterpart of today's interactive
`wrix run` (which uses `podman run -it`). Both modes share container
construction; they differ only in stdio attachment. The
`--spawn-config <file>` flag accepts a JSON file that mirrors loom's typed
`SpawnConfig` — avoiding a fat argv interface and giving loom a single
serialization boundary.

**`loom plan` is the exception.** It is an interactive spec interview
(human-in-the-loop terminal session), so it shells out to interactive
`wrix run` rather than driving a JSONL session. Loom prepares the
template-rendered prompt, sets environment, exec's `wrix run`, and lets
the selected interactive agent attach to the user's terminal. No subprocess
capture, no JSONL.

**Trade-off accepted:** parallelism is straightforward (N concurrent
`wrix spawn` invocations) and per-bead container spawn cost (~1-2s
on podman) is dominated by agent runtime for typical bead sizes
(minutes of agent work). The alternative — one long-lived container
sharing one agent across beads — was rejected because it conflicts
with per-bead profile/runtime selection and with the trust-boundary split
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
upgrading to this layout. Lock files remain outside the workspace under
`$XDG_STATE_HOME/loom/locks/<workspace-basename>/`, keyed by phase or
work root.

**Why the layering.**

- **Operator and loom as peer clones of origin.** The operator's
  edits cannot break loom's rebases (a dirty operator working tree
  is irrelevant to loom's machinery); loom's pushes go to origin,
  so the operator's working tree changes only when they `git pull`.
  Sync flows through `git push` / `git pull` against origin.
- **Bead workspace as a child of the loom workspace.** Same fractal
  pattern at every level: bead is to loom-workspace what
  loom-workspace is to origin. The bead workspace has a
  self-contained `.git/` inside the bind-mounted path so the wrix
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
`git clean -fdx --exclude=target --exclude=.git --exclude=.wrix`
before handing off to the agent. On the first attempt this is a
no-op against a freshly-cloned tree; on retries it discards any
mid-session leftovers and preserves the agent's prior commits on
the bead branch — the agent is responsible for amending or
branch-resetting if `previous_failure` context calls for a
different approach. `target/` survives so cargo + sccache start
warm; `.git/` and `.wrix/` (extra-mount staging) survive.
`create_worktree` is idempotent at the directory level: directory
exists → reuse; missing → clone fresh from the loom workspace.

**Garbage collection.** At `loom loop` startup, under the spec
advisory lock, loom enumerates `.loom/beads/` and drops closed bead
workspaces only when the bead is parented by the molecule this loop
owns. Closed workspaces from other molecules may still be in use by a
concurrently running loop, so they are left for that molecule's own
startup sweep or inline cleanup. In-loop, every bead the current loom
dispatches owns its own removal: on the bead transitioning to
`closed` the dispatch path reaps the workspace inline as part of
post-session cleanup. The startup sweep catches orphans from crashed
prior runs, not the happy path. No timer threshold; no
operator-explicit GC. A `loom gc` command is an additive follow-up if
hoarding ever materializes.

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
bead clone (e.g., starship prompt). `create_worktree` sets the bead
branch's upstream to `origin/<integration-branch>`, so `@{u}`
resolves to the bead's base: the starship ahead/behind count is
correct, and the pre-push hook's `loom gate verify --diff @{u}..HEAD`
scopes to exactly the bead's commits. Were the upstream unset, `@{u}`
could not resolve and the gate would exit non-zero naming the range —
a hard error, not a silent degrade to a whole-tree walk (see gate.md
§ Scope flags). The bead
container has no path mount to the loom workspace and cannot push
from inside; manual host-side pushes are harmless because the
integration step still owns rebase + verify + ff. The driver's
fetch is against a filesystem path (always present through the
bead's lifetime), not over a network or daemon.

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

The phase/work-root advisory locks (see [Concurrency & Locking](#concurrency--locking))
serialize planning, workspace-wide todo decomposition, and per-work-root
loop / gate / msg actions without using an active-spec pointer.

**Mounts.** Bead containers see two mandatory bind mounts plus an
optional sccache mount, all via `SpawnConfig`:

- **Mandatory: the bead workspace** at `/workspace`.
- **Mandatory: the host `wrix-beads` dolt socket** at
  `/workspace/.wrix/dolt.sock` via `SpawnConfig.mounts`
  (see [agent.md § SpawnConfig](agent.md#spawnconfig)). This
  replaces the historical host-side hardlink shim and survives
  changes to the bead-workspace path. Linux passes the socket
  through directly; on Darwin the wrix sandbox rejects
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
virtiofs is the standard wrix pattern. The clone-over-worktree
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
| **Create bead clone** | `git clone --local .loom/integration .loom/beads/<id>`, `git checkout -b loom/<id>`, then `git branch --set-upstream-to origin/<integration-branch>` (CLI) | hardlinks loom workspace's `.git/objects`; self-contained `.git/` inside bind mount; upstream makes `@{u}..HEAD` resolve for the push-gate hook |
| **Pre-attempt reset of bead clone** | `git reset --hard HEAD` + `git clean -fdx --exclude=target --exclude=.git --exclude=.wrix` (CLI) | clean working tree at bead-branch HEAD while preserving `target/`, `.git/`, and `.wrix/` |
| **Fetch bead branch from bead workspace** | `git fetch <bead-workspace-path> loom/<id>:loom/<id>` (CLI) | filesystem path as ad-hoc URL; runs in loom workspace inside `index.lock` |
| **Verify commit signatures** | `git verify-commit <commits>` (CLI) | gates integration on signed-by-wrix-key; conditional on signing key resolving (see [Commit signing](#commit-signing)) |
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
the operator's GPG passphrase. The driver-side rebase produces new
commits in the loom workspace, which is operated on the host only
(never bind-mounted into a bead container). It receives a local
`.git/config` block pointing at the wrix-injected SSH signing key,
making driver signing non-interactive.

Bead clones deliberately do **not** receive this local block. A bead
clone is the workspace wrix bind-mounts into the bead container,
where the worker's in-container commits are the load-bearing signed
path. wrix's `git-ssh-setup.sh` entrypoint configures container
signing in the **global** `~/.gitconfig`, pointing `user.signingkey`
at the in-container key copy (`/etc/wrix/keys/<basename>`). Because
a local `.git/config` always beats global, a loom-written local block
carrying the **host** key path — which does not exist in-container —
would shadow the entrypoint's correct container path and break the
worker's `git commit` ("Couldn't load public key …: No such file or
directory"). So loom writes no signing block into bead clones;
host-side operator debug/test commits in a clone fall through to the
operator's global gitconfig instead.

**Key resolution mirrors wrix's host-side rule** (see
`lib/sandbox/linux/default.nix` and `scripts/setup-deploy-key` in
the wrix flake — same two-tier precedence, set-but-missing fails
loud):

1. `$WRIX_SIGNING_KEY` pointing at an existing file. Set-but-
   missing exits non-zero at startup naming the path; silent
   fallback would mask a parent-process misconfiguration.
2. `$HOME/.ssh/deploy_keys/<repo>-<host>-signing` if the env var
   is unset and the file exists. `<repo>` is the repo segment of
   the loom workspace's origin URL (parsed as
   `github.com[:/]<user>/<repo>`); `<host>` is `hostname -s`
   (short form, fallback to `hostname`). Same derivation as
   wrix's `setup-deploy-key` script uses to choose the keyname
   at provisioning time, so the two ends stay in sync without
   shared config. If the origin URL doesn't match the GitHub
   pattern, the fallback is skipped.
3. If neither resolves, loom writes no signing block; the
   operator's global `~/.gitconfig` governs (and may prompt). This
   is the "wrix isn't set up on this host" path — intentionally
   noisy rather than silently degraded.

Auth is handled by the `GIT_SSH_COMMAND` env var wrix already
sets; loom inherits it and does not duplicate auth configuration.
Deploy-key resolution (same precedence with the suffix dropped:
`<repo>-<host>` instead of `<repo>-<host>-signing`) yields the host
key path loom hands to the `wrix spawn` **launcher** environment
(see *Launcher key environment* below); loom's own git invocations
never read it — the key is consumed by wrix to mount it into the
bead container.

**Launcher key environment.** A bead container's agent commits and
pushes from inside the sandbox, so it needs the deploy + signing
keys mounted in. wrix mounts them only when their host paths are
named in the launcher process environment (`$WRIX_DEPLOY_KEY` /
`$WRIX_SIGNING_KEY`) — the in-container `SpawnConfig.env`
allowlist cannot carry them, because that is the env wrix builds
*inside* the container, and the host key paths are meaningless
there. At bead dispatch the driver resolves both keys against the
loom workspace (`GitClient::launcher_key_env`, same two-tier
precedence as signing) and carries the resolved HOST paths on
`SpawnConfig.launcher_env`; the backend sets them on the `wrix
spawn` child process before exec. `launcher_env` is host-only
state: it is `#[serde(skip)]`-excluded from the spawn-config JSON
so host key paths never land in a world-readable file, and wrix
re-points `$WRIX_DEPLOY_KEY` / `$WRIX_SIGNING_KEY` to the fixed
in-container destinations (`/etc/wrix/keys/<basename>`) once the
keys are mounted. A key that does not resolve is simply omitted —
`wrix spawn` fails loudly on its own when a key it needs is
absent, and the "wrix isn't set up on this host" path stays
non-fatal on loom's side.

**Workspace gitconfig writes.** When `loom init` materializes the
loom workspace, the driver writes a local `.git/config` block keyed
to **host** paths (the loom workspace is operated on the host):

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

When `GitClient::create_worktree` materializes a bead clone, it
writes **no** signing block. The clone is bind-mounted into the bead
container, where the host key path the block would carry does not
exist, and a local block beats — and so would shadow — wrix's
`git-ssh-setup.sh` global config that points `user.signingkey` at the
in-container key copy. In-container worker commits therefore sign via
that wrix global config; host-side operator debug/test commits in
the clone fall through to the operator's global gitconfig. The driver
itself never commits in a bead clone (its commits land in the loom
workspace), so dropping the clone block does not affect any loom-driven
signing path. The public half is derived from the host key (the only
place the private key exists at write time). Local config beats the
operator's `~/.gitconfig` in git's hierarchy, so the loom-workspace
block is the sole authority on host-side signing there — operator
GPG/passphrase setup is bypassed without modification.

**allowed_signers derivation.** Wrix derives the allowed_signers
file inside the container via `ssh-keygen -y -f $SIGNING_KEY` against
the public-key half of the same pair. Loom mirrors this on the host:
at the same moment it writes the gitconfig block, it runs
`ssh-keygen -y -f <signing-key>` and writes the result with the
identity prefix (`$GIT_AUTHOR_EMAIL` or `sandbox@wrix.dev` per
wrix convention) to `<workspace>/.git/loom-allowed-signers`. The
signing key is passphrase-less, so the derivation is non-interactive.
The file lives under `.git/` so workspace removal cleans it up
automatically.

**Scope.** This subsection covers commit signing only.
SSH-over-git auth (deploy key for github.com push) flows through
wrix's existing `GIT_SSH_COMMAND` pathway, inherited via the
operator's shell environment; loom does not reconfigure it. Container
gitconfig is unchanged — bead-container commits are already signed
by wrix's `git-ssh-setup.sh` entrypoint and need no host-side
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
time that maps each workspace profile and agent runtime to the podman ref,
Nix store path, and optional content-digest path needed to spawn its image.
Loom reads it at startup and, for each container spawn, looks up the resolved
profile/runtime pair to populate `SpawnConfig.image_ref` (the podman ref),
`SpawnConfig.image_source` (the store path handed to the launcher install
step), and `SpawnConfig.image_digest_path` (the digest file wrix uses to
skip reloading already-present image content).

The file is a JSON object keyed first by profile name and then by agent
runtime. Each runtime entry has two required string fields plus an optional
`digest` string. An abbreviated example:

```json
{
  "base": {
    "claude": {
      "ref": "localhost/wrix-base-claude:abc123",
      "source": "/nix/store/...-image-base-claude",
      "digest": "/nix/store/...-image-base-claude-digest"
    },
    "pi": {
      "ref": "localhost/wrix-base-pi:def456",
      "source": "/nix/store/...-image-base-pi",
      "digest": "/nix/store/...-image-base-pi-digest"
    },
    "direct": {
      "ref": "localhost/wrix-base-direct:ghi789",
      "source": "/nix/store/...-image-base-direct",
      "digest": "/nix/store/...-image-base-direct-digest"
    }
  },
  "rust": {
    "pi": {
      "ref": "localhost/wrix-rust-pi:jkl012",
      "source": "/nix/store/...-image-rust-pi",
      "digest": "/nix/store/...-image-rust-pi-digest"
    }
  }
}
```

Built by the wrix `mkProfileImages` helper; the bundled flake output is
`packages.profile-images`. External flakes that add custom profiles call
`mkProfileImages` themselves to produce a manifest covering their full
profile/runtime set.

Loom reads the manifest path from the `LOOM_PROFILES_MANIFEST` environment
variable. The bundled devshell sets it to `${self'.packages.profile-images}`;
consumers integrating loom into their own flake set it the same way. If the
variable is unset or the file is missing, loom errors at startup before any
container spawn — there is no implicit search path or fallback default. The
manifest is parsed once at startup and held as a
`BTreeMap<ProfileName, BTreeMap<AgentRuntime, ImageEntry>>` in
`loom-driver`.

Per-bead dispatch is:

1. Parse the bead's labels; pick the highest-precedence `profile:X` (or the
   value of `--profile` if set on the CLI).
2. Resolve the phase backend to an `AgentRuntime` (`claude`, `pi`, or
   `direct`).
3. Look up `(X, AgentRuntime)` in the parsed manifest. Missing profile →
   exit immediately as `loom:blocked` with cause `unknown-profile`; missing
   runtime under an existing profile → `unknown-agent-runtime-for-profile`.
   The note names the requested profile/runtime and the manifest's declared
   set so the operator can relabel or rebuild the manifest. Same routing as
   `infra-preflight` (see *Verdict gate* below).
4. Build `SpawnConfig` with `image_ref = entry.ref`, `image_source =
   entry.source`, and `image_digest_path = entry.digest` when present. Hand it
   to `wrix spawn`.

Loom derives `WRIX_AGENT` from the same `AgentRuntime` and places it in
both `SpawnConfig.launcher_env` for the host-side `wrix spawn` child
process and the in-container `SpawnConfig.env` allowlist. The operator's
parent-shell `WRIX_AGENT` is never the source of truth; wrix consumes the
launcher env before podman startup and passes the same value to the
entrypoint — see [agent.md — Entrypoint Agent
Selection](agent.md#entrypoint-agent-selection).

`loom plan` and `loom msg --chat` are interactive, so they shell out to
`wrix run` (TTY-attached) rather than `wrix spawn`. To keep one
resolution path, both commands look up their profile/runtime pair (per
[Configuration](#configuration); default `base`) in the manifest and
export `WRIX_DEFAULT_IMAGE_REF=<entry.ref>`,
`WRIX_DEFAULT_IMAGE_SOURCE=<entry.source>`, and
`WRIX_AGENT=<runtime>` into the child environment before exec'ing
`wrix run`. The launcher reads those env vars when no `--spawn-config`
is supplied. `wrix run` has no `--profile` argv parser; any extra tokens
between
the workspace positional and the in-container command are forwarded into
the container as the command vector, so the env-var hand-off is the sole
image-selection contract on this path. The in-container command is
selected from the resolved phase backend: Claude uses `claude
--dangerously-skip-permissions`, while Pi uses `pi`.

### Concurrency & Locking

Multiple `loom` invocations on the same workspace are explicitly allowed.
The lock model is **phase/work-root advisory locks** plus a single
workspace-exclusive lock used only during destructive cache rebuild.

**Lock files** live **outside the workspace**, under
`$XDG_STATE_HOME/loom/locks/<workspace-basename>/` (default
`~/.local/state/loom/locks/<workspace-basename>/`):

- `plan.lock` — serializes interactive planning edits
- `todo.lock` — serializes workspace-wide changed-spec decomposition
- `<bead-or-epic-id>.lock` — serializes loop/gate/msg work roots
- `workspace.lock` — held by `loom init` and `loom init --rebuild`

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
| Planning | `plan` | exclusive on `plan.lock` |
| Todo decomposition | `todo` | exclusive on `todo.lock` (workspace-wide changed-spec preflight + one pending work epic) |
| Work-root mutating | `loop`, `gate`, `msg` | exclusive on each addressed bead/work-epic id; default `loop` resolves `loom:active` before locking |
| Workspace-exclusive | `init`, `init --rebuild` | exclusive on `workspace.lock` |

A mutating command waits up to 5 seconds for its lock, then errors naming
the held plan/todo/work root (no busy-loop, no silent stalls). `init` and
`init --rebuild` error immediately if any plan, todo, or work-root lock is
held.

**Why git is the second-order serialization point.** Two `loom loop`
invocations on different work epics share the loom workspace's
integration branch. They collide briefly at integration and at
origin push:

- Concurrent rebase + ff into the integration branch is serialized
  by git's own `index.lock` in the loom workspace; the losing
  process surfaces a clear error and retries.
- Concurrent driver push gates pushing to `origin/<integration-branch>`
  produce non-fast-forward on the second push; the losing push gate
  re-fetches, rebases, reruns verification/review for the new actual
  push range, and retries.

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

**Refused inside container:** driver/workspace-mutating or managed-
session-spawning surfaces: `loop`, `init`, `plan`, `todo`, `msg`,
`use`, `loom gate mint`, and LLM-spawning gate subcommands (`review`,
`judge`, `rubric`, `audit`).

**Allowed inside container:** `status`, `logs`, `spec`, and deterministic
gate inspection subcommands (`loom gate verify`, `check`, `test`,
`system`, `status`, `verify-marker`). The bead workspace is the agent's
worktree, so self-check commands such as
`loom gate verify --diff <base>..HEAD` are part of normal
implementation feedback; driver-spawning, LLM-spawning, and bd-mutating
surfaces are banned.

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

Gate invocations that run outside an agent session write their own raw
JSONL logs under `.loom/logs/gate/<scope-or-bead>-<utc>.jsonl` using
the same `AgentEvent` stream and flush contract. A parent bead/session
log records the gate log path as a breadcrumb rather than sharing the
same file, so concurrent gate subprocesses never interleave. A
gate-run `start` event without a matching `end` event is an incomplete
or interrupted gate, not a success.

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

- **Session lifecycle** — the public agent-driver contract is the
  command/event lifecycle shared by every backend. Workflow code
  selects a backend for a phase, spawns a session handle, sends
  prompt / steer / cancel / mode commands, and consumes the resulting
  `AgentEvent` stream. `loom-events` provides a `Session` trait for
  consumers that need a backend-neutral interoperability surface, but
  the spec does not require workflow code to erase backend types or use
  any particular Rust carrier representation. Host-side JSONL
  subprocess backends keep a typestate (`AgentSession<Idle|Active>`) as
  an internal mechanic — ready to prompt, active run in progress, stdin
  attached — but that typestate does not leak through the
  interoperability trait. Direct uses that host-side lifecycle for its
  runner subprocess; only its in-container `Conversation` loop lacks Pi
  / Claude handshake typestate.
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
discriminator plus a free-form `summary` and structured `payload`. The
Rust producer surface uses the typed `DriverKind` enum (with
`Other(String)` for unknown wire values), so producing code cannot typo
kind strings while consumers remain forward-compatible. The closed
producer set includes verdict / retry / push / container / infra /
Direct / observer variants (`verdict_gate`, `retry_dispatch`,
`push_gate_walk`, `push_gate_refuse`, `push_gate_clean`,
`container_spawn`, `container_oom`, `infra_failure`,
`doom_loop_tripped`, `duplicate_tool_result`, `token_usage`, `offload`)
plus gate and routing variants:
`gate_run_start`, `gate_run_scope`, `gate_run_lane`, `gate_run_end`,
`gate_run_skipped`, `marker_routed`, `clarify_downgraded`,
`bd_state_transition`, and `epic_auto_closed`. Payloads are typed at
producing/consuming sites
that Loom relies on (`GateRun`, marker routing, clarify downgrade, bd
transition); render-only consumers may treat them as generic JSON. The
observer-emitted variants (`doom_loop_tripped`,
`duplicate_tool_result`, `token_usage`) originate in `llm` rather than
`loom-driver`, so they fire on both Loom-binary runs and external
consumer-driven `Conversation` runs. `offload` originates in Direct's tool
context because Pi and Claude tool transcripts are owned by their
subprocess agents.

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

After every worker agent phase ends, the verdict gate classifies the
agent's terminal marker and mechanical state before the bead's state can
advance. Implementation beads then pass a deterministic post-integration
`loom gate verify --diff <pre-integration-head>..HEAD` at the loom
workspace before their integration is durable. LLM review is the
molecule-completion push-range step, not a default per-bead pass. The
review rubric, inputs, and concerns are defined in [gate.md](gate.md);
this section retains the execution layer — the decision table, recovery
mechanics, markers, labels, and infra-failure handling.

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
   investigation territory (likely a misconfigured wrix
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
5. **FF-merge** the bead branch into the integration branch and run
   deterministic post-integration verification:
   `loom gate verify --diff <pre-integration-head>..HEAD`. The run
   executes the project pre-commit lane via prek plus affected
   `[check]` / `[test]` annotations. On failure, roll back the
   integration branch and retry/reopen the same bead with the gate log
   path in `previous_failure`. No per-bead focused LLM review, no
   per-bead `loom gate mint`, no marker mint, no push.
6. **Delete the transient `loom/<id>` ref** with `git branch -D`,
   unconditionally — whether the path exited cleanly, rolled
   back from audit-fail, or aborted from rebase conflict. The
   bead clone keeps its copy of the branch until the bead's
   workspace is reaped on `bd close`.

On deterministic verify failure, the integration is rolled back to the
pre-integration head and the bead routes to recovery with cause
`post-integrate-fail`. On verify-pass, the bead's integration is
durable and the lock is released. Stage-2 failures discovered later at
molecule completion create or reuse same-molecule remediation beads;
they do not reopen already-integrated original beads. When attribution
is possible, the remediation detail records whether the failure appears
stage1-eligible, push-only, or unknown.

Molecule-completion push gate runs at the **loom workspace** after all
beads in the molecule have integrated and deferred remediation has
stabilized. The driver fetches origin, rebases local integration commits
when `origin/<integration-branch>` advanced, computes the actual push
range `origin/<integration-branch>..HEAD`, runs the actual prek pre-push
chain for that range, runs `loom gate review --diff <actual-push-range>`
only after deterministic pre-push success, mints `MarkerProof`, then
`git push origin <integration-branch>`. The critical section spans
**fetch/rebase + pre-push verify + review + mint + push** atomically —
releasing the lock between mint and push would let another verdict
gate's rebase mutate HEAD, invalidating the just-minted marker. If
origin advances again and push becomes non-fast-forward, the old gate
evidence is invalid and the driver fetches/rebases/reruns the gate.
prek's pre-push hook chain fires on the push; the `pre-push-checks`
wrapper around each hook reads the just-minted marker and short-
circuits only hooks covered by the marker.

Both stages at the loom workspace are load-bearing for
parallel-agent correctness: the marker bound to HEAD's tree OID
must match the state being pushed, and rebasing bead A onto
integration that already includes bead B mutates A's commit SHA
(and tree). Auditing at the bead workspace pre-integration would
mint a marker that becomes stale the moment another bead lands
first. The two stages also separate concerns: per-bead catches
cross-bead deterministic interactions (compile/lint/test
breakage) at the bead that introduced them; per-molecule catches
review-level concerns once over the cumulative diff, after deferred
molecule remediation has had a chance to drain.

**Cost and queue depth.** Parallel beads complete in their bead
containers concurrently, but their loom-workspace integration
steps serialize. Each per-bead pass pays deterministic verify only;
broad LLM review and remediation are amortized in the molecule
stabilization/push step instead of reminting tiny per-bead work. The
per-molecule push gate adds the pre-push deterministic chain plus one
review LLM call. At high parallelism (N beads landing simultaneously),
queue depth × per-pass time is the integration tail. For typical
workloads (N ≤ 4) this is sub-5min; for higher N, the queue is
the bottleneck and the throughput gain from per-bead-container
parallelism caps.

**Bead-container self-verify is feedback only.** The driver configures
the bead workspace's `core.hooksPath` to the canonical
`wrix.prekHooks` installation (see [pre-commit.md § Agent self-verify
in the bead container](pre-commit.md)), so prek fires on the agent's
commits, catching treefmt drift, integrity findings, and project hook
failures in-session. It is **not** the trust source for the marker. The
driver's verdict gate at the loom
workspace runs its own independent deterministic integration gate and
push-range review; the agent cannot mint a `MarkerProof` and cannot
bypass driver verification by emitting a structured "I verified" report.
The agent's hook chain is a feedback layer that reduces wasted recovery
iterations, not an authorization mechanism.

**Marker mint at the molecule-completion push gate.** Per
[gate.md § Marker](gate.md#marker), the mint trigger is the
molecule-completion push gate's typed `GateSuccess`: the gate verifies
and reviews the actual push range, constructs `GateSuccess`, calls
`MarkerProof::from_gate_success(success, loom_workspace)`, writes the
sealed marker atomically to `.loom/marker.json`, then runs the
integration push — all inside the same critical section. prek's
pre-push hook chain fires on the push; the `pre-push-checks` wrapper
around each hook reads the just-minted marker and short-circuits only
when that hook is covered by the marker (per
[pre-commit.md § Marker integration](pre-commit.md#marker-integration)).
There is no standalone `loom gate verify-marker` hook gating the chain;
the wrapper is the marker's only push-time consumer, and marker absence
is a fall-through condition (operator-manual push), not a push failure.
Per-bead integration steps do not mint markers (they do not push); the
marker is one per push-range gate, covering the cumulative integrated
state and the exact origin range being pushed. The marker's
content-addressed
validation is what makes the push fast in the warm-driver-loop
case without trusting an unverified stamp.

Driver-detected failures enter a bounded recovery loop; agent
self-reports go straight to human resolution via `loom msg`.

**Interactive vs worker sessions.** The verdict gate's reconciliation
applies to **worker sessions only** — single-shot agent dispatches
against a bead, work epic, or review scope (`loom loop`'s per-bead
worker, `loom todo`, `loom gate review`). **Interactive sessions** —
multi-turn chats with a human in the loop (`loom plan`, `loom msg -c`;
identifiable in the template layer by inclusion of
`chat_marker_final_turn_only.md`) — are agent-and-human authoritative:
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
`loom gate review` against the session's effects, no remediation bead
minting. On mid-session failure (container OOM, observer abort,
marker swallowed) the driver exits non-zero with a diagnostic
without auto-retry — the one-free-retry infra-failure path applies
to expensive worker dispatches; an interactive session re-invocation
is cheap and the user is right there to redispatch.

The decision table below therefore documents **worker-session**
outcomes only. Interactive sessions short-circuit before the table
is consulted.

**Decision table** (per-bead loop/review worker sessions only —
interactive sessions short-circuit before this table is consulted, and
`loom todo` uses the typed `LOOM_TODO` success protocol in
[Spec and Work Epic Lifecycle](#spec-and-work-epic-lifecycle)). The
gate inspects four signals — the agent's exit marker, whether the bead
was bd-closed, whether the bead-branch diff is empty (no commits since
dispatch), and whether the working tree is clean (`git status
--porcelain` empty). It produces `blocked`, `clarify`, `recovery` with
a cause, or a clean worker result that may then enter
post-integration verification:

| Marker | bd-closed | Diff | Tree clean | Outcome |
|--------|-----------|------|------------|---------|
| `LOOM_BLOCKED` | — | — | — | `blocked` |
| `LOOM_CLARIFY` | — | — | — | `clarify` |
| `LOOM_RETRY` | — | — | — | recovery (`agent-retry`) |
| (none) | — | — | — | recovery (`swallowed-marker` OR `observer-abort`; see below) |
| `LOOM_COMPLETE` | no | — | — | recovery (`incomplete-signaling`) |
| `LOOM_COMPLETE` | yes | empty | — | recovery (`zero-progress`) |
| `LOOM_COMPLETE` | yes | non-empty | no | recovery (`tree-not-clean`) |
| `LOOM_COMPLETE` | yes | non-empty | yes | clean worker result; proceed to post-integration verify |
| `LOOM_NOOP` | no | — | — | recovery (`incomplete-signaling`) |
| `LOOM_NOOP` | yes | * | no | recovery (`tree-not-clean`) |
| `LOOM_NOOP` | yes | empty | yes | `done` (intentional no-work result) |

In the table above, `—` means the signal isn't inspected because an
earlier signal already determined the outcome; `*` means any value is
accepted.

**Loom-workspace integration outcomes.** The decision table covers
the per-bead exit signals. Independently, the per-bead integration
step in the loom workspace can fail in four distinct ways even
when the bead's own exit signals were clean:

| Failure phase | Cause | Detail | Recovery |
|---------------|-------|--------|----------|
| Signature verification pass 1 (fetched commits) | `signature-verification-failed` | side = `worker` | `loom:blocked` — operator investigates wrix container signing setup |
| Rebase | `integration-conflict` | `{ files, new_base_sha }` | one agent-retry pass; second failure escalates to `loom:clarify` (same cause) |
| Signature verification pass 2 (rewritten commits) | `signature-verification-failed` | side = `driver` | `loom:blocked` — operator investigates loom-workspace gitconfig + key resolution |
| Audit (`prek run --hook-stage pre-push` + verify) | `post-integrate-fail` | `{ failures: Vec<VerifierFailure>, gate_log_path }` | rollback via `git reset --hard HEAD~1`; route to recovery so next iteration sees the cross-bead breakage |

`post-integrate-fail` covers cross-bead interactions (bead A's API
change breaks tests bead B introduced earlier in the molecule),
rebase-induced breakage, and integration-tree state that no
bead-workspace verify could anticipate. The recovery prompt includes
the audit's specific verifier failures so the next iteration can
address the cross-bead interaction.
Other beads in the molecule that haven't integrated yet continue to
be dispatched; their integrations queue at `index.lock` after
recovery resolves.

**Durable post-integrate gate logs.** Every post-integration gate
invocation that can fail after the bead branch was ff-merged writes a
durable gate log before rollback or cleanup. The log is preserved
under `.loom/logs/gate/` (or an equivalent gate-log root) and records:
command argv, scope flag, exit code, stdout, stderr, parsed terminal
marker when applicable, integration SHA, bead id, retry attempt,
rollback state, and the log path. The corresponding `driver_event`
payload and rendered summary name the log path so `loom msg` and
operators can inspect the exact failing verifier output without
reconstructing the rolled-back integration state. Retry attempts use
distinct log files.

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
environmental (wrix container misconfiguration on pass 1,
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
HEAD` plus `git clean -fdx` (with `target/`, `.git/`, `.wrix/`
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

`recovery` resolves to `retry` if the work epic's iteration counter is
below `[loop] max_iterations` (default 10), otherwise `blocked` with
the cause preserved in `bd update --notes`. The iteration counter is
**work-epic-level** state — cached in `work_epics.iteration_count` (see
the schema in *SQLite Cache Store* below) — and survives
`retry → [running]` round-trips. Per FR1, the same counter bounds
`loom loop`'s stabilization passes: every full work-epic pass (initial
implementation pass + each promoted deferred remediation pass)
consumes one slot. This is the same knob as the per-bead recovery loop
because a promoted remediation bead getting picked up *is* a work-epic
pass — the two concepts collapse onto one work-epic-level counter,
with in-session retry left to `[loop] max_retries` (default 2).

**Mechanical integration gate.** Marker parsing, bd-closed lookup,
diff inspection, and tree cleanliness are deterministic. After the bead
rebases and fast-forwards into `.loom/integration`, the driver runs
`loom gate verify --diff <pre-integration-head>..HEAD` against the
integrated tree. That verify run executes the project pre-commit lane
through prek and the affected `[check]` / `[test]` annotation lane per
[gate.md](gate.md); `[system]` is not part of the finite diff default.
Per verifier/hook, the gate captures pass/fail, stderr tail, and gate
log path.

Mechanical failure is sufficient grounds for same-bead recovery. The
integration branch is rolled back to `<pre-integration-head>`, the
worker's provisional `bd close` is reopened/retried according to the
existing recovery policy, and `PreviousFailure::PostIntegrateFail`
carries the durable gate log path. The default per-bead hot path does
not spawn a focused LLM reviewer session; the implementation prompt
requires worker self-review before `LOOM_COMPLETE`, and authoritative
LLM review runs once at molecule completion over the actual push range.

A `LOOM_CONCERN` marker from a review phase carries the parsed
`{"summary": "..."}` payload plus the buffered `LOOM_FINDING:` records
the walk streamed. Per-finding routing is decided by each finding's
`route`: `blocking` refuses the push and creates or reuses same-
molecule remediation work, `deferred` merges into a `status=deferred` /
`loom:deferred` molecule remediation bead, and `clarify` raises one
`loom:clarify` bead per finding hash with the `## Options — …` block
extracted from `evidence`. A clarify-route finding whose evidence lacks
a well-formed options block falls back to `loom:blocked` with cause
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

**Driver-detected causes flow through the stage's recovery surface.**
Worker-session causes (swallowed marker, incomplete signaling, zero-
progress, tree-not-clean, verify-fail, observer-abort, agent-retry, and
post-integrate-fail) enter same-bead recovery. Push-range review
concerns or bad walks refuse the push and route through same-molecule
remediation or human-resolution paths; already-integrated original
beads are not reopened solely because molecule review failed. Deferred
review findings do not create ready work immediately; they merge into
deferred remediation beads.

**Remediation beads bond to the originating molecule.** Every deferred,
promoted, tree-sweep, or clarify remediation bead created by the gate is
bonded to the originating molecule via `bd mol bond <molecule-id>
<remediation-bead-id>` before it can become eligible for dispatch. The
bond is mandatory and atomic with creation — a remediation bead that is
not bonded to a molecule by the time it leaves the gate is a bug.

Bonding is load-bearing in two places:

1. The **push gate** refuses to push while any bead in the molecule
   carries `loom:blocked`, `loom:clarify`, or `loom:deferred`. Orphan
   remediation beads are invisible to that check, so a molecule could
   push with unresolved work attached to a shadow bead the gate never
   saw.
2. **Auto-iteration** (Push gate, Functional #9) walks `bd mol
   progress <id>` to decide whether the molecule is clean. Orphan
   remediation beads are absent from that walk; the molecule looks done
   even when its remediation work is pending.

The originating molecule is resolved by reading the failing bead's
existing molecule bond — `bd show <id> --json` returns the molecule
ID. If the failing bead is itself unbonded (which is itself a bug
upstream), the verdict gate refuses to create remediation state and
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
| `verify-fail` | `VerifyFailures(Vec<VerifierFailure>)` | One `VerifierFailure { target, exit_code, stderr_tail }` per failing project hook or spec verifier in the relevant gate scope. Diff-scope per-bead verification includes project pre-commit plus affected `[check]` / `[test]`; tree/system scopes may include `[system]`. All failing verifiers are included; the budget is split across them with later failures truncated first; each `stderr_tail` is capped at ~1500 chars before split. If a separate review phase also raised a concern, its reasoning is set as `review_notes` (separate ~1000-char budget) rendered under a `Review notes:` heading. |
| `review-concern` | `ReviewConcern { summary: String, findings: Vec<Finding> }` | Summary is the parsed `summary` field from the terminal `LOOM_CONCERN: {"summary": "..."}` marker. `findings` is the buffered list of unsuppressed `LOOM_FINDING:` records after rubric suppression filtering (per the typed `Finding` record in [gate.md § Findings and Minting](gate.md#findings-and-minting)); a well-formed concern whose findings are all suppressed becomes a clean effective review, not this cause. Per-finding tokens drive `mint`'s routing: clarify-route findings mint as single-finding clarify beads (one per finding), all other findings bundle into per-spec remediation batches (one batch per lead-spec). The recovery prompt renders the summary plus a one-line-per-finding `evidence` digest. The in-code recovery cause is `RecoveryCause::ReviewConcern`. |
| `bad-walk` (concern-malformed) | `BadWalk(BadWalk::Concern { payload: String, parsed_findings: Vec<Finding> })` | "Your `LOOM_CONCERN:` payload did not parse as `{"summary": "<non-empty>"}`. Literal payload after the marker: `<payload>`." When `parsed_findings` is non-empty, append a per-finding digest so the agent's diagnosis from the well-formed streamed findings is not lost. Wrapped-enum pattern mirrors `RecoveryCause::ReviewConcern(ReviewFlag)`. |
| `bad-walk` (concern-without-findings) | `BadWalk(BadWalk::ConcernWithoutFindings { summary: String })` | "You emitted `LOOM_CONCERN` with summary `<summary>` but no `LOOM_FINDING:` lines streamed. Either emit findings before the terminator or terminate with `LOOM_COMPLETE`." |
| `bad-walk` (findings-without-concern) | `BadWalk(BadWalk::FindingsWithoutConcern { finding_count: usize, findings: Vec<Finding> })` | "You streamed `<finding_count>` `LOOM_FINDING:` line(s) but terminated with `LOOM_COMPLETE`. Use `LOOM_CONCERN: {"summary": "..."}` when findings are emitted." Per-finding digest of `findings` is appended so the agent's next iteration sees the diagnosis it just emitted. |
| `bad-walk` (malformed-finding) | `BadWalk(BadWalk::MalformedFinding { errors: Vec<FindingParseError>, terminal: TerminalSurface })` | "One or more `LOOM_FINDING:` lines failed parse." Per-line errors are enumerated; the well-formed terminal is rendered alongside so the agent fixes the malformation (typically: drop the surrounding markdown fence) without losing the surrounding well-formed context. This is the variant that fires on backtick-wrapped finding lines whose JSON otherwise would have parsed. |
| `integration-conflict` | `IntegrationConflict { files: Vec<PathBuf>, new_base_sha: GitOid }` | "Your bead branch could not be rebased onto integration — files conflict: <files>. The new integration tip is <new_base_sha>. Rebase your bead workspace onto the new tip, resolve, and re-commit." Single-retry cap (not full `[loop] max_retries`); a second rebase-conflict escalates the bead to `loom:clarify` with the same cause. The `signature-verification-failed` cause does **not** appear in this table because it routes to `loom:blocked` immediately without an agent-retry pass — there is no next dispatch and thus no `PreviousFailure` context. |
| `post-integrate-fail` | `PostIntegrateFail { failures: Vec<VerifierFailure>, gate_log_path: PathBuf }` | "After your bead was rebased onto the integration branch and ff'd, the post-integration verify failed at the loom workspace. The integration was rolled back. Gate log: <path>. Specific failure: <verifier-failure blocks>." Used for cross-bead deterministic breakage where the bead-workspace self-check may have passed but the integrated tree's verify failed. The default per-bead hot path has no focused review session, so review-style findings route at molecule completion via `review-concern` / `bad-walk` rather than `post-integrate-fail`. Capped at the shared `PREVIOUS_FAILURE_MAX_LEN` budget; the path is outside the truncation budget. |
| `agent-retry` | `AgentRetry { reason: String }` | "Previous attempt requested retry: <reason>. A fresh dispatch was scheduled." `reason` is the verbatim prose the agent wrote on the line preceding `LOOM_RETRY` (environmental detail or stuck-on-approach summary). Consumes one `[loop] max_retries` slot; on exhaustion the target bead is marked `loom:blocked` with cause `retry-exhausted`. The recovery prompt instructs the retry attempt to escalate to `LOOM_BLOCKED` (no candidate resolutions) or `LOOM_CLARIFY` (with `## Options — …`) if the same problem persists rather than emitting `LOOM_RETRY` again. |

When `previous_failure.is_some() && attempt > 0`, the `loop.md`
template prepends a first-instruction reframe: *"Re-read the
previous failure block above and address its specific concern
before re-implementing."* The `attempt` counter is per-bead
in-session (bounded by `[loop] max_retries`), resetting when a
fresh bead is dispatched; work-epic-level iteration is opaque to the
agent because each remediation bead is a different prompt context.

Transcript excerpts are deliberately not included — the agent can re-read
its own session log if it needs prior tool-call context.

**Labels.**

- `loom:blocked` is applied by either: (a) the `LOOM_BLOCKED` agent marker, or
  (b) driver-detected gate failure with recovery exhausted, or (c)
  `loom gate mint` refusing to apply `loom:clarify` to a
  clarify-route finding whose `evidence` lacks a well-formed
  `## Options — …` block (cause `clarify-without-options` — the
  agent should have emitted `LOOM_BLOCKED` directly, but the driver
  falls back to blocked rather than minting a stranded clarify bead
  the chat-drafter cannot resolve). All meanings are uniform from
  the human's perspective — the bead is blocked and `loom msg` is
  the resolution channel.
- `loom:clarify` is applied by either: (a) the `LOOM_CLARIFY` agent
  marker, (b) `loom gate mint` lifting a clarify-route finding
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
- `loom:conflict` is the non-terminal marker the parallel
  (`--parallel N`) verdict gate applies to a bead whose first
  driver-side rebase conflicted. It is the parallel-shaped home for
  the single `integration-conflict` retry budget the serial path
  holds in driver memory: a one-shot batch has no in-process agent
  left to re-dispatch once merge-back runs, so the budget rides on
  the bead instead. Unlike `loom:blocked` / `loom:clarify` it is
  *not* paired with a `status=blocked` transition, so `bd ready`
  keeps surfacing the bead for its one retry against the moved
  integration tip on the next `loom loop` pass. A second conflict —
  detected by the merge-back step re-reading this label off the
  re-fetched bead — escalates to `loom:clarify` with cause
  `integration-conflict` and the synthesized `## Options — …` block,
  exactly as the serial path's second rebase-conflict does.
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
- Closing a bead does not automatically remove stale `loom:blocked` or
  `loom:clarify` labels. `loom msg` filters out closed beads regardless
  of labels, so label cleanup is not a prerequisite for hiding resolved
  work from the human queue.

**Routing observability.** Every terminal-marker/finding route emits a
`DriverKind::MarkerRouted` event. A clarify-bound route that falls back
to blocked because its options block is missing or malformed emits
`DriverKind::ClarifyDowngraded` with the source route, marker/finding
identity, options parse result, evidence/reason hash + excerpt, gate log
path, and event sequence. Each bd mutation the driver applies emits
`DriverKind::BdStateTransition`, and clarify downgrades also write a
compact bd-note breadcrumb carrying cause `clarify-without-options` so
operators can diagnose the downgrade without replaying the whole log.

**Marker definitions.** The agent ends every phase by emitting exactly
**one** marker on its own line, as the final output of the session.
Markers are **mutually exclusive** — a session emits one and only one.
Todo has a typed success marker; other worker phases use the generic
success/self-report markers below.

- `LOOM_COMPLETE` — the loop/review work succeeded. For a bead worker,
  the agent has implemented the bead's criteria and `bd close`d the
  bead. The diff is non-empty (real changes); see `LOOM_NOOP` below for
  the zero-diff variant. Valid in `loop` and zero-finding `review`, and
  in final turns of interactive sessions where the template permits it;
  wrong phase for `loom todo`.
- `LOOM_NOOP` — loop work was already done in tree; the phase
  intentionally produced an empty diff. Without `LOOM_NOOP`, an empty
  diff is treated as `zero-progress` (a recovery cause). The agent
  emits `LOOM_NOOP` to distinguish "no work needed" from "work
  attempted but produced no diff." Valid in loop worker phases only;
  not valid in `loom todo` or the review phase.
- `LOOM_TODO: <json>` — todo-specific success marker. The JSON payload
  parses as `loom-protocol::todo::TodoSuccess` and is validated against
  the deterministic preflight roster before any cursor or active-epic
  state changes. It is the only successful terminal marker for
  `loom todo`; `LOOM_COMPLETE` and `LOOM_NOOP` are wrong-phase success
  markers there.
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
  (`loop`, `todo`, `review`); invalid in interactive sessions
  (`plan`, `msg`).
- `LOOM_BLOCKED` — the agent cannot proceed and is self-reporting a
  **genuine dead end** — no candidate resolutions to enumerate, no
  retry path the agent expects to succeed. Use `LOOM_RETRY` for
  environmental failures or stuck-on-this-approach cases; use
  `LOOM_CLARIFY` when the agent can frame the decision-point as a
  structured `## Options — …` block. Write the reason on prior lines
  before the marker; the gate applies `loom:blocked` to the target
  bead/work epic (the bead under dispatch for `loop` / `review`, the
  `loom:todo` work epic for `todo`) and exits the verdict evaluation
  without entering recovery. Other beads in the molecule continue
  running; the labelled bead or work epic waits for human resolution via
  `loom msg` (where `msg -c` walks the human through candidate
  enumeration in-session). Valid in worker phases only — invalid in
  interactive sessions (`plan`, `msg`).
- `LOOM_CLARIFY` — the agent has a specific question with structured
  options for the human (per the [Options Format
  Contract](gate.md#options-format-contract)). The discriminator
  against `LOOM_BLOCKED`: clarify means *I can enumerate the
  candidate resolutions*; blocked means *I cannot*. The target bead
  is **the bead under dispatch** for `loop` / `review`, and
  **the `loom:todo` work epic** for `loom todo` (per [templates.md —
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
  in worker phases only — invalid in interactive sessions (`plan`,
  `msg`).
- `LOOM_CONCERN` — the review phase found a quality issue with the
  molecule's work; push must not fire. Carries a JSON payload:
  `LOOM_CONCERN: {"summary": "<one-sentence summary>"}`. The
  payload is **terminator-shaped**, not routing-shaped — `summary`
  is a verdict-log entry, nothing else. Per-finding routing
  (concern token → remediation bead, `invariant-clash` → clarify,
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
both require typed gate evidence constructed only by gate-invocation
code; `NoGate` is the only legitimate "gate did not fire" terminal and
carries the reason so the human-readable summary names it:

```rust
#[must_use]
pub enum GateOutcome {
    Success(GateSuccess),                              // clean ship
    Fail(GateFail),                                    // gate ran, found problems
    NoGate { beads_processed: u32, reason: NoGateReason },
}

pub enum NoGateReason {
    NoBeadsReady,       // selected work roots had no ready child work
    SelectionPartial,   // explicit bead/root selection processed work but its epic is not complete
}
```

**`GateSuccess`** — the bulletproof variant. Construction asserts
matching typed gate evidence for the final pushable state *plus* on-
disk evidence that the relevant gate child processes actually ran.
Field shapes are non-`Option`: absence of any value is a failure path
that constructs `GateFail` instead. The constructor lives in
`loom-gate` (alongside `MarkerProof::from_gate_success`, the mint
authority that consumes a sealed `GateSuccess`); the `_private: ()`
field is the structural seal that prevents struct-literal construction
outside the crate, so `GateSuccess::new` is the sole minting path
regardless of its `pub` visibility.

```rust
pub struct GateSuccess {
    pub verified: VerifiedScope,        // deterministic success for exact range
    pub reviewed: ReviewedScope,        // review success for same exact range
    pub pre_push: PrePushCoverage,      // hook ids / entries proven passed
    pub tree_oid: GitOid,               // current clean HEAD tree
    pub push_range: ResolvedDiffRange,  // origin/<branch>..HEAD at push time
    pub gate_log_paths: Vec<PathBuf>,   // evidence files exist and contain end events
    _private: (),                       // structural seal — no struct-literal path
}

impl GateSuccess {
    /// Asserts: deterministic and review evidence refer to the same
    /// resolved range and tree; pre-push coverage includes every hook
    /// the marker may authorize; relevant config digests match;
    /// gate_log_paths exist and contain successful GateRun end events;
    /// the workspace is porcelain-clean at the tree_oid. Any failure
    /// returns Err(GateFail::new(...)).
    pub fn new(...) -> Result<Self, GateFail> { ... }
}
```

**Gate evidence types.** `GateRun` is the typed record for any gate
invocation (success, failure, skip, or abort). `VerifiedScope` is a
sealed deterministic-success value derived only from a successful
`GateRun`. `ReviewedScope` is a sealed review-success value derived
only from a successful review run. These types are architecture-bearing:
review and marker minting consume typed evidence, never a scalar such
as `--verify-exit 0`.

**`GateFail`** — carries the failure reason explicitly so CLI/log
summaries and the next outer-loop iteration consume it directly,
without reverse-engineering from exit codes:

```rust
pub struct GateFail {
    pub reason: GateFailReason,
    pub gate_runs: Vec<GateRun>,
    pub review_marker: Option<ExitSignal>,
    pub review_log_path: Option<PathBuf>,
    pub total_handoffs: u32,
    pub stalled_at_max_iterations: bool,
    _private: (),
}

pub enum GateFailReason {
    VerifierFailed,                          // deterministic GateRun failed
    PrePushHookFailed,                       // pre-push stage failed before review
    ReviewConcern { summary, finding_count },// marker is LOOM_CONCERN; per-finding detail in mint output
    BadWalk(BadWalk),                        // review walk terminator malformed or mismatched
    EmptyDiffNoop,                           // marker is LOOM_NOOP — no reviewable work
    StalledMaxIterations,                    // outer-loop counter exhausted
    SignalKilled,                            // child terminated by signal
    ReviewEvidenceMissing,                   // log file absent / empty / mismatched marker
    MarkerCoverageMissing,                   // marker would not cover one or more hooks
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
open `loom:blocked` and `loom:clarify` bead across all specs. Closed
beads are excluded from list/chat queues even if their labels remain on
the bead. `-s <label>` is the only narrowing path. No active-spec cache
or work-epic selection is consulted for any msg mode.

**Chat session shape.** `loom msg -c` (optionally with `-s <label>`)
launches the resolved profile/runtime via interactive `wrix run`, runs
the selected chat-capable agent with the `msg.md` template, and walks
the user through outstanding beads interactively. The session has **full
bd-write authority** on the beads in its queue: notes via
`bd update --notes`, label add/remove
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
human, not by driver auto-remediation.

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
| `loom-driver` | internal | Host-side runtime — `AgentBackend` trait, `CacheDb`, `Config`, `BdClient`, `Clock`, profile manifest, lock files, scratch dir, git ops, workflow-layer driver-event emission (verdict-gate, push-gate, container-spawn). |
| `loom-render` | internal | `Renderer` trait + `Pretty` / `Plain` / `Json` / `Raw` impls; `LogSink` (impl `EventSink`) driving disk JSONL from the same event stream the renderer consumes. |
| `agent` | internal | `AgentBackend` implementations (pi, claude, direct). Pi/Claude drive subprocess agents; `direct` composes `llm` with Loom's six sandbox-aware tools behind the Direct runner. Adapters flatten backend wire schemas into `loom-events` variants. |
| `loom-workflow` | internal | Workflow engine — plan, todo, loop, gate, msg. Selects concrete backends per phase and drives the shared session lifecycle. Owns orchestration loop, bead lifecycle, retry logic, push gate, verdict gate. |

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
   deserializes into `BTreeMap<ProfileName, BTreeMap<AgentRuntime, ImageEntry { ref, source, digest? }>>`
   once at loom startup. Downstream code receives typed profile/runtime keys
   and `&ImageEntry`, never raw JSON.

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
  `spec:`/`profile:`/`loom:spec`/`loom:todo`/`loom:active`/
  `loom:clarify`/`loom:blocked` prefix families once at the boundary,
  so call sites read through typed accessors (`spec_label()`,
  `profile_name()`, `is_spec_epic()`, `is_todo_stage()`,
  `is_active()`, `is_clarify()`, `is_blocked()`) rather than re-doing
  `strip_prefix` walks. `loom:active` is an execution bookmark for the
  default work epic, never a spec-discovery filter.
- Maps CLI errors to typed error variants
- All subprocess calls have a 60-second timeout (configurable). Prevents
  unbounded hangs from a stuck `bd` process.
- Key operations: `show`, `create`, `close`, `update`, `list`, `dep_add`,
  `mol_bond`, `mol_progress`. No `dolt_push` / `dolt_pull` wrappers — loom
  relies on the bind-mounted Dolt socket so every `bd` call is already
  authoritative.

### SQLite Cache Store

Workflow cache lives in `.loom/cache.db`. The filename is deliberate:
this database is local, optional, reconstructable cache state, not a
source of truth. Git (code + specs), Beads/Dolt (beads, epics,
labels, metadata), and the current `docs/README.md` spec index are the
durable shared sources. A missing, stale, or corrupt cache may make Loom
slower or lose transient hints; it must never make Loom conclude that
changed spec work is clean.

The previous `.loom/state.db` name is retired. Existing installations
migrate by moving compatible rows into `.loom/cache.db` or rebuilding
from durable sources; correctness paths must not require either file
to exist before they run.

```sql
CREATE TABLE specs (
    label TEXT PRIMARY KEY,
    spec_path TEXT NOT NULL
);

CREATE TABLE spec_epics (
    spec_label  TEXT PRIMARY KEY REFERENCES specs(label),
    epic_id     TEXT NOT NULL,
    todo_cursor TEXT                         -- cache of spec-epic metadata `loom.todo_cursor`
);

CREATE TABLE work_epics (
    epic_id          TEXT PRIMARY KEY,
    todo_head        TEXT,
    todo_fingerprint TEXT,
    is_active        INTEGER NOT NULL DEFAULT 0,
    iteration_count  INTEGER NOT NULL DEFAULT 0
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

CREATE TABLE criterion_status (
    spec_label        TEXT NOT NULL REFERENCES specs(label),
    criterion_id      TEXT NOT NULL,
    annotation_json   TEXT NOT NULL,
    result            TEXT NOT NULL,
    last_timestamp_ms INTEGER,
    last_commit       TEXT,
    evidence          TEXT,
    PRIMARY KEY (spec_label, criterion_id)
);

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- meta rows: schema_version and cache-only implementation details.
-- There is no current_spec/current_molecule pointer.
```

The gate's criterion-status cache is folded into `.loom/cache.db`;
there is no separate `.loom/gate-cache.sqlite`. Gate runs write
`criterion_status` rows keyed by the typed join `(SpecLabel,
CriterionId)`. Todo preflight parses current spec criteria and joins
against this cache after parsing cached strings back into typed
`CriterionId`, `CriterionAnnotation`, and `GitSha` values. Cache parse
failures are cache corruption diagnostics, not reasons to skip a spec.

Typed Rust API — no raw SQL outside `loom-driver`. The cache handle
opens `.loom/cache.db`, rebuilds cache rows from durable sources,
manages transient notes, records criterion evidence from gate runs,
and caches active work-epic iteration counts. Durable workflow
answers (which specs changed, which spec epic owns a cursor, which
work epic is active) are re-derived from Git + Beads before use; cache
rows are advisory mirrors.

**Rebuild (`loom init --rebuild`):** Drops and recreates all cache
tables, then repopulates from durable sources:

1. Parse `docs/README.md`'s spec index and cross-check `specs/*.md`.
   The index is authoritative for discoverable specs. An indexed row
   whose file is missing, an unindexed `specs/*.md` file, duplicate
   rows/files for one label, or a label/path mismatch is a structural
   diagnostic. Planned spec rename/delete work is allowed only when a
   planning session left durable evidence (spec/index diff plus, when
   needed, implementation notes); otherwise todo blocks with an
   Options-format diagnostic rather than guessing.
2. Query Beads for spec epics: exactly one bead labelled
   `loom:spec` and `spec:<label>` per indexed spec. Status is ignored;
   spec epics may be open or closed. The cache records the epic id and
   its `loom.todo_cursor` metadata when present. More than one spec
   epic for a label is a hard invariant violation naming the
   conflicting ids.
3. Query Beads for work epics: epics labelled `loom:todo` (pending
   decomposition) and the sole `loom:active` epic (default loop
   target). At most one open `loom:todo` epic and at most one
   `loom:active` epic may exist in a workspace; violations fail loud.
4. Parse each spec markdown for `## Companions` and populate
   `companions` rows.

Criterion evidence rows are not durable and are not reconstructed by
rebuild. Missing rows become `EvidenceState::Missing` when a prompt is
rendered.

Iteration counters reset to 0 on rebuild. **Notes are lost on
rebuild** — they live only in the cache and have no filesystem source
to reconstruct from. This can lose implementation hints but cannot
lose correctness: `loom todo` must still derive changed specs from
Git + Beads and fail loud when evidence is unknown.

**Companion declaration in specs.** Specs declare their companion paths
in a single, parseable section so rebuild is lossless:

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

### Spec and Work Epic Lifecycle

Loom uses two distinct epic kinds.

**Spec epic.** A spec epic is the durable per-spec metadata carrier.
It has labels `loom:spec` and `spec:<label>`, exactly one exists per
indexed spec, and its status does not affect cursor lookup. It carries
`loom.todo_cursor=<full git sha>`, meaning: `loom todo` has
successfully finalized decomposition coverage for this spec through
that commit. Normal `loom todo` never creates implementation child
beads under a spec epic.

**Work epic.** A work epic is the execution batch created by
`loom todo` for a deterministic changed-spec set. The driver creates
it before the todo agent runs, labels it `loom:todo`, and writes
`loom.todo_head=<sha>`, `loom.todo_fingerprint=<TodoFingerprint>`,
and the changed spec labels as metadata. While `loom:todo` is present,
the epic is still in the decomposition stage and is not a default loop
target. After a validated `LOOM_TODO` handoff, finalization removes
`loom:todo`, adds `loom:active`, advances every changed spec epic's
`loom.todo_cursor` to the preflight head, and removes `loom:active`
from any previous work epic. At most one open work epic may carry
`loom:todo`, and at most one epic may carry `loom:active`.

`loom:active` is an execution bookmark only: `loom loop` with no
positional ids runs the sole active work epic. It is never consulted by
`loom todo` changed-spec discovery.

**Lifecycle events:**

- **`loom plan [SPEC_LABEL ...]`** — interactive spec interview.
  Positional labels are initial context anchors only; zero labels
  starts from the project overview/spec index, an existing label pins
  that spec body, and a missing label is a proposed new spec. Plan
  may edit any spec and the index as the interview discovers
  cross-cutting scope. It does not write bd state, create epics, or
  record a touched-set manifest; Git records the touched set.
- **`loom todo` preflight** — deterministic, before any LLM prompt:
  1. Parse the current `docs/README.md` spec index and spec files; any
     inconsistency blocks.
  2. Ensure exactly one spec epic per indexed spec. Missing spec epic
     is created by the driver and marks that spec uninitialized for
     this preflight. More than one blocks.
  3. Read each spec epic's `loom.todo_cursor`. Missing cursor on a
     newly-created spec epic is expected and makes the spec changed;
     missing cursor on an existing spec epic blocks with an exact
     repair diagnostic. Malformed cursor, unknown commit, or cursor
     not an ancestor of `HEAD` blocks.
  4. Compute changed specs from Git using the durable cursor, current
     `HEAD`, the spec file blob, and the spec-index row. New indexed
     specs, index-row changes, and spec-file changes are changed.
     Missing local criterion evidence is not part of discovery.
  5. Parse Success Criteria for every changed spec and build typed
     `CriterionStatus` rows by joining against `.loom/cache.db`.
     Missing evidence becomes `EvidenceState::Missing`; malformed
     criteria (no annotation, multiple annotations, malformed syntax)
     block preflight.
  6. If no specs changed, no todo agent runs, no work epic is created,
     no cursor changes, and `loom:active` is unchanged.
  7. If an open `loom:todo` epic exists with matching
     `loom.todo_head` and `loom.todo_fingerprint`, reuse it so the
     agent repairs/completes the pending decomposition. Multiple
     matching epics, or any non-matching open `loom:todo` epic, block
     with an Options-format diagnostic; Loom never silently ignores
     pending decomposition state.
  8. Otherwise create one new `loom:todo` work epic for the changed
     set. The work epic is not active.
- **`loom todo` agent handoff** — the prompt contains the preflight
  roster, the work epic id, spec epic ids, diffs, implementation
  notes, and typed criterion evidence. The agent may create child
  implementation beads only under the named work epic. The only
  successful terminal marker for a todo session is
  `LOOM_TODO: <json>` (typed in `loom-protocol::todo`); generic
  `LOOM_COMPLETE` / `LOOM_NOOP` are wrong-phase success markers for
  todo. Decision-needed or dead-end exits use `LOOM_CLARIFY` /
  `LOOM_BLOCKED` with the Options Format Contract when applicable.
- **`loom todo` validation/finalization** — the driver parses
  `LOOM_TODO`, validates `head`, `TodoFingerprint`, exact changed-spec
  coverage, bead existence, bead parentage under the work epic, and
  per-spec outcomes. `Decomposed` outcomes must name a non-empty bead
  list; `NoWork` outcomes must carry a non-empty reason. Any mismatch
  leaves the work epic labelled `loom:todo`, records diagnostics on it,
  advances no cursor, and does not change `loom:active`. A validated
  handoff finalizes atomically as described above. Cursor advancement
  is all-or-nothing across the changed set and applies to both
  `Decomposed` and `NoWork` outcomes. Every todo run prints a
  driver-authored summary that lists each changed spec and its outcome
  (`decomposed`, `no-work`, or blocked/diagnostic before success), plus
  the work epic id when one exists; a spec absent from the summary is a
  validation failure, not a successful skip.
- **`loom loop`** — executes work. With no positional ids it runs the
  sole `loom:active` work epic. With one or more positional ids, each
  id may be an epic (run ready child work under that epic/molecule) or
  a task bead (run exactly that bead). Loop never narrows by spec.
- **`loom gate mint --tree`** — creates remediation work under work
  epics; spec epics remain metadata carriers and never receive normal
  implementation children. See [gate.md — Findings and
  Minting](gate.md#findings-and-minting).

**Todo success protocol.** The final non-empty line of a successful
todo agent session is exactly one JSON payload prefixed by
`LOOM_TODO:`. The Rust contract lives in `loom-protocol::todo`:

```rust
pub struct TodoSuccess {
    pub head: GitSha,
    pub fingerprint: TodoFingerprint,
    pub work_epic: BeadId,
    pub specs: Vec<TodoSpecSuccess>,
}

pub struct TodoSpecSuccess {
    pub label: SpecLabel,
    pub outcome: TodoSpecOutcome,
}

#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum TodoSpecOutcome {
    Decomposed { beads: NonEmptyVec<BeadId> },
    NoWork { reason: NonEmptyString },
}
```

`TodoSuccess.specs` contains exactly the preflight changed-spec set —
no omissions, no extras, no unchanged specs. Initialization is driver
preflight state, not an agent-reported outcome; the driver summary may
render "initialized + decomposed" when it created a spec epic and the
agent reported `Decomposed`.

`TodoFingerprint` is an opaque newtype constructed by the driver from
canonical JSON containing the preflight head, sorted changed spec
labels, each changed spec's path, spec-file blob SHA, spec epic id,
current cursor-or-null, initialized flag, and the `docs/README.md`
blob SHA. The canonical bytes are hashed and encoded by the driver;
callers compare typed values, not raw strings.

**Notes lifecycle.** Notes are transient hints attached to a spec —
bug-or-gotcha context, file paths to touch, design trade-offs left to
the implementer's judgement, decisions captured during a review, etc.
They are never canonical: the spec markdown and Beads metadata hold
durable design/cursor state, while the cache holds in-flight scratch.
Notes are discriminated by `kind`, with `implementation` consumed by
`loom todo` to seed bead bodies.

The note CLI remains:

```
loom note set   <label> [--kind implementation] --json '["note 1", …]'
loom note add   <label> [--kind implementation] --text "single note"
loom note clear <label> [--kind implementation | --all-kinds]
loom note list  [<label>] [--kind implementation | --all-kinds]
loom note rm    <id>
```

Lifecycle for `kind = implementation`:

| Event | Effect on notes rows where `kind = 'implementation'` |
|-------|------------------------------------------------------|
| `loom plan [labels...]` | Interview reads notes for anchor specs and any sibling it touches, then writes a merged array back via `loom note set` for each affected spec. Plan does not touch bd. |
| `loom todo` with no changed specs | Notes untouched. |
| Validated `LOOM_TODO` finalization | Notes for every changed spec are rendered into the created work beads as applicable, then deleted when the corresponding spec cursor advances. |
| Any non-finalized todo terminal (`LOOM_BLOCKED`, `LOOM_CLARIFY`, malformed/missing `LOOM_TODO`, validation failure, nonzero exit) | Notes untouched and all spec cursors untouched; the next invocation reprocesses the same changed set. |
| `loom init --rebuild` | All notes drop with the cache — no filesystem source reconstructs them. |

**Container exposure:** `.loom/cache.db` is inside the workspace
bind-mounted into containers. A malicious agent could modify it
directly. This is accepted because the cache is reconstructable and
non-authoritative: durable correctness comes from Git, Beads metadata,
and spec files. Cache tampering can lose hints or stale evidence; it
cannot make `loom todo` skip a changed spec because preflight treats
absent/invalid local evidence as unknown or blocking, never as clean.

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
- `offload/` — Direct-backend sessions only: a subdirectory the Direct
  runner creates lazily to hold tool output that exceeded the inline cap.
  Semantics are owned by [agent.md § Direct Output
  Bounding](agent.md#direct-output-bounding); the directory is removed by
  the same session-end cleanup below.

`<key>` is the session concurrency unit, matching the existing locks:
the joined anchor-label set (or `plan`) for `loom plan`, the work epic
id for `loom todo`, and the bead id for `loom loop` / `loom gate` /
`loom msg`. Two parallel loop workers on different beads of the same
work epic get independent scratch directories.

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
# Timeout (seconds) for git operations whose hooks legitimately run for
# minutes — notably the gate's `git push`, which fires the workspace's
# pre-push CI stage (nextest + nix build). Surfaces true hangs without
# aborting legitimate CI. Default 600 (10 minutes).
# git_hook_timeout_secs = 600

[loop]
# Work-epic-level: bounds `loom loop`'s outer loop on promoted deferred
# remediation beads (each full work-epic pass — initial pass + every
# promoted remediation pass — consumes one slot). Cached as
# `work_epics.iteration_count` in `.loom/cache.db` and surfaced in
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
# bead's `profile:X` label first, then [phase.loop] / [phase.default];
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
# [phase.gate.review]
# agent.backend = "claude"

[claude]
# Agent-runtime settings, applied wherever claude is selected. Seconds to
# wait for clean exit after `result` before SIGTERM (shutdown watchdog).
post_result_grace_secs = 5

# Backend-agnostic liveness knobs. `handshake_timeout_secs` bounds the pi
# startup probe + optional set_model response — a non-responsive launcher
# fails fast with `HandshakeTimeout` instead of hanging. `stall_warn_secs`
# emits a `warn!` every N seconds of agent silence on the agent event loop
# without aborting; claude can legitimately think for minutes, so this is a
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
- `wrix spawn --spawn-config <file> --stdio` accepts a JSON config,
      reuses container construction from existing `wrix run`, omits TTY
  [test](wrix_spawn_invocation_records_correct_argv)
- `SpawnConfig` JSON shape is stable: serialization round-trip preserves
      all fields and key names, including the `image_ref`, `image_source`,
      and optional `image_digest_path` fields
  [test](spawn_config_with_image_digest_path_round_trips)
- `wrix spawn` installs from `image_source` (a Nix store path) before
      invoking podman with `image_ref` as the ref; when `image_digest_path`
      is present, wrix skips reloading bytes if the same content already
      exists in the local image store
  [system](nix run .#test)
- Per-bead profile/runtime selection: two beads with different profile
      labels or backend runtimes result in `wrix spawn` invocations with
      the matching `image_ref` and `image_source` (and digest paths when
      present)
  [test?](per_bead_profile_runtime_dispatch_produces_distinct_image_refs)
- Loom reads `LOOM_PROFILES_MANIFEST` at startup and parses it into
      `BTreeMap<ProfileName, BTreeMap<AgentRuntime, ImageEntry>>`; missing
      env var or missing file errors before any bead spawn
  [test](from_path_missing_file_returns_manifest_not_found)
- A bead with `profile:X` where `X` is not in the manifest fails with a
      typed `ProfileError::UnknownProfile` naming the missing profile
  [test](lookup_unknown_profile_carries_manifest_path)
- A resolved backend runtime missing under an existing profile fails with a
      typed profile-manifest error naming the profile and runtime
  [test?](lookup_missing_runtime_for_profile_carries_profile_and_runtime)
- `--profile` CLI override takes precedence over bead labels
  [test](cli_override_swaps_resolved_image)
- `loom plan` shells out to interactive `wrix run` (TTY attached); does
      not capture stdio for JSONL
  [test?](plan_argv_starts_with_wrix_run_subcommand)

### Concurrency & locking

- `plan.lock`, `todo.lock`, and `<bead-or-epic-id>.lock` files are
      created outside the workspace and released on process exit
  [test?](phase_and_work_root_locks_create_expected_files)
- Two mutating commands for the same phase/work root serialize: the
      second waits up to 5s, then errors clearly naming the held root
  [test?](second_acquire_times_out_with_work_root_busy)
- Independent work-root commands run concurrently when they address
      different bead/epic ids
  [test?](different_work_root_locks_do_not_block)
- Read-only commands (`status`, `logs`, `spec`) acquire no lock and run
      during an active `loom loop`
  [test](readonly_paths_unaffected_by_spec_lock)
- `loom init` and `loom init --rebuild` acquire the workspace lock
      and error immediately if any plan/todo/work-root lock is held
  [test?](acquire_workspace_errors_when_phase_or_work_root_lock_held)
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
- With `LOOM_INSIDE=1`, driver/workspace-mutating or LLM-spawning
      subcommands (`loop`, `init`, `plan`, `todo`, `msg`,
      `loom gate mint`, `loom gate review`, `loom gate judge`,
      `loom gate rubric`, and `loom gate audit`) refuse with a clear
      error
  [test?](mutating_and_llm_spawning_subcommands_refuse_with_loom_inside_set)
- With `LOOM_INSIDE=1`, read-only/deterministic inspection subcommands
      (`status`, `logs`, `spec`, and deterministic `loom gate`
      subcommands such as `verify`) still run normally
  [test?](readonly_and_deterministic_gate_subcommands_run_under_loom_inside_set)

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
- Verdict gate, retry dispatch, push gate walk/refuse/clean, container
  spawn/oom, gate-run lifecycle, marker routing, clarify downgrade, and
  bd state transitions all emit `driver_event` with typed `DriverKind`
  producer variants
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
- `Session` interoperability trait defined in `loom-events` with methods `prompt`, `steer`, `cancel`, `set_mode`
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
- Gate invocations outside an agent session write separate JSONL logs
      under `.loom/logs/gate/` using the same `AgentEvent` stream; parent
      bead/session logs record the gate log path as a breadcrumb
  [test?](gate_invocations_write_separate_jsonl_logs_with_parent_breadcrumb)
- A gate log with `gate_run_start` and no matching `gate_run_end` is
      classified as incomplete/interrupted evidence and cannot construct
      `VerifiedScope`, `ReviewedScope`, or `GateSuccess`
  [test?](incomplete_gate_log_cannot_construct_scope_evidence)

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
- `loom init` configures `.loom/integration` with the canonical
      `wrix.prekHooks` `core.hooksPath`; it does not rely on the
      operator checkout's `.git/config` hook path
  [test](loom_init_configures_integration_hooks_path_from_wrix_prekhooks)
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
- Every created bead workspace is configured with the canonical
      `wrix.prekHooks` `core.hooksPath` before the agent receives it;
      if the hook path is missing or drifted on redispatch, the driver
      repairs it before spawning the container
  [test](bead_workspace_configures_and_repairs_hooks_path)
- Bead workspaces persist across attempts, recovery iterations,
      and `loom loop` invocations until the bead's first attempt
      after `bd close`
  [test?](bead_workspace_survives_retry_until_close)
- A bead workspace is reaped on the first `loom loop` iteration that
      observes the bead in `closed` status
  [test?](bead_workspace_reaped_on_bd_close)
- Every dispatch attempt sees a clean working tree against the bead
      workspace's current HEAD; `target/`, `.git/`, and `.wrix/`
      survive the pre-attempt reset
  [test](bead_workspace_reset_preserves_target_and_dotwrix)
- `loom loop` / `loom init` startup fast-forwards the loom
      workspace's integration branch to `origin/<integration-branch>`
      before any bead clone is materialized, so `loom/<id>` always
      branches off published HEAD
  [test](loop_start_fast_forwards_integration_to_origin_main)
- When the integration branch has diverged from
      `origin/<integration-branch>` (local commits not on origin),
      startup fails loud naming the divergent commits instead of
      branching beads off the stale base
  [test](loop_start_fails_loud_when_integration_diverged_from_origin)
- After the startup fast-forward, a bead clone forks from published
      HEAD, carrying commits that landed on `origin/<integration-branch>`
      rather than the pre-reconciliation local base
  [test](bead_clone_branches_off_published_head_not_stale_base)
- `loom loop` startup drops every bead workspace under
      `.loom/beads/` whose bead is `closed` and parented by the
      selected work epic/molecule, under the work-root advisory lock
  [test?](loop_startup_gc_drops_closed_bead_workspaces_for_selected_work_root)
- `loom loop` startup leaves closed bead workspaces from other
      molecules alone
  [test](loop_startup_gc_skips_closed_bead_workspaces_from_other_molecules)
- `loom loop` startup leaves bead workspaces alone whose bead is in
      any non-closed state
  [test](loop_startup_gc_skips_open_bead_workspaces)
- Each bead workspace's dispatch spawns its own `wrix spawn`;
      spawns overlap in wall-clock under `--parallel N > 1`
  [test](concurrent_spawns_overlap_in_wall_clock)
- Successful bead branches are fetched by the driver from the bead
      workspace path into the loom workspace, then rebased + fast-
      forwarded into the integration branch (linear history, no
      merge commits); the worker never invokes `git push`
  [test](driver_fetches_bead_branch_from_workspace_path)
- The bead-branch ref `loom/<id>` in the loom workspace is deleted
      unconditionally at the end of the per-bead critical section —
      clean exit, audit-fail rollback, and rebase-conflict abort
      all delete the ref
  [test](bead_branch_ref_deleted_on_every_exit_path)
- The bead clone's `origin` remote remains pointing at the loom
      workspace path after `create_worktree` so host-side
      ahead/behind tracking works; the bead container has no path
      mount to the loom workspace and cannot push from inside
  [test](bead_clone_origin_unchanged_under_a3)
- Parallel dispatch's second-and-later beads rebase onto the moved
      integration-branch HEAD before fast-forwarding
  [test](merge_branch_rebases_bead_branch_onto_head_before_ff)
- Driver-side rebase that conflicts textually aborts (`git rebase
      --abort`) and routes the bead to recovery with cause
      `integration-conflict` carrying the conflict files and the
      new integration tip SHA
  [test](rebase_conflict_routes_to_integration_conflict)
- `integration-conflict` recovery dispatches the agent at most
      once; a second rebase-conflict on the retry escalates to
      `loom:clarify` with the same cause
  [test](integration_conflict_one_retry_then_clarify)
- Driver-applied `integration-conflict` clarify beads carry a
      synthesized `## Options — …` block satisfying the Options
      Format Contract with two `### Option N — …` subsections
      (resolve-in-bead-clone and abandon-the-bead)
  [test](driver_applied_integration_conflict_clarify_carries_synthesized_options)
- `loom init` writes `[rerere] enabled = true` and `[rerere]
      autoupdate = true` into the loom workspace's local
      `.git/config` so the driver-side rebase replays previously-
      recorded conflict resolutions before falling through to
      `integration-conflict` recovery
  [test](loom_init_enables_rerere_in_loom_workspace_gitconfig)
- The driver-side rebase drives a rerere-replayed resolution to
      completion: when `rerere.autoupdate` auto-stages a recorded
      resolution and the rebase pauses awaiting `--continue`, the
      rebase is carried through (no remaining unmerged paths) rather
      than aborted, so a recorded resolution lands instead of falling
      to `integration-conflict` recovery
  [test](merge_branch_replays_recorded_rerere_resolution)
- The driver-side rebase (`rebase_onto_integration`) does not advance
      the integration branch — the fast-forward is a separate step
      (`ff_merge_integration`), so pass-2 signature verification runs
      on the rewritten commits before anything lands durably and a
      pass-2 failure leaves the integration line untouched
  [test](rebase_onto_integration_leaves_integration_branch_unmoved)
- The cross-spec rebase + ff critical section in the shared loom
      workspace is serialized by git's `index.lock`; a peer holding the
      lock makes the losing `rebase_onto_integration` /
      `ff_merge_integration` retry from its current view of the
      integration tip rather than surface a spurious conflict
  [test](rebase_onto_integration_retries_through_index_lock_contention)
- A stale loom-workspace `index.lock` that never clears exhausts the
      bounded retry budget and surfaces a typed `GitError::IndexLocked`
      naming the workspace (distinct from a content failure), instead of
      looping forever
  [test](rebase_onto_integration_surfaces_index_locked_on_stale_lock)
- Origin push of the integration branch retries non-fast-forward
      errors by fetching and re-rebasing onto
      `origin/<integration-branch>`
  [test](clean_review_reruns_loop_when_origin_push_races)
- On rebase abort, audit-fail rollback, signature-verification
      failure, agent failure, retry, tree-not-clean recovery, block,
      or clarify, the bead workspace persists (the default
      per-bead-close behavior) and the bead is routed to `Blocked` or
      `Clarify` per the verdict gate
  [test](workspace_persists_on_all_failure_paths)
- Bead containers receive the host `wrix-beads` dolt socket as a
      single-file bind mount at `/workspace/.wrix/dolt.sock` via
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
      pointing at the resolved wrix signing key,
      `commit.gpgsign=true`, and `gpg.ssh.allowedSignersFile`
      pointing at `<workspace>/.git/loom-allowed-signers`, when
      `$WRIX_SIGNING_KEY` or the
      `$HOME/.ssh/deploy_keys/<repo>-<host>-signing` fallback
      resolves
  [test](loom_init_writes_signing_gitconfig)
- `GitClient::create_worktree` writes **no** signing block into a
      bead clone's local `.git/config`, even when the signing key
      resolves: the clone is bind-mounted into the bead container,
      where a local block carrying the host key path would shadow
      wrix's `git-ssh-setup.sh` global config and break the
      worker's in-container `git commit`. In-container commits sign
      via that wrix global config; host-side clone commits fall
      through to the operator's global gitconfig
  [test](create_worktree_omits_signing_block_in_bead_clone)
- The fallback keyname is derived as `<repo>-<host>` where `<repo>`
      is parsed from the origin URL (`github.com[:/]<user>/<repo>`)
      and `<host>` is `hostname -s`, matching wrix's
      `setup-deploy-key` derivation rule
  [test](signing_key_fallback_uses_wrix_repo_host_derivation)
- The allowed_signers file at
      `<workspace>/.git/loom-allowed-signers` is derived via
      `ssh-keygen -y -f <signing-key>` at gitconfig-write time and
      contains the wrix signing identity
  [test](allowed_signers_derived_from_signing_key)
- Driver-side rebase in the loom workspace produces signed commits
      whose `gpgsig` header is present in the commit object, without
      prompting for a passphrase
  [test](driver_rebase_signs_with_wrix_key)
- `git log --show-signature` against a driver-rebased commit in the
      loom workspace prints `Good "git" signature` using the derived
      allowed_signers file
  [test](rebased_commits_verify_via_derived_allowed_signers)
- `$WRIX_SIGNING_KEY` set to a non-existent file aborts loom
      startup with a non-zero exit and an error naming the missing
      path
  [test](wrix_signing_key_missing_file_fails_loud)
- When neither `$WRIX_SIGNING_KEY` nor the
      `$HOME/.ssh/deploy_keys/<repo>-<host>-signing` fallback
      resolves, no signing block is written and the operator's
      global gitconfig governs signing in loom-materialized
      workspaces
  [test](no_wrix_keys_leaves_global_gitconfig_governing)
- When the signing key resolves, the per-bead integration step
      runs `git verify-commit` against the fetched commits
      (pass 1) and against the rebased commits (pass 2); pass-1
      failure routes the bead to `loom:blocked` with cause
      `signature-verification-failed` (worker-side) and pass-2
      failure routes to `loom:blocked` with the same cause but the
      detail naming "driver-side"
  [test](integration_step_verifies_signatures_in_two_passes)
- When the signing key does not resolve, signature verification is
      skipped at both passes and the integration step proceeds
  [test](signature_verification_skipped_when_no_key)
- `GitClient::launcher_key_env` surfaces each resolved key as a
      `WRIX_DEPLOY_KEY` / `WRIX_SIGNING_KEY` → HOST-path pair so
      loom can hand them to the `wrix spawn` launcher; an
      unresolved key is omitted rather than erroring
  [test](launcher_key_env_exposes_signing_key_host_path)
- Bead dispatch threads the resolved launcher keys onto
      `SpawnConfig.launcher_env` and keeps them out of the
      in-container `SpawnConfig.env` allowlist
  [test](launcher_env_threads_onto_spawn_config_not_container_env)
- `SpawnConfig.launcher_env` is `#[serde(skip)]`-excluded from the
      spawn-config JSON so host key paths never leak into the
      world-readable file the wrapper reads
  [test](launcher_env_is_never_serialized)
- Each backend applies `SpawnConfig.launcher_env` to the `wrix
      spawn` child process environment before exec
  [test](apply_launcher_env_sets_child_process_env)
- Backend-derived `WRIX_AGENT` is inserted into both
      `SpawnConfig.launcher_env` and the in-container
      `SpawnConfig.env` allowlist, overriding an absent or conflicting
      parent-shell value
  [test?](wrix_spawn_child_env_sets_backend_derived_wrix_agent)

### Workflow commands

- `loom plan [SPEC_LABEL ...]` spawns an interactive container with
      the base profile and runs the spec interview. Positional labels
      are optional initial anchors (existing specs are pinned; missing
      labels are proposed new specs). Options may appear before,
      between, or after labels. Plan edits spec/index markdown and
      implementation notes only — no bd writes and no touched-set
      manifest
  [test?](plan_accepts_optional_anchor_labels_and_interspersed_options)
- `loom todo` performs deterministic changed-spec preflight from
      durable spec-epic cursors (`loom.todo_cursor`), Git, and the
      current `docs/README.md` spec index before rendering any agent
      prompt. It never consults `loom:active`, a current-spec cache key,
      or the LLM to decide the changed-spec set
  [test?](todo_preflight_discovers_changed_specs_from_durable_cursors)
- `loom todo` ensures exactly one `loom:spec spec:<label>` spec epic
      per indexed spec. Missing spec epics are created and make the
      spec uninitialized/changed; duplicate spec epics block with
      conflicting IDs; missing cursor metadata on an existing spec
      epic blocks with an exact repair diagnostic
  [test?](todo_ensures_spec_epics_and_blocks_missing_existing_cursor)
- `loom todo` creates or reuses one `loom:todo` work epic for the
      preflight changed-spec set and requires a final `LOOM_TODO:`
      marker whose typed payload covers exactly that set. Generic
      `LOOM_COMPLETE` / `LOOM_NOOP`, missing rows, malformed JSON,
      nonexistent beads, beads outside the work epic, or extra/omitted
      specs fail validation
  [test?](todo_success_marker_must_cover_exact_changed_spec_set)
- Validated `LOOM_TODO` finalization is all-or-nothing across changed
      specs: every changed spec cursor advances to the preflight HEAD
      (including `NoWork` outcomes), `loom:todo` is removed from the
      work epic, `loom:active` is applied to it, and previous active
      state is cleared. Any failure leaves cursors and active state
      unchanged
  [test?](todo_finalization_advances_cursors_and_active_epic_atomically)
- Missing criterion evidence in `.loom/cache.db` produces typed
      `EvidenceState::Missing` rows in `criterion_status`; it is never
      treated as no criteria or no work. Malformed criteria block
      preflight
  [test?](todo_missing_criterion_evidence_is_missing_not_clean)
- `loom loop [OPTIONS] [BEAD_OR_EPIC_ID ...]` runs the sole
      `loom:active` work epic when no ids are provided. Positional ids
      may be task beads (run exactly that bead) or epics (run ready
      child work under that epic/molecule). Options may appear before,
      between, or after ids; `--spec`, `--once`, and `--all-specs` are
      not part of the loop surface
  [test?](loop_accepts_positional_work_roots_and_defaults_to_active_epic)
- `loom loop --parallel N` (alias `-p N`) accepts a positive integer; non-
      positive or non-integer values fail with a clear error
  [test](default_is_one)
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
      (`pub(crate)` constructor, no struct-literal path) and only from
      matching typed evidence: successful `VerifiedScope`, successful
      `ReviewedScope`, pre-push hook coverage for the actual push range,
      matching tree/config/range fingerprints, and existing gate logs
      containing successful `GateRun` end events; any condition failing
      returns `GateFail` instead
  [test](gate_success_constructor_requires_typed_scope_and_coverage_evidence)
- Every `loom loop` returning `LoopOutcome { gate: Success(r), .. }`
      references non-empty gate JSONL logs in `r.gate_log_paths`; each
      log contains a `gate_run_start` and matching successful
      `gate_run_end`, and the review evidence contains a terminal
      `AgentEvent` whose effective marker is complete. Holds for all
      execution modes (explicit bead roots, explicit epic roots,
      `--parallel`, and default active-epic continuous mode)
  [test?](every_successful_loom_loop_references_completed_gate_logs)
- `run_parallel_loop` returns `Result<LoopOutcome, LoopError>` —
      identical type to the sequential codepath; parallel mode invokes
      the same molecule push-gate chokepoint after the batch drains,
      constructs `GateOutcome` from typed gate evidence, and returns.
      There is no parallel-specific summary type
  [test](parallel_codepath_returns_loop_outcome_with_gate_field)
- `loom loop` reads profile from bead label and spawns correct container
  [test](resolve_profile_reads_label)
- `loom loop` retries failed beads with previous error context
  [test](default_policy_is_two_retries)
- Before molecule push verification, the driver verifies that
      `.loom/integration` has the canonical `wrix.prekHooks`
      `core.hooksPath` configured and fails loudly if the expected path
      cannot be resolved
  [test](push_gate_requires_integration_hooks_path_configured)
- On molecule completion, after stabilization has drained promoted
      remediation, `loom loop` fetches/rebases against
      `origin/<integration-branch>`, computes the actual push range
      `origin/<integration-branch>..HEAD`, runs the actual prek
      pre-push chain for that range, then runs
      `loom gate review --diff <actual-push-range>` only after
      deterministic success
  [test](molecule_push_gate_verifies_and_reviews_actual_push_range)
- After each per-bead agent run signals Success and the bead's branch
      is rebased onto the integration branch + ff'd at the loom
      workspace (inside `index.lock`), the loop invokes exactly
      `loom gate verify --diff <pre-integration-head>..HEAD`. The
      per-bead hot path never invokes focused LLM review or `mint`
  [test](exec_per_bead_gate_invokes_post_integration_verify_only)
- The molecule-completion handoff evidence is populated from typed
      `GateRun`, `VerifiedScope`, and `ReviewedScope` values parsed
      from actual gate JSONL logs. No trust field is left at default
      `None` when a child process produced a parseable run; absence
      surfaces as a `GateFail` variant per [Loop Outcome
      Types](#loop-outcome-types)
  [test?](handoff_evidence_populates_typed_gate_scope_values)
- When the molecule-completion audit review produces ≥1 unsuppressed
      streamed `LOOM_FINDING:` line and a `LOOM_CONCERN:` terminator,
      `route="deferred"` findings merge into the molecule's deferred
      remediation set and cause another stabilization pass within the
      molecule iteration cap; `route="clarify"` findings materialize
      one `loom:clarify` bead per finding hash. If every streamed
      finding is suppressed, the effective review marker is Complete and
      no recovery prompt is produced. Mint does NOT fire during the
      per-bead hot path; deferred findings are promoted by
      `loom gate mint -m <molecule-id>` during stabilization
  [test?](molecule_completion_review_routes_findings_to_stabilization_or_clarify)
- A molecule-completion review finding with `route="blocking"` refuses
      the push and creates or reuses same-molecule remediation work;
      already-integrated original beads are not reopened solely because
      the push-stage review found a concern
  [test?](molecule_review_blocking_finding_creates_same_molecule_remediation)
- A molecule-completion review finding with `route="deferred"` merges
      into a molecule child bead with `status=deferred` and label
      `loom:deferred`; `bd ready` does not return it until molecule
      stabilization promotes it
  [test?](molecule_review_deferred_finding_creates_deferred_bead)
- Structural bd conflicts while recording deferred or clarify findings
      route the molecule to `loom:blocked` with cause
      `gate-routing-structural-violation`; already-integrated commits
      are not unwound
  [test?](molecule_routes_gate_routing_structural_conflict_to_blocked)
- A synthetic post-integrate verify failure writes a durable gate log
      under `.loom/logs/gate/` containing command argv, resolved scope,
      per-lane hook/verifier results, exit code, stdout/stderr tails,
      integration SHA, bead id, retry attempt, rollback state, and log
      path
  [test](post_integrate_verify_failure_writes_durable_gate_log)
- The `driver_event` emitted for `post-integrate-fail` names the gate
      log path in its payload / rendered summary, and retry attempts
      produce distinct log paths while successful integration flow is
      unchanged
  [test](post_integrate_fail_driver_event_names_gate_log_path)
- Transient errors while recording deferred or clarify findings thread
      their detail into `PreviousFailure` and re-run through the
      existing per-bead recovery loop bounded by `[loop] max_retries`;
      after exhaustion the bead routes to `loom:blocked` with cause
      `retry-exhausted`
  [test](loop_per_bead_routes_gate_recording_errors_through_recovery_loop_bounded_by_max_retries)
- `loom loop`'s outer loop, after original non-deferred work drains,
      invokes `loom gate mint -m <molecule-id>` to promote deferred
      remediation beads, re-polls `bd ready`, and processes promoted
      remediation before the final push gate can succeed. The outer loop
      is bounded by `[loop] max_iterations` (default 10) and exits
      cleanly on push success, a fully-stuck molecule, or counter
      exhaustion
  [test?](continuous_outer_loop_promotes_deferred_remediation_then_exits_on_stall)
- Push gate is a typed-evidence AND: no unresolved blocked/clarify/
      deferred molecule state, successful deterministic pre-push
      `GateRun`/`VerifiedScope`, successful `ReviewedScope`, no
      terminal integrity finding, and marker coverage for the hooks the
      push will encounter. Failure on any input refuses the push. The
      verdict is encoded as `GateOutcome` (`Success` when all inputs
      hold, `Fail { reason }` otherwise); the constructor for
      `GateSuccess` asserts each condition structurally — see
      [Loop Outcome Types](#loop-outcome-types)
  [test?](push_gate_evaluates_typed_evidence_and_marker_coverage)
- On a **clean** push gate the `MarkerProof` is minted to
      `.loom/marker.json` **immediately before** `git push`, inside
      the gate's critical section, after deterministic pre-push and
      review have both covered the actual push range. A **refused**
      push (blocked/clarify/deferred bead, pre-push failure,
      verify-fail, review-concern, integrity finding, or missing
      marker coverage) mints nothing. A missing or invalid marker falls
      the pre-push consumer through to running hooks rather than failing
      the push by itself
  [test?](clean_push_mints_marker_after_covered_verify_and_review)
- Push gate refuses when `loom gate review`'s `--diff`-scoped
      invocation emits `LOOM_CONCERN`; molecule routes to recovery
      with cause `review-concern`
  [test](push_blocked_on_review_concern_with_id_payload)
- Push gate handles the integrity-gate findings that
      [gate.md § Integrity gate](gate.md#integrity-gate) defines as
      push-gate-terminal within the molecule's diff scope by
      **recovery-first then escalate**: while the molecule's
      iteration counter is below cap, the gate normalizes findings
      to typed `Finding`s and merges them into the molecule's deferred
      remediation set (per [gate.md § Findings and Minting](gate.md#findings-and-minting)).
      Findings coalesce by lead spec / concern family, the push is
      refused, the counter is incremented, `loom gate mint -m` promotes
      deferred remediation, and the outer loop re-enters so the worker
      can address the batch. On cap exhaustion, the gate
      falls back to the terminal escalation: `loom:clarify` on the
      molecule's epic with one composed auto-generated `## Options
      — …` block (kind-grouped resolutions per [gate.md § Integrity
      gate](gate.md#integrity-gate))
  [test](push_gate_recovers_integrity_findings_until_cap_then_clarifies)
- Push gate refuses on any verify-tier dispatch error (exit code
      2 = unknown verifier, command not found); dispatch errors
      count as fails, not skips
  [test](push_blocked_on_verify_dispatch_error)
- `loom loop` auto-iterates on remediation beads (up to max iterations)
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
- `loom todo --help` documents deterministic all-spec changed-spec
      preflight, the fail-loud guarantee for blocked/unregistered/stale
      specs, and that successful todo sets the active work epic only
      after every changed spec is represented
  [test?](loom_todo_help_documents_multispec_fail_loud_behavior)
- `loom loop --help` documents `[BEAD_OR_EPIC_ID ...]`, the default
      `loom:active` work epic, interspersed options, and absence of
      `--spec` / `--once` / `--all-specs`
  [test?](loom_loop_help_documents_work_roots_and_removed_selectors)
- Bare `loom msg` lists every outstanding open `loom:blocked` and
      `loom:clarify` bead across all specs (cross-spec default); no
      active-spec cache value is consulted, and closed beads are
      excluded even when labels remain
  [test?](msg_list_excludes_closed_blocked_or_clarify_beads)
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
      every outstanding clarify regardless of active work epic
  [test](loom_msg_chat_scope_filters_to_spec)
- `loom spec` queries spec annotations (`[check]` / `[test]` /
      `[system]` / `[judge]`) parsed via `loom-gate`'s annotation parser
  [test](list_for_label_reads_all_four_tiers)
- `loom spec <label> --deps` walks file-shaped `[test]`/`[judge]`
      targets and `[check]`/`[system]` command strings in the named
      spec, printing the required nixpkgs
  [test](deps_for_label_walks_file_targets_and_command_strings)
- `loom spec <label> --targets` prints one annotation per line as
      `[tier] target`; `--tier <tier>` narrows to that tier; `--plain`
      prints exact target strings without the `[tier] ` prefix
  [test?](spec_targets_lists_annotation_targets_with_tier_and_plain_modes)

### Verdict gate

- After every worker agent phase, the verdict-gate decision table
      classifies the terminal marker plus mechanical signals
      (bd-closed, diff, tree cleanliness) without an LLM call
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
      validates on a clarify-route finding's evidence. Forgetful-
      agent case (marker emitted, options block absent or malformed)
      falls back to `loom:blocked` with cause `clarify-without-options`
      — no stranded clarify bead reaches `loom msg`
  [test?](direct_emit_clarify_without_options_block_falls_back_to_blocked)
- Clarify downgrades emit `DriverKind::ClarifyDowngraded`, write a bd
      note breadcrumb with cause `clarify-without-options`, and pair the
      resulting bd label/status mutation with `DriverKind::BdStateTransition`
  [test?](clarify_downgrade_emits_driver_events_and_bd_breadcrumb)
- `LOOM_RETRY` agent marker → recovery with cause `agent-retry`,
      `previous_failure` populated with `AgentRetry { reason }` from
      the prose preceding the marker; one `[loop] max_retries` slot
      consumed
  [test?](retry_marker_routes_to_agent_retry_recovery_with_reason_carried)
- `LOOM_RETRY` recovery exhaustion → `loom:blocked` with cause
      `retry-exhausted` (the same exhaustion path as other
      driver-detected recoveries)
  [test?](retry_marker_exhaustion_routes_to_retry_exhausted_blocked)
- `LOOM_RETRY` from an interactive session (`plan`, `msg`) is a
      wrong-phase-marker error; the driver exits non-zero with a
      diagnostic and does not apply any label
  [test?](retry_marker_from_interactive_session_is_wrong_phase_error)
- `LOOM_CLARIFY` from a `loom todo` session targets the **`loom:todo`
      work epic** (rationale per
      [templates.md — Decomposition Discipline](templates.md));
      the agent's `## Options — …` block is persisted to the work
      epic's notes per [gate.md](gate.md)'s Options Format Contract
      before the label is applied
  [test?](todo_clarify_marks_work_epic)
- No marker emitted → recovery with cause `swallowed-marker`
  [test](missing_marker_routes_to_swallowed_marker_recovery)
- `LOOM_COMPLETE` + bead not bd-closed → recovery with cause
      `incomplete-signaling`
  [test](complete_without_bd_closed_routes_to_incomplete_signaling)
- `LOOM_COMPLETE` + closed + empty diff → recovery with cause
      `zero-progress`
  [test](complete_with_empty_diff_routes_to_zero_progress)
- `LOOM_NOOP` + closed + empty diff → accepted as intentional no-work
      output rather than zero-progress; no post-integration verify runs
      for an empty bead diff
  [test?](noop_with_empty_diff_is_done_not_zero_progress)
- `LOOM_COMPLETE` + closed + non-empty diff + dirty working tree
      (`git status --porcelain` non-empty) → recovery with cause
      `tree-not-clean`; post-integration verify is NOT run (recovery
      precedes it so verifiers don't execute against a half-staged
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
- Post-integration per-bead verify runs the project pre-commit lane and
      every affected `[check]` / `[test]` verifier for
      `<pre-integration-head>..HEAD`; `[system]` is excluded from the
      finite diff default; none of the eligible lanes short-circuit each
      other, and per-hook/verifier pass/fail + stderr is captured
  [test?](post_integration_verify_runs_project_precommit_and_affected_check_test)
- One or more `loom gate verify` failures → recovery with cause
      `verify-fail`; `previous_failure` carries every failure (not just
      the first), with a 4000-char budget split across them
  [test](verify_fail_carries_every_failure_block_for_previous_failure)
- Per-bead focused review does not run after post-integration verify;
      mechanical verify failure routes directly to `verify-fail` /
      `post-integrate-fail` with gate-log evidence, while molecule-
      completion review runs only after deterministic pre-push success
  [test](post_integrate_verify_failure_writes_durable_gate_log)
- Review's primary concern is live-path coverage: relevant
      `[check]` / `[test]` / `[system]` verifiers on the reviewed range
      must exercise the live path (same binary, same argv shape, same
      env). All-mock verifier sets raise a `LOOM_CONCERN`
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
- Production wiring obligation: the review-phase verdict-gate caller
      that constructs `GateInputs` must populate `streamed_findings`
      from the parsed walk output rather than relying on
      `..GateInputs::default()` (which leaves it empty).
      `classify_review_phase` at
      `crates/loom-workflow/src/review/production.rs` invokes
      `parse_walk_output` against the agent's combined stdout before
      constructing `GateInputs`. A well-formed `LOOM_CONCERN` with `≥1`
      streamed `LOOM_FINDING:` lines routes to
      `RecoveryCause::ReviewConcern { summary, findings }`, never
      collapses to `BadWalk::ConcernWithoutFindings` because the
      findings were left at default. The loop classifier
      (`neutral_gate_inputs` in `crates/loom-workflow/src/loop/production.rs`)
      is deliberately exempt: it passes an empty findings vec because
      worker phases have no findings stream, and `classify_session`
      rejects `LOOM_CONCERN`/`BadWalk` markers as review-phase-only
      before `decide` is reached, so populated findings could not affect
      routing — wiring it would
      instead risk mis-routing a loop-phase `LOOM_COMPLETE` to
      `FindingsWithoutConcern`
  [test](classify_review_phase_invokes_parse_walk_output_and_threads_findings_through_gate_inputs)
- Wire-format dead-code excision: no production code path
      constructs `ReviewError::ConcernWithoutBeadDeltas`; the variant
      is removed from `review/error.rs` and its raise site at
      `review/runner.rs` is deleted. Concern handling routes through
      `decide_concern` + `RecoveryCause::ReviewConcern` exclusively
  [test](no_path_constructs_concern_without_bead_deltas_in_production_harness_lane)
- Recovery iter < `[loop] max_iterations` (default 10) → promotes
      deferred remediation OR retries the bead with prior failure context
  [test](under_max_recovers_with_previous_failure)
- Every remediation bead created by the verdict gate is bonded to the
      originating bead's molecule via `bd mol bond` before becoming
      eligible for `loom loop` dispatch; the bond is atomic with bead
      creation (no transient orphan window)
  [test](spawned_outcome_bonds_to_origins_parent_molecule)
- If the originating bead is unbonded (no molecule), the verdict gate
      refuses to create remediation state and instead applies
      `loom:blocked` with cause `unbonded-origin` to surface the
      upstream inconsistency
  [test](refused_outcome_applies_unbonded_origin_blocked_to_origin)
- The push gate walks `bd mol progress <id>` and refuses to push when
      any bead in the molecule — including bonded remediation beads —
      carries `loom:blocked`, `loom:clarify`, or `loom:deferred`; an
      orphan remediation bead would slip past this check, so the bond
      invariant is what makes the gate sound
  [test?](remediation_beads_under_cap_auto_iterate)
- Recovery iter ≥ max_iterations → applies `loom:blocked` with cause
      in `bd update --notes`
  [test](at_or_above_max_applies_blocked_with_retry_exhausted_cause)
- Iteration count is **work-epic-level** state (cached in
      `work_epics.iteration_count`, not on individual beads) and
      survives `retry → [running]` round-trips; every promoted
      remediation pass consumes one slot of `[loop] max_iterations`
  [test?](iteration_counter_round_trips_through_cache_db)
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
- The push gate refuses to push while any bead in the molecule carries
      `loom:blocked` or `loom:clarify`
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
      set) and `.loom/cache.db` with the default cache schema
  [test?](init_creates_config_and_cache_db)
- `loom init --rebuild` drops and repopulates `.loom/cache.db` from
      durable sources: the spec index, `specs/*.md`, bd spec/work
      epics, and each spec's `## Companions` section. It also folds
      gate criterion-status storage into the unified cache; there is no
      `.loom/gate-cache.sqlite`
  [test?](rebuild_drops_and_repopulates_cache_db)
- `loom status` prints the active work epic, any pending `loom:todo`
      work epic, cached iteration counts, and cache health; no active
      spec/current-spec value is displayed or read
  [test?](status_reports_active_work_epic_not_current_spec)
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

### Cache database

- `CacheDb::open` creates `.loom/cache.db` tables on first open
      (`specs`, `spec_epics`, `work_epics`, `companions`, `notes`,
      `criterion_status`, and `meta`)
  [test?](cache_db_init_creates_tables)
- `CacheDb::rebuild` populates `specs` from `docs/README.md`'s spec
      index and cross-checks `specs/*.md`; unindexed spec files,
      missing indexed files, duplicate labels, and label/path mismatches
      fail loud
  [test?](cache_rebuild_cross_checks_spec_index_and_files)
- `CacheDb::rebuild` mirrors exactly one `loom:spec spec:<label>` spec
      epic per indexed spec, regardless of epic status; duplicates fail
      with conflicting IDs
  [test?](cache_rebuild_requires_one_spec_epic_per_indexed_spec)
- `loom todo` creates a missing spec epic during preflight, treats the
      spec as uninitialized/changed, and blocks when an existing spec
      epic lacks `loom.todo_cursor` metadata
  [test?](todo_missing_spec_epic_initializes_existing_missing_cursor_blocks)
- `loom todo` rejects malformed, missing, non-ancestor, or unknown
      `loom.todo_cursor` SHAs with diagnostics that name the spec epic
      and repair surface
  [test?](todo_invalid_spec_cursor_blocks_loudly)
- `loom todo` discovers changed specs by comparing each spec/index row
      at `HEAD` against the spec epic's durable cursor; it includes
      inactive/stale specs and brand-new indexed specs regardless of
      `loom:active`
  [test?](todo_discovers_active_inactive_and_new_specs_from_cursors)
- `loom todo` creates one `loom:todo` work epic before rendering the
      agent prompt, records `loom.todo_head`, `loom.todo_fingerprint`,
      and changed spec labels on it, and does not add `loom:active`
      until validation succeeds
  [test?](todo_creates_pending_work_epic_before_agent_prompt)
- A pre-existing open `loom:todo` work epic with matching head and
      `TodoFingerprint` is reused; multiple matches or non-matching
      pending work epics block with an Options-format diagnostic
  [test?](todo_reuses_matching_pending_work_epic_else_blocks)
- `loom-protocol::todo::parse_todo_success` accepts exactly
      `LOOM_TODO: <json>` final lines and returns typed `TodoSuccess`;
      malformed JSON, missing fields, empty `Decomposed.beads`, empty
      `NoWork.reason`, or wrong prefix fail parse
  [test?](todo_success_marker_parses_to_typed_protocol)
- `loom todo` validates `TodoSuccess.head`, `TodoFingerprint`, work
      epic id, exact changed-spec coverage, bead existence, and bead
      parentage under the work epic before finalization
  [test?](todo_success_validation_rejects_missing_extra_or_misparented_beads)
- Validated `NoWork` outcomes advance the spec cursor just like
      `Decomposed` outcomes; no-work rows require a non-empty reason
  [test?](todo_no_work_outcome_advances_cursor_with_reason)
- Failed todo validation leaves the work epic labelled `loom:todo`,
      writes diagnostics to it, advances no spec cursor, and does not
      change `loom:active`
  [test?](todo_validation_failure_leaves_pending_without_advancing)
- Validated or blocked `loom todo` output prints a driver-authored
      per-spec summary covering every changed spec and its outcome; a
      changed spec missing from the summary is a validation failure
  [test?](todo_output_summarizes_every_changed_spec_outcome)
- Validated todo finalization removes `loom:todo`, applies the sole
      `loom:active` label to the work epic, clears any previous active
      epic, and advances every changed spec epic's `loom.todo_cursor`
      to the preflight HEAD all-or-nothing
  [test?](todo_finalization_sets_active_and_advances_all_cursors)
- `criterion_status` cache rows join to current criteria by typed
      `(SpecLabel, CriterionId)`; stale annotation evidence renders as
      `EvidenceState::StaleAnnotation`, absent rows as
      `EvidenceState::Missing`
  [test?](criterion_status_joins_by_typed_criterion_id)
- `CacheDb::rebuild` parses each spec's `## Companions` section and
      writes one `companions` row per listed path; specs without the
      section contribute zero rows (not an error)
  [test?](cache_db_rebuild_companions)
- `CacheDb::rebuild` resets work-epic iteration counters to 0
  [test?](cache_rebuild_resets_work_epic_counters)
- Corrupted cache file → `loom init --rebuild` recovers from durable
      sources or reports the exact durable inconsistency; it never
      treats cache loss as clean todo state
  [test?](cache_corruption_recovery_never_implies_clean_todo)
- `loom plan [labels...]` does NOT create epics and does NOT write to
      bd; plan sessions edit specs/index/notes only
  [test?](plan_does_not_create_epic_or_touch_bd)
- `loom plan [labels...]` reads existing implementation notes for
      anchor/touched specs and writes back merged arrays via
      `loom note set` (interview-driven keep/drop/add — not blind
      append, not blind replace)
  [judge](../tests/judges/loom.sh#judge_plan_update_merges_notes)
- `loom todo` renders implementation notes for each changed spec into
      the relevant work beads and deletes those notes only after the
      spec cursor advances during validated finalization
  [test?](todo_consumes_notes_only_after_validated_finalization)
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

### Compaction recovery

- At session start, `.loom/scratch/<key>/` contains
      `prompt.txt`, `scratch.md`, `repin.sh` for every phase command
      (plan, todo, loop, gate, msg)
  [test](open_creates_layout_and_drop_removes_it)
- `<key>` is the joined anchor-label set (or `plan`) for `loom plan`,
      the work epic id for `loom todo`, and the bead id for loop/gate/msg
      worker sessions
  [test?](resolve_scratch_key_uses_plan_anchors_work_epic_or_bead)
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
      with a shared cargoArtifacts cache

## Requirements

### Functional

1. **Command set** — commands fall into three groups that MUST be
   rendered as separate sections under those headings in
   `loom --help` output (in this order). Order within each group is
   as listed.

   **Workflow** — the loom loop, in execution order:
   - `loom plan [SPEC_LABEL ...]` — spec interview (interactive agent
     session). Positional labels are optional initial anchors, not the
     touched set: zero labels starts from the overview/index; existing
     labels pin those spec bodies; missing labels are proposed new
     specs. Options may appear before, between, or after labels. Plan
     sessions edit specs/index/notes only — they do **not** create
     epics or write to bd.
   - `loom todo` — deterministic spec-to-beads decomposition. It
     discovers every changed spec from spec epics' durable
     `loom.todo_cursor` metadata, Git, and the current `docs/README.md`
     spec index — never from `loom:active`, current-spec cache state, or
     the LLM.
     It creates/ensures spec epics, creates or reuses one `loom:todo`
     work epic, renders the changed-spec roster to the todo agent, and
     accepts success only via a validated `LOOM_TODO:` payload covering
     exactly that roster. Finalization removes `loom:todo`, applies
     `loom:active`, and advances every changed spec cursor to the
     preflight HEAD all-or-nothing.
   - `loom loop [OPTIONS] [BEAD_OR_EPIC_ID ...]` — execute work. With
     no ids, runs the sole `loom:active` work epic. With ids, each
     positional may be a task bead (run exactly that bead) or an epic
     (run ready child work under that epic/molecule). Options may
     appear before, between, or after ids. The loop pulls ready child
     beads filtered to exclude `loom:blocked` / `loom:clarify` beads;
     an epic positional is a work root, never a worker task itself.
     Under `--parallel N`, a clarify or block on one of the N
     concurrent beads does not cancel the others. On work-epic
     completion, the driver fetches/rebases against
     `origin/<integration-branch>`, verifies the actual push range via
     the prek pre-push chain, runs `loom gate review --diff
     <actual-push-range>`, then evaluates the push gate per FR9. The
     outer loop iterates over work-epic passes (initial pass + each
     promoted remediation pass) bounded by `[loop] max_iterations`.
     **`loom loop` returns a typed [`LoopOutcome`](#loop-outcome-types)
     whose `gate: GateOutcome` field is non-optional; the binary's exit
     code is a pure function of the `GateOutcome` variant.**
   - `loom gate` — quality gate (annotation-dispatched verifiers +
     LLM rubric). Subcommands per [gate.md](gate.md)
     Commands table: bare `loom gate` prints subcommand help;
     `loom gate status` reads the status cache for an explicit scope;
     `loom gate audit` runs verify then review for explicit `--diff` or
     `--tree`; `loom gate verify` runs scope-derived deterministic
     lanes; per-tier subcommands (`loom gate check`, `loom gate test`,
     `loom gate system`) run one tier in isolation; `loom gate review`
     runs the LLM rubric; `loom gate judge` / `loom gate rubric` run one
     lane each. Inspection subcommands require explicit `--files`,
     `--diff`, `--tree`, or exact `--target` where valid; no gate
     subcommand accepts `--spec` or a positional selector. `--bead` is
     review context only and must be paired with `--diff`; deterministic
     trust paths use explicit diffs. `mint` accepts only
     `-m/--molecule <id>` or `--tree` and has no bare default. The
     surface-conformance walk (FR13) ships as a `[check]`-tier verifier
     dispatched by `loom gate check`.
   - `loom msg` — clarify resolution

   **Inspection** — read-only views over cache, bd state, and logs:
   - `loom status` — print the active work epic, any pending
     `loom:todo` work epic, cached iteration counts, and cache health;
     it does not report or depend on an active-spec pointer
   - `loom logs` — pretty-render a bead's JSONL log under
     `.loom/logs/` via the same `AgentEvent` renderer used by
     `loom loop`. Full flag set in [Logs UX](#logs-ux).
   - `loom spec` — query spec annotations; supports `--deps` to print
     nixpkgs required by the spec's `[check]` / `[test]` / `[system]`
     / `[judge]` verifier targets, and `loom spec <label> --targets`
     to print annotation targets (`--tier <tier>` narrows; `--plain`
     prints exact target strings for piping)

   **State** — workspace lifecycle and cached state:
   - `loom init` — create `.loom/` config + `.loom/cache.db`.
     `--rebuild` drops and repopulates the cache from the spec index,
     spec files, bd spec/work epics, and each spec's `## Companions`
     section. The cache is non-authoritative; hot correctness paths
     re-read durable Git/Beads inputs.
   - `loom use <label>` — legacy active-spec selector retained for
     compatibility; deterministic `loom todo` does not read it, and
     `loom loop` defaults from `loom:active` work-epic state instead.
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
3. **SQLite cache store** — workflow cache persisted in
   `.loom/cache.db` (renamed from `.loom/state.db`). Tracks indexed
   spec rows, spec/work epic mirrors, criterion evidence cache,
   companions, iteration counters, and implementation notes. It is
   reconstructable or disposable: correctness-sensitive decisions use
   Git + Beads/Dolt metadata + current spec files/index. There is no
   `current_spec` pointer. `loom:active` is a bd label on the default
   work epic for `loom loop`, not cache state and not a todo-discovery
   input.
4. **Beads integration** — interacts with beads via the `bd` CLI (subprocess
   calls). Bead operations: create, show, close, update, list, dep add, mol
   bond, mol progress. CLI output parsed into typed Rust structs.
5. **Profile/runtime selection** — reads `profile:X` labels from beads,
   resolves the phase backend to an `AgentRuntime`, and resolves the pair
   via the [Profile-Image Manifest](#profile-image-manifest). Unknown
   labels or missing runtime variants fail at dispatch (no silent default).
   `--profile` overrides bead labels.
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
8. **Verdict gate per phase** — worker sessions are classified by the
   verdict-gate decision table after the agent emits its terminal marker.
   For implementation beads, the driver then runs deterministic
   post-integration verification (`loom gate verify --diff
   <pre-integration-head>..HEAD`) before the bead's integration is
   durable. LLM review is not part of the default per-bead hot path;
   the worker prompt requires self-review, and authoritative LLM review
   runs at molecule completion over the actual push range. See
   [Verdict Gate](#verdict-gate) for the execution layer (decision table,
   recovery mechanics, markers, labels) and [gate.md](gate.md) for the
   review rubric. Driver-detected gate failures and `LOOM_RETRY` self-
   reports enter a bounded recovery loop; agent self-reports
   `LOOM_BLOCKED` / `LOOM_CLARIFY` escalate directly to the human via
   `loom msg`. The verdict gate applies to **worker sessions only**
   (`loop`, `todo`, `review`); interactive sessions (`plan`, `msg`)
   are agent-and-human authoritative — the driver does not mutate bd
   state as a consequence of an interactive session. See [Verdict Gate §
   Interactive vs worker sessions](#verdict-gate) for the full
   no-reconciliation contract.
9. **Push gate — typed-evidence AND, structurally enforced.** Push
   fires only when every required input below holds; failure on any one
   refuses push. The driver computes each input explicitly — no implicit
   short-circuit, no `&&` chaining that could mask a failure. The push
   verdict is encoded in the typed [`GateOutcome`](#loop-outcome-types)
   variant: `Success(GateSuccess)` when all inputs hold,
   `Fail(GateFail { reason, .. })` on any failure. **`GateSuccess` is
   constructible only inside the gate-invocation code and only when
   every condition below is satisfied** — the constructor
   (`pub(crate)`, no struct-literal path) asserts each condition before
   returning. There is no code path that yields `GateSuccess` without
   the gate actually firing clean. Combined with FR1's worker-queue
   filter (epics never reach worker dispatch) and the existing close-
   on-Clean walk, this composes "epic close is reachable only via a
   `GateSuccess`" as a structural invariant.

   1. **Molecule state.** Every bead in the molecule has reached
      `[done]` — no `loom:blocked`, `loom:clarify`, or `loom:deferred`
      outstanding.
   2. **Origin-synchronized push range.** The driver has fetched origin,
      rebased local integration commits if needed, and resolved the
      actual range `origin/<integration-branch>..HEAD` that `git push`
      will update.
   3. **Deterministic pre-push evidence.** The actual prek pre-push
      chain has passed for that range. The resulting `GateRun` /
      `VerifiedScope` includes the project pre-push hooks, the nested
      `loom gate verify --diff <range>` hook, and affected `[check]` /
      `[test]` annotations per [gate.md](gate.md). Dispatch errors
      count as fails, not skips.
   4. **Review evidence.** `loom gate review --diff <actual-push-range>`
      has produced a successful `ReviewedScope` for the same resolved
      range/tree as the deterministic evidence. Any non-complete marker
      refuses the push and routes per marker semantics.
   5. **Integrity findings.** Zero push-gate-terminal integrity findings
      across the actual push range, where the finding set is defined by
      [gate.md § Integrity gate](gate.md#integrity-gate). Integrity
      findings are recoverable within the molecule's iteration cap:
      while below cap, the verdict gate normalizes findings to typed
      `Finding`s, merges them into deferred remediation batches,
      refuses the push, increments the counter, promotes them with
      `loom gate mint -m`, and re-enters the loop. On cap exhaustion,
      the gate falls back to terminal escalation — `loom:clarify` on
      the molecule's epic with the integrity gate's auto-generated
      `## Options — …` block.
   6. **Marker coverage.** The marker to be minted can prove same clean
      tree, same `.pre-commit-config.yaml` digest, same resolved push
      range, pre-push hook coverage, and matching `VerifiedScope` /
      `ReviewedScope` so the subsequent pre-push wrapper may
      short-circuit only covered hooks.

   **Production wiring requirement.** The push-gate verdict MUST
   consume typed `GateRun` / `VerifiedScope` / `ReviewedScope` evidence,
   marker coverage, and integrity findings — not just bead labels or
   scalar exit codes. Any path that pushes without evaluating all inputs
   is a bug.

   Per FR1, auto-iteration on promoted deferred remediation beads is
   owned by `loom loop`'s outer loop, bounded by `[loop]
   max_iterations`; this requirement is the molecule-final condition
   the outer loop drives toward, not a separate iteration mechanism.

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
    `wrix-beads` Dolt server bind-mounted at
    `/workspace/.wrix/dolt.sock` via `SpawnConfig.mounts`
    (see [Bead Dispatch](#bead-dispatch)); in-container `bd` writes
    go straight to the authoritative state. No per-bead `bd dolt
    push/pull` handoff. Loom on the host reads the same state
    through the same socket. The legacy `.beads/issues.jsonl` path
    is not used — beads no longer supports it.
11. **Spec label parsing** — workflow commands that accept spec labels
    parse them into `SpecLabel` values at the CLI boundary. No command
    falls back to a `current_spec` cache key: `loom plan` labels are
    optional anchors, `loom todo` discovers specs from durable cursors,
    and `loom loop` executes work roots. `loom gate` is not a
    spec-scoped surface; gate affectedness comes from work scopes and
    target discovery uses `loom spec <label> --targets`.
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
18. **Decomposition-phase wiring.** `loom todo` runs deterministic
    changed-spec preflight before rendering the prompt, creates or
    reuses the `loom:todo` work epic, and surfaces a per-criterion
    `CriterionStatus` row (shape owned by [templates.md](templates.md))
    for every changed spec. Criterion evidence is read from the unified
    `.loom/cache.db`; empty cache surfaces as `EvidenceState::Missing`
    rows — staleness is exposed, not papered over. The todo agent's
    only success terminal is `LOOM_TODO: <json>`, parsed by
    `loom-protocol::todo` and validated against the preflight roster.
    `LOOM_CLARIFY` from a todo session targets the `loom:todo` work
    epic because the child beads under negotiation may not yet exist.

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
   `AgentRuntime` is an enum (`Pi`, `Claude`, `Direct`), not a newtype.
3. **Nix integration** — built via the wrix Rust package builder.
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
