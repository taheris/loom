# Loom Harness

Rust workspace, build system, workspace lint config, process architecture,
state store, and command-set platform for the Loom agent driver.

## Problem Statement

Loom is a Rust binary that owns a complete spec-driven workflow:
spec interview (plan), spec-to-beads decomposition (todo), per-bead
agent dispatch (run), deterministic and LLM-judged review (check +
review), and human clarification (msg). The binary holds the
workflow's state in typed domain objects, parses agent protocols
against typed schemas, and renders templates with compile-time
variable validation.

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

### Worktree Dispatch

`loom loop --parallel N`:

1. Pull up to N ready beads (`bd ready --limit=N`).
2. For each bead, create a per-bead workspace at
   `.wrapix/worktree/<label>/<bead-id>/` via `git clone --local
   <main-workdir> <bead-workdir>`, then `git -C <bead-workdir>
   checkout -b loom/<label>/<bead-id>`. The clone's `.git/` is a
   regular directory inside the workspace (not a worktree pointer
   file), so the wrapix container's bind-mount of `<bead-workdir>` as
   `/workspace` carries a self-contained git repository — workers
   inside the container can commit without external `.git/`-mount
   wiring.
3. Spawn one `wrapix spawn --spawn-config <file> --stdio` per workspace
   (concurrently when `N > 1`). Each container's workdir bind mount
   points at the per-bead workspace, not the main checkout.
4. Await all bead futures and collect per-bead results.
5. **Sequentially**, push each successful bead branch from its clone
   back to the main repo (`git -C <bead-workdir> push origin
   loom/<label>/<bead-id>`), then merge it into the driver branch
   (single-threaded merge avoids index lock contention). On merge
   conflict, the bead is marked failed and the workspace is preserved
   for inspection.
6. On agent failure, the bead's workspace is removed (the bead branch
   never reached the main repo, so there is no branch ref to delete)
   and the bead is retried per the retry policy.

**Universal isolation.** The main checkout is never the workdir for a
bead's session — commits land in the driver branch only via the
sequential merge-back step (step 5). The bead's worktree starts clean
at HEAD by construction (step 2), so the verdict gate's tree-clean
check below can trust that any post-bead working-tree dirt is
agent-caused, and a developer's concurrent edits in the main checkout
cannot be mis-attributed to the agent. `--parallel 1` (default) runs
one bead at a time; `--parallel N > 1` runs N concurrently. Both modes
use the same worktree-per-bead dispatch.

**Git operations: hybrid `gix` + `git` CLI.** Worktree, branch, status,
and merge operations go through a typed `GitClient` in `loom-driver`. The
implementation is hybrid, encapsulated inside the module; callers see only
typed Rust methods.

| Operation | Backend | Reason |
|-----------|---------|--------|
| `status` (working tree vs HEAD) | [`gix`](https://docs.rs/gix) `Repository::status()` | mature (`crate-status.md`: checked) |
| `diff` (HEAD vs HEAD~) | `gix::diff` (`blob-diff` feature) | mature |
| List refs / branches | `gix::Repository::references()` | mature |
| Read commit graph / HEAD | `gix` | mature |
| List worktrees | `gix::Repository::worktrees()` | mature (open/iter only) |
| **Create per-bead workspace + branch** | `git clone --local` then `git checkout -b` (CLI) | self-contained `.git/` inside the workspace; workers in the wrapix container can resolve gitdir without an extra `.git/`-mount. `gix-clone` is unchecked in `crate-status.md` |
| **Push bead branch to origin** | `git push origin <branch>` (CLI) | run from the bead's clone after a successful agent session so `merge_branch` can fold the bead's work into the driver branch |
| **Remove per-bead workspace** | `std::fs::remove_dir_all` | a clone is a standalone tree (not a registered worktree), so cleanup is recursive directory removal — no `git worktree remove`/`prune` |
| **Merge bead branch back** | `git merge` (CLI) | `gix-merge` writes a merged tree but cannot persist `MERGE_HEAD`/`MERGE_MSG` (unchecked); avoids reimplementing the index dance |

`gix` 0.83+ is pinned with features `["status", "blob-diff", "revision",
"parallel", "sha1"]` (the `sha1` feature is required for gix-hash to compile;
without it the `Kind` enum has no variants). For tokio integration:
`gix::Repository` is `!Sync`; loom holds a
`ThreadSafeRepository` and clones a thread-local handle inside
`spawn_blocking` per call. CLI shell-outs use `tokio::process::Command`
with arguments passed via `.arg()` — never shell interpolation — and a
60-second timeout matching `BdClient`.

The hybrid line is reviewed each loom release; gix operations migrate
inward as the corresponding `crate-status.md` items become checked.

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

`loom plan` is interactive, so it shells out to `wrapix run` (TTY-attached)
rather than `wrapix spawn`. To keep one resolution path, plan looks up its
profile (per [Configuration](#configuration); default `base`) in the
manifest and exports `WRAPIX_DEFAULT_IMAGE_REF=<entry.ref>` plus
`WRAPIX_DEFAULT_IMAGE_SOURCE=<entry.source>` into the child environment
before exec'ing `wrapix run`. The launcher reads those env vars when no
`--spawn-config` is supplied — see
[sandbox.md — Launcher Subcommands](sandbox.md#launcher-subcommands).

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

Lock files **must not** live inside the workspace, because the bead
container has the workspace bind-mounted read-write and could `rm` the
lock file out from under the host driver, silently breaking mutual
exclusion. Putting locks under `$XDG_STATE_HOME` keeps them on the host
filesystem only.

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
invocations on *different* specs share the driver git branch. They will
collide briefly at merge-back and push:

- Concurrent `git merge` is serialized by git's own `index.lock`; the
  losing process surfaces a clear error and retries.
- Concurrent `git push` from `loom gate verify` produces non-fast-forward
  on the second push; the gate's push gate re-fetches and retries.

These are accepted, recoverable failure modes — not silent corruption —
which is why a workspace-wide lock is *not* required for `run` / `gate
verify`.

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
.wrapix/loom/logs/<spec-label>/<bead-id>-<utc-timestamp>.jsonl
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
`.wrapix/loom/logs/` prints a one-line message
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

Driver-detected failures enter a bounded recovery loop; agent
self-reports go straight to human resolution via `loom msg`.

**Decision table.** The gate inspects five signals — the agent's exit marker,
whether the bead was bd-closed, whether the worktree diff is empty, whether
the working tree is clean (`git status --porcelain` empty), and the review
verdict — and produces one of four outcomes (`done`, `blocked`, `clarify`,
or `recovery` with a cause):

| Marker | bd-closed | Diff | Tree clean | Review | Outcome |
|--------|-----------|------|------------|--------|---------|
| `LOOM_BLOCKED` | — | — | — | — | `blocked` |
| `LOOM_CLARIFY` | — | — | — | — | `clarify` |
| (none) | — | — | — | — | recovery (`swallowed-marker` OR `observer-abort`; see below) |
| `LOOM_COMPLETE` | no | — | — | — | recovery (`incomplete-signaling`) |
| `LOOM_COMPLETE` | yes | empty | — | — | recovery (`zero-progress`) |
| `LOOM_COMPLETE` | yes | non-empty | no | — | recovery (`tree-not-clean`) |
| `LOOM_COMPLETE` | yes | non-empty | yes | verify-fail (review may also raise a concern) | recovery (`verify-fail`; review notes appended if any) |
| `LOOM_COMPLETE` | yes | non-empty | yes | verify-pass + review-concern | recovery (`review-concern`) |
| `LOOM_COMPLETE` | yes | non-empty | yes | verify-pass + review-pass | `done` |
| `LOOM_NOOP` | yes | * | no | — | recovery (`tree-not-clean`) |
| `LOOM_NOOP` | yes | * | yes | verify-fail (review may also raise a concern) | recovery (`verify-fail`; review notes appended if any) |
| `LOOM_NOOP` | yes | * | yes | verify-pass + review-concern | recovery (`review-concern`) |
| `LOOM_NOOP` | yes | * | yes | verify-pass + review-pass | `done` |

In the table above, `—` means the signal isn't inspected because an
earlier signal already determined the outcome (e.g. an agent self-report
short-circuits before review runs); `*` means any value is accepted.

**Tree-clean check.** After the agent emits `LOOM_COMPLETE` or
`LOOM_NOOP` and the bead is bd-closed, the driver runs `git status
--porcelain` against the bead's worktree. The bead's worktree was
created fresh at dispatch (see *Worktree Dispatch* above), so its
starting state is necessarily empty — any non-empty porcelain output
post-bead is unambiguously agent-caused, with no risk of misattributing
a developer's concurrent edits in the main checkout. A non-empty
result — tracked files modified-but-not-staged, staged-but-not-
committed, or untracked files outside the ignore set — routes to
`recovery` with cause `tree-not-clean`, taking precedence over the
verify / review steps (running verifiers against a half-staged tree
would conflate the agent's intended diff with its leftover scratch).
The recovery detail names the dirty paths so the next iteration's
agent can stage them into a follow-up commit or revert them. This
catches the failure mode where a worker runs a formatter (or other
tree-touching tool), stages only its own paths, and leaves unrelated
tracked files dirty — pre-commit hooks structurally cannot see this
because their stash/restore architecture hides unstaged changes (see
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
*observable* — the agent invokes `bd close <id>` itself per the run-phase
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
with cause `review-concern`; the detail carries the concern token
emitted in the marker payload (see the per-diff rubric table in
[gate.md](gate.md) for the full set of concern tokens).
Invariant-clash concerns raise `loom:clarify` instead of entering
the recovery loop.

**Self-reports skip recovery.** `LOOM_BLOCKED` and `LOOM_CLARIFY` are agent
self-reports — re-running the same prompt won't recover, so the gate exits
straight to `[blocked]` / `[clarify]` for human resolution.

**Driver-detected causes flow through recovery.** Swallowed marker,
incomplete signaling, zero-progress, tree-not-clean, verify-fail, and
review-concern all enter the recovery loop. Each recovery iteration
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
| `review-concern` | `ReviewConcern { concern: ReviewConcernKind, reason }` | The review LLM's verbatim concern reasoning emitted in the `LOOM_CONCERN` marker payload. `ReviewConcernKind` is a typed enum with `Other(String)` fallback; concrete variants per [gate.md](gate.md) (`SpecCoherence`, `OrphanIntegration`, `VerifierBypass`, `FabricatedResult`, `WeakAssertion`, `CoincidentalPass`, `MockDiscipline`, `VerifierTooNarrow`, `ConcurrencyUntested`, `ScopeCreep`, `ScopeShortfall`, `JudgeFlag`). The in-code recovery cause is `RecoveryCause::ReviewConcern`. |

When `previous_failure.is_some() && attempt > 0`, the `run.md`
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
  (b) driver-detected gate failure with recovery exhausted. Both meanings are
  uniform from the human's perspective — the bead is blocked and `loom msg`
  is the resolution channel.
- `loom:clarify` is applied only by the `LOOM_CLARIFY` agent marker — the
  agent has a specific question with structured options for the human.
- The cause of a driver-applied `loom:blocked` (`swallowed-marker`,
  `incomplete-signaling`, `zero-progress`, `tree-not-clean`, `verify-fail`,
  `review-concern`, `observer-abort`, `retry-exhausted`) is preserved in
  the bead's notes. Per-cause sub-labels can be stacked on top later if
  filtering becomes important; the gate's terminal label stays
  `loom:blocked`.

**Marker definitions.** The agent ends every phase by emitting exactly
**one** marker on its own line, as the final output of the session.
Markers are **mutually exclusive** — a session emits one and only one.
Five markers are defined:

- `LOOM_COMPLETE` — the work succeeded. The agent has implemented the
  bead's criteria and `bd close`d the bead. The diff is non-empty
  (real changes); see `LOOM_NOOP` below for the zero-diff variant.
  Valid in every phase.
- `LOOM_NOOP` — the work was already done in tree; the phase
  intentionally produced an empty diff. Without `LOOM_NOOP`, an empty
  diff is treated as `zero-progress` (a recovery cause). The agent
  emits `LOOM_NOOP` to distinguish "no work needed" from "work
  attempted but produced no diff." Valid in worker phases (`run`,
  `todo`); not valid in the review phase.
- `LOOM_BLOCKED` — the agent cannot proceed and is self-reporting,
  *without* a structured set of options for the human. Write the
  reason on prior lines before the marker; the gate applies
  `loom:blocked` to *this bead* and exits the verdict evaluation
  without entering recovery. Other beads in the molecule continue
  running; the labelled bead waits for human resolution via
  `loom msg`. Valid in every phase except `msg` (msg is itself the
  resolution channel).
- `LOOM_CLARIFY` — the agent has a specific question with structured
  options for the human (per the [Options Format
  Contract](gate.md#options-format-contract)). Write the
  question / option block to bead state before the marker; the gate
  applies `loom:clarify` to *this bead* and exits the verdict
  evaluation without entering recovery. Other beads in the molecule
  continue running; the labelled bead waits for `loom msg`
  resolution. Valid in every phase except `msg`.
- `LOOM_CONCERN` — the review phase found a quality issue with the
  molecule's work; push must not fire. Carries a structured payload:
  `LOOM_CONCERN: <concern-token> -- <one-sentence reasoning>`.
  Concern tokens are enumerated in the rubric-checks table in
  [gate.md § Per-diff stage checks](gate.md#per-diff-stage-checks)
  (e.g. `verifier-bypass`, `fabricated-result`, `weak-assertion`,
  `coincidental-pass`, `spec-coherence-fail`, `style-rule-violation`,
  `mock-discipline`, `judge-flag`). The gate routes `LOOM_CONCERN` to
  `Recovery { cause: ReviewConcern }`; the molecule re-enters the
  loop with `previous_failure` carrying the typed concern.
  **Review-phase-only** — emitting `LOOM_CONCERN` from any other
  phase is a `wrong-phase-marker` error in the verdict gate.

**Choosing a marker in the review phase.** Four markers are valid:

- `LOOM_COMPLETE` — clean review, no concerns.
- `LOOM_CONCERN: <token> -- <reason>` — review found a quality issue;
  push refused, molecule re-enters recovery.
- `LOOM_BLOCKED` — review *itself* cannot run (logs corrupt, can't
  access worktree, missing prerequisite). Distinct from `LOOM_CONCERN`:
  blocked means "I couldn't review"; concern means "I reviewed and
  found a problem."
- `LOOM_CLARIFY` — rare: review surfaces a spec ambiguity that
  requires human resolution before the verdict can be rendered.

The four are mutually exclusive — exactly one per session. The
common case is `LOOM_COMPLETE` xor `LOOM_CONCERN`. Multiple
concerns → the agent picks the strongest one for the `LOOM_CONCERN`
marker; the rest go in the prose body (visible via `loom logs` and
captured into `previous_failure` for recovery).

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
constructs `GateFail` instead. The `pub(crate)` constructor lives
in the same module as `exec_review` — outside the workflow crate,
`GateSuccess` cannot be minted.

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
    pub(crate) fn new(...) -> Result<Self, GateFail> { ... }
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
    VerifierFailed,                   // verify_exit != 0
    ReviewConcern { token, reason },  // marker is LOOM_CONCERN
    EmptyDiffNoop,                    // marker is LOOM_NOOP — no reviewable work
    StalledMaxIterations,             // outer-loop counter exhausted
    SignalKilled,                     // child terminated by signal
    ReviewEvidenceMissing,            // log file absent / empty / mismatched marker
    IntegrityFinding,                 // unresolved annotation / stub test
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
interactively. The session writes resolution notes via
`bd update --notes` and clears the label via `bd update
--remove-label=loom:clarify` (or `loom:blocked`) per resolved bead.
Mid-walk exit is a clean `LOOM_COMPLETE`; unresolved beads remain
visible in the next `loom msg` session. The chat session emits
`LOOM_COMPLETE` only — `LOOM_BLOCKED` and `LOOM_CLARIFY` are not valid
exit signals for `msg` (the session itself is the resolution channel,
not a producer of new clarifies).

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

Workflow state lives in `.wrapix/loom/state.db`. The schema is owned by
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
  needs one (`loom todo`, `loom gate audit --tree`) mints it.
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
- **`loom gate audit --tree`** — for each concern about spec X,
  applies single-tier resolution: bonds fix-up beads to X's open
  epic, or mints molecule + epic if none exists. See
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
`loom gate audit --tree` mint a new molecule because no open epic
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
`.wrapix/loom/scratch/<key>/`:

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

Loom reads `<workspace>/config.toml` by default; setting the
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
# Delete log files under .wrapix/loom/logs/ older than this many days on
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
`[runner.<tier>.<name>]` block — lives in `<workspace>/config.toml`.
There is no separate `.loom/config.toml` or auxiliary config file, so
the entire `.wrapix/` tree can be gitignored without carve-outs. Set
`LOOM_CONFIG` to relocate.

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
  [system](nix run .#test-wrapix-spawn-load)
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
- With `LOOM_INSIDE=1`, mutating subcommands (`run`, `init`, `plan`,
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
      `.wrapix/loom/logs/<spec-label>/<bead-id>-<timestamp>.jsonl` for every
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

### Worktree dispatch

- `loom loop --parallel N` (default) creates one worktree per
      dispatched bead under `.wrapix/worktree/<label>/<bead-id>/`,
      regardless of `N`; the main checkout is never the bead's workdir
  [test](bead_dispatch_creates_worktree)
- The bead's worktree is freshly created at HEAD with an empty
      working-tree state, so verdict-gate checks that depend on a clean
      starting tree (notably `tree-not-clean`) are sound by construction
  [test](bead_worktree_starts_with_empty_porcelain)
- Each worktree spawns its own `wrapix spawn` and the spawns run
      concurrently (overlapping wall-clock)
  [test](concurrent_spawns_overlap_in_wall_clock)
- Successful bead branches are merged back to the driver branch after
      the batch completes
  [test](parallel_merge_back)
- On worker failure, the bead worktree branch is cleaned up and the bead
      is queued for retry per the retry policy
  [test](parallel_failure_cleanup)
- On merge conflict, the worktree is preserved and the bead is marked
      failed (not silently overwritten)
  [test](parallel_conflict_preserves_worktree)
- `GitClient` is the only module that imports `gix` or invokes the `git`
      CLI; callers see typed Rust methods
  [check](cargo run -p loom-walk -- git_client_encapsulation)

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
  [test](loom_use_sets_current_spec_only)
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
  [test](once_mode_fires_gate_when_molecule_closes_else_no_gate_partial)
- `loom loop --parallel N` (alias `-p N`) accepts a positive integer; non-
      positive or non-integer values fail with a clear error
  [test](default_is_one)
- `loom loop --all-specs` iterates every spec with an open epic
      (resolved via single bd query per spec in the index), ordered
      by bd's `Updated` timestamp ascending (oldest first), running
      the per-spec outer loop to completion for each; per-spec
      `GateOutcome::Fail` does not stop the iteration
  [test](all_specs_iterates_by_bd_updated_asc)
- `loom loop --all-specs` aggregate exit code is non-zero iff any
      spec ended in `GateOutcome::Fail`; `Success` and `NoGate`
      across all specs → exit 0
  [test](all_specs_exit_code_reflects_aggregate)
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
      refuses the push. The verdict is encoded as `GateOutcome`
      (`Success` when all four hold, `Fail { reason }` otherwise);
      the constructor for `GateSuccess` asserts each condition
      structurally — see [Loop Outcome Types](#loop-outcome-types)
  [test](push_gate_evaluates_all_four_conditions)
- Push gate refuses when `loom gate review`'s `--diff`-scoped
      invocation emits `LOOM_CONCERN`; molecule routes to recovery
      with cause `review-concern`
  [test](push_blocked_on_review_concern_with_id_payload)
- Push gate refuses on any integrity-gate finding
      (`UnresolvedAnnotation`, `StubTestFunction`) within the
      molecule's diff scope; applies `loom:clarify` to the
      molecule's epic with auto-generated `## Options — …` block
  [test](push_blocked_on_integrity_finding_applies_clarify)
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
- The chat session writes resolution notes via `bd update --notes`
      and clears the label via `bd update --remove-label=loom:clarify`
      (or `loom:blocked`) per resolved bead
  [test](loom_msg_chat_writes_notes_and_clears_labels)
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
      (no `LOOM_BLOCKED`, no `LOOM_CLARIFY`, no `LOOM_NOOP`)
  [test](loom_msg_chat_rejects_non_complete_exit_signal)
- `loom msg -c` with `-s <label>` scopes the chat session to
      clarifies labeled `spec:<label>`; without `-s`, the session sees
      every outstanding clarify regardless of `current_spec`
  [test](loom_msg_chat_scope_filters_to_spec)
- `loom spec` queries spec annotations (verify/judge)
  [test](returns_no_criteria_error_when_section_missing)
- `loom spec --deps` scans verify/judge test files in the active spec
      and prints required nixpkgs
  [test](maps_known_tools_to_nix_packages)

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
  [test](complete_with_review_concern_routes_to_review_concern)
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

- `loom init` creates `<workspace>/config.toml` (or `$LOOM_CONFIG` when
      set) and `.wrapix/loom/state.db` with the default schema
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
- Bare `loom logs` against an empty `.wrapix/loom/logs/` exits 0
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
  [test](plan_does_not_create_epic_or_touch_bd)
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

- At session start, `.wrapix/loom/scratch/<key>/` contains
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
     `loom gate audit --tree`'s mint-if-missing branch (see
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
     `.wrapix/loom/logs/` via the same `AgentEvent` renderer used by
     `loom loop`. Full flag set in [Logs UX](#logs-ux).
   - `loom spec` — query spec annotations; supports `--deps` to print
     nixpkgs required by the spec's `[check]` / `[test]` / `[system]`
     / `[judge]` verifier targets

   **State** — workspace lifecycle and persisted state:
   - `loom init` — create `.wrapix/loom/` config + state DB. `--rebuild`
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
   (`.wrapix/loom/state.db`). Tracks active specs, per-molecule
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
6. **Worktree dispatch** — `loom loop --parallel N` (alias `-p N`) dispatches
   up to N ready beads, each in its own git worktree on a per-bead branch,
   regardless of `N`. The main checkout is never the bead's workdir.
   `--parallel 1` (default) runs one bead at a time; `--parallel N > 1`
   runs N concurrently. After workers finish, branches are merged back
   to the driver branch sequentially.
7. **Retry with context** — on in-session worker failure, retries with the
   prior error output injected as the `previous_failure` template variable.
   Configurable max retries per bead (default 2). After in-session retries
   exhaust, the phase ends; the verdict is delegated to the
   [Verdict Gate](#verdict-gate).
8. **Verdict gate per phase** — `loom gate verify` (deterministic)
   followed by `loom gate review` (LLM) evaluates each phase's result
   before the bead's state can advance. See [Verdict Gate](#verdict-gate)
   for the execution layer (decision table, recovery mechanics,
   markers, labels) and [gate.md](gate.md) for the review
   rubric. Driver-detected gate failures enter a bounded recovery
   loop; agent self-reports (`LOOM_BLOCKED` / `LOOM_CLARIFY`) escalate
   directly to the human via `loom msg`.
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
   4. **Integrity gate.** Zero `UnresolvedAnnotation` and zero
      `StubTestFunction` findings across the molecule's diff scope.
      An integrity finding refuses the push and applies `loom:clarify`
      to the molecule's epic with the integrity gate's auto-generated
      `## Options — …` block (Options Format Contract — see
      [gate.md](gate.md)).

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
    `/workspace/.wrapix/dolt.sock`; in-container `bd` writes go straight to
    the authoritative state. No per-bead `bd dolt push/pull` handoff. Loom
    on the host reads the same state through the same socket. The legacy
    `.beads/issues.jsonl` path is not used — beads no longer supports it.
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
- **Parallelism beyond worktree-per-bead** — `loom loop --parallel N`
  dispatches one git worktree per bead in parallel. New parallelism
  strategies (cross-spec, distributed, scheduler-aware) are future
  work.
- **Hidden specs (`-h` flag)** — scratch / private specs are not a
  first-class concept. The use case — keeping a spec out of git — is
  covered by `.git/info/exclude` on `specs/<label>.md`. Eliminating
  the flag keeps `plan` / `todo` / `run` path-resolution
  single-shaped. Reintroducing it later is a non-breaking additive
  change if the workflow asks for it.
- **Override of Loom's workflow templates** — Loom's `plan` / `todo`
  / `run` / `gate review` / `msg` templates are Askama, compiled
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
