# Loom Harness

Platform contract for Loom's Rust workspace, host-side workflow orchestration,
reconstructable cache, and command set.

## Problem Statement

Loom drives a spec-to-implementation workflow through `plan`, `todo`, `loop`,
`gate`, `inbox`, and `tune`. The harness keeps orchestration trusted on the
host, isolates agent work in per-session containers, and represents workflow
state and outcomes with typed boundaries. This spec owns the platform and
workflow composition. Repository-level orientation lives in
[`docs/architecture.md`](../docs/architecture.md), and shared terminology lives
in [`docs/README.md`](../docs/README.md).

Concern-specific contracts remain single-sourced: [events.md](events.md) owns
the event stream, rendering, and logs; [templates.md](templates.md) owns Askama
prompts and typed contexts; [agent.md](agent.md) owns backends and their wire
protocols; [gate.md](gate.md) owns gate evidence and review policy;
[skills.md](skills.md) owns skills and tuning; and [llm.md](llm.md) owns public
LLM primitives.

## Architecture

### Process Architecture

Loom is a host-side orchestrator. Agent-bearing worker phases receive an
isolated container for each dispatch, selected from the bead's workspace
profile and the phase's agent runtime. Loom delegates sandbox construction to
`wrix spawn`; it supplies typed launch configuration and consumes canonical
agent events over piped JSONL. It does not construct containers directly.

The trust boundary is stable: Loom, Git integration, gate decisions, and pushes
run on the host; agent tools run in the sandbox. Interactive `plan` and `inbox
chat` sessions use `wrix run` with inherited terminal IO, while non-interactive
worker sessions use `wrix spawn --stdio`. Backend-specific launch and protocol
details are owned by [agent.md](agent.md).

### Bead Dispatch

Loom operates from a dedicated integration clone rather than the operator's
checkout:

```text
<workspace>/                    operator checkout
<workspace>/.loom/integration/  host-side integration clone
<workspace>/.loom/beads/<id>/   persistent per-bead clone
```

Each bead clone has a self-contained Git directory and branch `loom/<id>`, so
its `/workspace` container mount remains usable without exposing the integration
clone. A clone persists across retries and loop invocations until its bead is
closed. Startup and post-close cleanup remove closed-bead clones owned by the
selected molecule.

Before dispatch, Loom applies the selected repository Git policy, then saves
dirty tracked, staged, and non-ignored untracked work in a named recovery stash.
It aligns committed bead work with the current integration tip when possible.
The next worker receives typed `workspace_recovery` context describing any stash
or conflict; cleanup never silently discards preserved work.

Workers commit but do not push. After a successful worker verdict, the host
fetches the bead branch, verifies signatures under the selected Git policy,
rebases it onto the integration branch, verifies rewritten commits, and
fast-forwards under Git's integration lock. The transient integration-clone
`loom/<id>` ref is removed on every exit path. Integration conflicts receive
one worker recovery attempt before clarification.

A bead container mounts its clone at `/workspace` and the authoritative Beads
Dolt socket at `/workspace/.wrix/dolt.sock`. A configured shared sccache
directory is an additional mount. `SpawnConfig` owns these per-launch mounts;
[agent.md § SpawnConfig](agent.md#spawnconfig) owns their wire shape.

### Repository Git Isolation

Every Loom command that launches Wrix defaults to repository-scoped Git
authority: `plan`, `loop`, `todo`, `inbox chat`, proposal tuning, and the review,
audit, judge, rubric, and mint gate paths. Before selecting beads or launching
`wrix run` / `wrix spawn`, each entry point resolves both deploy and signing
keys using Wrix precedence: an explicit `WRIX_DEPLOY_KEY` or `WRIX_SIGNING_KEY`
must be an absolute path to an existing file; otherwise each key falls back to
`$HOME/.ssh/deploy_keys/<repo>-<host>` with the signing-key suffix. If either
key remains unresolved, Loom fails before querying Beads or spawning an agent.
Repository mode also rejects ambient `GIT_SSH_COMMAND` and `GIT_SSH` overrides.
Diagnostics identify `--host-key` as the sole ambient-host opt-in.

After resolution, Loom runs `wrix init --offline --no-hooks --key <key-name>` in
the selected checkout, exposing key paths only to that child. Loop startup
selects `.loom/integration`; other Wrix-bearing commands select their active
workflow checkout. Wrix installs context-stable repository-local transport and
SSH-signing policy, including `.git/wrix/allowed_signers`; Loom validates the
expected configuration and policy files and fails closed on a no-op or partial
result.

New and reused bead clones receive the same policy before dirty-work stashing,
origin fetch, or host-side rebase. Context-stable helpers let the host and the
container use the repository key without persisting a host private-key path.
Repository mode treats a missing allowed-signers file as an error and verifies
both fetched and rebased commits.

The global `--host-key` option removes managed Wrix and legacy
signing/transport configuration from the selected checkout and permits ambient
host Git policy for all Wrix-bearing commands. Without that flag, Loom never
falls back to the host GPG key.

Resolved host key paths are applied to every `wrix run` child and travel to
`wrix spawn` only through `SpawnConfig.launcher_env` as `WRIX_DEPLOY_KEY` and
`WRIX_SIGNING_KEY`. That map is excluded from serialized spawn JSON; Wrix
stages fixed in-container paths. Independently, `loom init` enables Git rerere
for integration-conflict replay.

### Profile-Image Manifest

The Nix-produced profile-image manifest maps `(ProfileName, AgentRuntime)` to an
image entry. Each entry carries the podman reference, immutable image source,
source kind, and optional host-only launcher, wrix profile configuration, and
digest paths. Loom parses the manifest once from `LOOM_PROFILES_MANIFEST` and
has no implicit search fallback.

Dispatch resolves the CLI profile override or bead `profile:X` label together
with the phase backend, then looks up the pair. Missing profiles or runtimes are
static `loom:infra` diagnostics that name the requested and available values.
Non-interactive launches place the selected values in `SpawnConfig`;
interactive launches hand the same selected image to `wrix run` through
`WRIX_DEFAULT_IMAGE_REF` and `WRIX_DEFAULT_IMAGE_SOURCE`.

### Concurrency & Locking

Independent work roots may run concurrently. Host-only advisory lock files live
under `$XDG_STATE_HOME/loom/locks/<workspace-basename>/`, outside every
container mount:

- `plan.lock` serializes planning;
- `todo.lock` serializes changed-spec decomposition;
- `<bead-or-epic-id>.lock` serializes a mutating work root;
- `tune.lock` serializes tune proposal allocation; and
- `workspace.lock` protects initialization and cache rebuild.

Read-only inspection takes no lock. A mutating command waits at most five
seconds before reporting the held root. The kernel releases locks on process
exit. Git's `index.lock` separately serializes short integration critical
sections, while non-fast-forward push races trigger fetch, rebase, re-gate, and
retry against the new concrete push range.

### Nested-Loom Guard

Every managed container receives `LOOM_INSIDE=1`. Under that marker, Loom
rejects container-spawning, workspace-mutating, LLM-review, mint, inbox, and
tune commands before dispatch. Read-only `status`, `logs`, and `spec` commands,
plus deterministic gate inspection such as `loom gate verify`, remain available
for worker feedback.

### Events and Rendering

[events.md](events.md) owns canonical events, rendering, persisted JSONL logs,
`EventSink`, and replay. Harness workflow code consumes that public surface and
emits driver events for routing, recovery, integration, and push decisions.

### Logs UX

[events.md](events.md) owns log semantics. This table is the harness-owned CLI
index consumed by the surface-conformance verifier:

| Flag | Behavior |
|------|----------|
| default | Render the most recent log and exit at EOF. |
| `-f` / `--follow` | Continue rendering the selected log. |
| `-b` / `--bead <id>` | Select the latest log for a bead. |
| `-v` / `--verbose` | Include diagnostic event metadata. |
| `--raw` | Emit raw JSONL bytes. |
| `--path` | Print the selected log path and exit. |

### Verdict Gate

Worker terminal output is untrusted until reconciled with mechanical state.
Interactive `plan` and `inbox chat` sessions are human-authoritative and bypass
worker reconciliation; they accept only their phase-valid completion or apply
handoff. Review sessions use the finding/terminator protocol in
[gate.md](gate.md).

For a loop worker, the final marker, bead closure, branch diff, and tree state
produce the following result:

| Marker and state | Result |
|------------------|--------|
| `LOOM_BLOCKED` with no safe options | semantic block |
| `LOOM_CLARIFY` with a persisted Options block | human clarification |
| `LOOM_RETRY` with a preceding reason | bounded worker recovery |
| no valid final marker | `swallowed-marker` recovery |
| `LOOM_COMPLETE` while the bead is open | `incomplete-signaling` recovery |
| `LOOM_COMPLETE` with an empty diff | `zero-progress` recovery |
| successful marker with a dirty tree | `tree-not-clean` recovery |
| `LOOM_COMPLETE`, closed bead, non-empty diff, clean tree | integrate and verify |
| `LOOM_NOOP`, closed bead, empty diff, clean tree | intentional no-work success |

`LOOM_CLARIFY` is valid only after the worker persists a well-formed
[Options Format Contract](gate.md#options-format-contract) block on the target.
A missing or malformed block becomes `loom:blocked` with cause
`clarify-without-options`. `LOOM_RETRY` consumes the in-session retry budget;
`LOOM_BLOCKED` and valid clarification go directly to inbox.

Per-bead integration verifies worker signatures, rebases, verifies rewritten
signatures, fast-forwards, and runs `loom gate verify --diff
<pre-integration-head>..HEAD`. A deterministic failure rolls back the
integration head, writes a durable gate log, and returns typed failure evidence
to the same bead's recovery prompt. Per-bead integration does not run LLM
review or mint a push marker.

After all molecule work and promoted remediation drain, the push gate fetches
origin, resolves the actual push range, runs pre-push deterministic checks, runs
LLM review, constructs the gate-owned `GateSuccess` receipt, mints the marker,
and pushes inside one critical section. Any changed range invalidates prior
evidence and reruns the gate. Successful Git and Beads publication is followed
by inside-out closure of ancestor epics whose direct children are closed.

Infrastructure failures remain distinct from semantic worker outcomes. Static
configuration/dispatch faults pause the bead as `loom:infra` without transport
retry. Spawn, handshake, transport, framing, or premature-stream failures use a
separate per-loop infra attempt budget and round-robin behind other ready work.
Exhaustion pauses the bead as `loom:infra`; a later loop invocation gets a fresh
budget. `first_event_seen` distinguishes pre-stream from interrupted sessions.

Every driver-detected recovery carries a typed `PreviousFailure`, bounded
attempt count, and durable evidence such as dirty paths, verifier failures,
conflict files, gate log path, or agent retry reason. Recovery exhaustion
preserves the cause in Beads. Remediation work is bonded to its originating
molecule before becoming dispatchable so molecule progress and push refusal see
all unresolved blocked, clarify, deferred, and infra state.

Marker ownership is phase-specific:

- `LOOM_COMPLETE` is generic success for loop, clean review, plan, and inbox;
- `LOOM_NOOP` is loop-only empty-diff success;
- `LOOM_TODO: <json>` is todo-only typed success;
- `LOOM_APPLY: {"proposals":[...]}` is inbox's trusted apply handoff;
- `LOOM_RETRY`, `LOOM_BLOCKED`, and direct `LOOM_CLARIFY` are worker
  self-reports subject to their phase restrictions; and
- `LOOM_CONCERN: {"summary":"..."}` terminates a review that streamed one or
  more `LOOM_FINDING:` records.

Exactly one phase-valid marker appears on the final non-empty line. Exit status
alone does not authorize state transitions.

### Loop Outcome Types

`LoopOutcome` and `GateOutcome` are architecture-bearing types: a successful
loop invocation cannot omit the push-gate result. `LoopOutcome` has no default,
is `must_use`, records processing counts, and carries a non-optional
`GateOutcome`.

`GateOutcome` has three shapes: `Success(GateSuccess)`, `Fail(GateFail)`, and
`NoGate { beads_processed, reason }`. `GateSuccess` is sealed and constructed
only from the evidence defined in
[gate.md § Gate success receipt](gate.md#gate-success-receipt). `NoGate` is
limited to no ready work or an explicitly partial selection. The CLI exits zero
for `Success` and `NoGate`, non-zero for `Fail` or `LoopError`.

### Inbox Modes

`loom inbox` is the cross-spec human queue for non-closed `loom:clarify`,
`loom:blocked`, and `loom:infra` beads plus pending, blocked, and apply-failed
tune proposals. List and view run on the host without mutation; chat launches an
interactive backend.

| Mode | Invocation |
|------|------------|
| List | `loom inbox` / `loom inbox list` |
| View by number | `loom inbox view <N>` |
| View by bead | `loom inbox view -b <id>` |
| View by proposal | `loom inbox view -p <proposal-id>` |
| Chat queue | `loom inbox chat` |
| Chat by number | `loom inbox chat <N>` |
| Chat by bead | `loom inbox chat -b <id>` |
| Chat by proposal | `loom inbox chat -p <proposal-id>` |

| Flag | Purpose |
|------|---------|
| `-s` / `--spec <label>` | Filter to a spec. |
| `-k` / `--kind clarify\|blocked\|infra\|tune` | Filter to one item kind. |
| `-b` / `--bead <id>` | Address a bead-backed item. |
| `-p` / `--proposal <id>` | Address a tune proposal. |

Filters apply before stable kind/FIFO ordering. View renders durable options,
diagnostics, and repair paths.

Inbox chat has human-authorized Beads write access to queued items and may
repair tune artifacts only in the proposal checkout. The driver does not
reconcile those interactive Beads changes afterward. `LOOM_COMPLETE` ends chat
without host apply; `LOOM_APPLY` requests one validated all-or-nothing tune
proposal batch through integration, gate, and push. A failed batch publishes
nothing and leaves every proposal in `apply_failed` for explicit later review.

There is no host-side pick, reply, resolve, dismiss, or apply mutation surface.
Options are discussion context for chat rather than executable menu entries.

### Tune Modes

[skills.md § Tune Command Surface](skills.md#tune-command-surface) and its
proposal-worktree section own tuning. Harness supplies `tune.lock`, workflow
placement, inbox routing, and the trusted post-chat apply path.

### Crate Layout

The fixed workspace separates public contracts from internal orchestration:

- `loom` — CLI parsing and dispatch (internal).
- `loom-driver` — host configuration, state, locking, Git, Beads, and session
  support (internal).
- `loom-events` — canonical events, identifiers, `Session`, and `EventSink`
  (public contract).
- `loom-llm` — provider-neutral LLM and conversation primitives (public
  contract).
- `loom-skills` — skill artifacts, discovery, resolution, and materialization
  (public contract).
- `loom-tune` — tuning cases, evidence, scoring, and proposal metadata
  (internal).
- `loom-render` — event renderers and log sink (internal).
- `loom-agent` — Pi, Claude, and Direct backend adapters (internal).
- `loom-direct-runner` — sandboxed Direct conversation entrypoint (internal).
- `loom-gate` — verifier, review, marker, and finding implementation
  (internal).
- `loom-protocol` — shared subprocess wire protocols (public contract).
- `loom-workflow` — phase orchestration and lifecycle state machines
  (internal).
- `loom-templates` — typed contexts, partials, and compiled prompts (public
  contract).
- `loom-test-support` — shared test fixtures (internal, test-only).
- `loom-walk` — deterministic source/spec conformance walks (internal).

### Dependency Graph

Public contracts stay at the bottom of the graph. `loom-events` is the base
contract and imports no internal crate. `loom-protocol`, `loom-llm`, and
`loom-skills` build only on their documented public-contract dependencies;
`loom-templates` builds on `loom-events` and `loom-protocol`. They do not import
runtime orchestration.

`loom-render`'s dependency direction is owned by
[events.md § Crate Boundaries](events.md#crate-boundaries). `loom-agent`
consumes events, LLM primitives, and materialized skills;
`loom-direct-runner` composes agent tools with the LLM conversation.
`loom-tune` consumes events and skills. `loom-gate` consumes events and
protocol values. `loom-workflow` is the orchestration top and may compose the
internal crates. Runtime crates do not depend on `loom-test-support` or
`loom-walk`.

### Workspace Dependencies

Third-party versions are pinned once at workspace scope and inherited by member
crates. Public-contract crates retain narrow dependency floors so they can
version without importing host orchestration.

### Workspace Lints

Lint policy is workspace-owned and inherited by every crate. The enforceable
rule set and override policy live only in
[`docs/style-rules.md`](../docs/style-rules.md).

### Parse, Don't Validate

Raw CLI values, manifest JSON, Beads JSON, cache rows, and backend protocol
frames are parsed once at their boundaries. Downstream workflow code receives
typed domain values and canonical events rather than re-parsing strings.

`BeadId`, `SpecLabel`, `MoleculeId`, `ProfileName`, `SessionId`, `ToolCallId`,
and `RequestId` are transparent newtypes. `BeadId` validates its canonical
shape on construction and deserialization. `AgentRuntime` is a closed enum.
Typestate for subprocess sessions and the non-optional loop/gate outcome shapes
make invalid lifecycle transitions unrepresentable.

### Askama Template System

[templates.md](templates.md) owns the template engine, typed contexts,
partials, pinning policy, and public composition surface.

### Beads CLI Wrapper

The host interacts with Beads through a typed subprocess boundary. JSON output
is parsed into typed beads, molecules, labels, and progress; arguments are
passed without shell interpolation; errors and timeouts remain typed. The
workflow uses create, show, close, update, list, dependency, bond, and progress
operations against the shared Dolt service.

### SQLite Cache Store

`.loom/cache.db` is disposable, reconstructable workflow cache. Git, Beads
metadata, specs, and the current spec index are durable authority; missing or
corrupt cache data cannot establish that changed work is clean.

The schema caches indexed specs, spec-epic ids/cursors, work-epic metadata and
iteration counts, companion paths, typed notes, criterion evidence, and schema
metadata. Criterion evidence joins on typed `(SpecLabel, CriterionId)` and
exposes missing or stale values rather than treating them as success. Cache APIs
own SQL access.

`loom init --rebuild` recreates this state from the spec index, spec files,
Beads spec/work epics, and companion declarations. It rejects structural
inconsistencies such as missing or duplicate indexed specs/epics. Criterion
results and notes are transient and are not reconstructed; iteration counters
restart at zero.

## Companions

## Spec and Work Epic Lifecycle

A spec epic is the durable metadata carrier for one indexed spec, labelled
`loom:spec` and `spec:<label>`. Exactly one exists per indexed spec. Its
`loom.todo_cursor` records the Git commit through which decomposition was
finalized; status does not affect lookup, and implementation beads are not
parented beneath it.

A work epic is an execution batch. `loom todo` creates or reuses one pending
`loom:todo` epic for the deterministic changed-spec roster. A validated
`LOOM_TODO` handoff applies its final title, advances every roster spec's cursor
atomically, removes `loom:todo`, and makes it the sole `loom:active` epic.
Standing tree remediation similarly creates a non-empty active work epic only
after actionable findings exist. `loom:active` selects the default loop root;
it does not select changed specs for todo.

Todo preflight derives changed specs from the current index, spec blobs, Git
ancestry, and each spec epic's cursor. It ensures one spec epic per indexed
spec, surfaces missing or invalid durable metadata, parses every changed spec's
criteria, and represents absent cache evidence as missing. No changed specs
means no agent, no work epic, and no cursor movement.

Todo success is the public `loom-protocol::todo::TodoSuccess` wire value. It
binds the preflight head and fingerprint, pending work epic, final non-empty
title, and exactly one outcome for each changed spec. A decomposed outcome names
non-empty child beads under the work epic; a no-work outcome gives a non-empty
reason. Validation failure leaves pending state and every cursor unchanged.

Implementation notes are typed, transient cache hints. Planning may merge them;
validated todo finalization renders and consumes notes for the changed specs.
Failed or non-finalized decomposition leaves them intact. Rebuild drops them
without changing durable workflow truth.

## Compaction Recovery

Each agent-bearing session starts with a private
`.loom/scratch/<key>/` containing the full rendered `prompt.txt`, an empty
append-only `scratch.md`, a compact-session re-pin script, and any materialized
built-in skills. Direct may lazily add its output-offload directory. The key is
the phase's concurrency unit, so parallel beads do not share recovery state.

The initial prompt remains active instruction context after compaction. Backend
delivery reintroduces the full prompt before the live scratchpad, and
post-compaction output is not trusted until that pin is effective. Ordinary
conversation history yields before pinned protocol and mode instructions when a
backend limit forces a choice. [agent.md § Compaction
Handling](agent.md#compaction-handling) owns delivery mechanics.

The scratch tree is removed on every session exit and recreated empty on the
next session. It is a recovery aid, not durable workflow state.

## Loom-LLM

[llm.md](llm.md) owns `LlmClient`, typed completion and cache controls,
`Conversation`, tools, usage, and observers. Harness owns only crate placement,
configuration composition, and routing observer aborts into typed recovery.

## Configuration

Loom reads `<workspace>/loom.toml`; `LOOM_CONFIG` may select another path.
Missing files and fields use typed defaults. The root file is the sole
configuration source, including gate runner blocks; `.loom/` contains runtime
state rather than hidden configuration.

Harness-owned settings include Beads creation defaults; integration branch,
sccache paths, and Git timeout; loop iteration/retry/infra budgets; event-log
retention; phase profile/backend defaults and overrides; skill registration;
and runner dispatch. Backend, LLM-observer, gate, skill, and tuning field
semantics are owned by their respective specs.

Phase values resolve from `[phase.<name>]`, then `[phase.default]`, then built-in
defaults. A loop bead's `profile:X` label precedes phase profile defaults, while
the CLI profile override has highest precedence. Default configuration selects
the base profile, Claude backend, ten work-epic iterations, two worker retries,
and three infra attempts.

## Success Criteria

### Crate structure

- Workspace builds with `cargo build` from `loom/` root
  [check](cargo build --workspace)
- Target v1 crate set matches the fixed workspace members exactly: loom,
      loom-driver, loom-events, loom-llm, loom-skills, loom-tune,
      loom-render, loom-agent, loom-direct-runner, loom-gate,
      loom-protocol, loom-workflow, loom-templates, loom-test-support,
      and loom-walk
  [check](cargo run -p loom-walk -- crate_structure_includes_loom_tune)
- Five public-contract crates declared in workspace manifest metadata: loom-events, loom-protocol, loom-llm, loom-templates, loom-skills; no other crate declares the marker
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
- `SpawnConfig` JSON shape is stable: serialization round-trip preserves
      documented per-launch fields and key names, including `image_ref`,
      `image_source`, and `image_source_kind`, while omitting
      ProfileConfig-only host fields
  [test](spawn_config_omits_profile_manifest_host_only_fields_from_wrix_json)
- `wrix spawn` installs from `image_source` (a Nix store path) before
      invoking podman with `image_ref` as the ref; the selected ProfileConfig
      image digest lets wrix skip reloading bytes if the same content already
      exists in the local image store
  [system](nix run .#smoke)
- Per-bead profile/runtime selection: two beads with different profile
      labels or backend runtimes result in `wrix spawn` invocations with
      the matching `image_ref`, `image_source`, `image_source_kind`, and
      ProfileConfig
  [test](per_bead_profile_runtime_dispatch_produces_distinct_image_refs)
- Loom reads `LOOM_PROFILES_MANIFEST` at startup and parses it into
      `BTreeMap<ProfileName, BTreeMap<AgentRuntime, ImageEntry>>`; missing
      env var or missing file errors before any bead spawn
  [test](from_path_missing_file_returns_manifest_not_found)
- A bead with `profile:X` where `X` is not in the manifest fails with a
      typed `ProfileError::UnknownProfile` naming the missing profile
  [test](lookup_unknown_profile_carries_manifest_path)
- A resolved backend runtime missing under an existing profile fails with a
      typed profile-manifest error naming the profile and runtime
  [test](lookup_missing_runtime_for_profile_carries_profile_and_runtime)
- `--profile` CLI override takes precedence over bead labels
  [test](cli_override_swaps_resolved_image)
- `loom plan` shells out to interactive `wrix run` (TTY attached); does
      not capture stdio for JSONL
  [test](argv_starts_with_run_subcommand)

### Concurrency & locking

- `plan.lock`, `todo.lock`, and `<bead-or-epic-id>.lock` files are
      created outside the workspace and released on process exit
  [test](phase_and_work_root_locks_create_expected_files)
- Two mutating commands for the same phase/work root serialize: the
      second waits up to 5s, then errors clearly naming the held root
  [test](second_acquire_times_out_with_work_root_busy)
- Independent work-root commands run concurrently when they address
      different bead/epic ids
  [test](different_work_root_locks_do_not_block)
- Read-only commands (`status`, `logs`, `spec`) acquire no lock and run
      during an active `loom loop`
  [test](readonly_paths_unaffected_by_work_root_lock)
- `loom init` and `loom init --rebuild` acquire the workspace lock
      and error immediately if any plan/todo/work-root lock is held
  [test](acquire_workspace_errors_when_phase_or_work_root_lock_held)
- Crashed loom process leaves no stale lock (kernel releases flock on
      exit; new invocation acquires immediately)
  [test](crash_releases_work_root_lock)
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
      subcommands (`loop`, `init`, `plan`, `todo`, `inbox`, `tune`,
      `loom gate mint`, `loom gate review`, `loom gate judge`,
      `loom gate rubric`, and `loom gate audit`) refuse with a clear
      error
  [test](mutating_and_llm_spawning_subcommands_refuse_with_loom_inside_set)
- With `LOOM_INSIDE=1`, read-only/deterministic inspection subcommands
      (`status`, `logs`, `spec`, and deterministic `loom gate`
      subcommands such as `verify`) still run normally
  [test](readonly_and_deterministic_gate_subcommands_run_under_loom_inside_set)

### Events and rendering

Owned by [events.md](events.md); see that spec's Success Criteria.

### Dependency graph

- `loom-events` is a leaf crate — no internal deps on `loom-driver` / `loom-render` / `loom-workflow` / `loom-templates` / `loom-llm` / `loom-agent` / `loom-skills` / `loom-tune`
  [check](cargo run -p loom-walk -- loom_events_is_leaf)
- `loom-llm` depends on `loom-events` only (no `loom-driver` / `loom-agent` / `loom-workflow` / `loom-skills` / `loom-tune` import)
  [check](cargo run -p loom-walk -- loom_llm_deps)
- `loom-templates` depends on `loom-events` and `loom-protocol` only among internal crates (no `loom-driver` / `loom-llm` / `loom-agent` / `loom-workflow` / `loom-skills` / `loom-tune` import)
  [check](cargo run -p loom-walk -- loom_templates_deps)
- `loom-skills` depends on `loom-events` but not `loom-driver` / `loom-agent` / `loom-templates` / `loom-tune` / `loom-workflow`
  [check](cargo run -p loom-walk -- loom_skills_deps)
- `loom-tune` depends on `loom-events` and `loom-skills`, but not `loom-driver` / `loom-agent` / `loom-workflow`
  [check](cargo run -p loom-walk -- loom_tune_deps)
- `loom-agent` depends on `loom-llm`, `loom-events`, and `loom-skills`; its `direct` backend wraps `loom-llm::Conversation`
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
  [test](loom_loop_does_not_touch_operator_workspace)
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
  [test](bead_workspace_survives_retry_until_close)
- A bead workspace is reaped on the first `loom loop` iteration that
      observes the bead in `closed` status
  [test](bead_workspace_reaped_on_bd_close)
- Pre-dispatch dirty bead workspaces are preserved before destructive
      cleanup: tracked modifications, staged changes, and untracked files
      outside the ignore set are saved with a named `git stash push
      --include-untracked`; the driver records pre-stash status, stash
      selector/message, stash commit, and target integration tip
  [test](pre_dispatch_dirty_workspace_creates_recovery_stash)
- After a recovery stash is created, the driver leaves it unapplied,
      rebases committed bead work onto the current integration tip (or
      fast-forwards when no local commits exist), and injects
      `workspace_recovery` prompt context without consuming
      `[loop] max_retries`
  [test](workspace_recovery_stash_left_unapplied_and_context_injected)
- Clean pre-dispatch workspaces, and dirty workspaces after successful
      recovery stashing/alignment, still run the reset/clean path that
      preserves `target/`, `.git/`, and `.wrix/`
  [test](bead_workspace_prepare_preserves_target_and_dotwrix)
- If branch alignment conflicts after recovery stashing, the worker is
      dispatched in the conflict state with stash/conflict context rather
      than immediately routing to `loom:clarify` or `loom:blocked`
  [test](workspace_recovery_rebase_conflict_dispatches_agent_with_context)
- `LOOM_COMPLETE` is not rejected solely because a recovery stash still
      exists; stash relevance is judged by review/gate evidence rather
      than a hard driver state machine
  [test](loop_complete_does_not_require_recovery_stash_removed)
- Recovery-stash preflight emits `DriverKind::WorkspaceRecovery` with bead
      id, pre-stash status, stash selector/message, stash commit,
      integration tip, alignment outcome, and conflict files when present
  [test](workspace_recovery_event_records_stash_and_alignment)
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
  [test](loop_startup_gc_drops_closed_bead_workspaces_for_current_molecule)
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
  [judge](../tests/judges/loom.sh#sccache_hits_visible_across_beads)
- `GitClient` is the only module that imports `gix` or invokes the
      `git` CLI; callers see typed Rust methods
  [check](cargo run -p loom-walk -- git_client_encapsulation)
- Every non-loop Wrix-bearing command requires both repository deploy and
      signing keys by default and invokes `wrix init --offline --no-hooks
      --key <key-name>` in its active checkout before selecting beads or
      launching Wrix
  [test](all_non_loop_wrix_launch_surfaces_preflight_repository_policy)
- Default `loom loop` startup applies that policy in `.loom/integration` with
      the exact resolved paths before selecting beads
  [test](loom_loop_startup_initializes_repository_git_policy)
- If either repository key is unresolved, `loom loop` exits non-zero before
      querying Beads or spawning an agent, and the error names `--host-key` as
      the only ambient-host opt-in
  [test](loom_loop_missing_repository_keys_fails_before_bead_selection)
- Repository mode rejects ambient `GIT_SSH_COMMAND` / `GIT_SSH` overrides
      that would outrank its repository deploy-key transport
  [test](repository_mode_rejects_ambient_git_transport_override)
- New and reused bead clones receive context-stable Wrix transport/signing
      config before host-side preflight; no host private-key path is persisted,
      and stale host signing config is repaired on redispatch
  [test](create_worktree_applies_context_stable_wrix_git_policy)
- Loom validates Wrix's expected local config and policy files after init and
      rejects a successful no-op or partial policy rather than failing open
  [test](repository_policy_rejects_success_without_wrix_config)
- Repository mode treats a missing allowed-signers file as an error, never as
      permission to skip signature verification
  [test](repository_policy_does_not_skip_when_allowed_signers_disappears)
- `loom loop --host-key` clears managed Wrix and legacy signing/transport
      config from Loom-owned clones before permitting ambient host Git policy
  [test](host_key_policy_clears_managed_repo_config)
- The fallback keyname is derived as `<repo>-<host>` where `<repo>`
      is parsed from the origin URL (`github.com[:/]<user>/<repo>`)
      and `<host>` is `hostname -s`, matching Wrix key provisioning
  [test](signing_key_fallback_uses_wrix_repo_host_derivation)
- Driver-side rebase in the loom workspace produces signed commits
      whose `gpgsig` header is present in the commit object, without
      prompting for a passphrase
  [test](driver_rebase_signs_with_wrix_key)
- `git log --show-signature` against a driver-rebased commit in the
      loom workspace prints `Good "git" signature` using the configured
      allowed-signers file
  [test](rebased_commits_verify_via_derived_allowed_signers)
- In repository mode, the per-bead integration step runs
      `git verify-commit` against fetched commits (pass 1) and rebased
      commits (pass 2); failures distinguish worker-side from driver-side
  [test](integration_step_verifies_signatures_in_two_passes)
- `GitClient::launcher_key_env` surfaces both startup-resolved keys as
      `WRIX_DEPLOY_KEY` / `WRIX_SIGNING_KEY` host-path pairs for Wrix launchers
  [test](loom_loop_startup_initializes_repository_git_policy)
- Interactive `wrix run` dispatch applies both resolved key paths to the child
      environment
  [test](plan_threads_repository_keys_to_wrix_run)
- Bead dispatch threads the resolved launcher keys onto
      `SpawnConfig.launcher_env` and keeps them out of the
      in-container `SpawnConfig.env` allowlist
  [test](launcher_env_threads_onto_spawn_config_not_container_env)
- Review dispatch threads the checkout-resolved launcher keys onto the
      reviewer `wrix spawn` child process so host deploy/signing keys are
      available before container setup resolves git auth and SSH signing
  [test](loom_gate_review_threads_launcher_keys_to_wrix_spawn)
- `SpawnConfig.launcher_env` is `#[serde(skip)]`-excluded from the
      spawn-config JSON so host key paths never leak into the
      world-readable file the wrapper reads
  [test](launcher_env_is_never_serialized)
- Each backend applies `SpawnConfig.launcher_env` to the `wrix
      spawn` child process environment before exec
  [test](apply_launcher_env_sets_child_process_env)

### Workflow commands

- `loom plan [SPEC_LABEL ...]` spawns an interactive container with
      the base profile and runs the spec interview. Positional labels
      are optional initial anchors (existing specs are pinned; missing
      labels are proposed new specs). Options may appear before,
      between, or after labels. Plan edits spec/index markdown and
      implementation notes only — no bd writes and no touched-set
      manifest
  [test](plan_accepts_optional_anchor_labels_and_interspersed_options)
- `loom todo` performs deterministic changed-spec preflight from
      durable spec-epic cursors (`loom.todo_cursor`), Git, and the
      current `docs/README.md` spec index before rendering any agent
      prompt. It never consults `loom:active`, a current-spec cache key,
      or the LLM to decide the changed-spec set
  [test](todo_preflight_discovers_active_inactive_and_new_specs)
- `loom todo` ensures exactly one `loom:spec spec:<label>` spec epic
      per indexed spec. Missing spec epics are created and make the
      spec uninitialized/changed; duplicate spec epics block with
      conflicting IDs; missing cursor metadata on an existing spec
      epic blocks with an exact repair diagnostic
  [test](todo_missing_spec_epic_initializes_existing_missing_cursor_blocks)
- `loom todo` creates or reuses one `loom:todo` work epic for the
      preflight changed-spec set and requires a final `LOOM_TODO:`
      marker whose typed payload includes a non-empty final title and
      covers exactly that set. Generic `LOOM_COMPLETE` / `LOOM_NOOP`,
      missing rows, missing/empty title, malformed JSON, nonexistent
      beads, beads outside the work epic, or extra/omitted specs fail
      validation
  [test](todo_success_marker_must_cover_exact_changed_spec_set)
- Validated `LOOM_TODO` finalization is all-or-nothing across changed
      specs: every changed spec cursor advances to the preflight HEAD
      (including `NoWork` outcomes), `LOOM_TODO.title` is applied to
      the work epic, `loom:todo` is removed from the work epic,
      `loom:active` is applied to it, and previous active state is
      cleared. Any failure leaves cursors and active state unchanged
  [test](todo_finalization_advances_cursors_and_active_epic_atomically)
- Missing criterion evidence in `.loom/cache.db` produces typed
      `EvidenceState::Missing` rows in `criterion_status`; it is never
      treated as no criteria or no work. Malformed criteria block
      preflight
  [test](todo_missing_criterion_cache_rows_are_missing_evidence)
- `loom gate mint --tree` creates one standing remediation work epic for
      all actionable tree-scope fix-up / blocked-clarify / clarify
      beads in the run, parents every child under that epic, applies
      `loom:active` to it, clears `loom:active` from any previous work
      epic, and prints the epic id plus the follow-up `loom loop`
      command
  [test](mint_tree_sets_single_active_remediation_work_epic)
- `loom gate mint --tree` creates no work epic and leaves
      `loom:active` unchanged when no actionable child bead remains
      after suppression, dedup, and structural validation
  [test](mint_tree_no_actionable_findings_leaves_active_unchanged)
- `loom gate mint --tree` never returns with an open active remediation
      work epic that has zero child beads; failure before the first child
      closes or neutralizes the epic and restores `loom:active` to its
      pre-run state, while failure after at least one child leaves the
      non-empty epic open/active for dedup-friendly rerun
  [test](mint_tree_never_leaves_empty_active_remediation_epic)
- `loom loop [OPTIONS] [BEAD_OR_EPIC_ID ...]` runs the sole
      `loom:active` work epic when no ids are provided. Positional ids
      may be task beads (run exactly that bead) or epics (run ready
      child work under that epic/molecule). Options may appear before,
      between, or after ids. `--host-key` explicitly opts into ambient host
      Git credentials; `--spec`, `--once`, and `--all-specs` are not part of
      the loop surface
  [test](loop_accepts_positional_work_roots_and_defaults_to_active_epic)
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
- Every `loom loop` returning `LoopOutcome { gate: Success(r), .. }`
      references non-empty gate JSONL logs in `r.gate_log_paths`; each
      log contains a `gate_run_start` and matching successful
      `gate_run_end`, and the review evidence contains a terminal
      `AgentEvent` whose effective marker is complete. Holds for all
      execution modes (explicit bead roots, explicit epic roots,
      `--parallel`, and default active-epic continuous mode)
  [test](every_successful_loom_loop_references_completed_gate_logs)
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
- Before molecule push verification, the driver reconciles
      `.loom/integration` to the canonical `wrix.prekHooks`
      `core.hooksPath`, repairing stale store paths and failing loudly if
      the expected path cannot be resolved
  [test](push_gate_repairs_stale_integration_hooks_path)
- On molecule completion, after stabilization has drained promoted
      remediation, `loom loop` fetches/rebases against
      `origin/<integration-branch>`, resolves the remote tip and `HEAD`
      to the concrete OID pair for the actual push range, runs the actual
      prek pre-push chain for that range, then runs
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
  [test](handoff_evidence_populates_typed_gate_scope_values)
- When the molecule-completion audit review produces ≥1 unsuppressed
      streamed `LOOM_FINDING:` record and a `LOOM_CONCERN:` terminator,
      `route="deferred"` findings merge into the molecule's deferred
      remediation set and cause another stabilization pass within the
      molecule iteration cap; `route="clarify"` findings materialize
      one `loom:clarify` bead per finding hash. If every streamed
      finding is suppressed, the effective review marker is Complete and
      no recovery prompt is produced. Mint does NOT fire during the
      per-bead hot path; deferred findings are promoted by
      `loom gate mint -m <molecule-id>` during stabilization
  [test](molecule_completion_review_routes_findings_to_stabilization_or_clarify)
- A molecule-completion review finding with `route="blocking"` refuses
      the push and creates or reuses same-molecule remediation work;
      already-integrated original beads are not reopened solely because
      the push-stage review found a concern
  [test](molecule_review_blocking_finding_creates_same_molecule_remediation)
- A molecule-completion review finding with `route="deferred"` merges
      into a molecule child bead with `status=deferred` and label
      `loom:deferred`; `bd ready` does not return it until molecule
      stabilization promotes it
  [test](molecule_review_deferred_finding_creates_deferred_bead)
- Structural bd conflicts while recording deferred or clarify findings
      route the molecule to `loom:blocked` with cause
      `gate-routing-structural-violation`; already-integrated commits
      are not unwound
  [test](molecule_routes_gate_routing_structural_conflict_to_blocked)
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
  [test](gate_invocations_write_separate_jsonl_logs_with_parent_breadcrumb)
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
  [test](continuous_outer_loop_promotes_deferred_remediation_then_exits_on_stall)
- Push gate supplies the completed molecule state and actual push-range
      gate runs to the gate-owned `GateSuccess` constructor. Receipt
      rejection refuses the push and becomes `GateOutcome::Fail`; the
      accepted evidence and matching rules are defined only in
      [gate.md § Gate success receipt](gate.md#gate-success-receipt)
  [test](push_gate_evaluates_typed_evidence_and_marker_coverage)
- On a **clean** push gate the `MarkerProof` is minted to
      `.loom/marker.json` **immediately before** `git push`, inside
      the gate's critical section, after deterministic pre-push and
      review have both covered the actual push range. A **refused**
      push (blocked/clarify/deferred/infra bead, pre-push failure,
      verify-fail, review-concern, integrity finding, or missing
      marker coverage) mints nothing. A missing or invalid marker falls
      the pre-push consumer through to running hooks rather than failing
      the push by itself
  [test](clean_push_mints_marker_after_covered_verify_and_review)
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
  [test](loom_todo_help_documents_multispec_fail_loud_behavior)
- `loom loop --help` documents `[BEAD_OR_EPIC_ID ...]`, the default
      `loom:active` work epic, interspersed options, explicit ambient-host
      meaning of `--host-key`, and absence of `--spec` / `--once` /
      `--all-specs`
  [test](loom_loop_help_documents_work_roots_and_removed_selectors)
- Bare `loom inbox` / `loom inbox list` lists every outstanding non-closed
      bead carrying `loom:blocked`, `loom:clarify`, or `loom:infra` across all
      specs plus pending, blocked, and apply-failed tune proposal beads
      (cross-spec default); no active-spec cache value is consulted, and closed
      beads are excluded even when labels remain
  [test](inbox_list_includes_infra_and_excludes_closed_items)
- `loom inbox list -s <label>` (alias `--spec`) filters the list to items
      carrying the `spec:<label>` bead label or proposal metadata
  [test](inbox_spec_filter_narrows_list_to_matching_spec)
- `loom inbox list -k clarify|blocked|infra|tune` filters by exclusive item
      kind; absence of `--kind` means all kinds. Filters narrow before
      positional numbering and default ordering is group-first (`clarify`,
      `blocked`, `infra`, `tune`) then FIFO within each group
  [test](inbox_kind_filter_narrows_list_including_infra)
- `loom inbox view <N>` / `loom inbox view -b <id>` /
      `loom inbox view -p <proposal-id>` renders the addressed item host-side
      without launching a container, including durable ids, infra diagnostic
      fields when present, and manual repair paths; corrupt/unavailable tune
      proposals remain tune-kind items with blocked status rather than being
      skipped
  [test](inbox_view_modes_render_host_side_with_infra_diagnostics)
- `loom inbox` exposes no host-side `pick`, `reply`, `resolve`, `apply`,
      `--option`, `--text`, `-c/--chat`, or `-d/--dismiss`; conflicting address
      flags error before any side effects
  [test](inbox_removed_flags_and_address_exclusivity)
- `loom inbox chat`, `loom inbox chat <N>`, `loom inbox chat -b <id>`, and
      `loom inbox chat -p <proposal-id>` launch an interactive session in a
      container using the `inbox.md` template; list/view stay host-side
  [test](loom_inbox_chat_launches_container)
- The chat session has full bd-write authority on bead-backed items in its
      queue and may repair tune proposal artifacts only under
      `.loom/tune/<id>/repo/`; it never pushes and never leaves
      `.loom/integration` dirty
  [test](inbox_chat_bd_authority_and_tune_repair_scope)
- The driver does **not** reconcile bd state after an interactive session —
      no canonical unblock, no status reversion, no label re-application.
      Whatever bd/proposal state the chat agent (with human authorization)
      established at session end IS the state, except for the explicit
      `LOOM_APPLY` handoff
  [test](inbox_chat_driver_does_not_reconcile_bd_state_after_session)
- `LOOM_COMPLETE` from inbox exits cleanly with no driver-side apply;
      `LOOM_APPLY: {"proposals":[...]}` validates accepted tune proposal ids
      and triggers one end-of-chat driver apply batch. `LOOM_APPLY` is the sole
      terminal marker for that session, never paired with `LOOM_COMPLETE`
  [test](inbox_apply_marker_triggers_single_driver_handoff)
- The end-of-chat tune apply batch is all-or-nothing: `cherry_pick_conflict`,
      `verify_failed`, `review_failed`, or `push_failed` aborts the batch,
      pushes nothing, leaves `.loom/integration` clean, and marks every proposal
      in the batch `apply_failed` with shared diagnostics
  [test](inbox_apply_batch_is_all_or_nothing)
- `apply_failed` tune proposals appear in the next default inbox and are not
      retried automatically; a later chat must explicitly repair/reauthorize a
      subset or reject/regenerate them
  [test](apply_failed_tune_proposals_require_reauthorization)
- Interactive-session crashes (container OOM, observer abort, swallowed marker)
      exit non-zero with a diagnostic; the driver does NOT auto-retry
  [test](loom_inbox_chat_crash_exits_nonzero_without_auto_retry)
- `loom inbox chat` with `-s <label>` and/or `-k <kind>` scopes the chat
      queue; without filters, the session sees every outstanding human decision
      item regardless of active work epic and normally works them one at a time
  [test](loom_inbox_chat_scope_filters_queue)
- The tune CLI surface owned by [skills.md](skills.md#tune-command-surface) is
      wired into the binary, and read-only tune invocations do not allocate tune
      proposal envelopes
  [test](loom_tune_bare_prints_help_without_proposal)
- Tune dry-runs exercise the same planning path as proposal creation and stop
      before candidate generation, preserving deterministic checker-plan output
  [test](loom_tune_level_seed_dry_run_shape_plan)
- Tune evidence harvesting stays workspace-first and reports only explicitly
      configured external evidence roots; home-directory transcript roots are
      not implicit
  [test](skill_tune_evidence_roots_and_gate)
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
  [test](spec_targets_lists_annotation_targets_with_tier_and_plain_modes)

### Verdict gate

- After every per-bead `loom loop` worker phase, the verdict-gate
      decision table classifies the terminal marker plus mechanical
      signals (bd-closed, diff, tree cleanliness) without an LLM call
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
- `LOOM_BLOCKED` agent marker with a non-empty reason transitions the bead
      to `[blocked]` and skips the recovery loop
  [test](blocked_marker_routes_to_blocked_with_reason)
- Worker/review guidance reserves `LOOM_BLOCKED` for semantic dead ends
      whose reason explains why candidate options cannot be enumerated;
      `LOOM_CLARIFY` is required when options can be framed
  [judge](../tests/judges/loom.sh#judge_blocked_no_options_rationale)
- `LOOM_CLARIFY` agent marker → bead transitions to `[clarify]`,
      recovery loop is skipped
  [test](clarify_marker_routes_to_clarify_with_question)
- Direct-emit `LOOM_CLARIFY` (`loop` / `todo` only): the gate validates
      the target bead/work epic's notes ∪ description for a well-formed
      `## Options — <summary>` heading with at least one
      `### Option <N> — <title>`
      subsection before applying `loom:clarify`. Same shape mint
      validates on a clarify-route finding's evidence. Forgetful-
      agent case (marker emitted, options block absent or malformed)
      falls back to `loom:blocked` with cause `clarify-without-options`
      — no stranded clarify bead reaches `loom inbox`
  [test](direct_emit_clarify_without_options_block_falls_back_to_blocked)
- Clarify downgrades emit `DriverKind::ClarifyDowngraded`, write a bd
      note breadcrumb with cause `clarify-without-options`, and pair the
      resulting bd label/status mutation with `DriverKind::BdStateTransition`
  [test](clarify_downgrade_emits_driver_events_and_bd_breadcrumb)
- `LOOM_RETRY` agent marker → recovery with cause `agent-retry`,
      `previous_failure` populated with `AgentRetry { reason }` from
      the prose preceding the marker; one `[loop] max_retries` slot
      consumed
  [test](agent_retry_consumes_max_retries_slot_and_threads_reason)
- `LOOM_RETRY` recovery exhaustion → `loom:blocked` with cause
      `retry-exhausted` (the same exhaustion path as other
      driver-detected recoveries)
  [test](consecutive_agent_retry_exhaustion_routes_to_loom_blocked_retry_exhausted)
- `LOOM_RETRY` from an interactive session (`plan`, `inbox`) is a
      wrong-phase-marker error; the driver exits non-zero with a
      diagnostic and does not apply any label
  [test](retry_marker_from_interactive_phase_is_wrong_phase_marker)
- `LOOM_CLARIFY` from a `loom todo` session targets the **`loom:todo`
      work epic** (rationale per
      [templates.md — Decomposition Discipline](templates.md));
      the agent's `## Options — …` block is persisted to the work
      epic's notes per [gate.md](gate.md)'s Options Format Contract
      before the label is applied
  [test](todo_clarify_marks_work_epic)
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
  [test](run_bead_noop_empty_branch_is_done_not_zero_progress)
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
  [test](post_integration_verify_runs_project_precommit_and_affected_check_test)
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
      streamed `LOOM_FINDING:` records routes to
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
      carries `loom:blocked`, `loom:clarify`, `loom:deferred`, or
      `loom:infra`; an orphan remediation bead would slip past this check,
      so the bond invariant is what makes the gate sound
  [test](remediation_beads_under_cap_auto_iterate)
- Recovery iter ≥ max_iterations → applies `loom:blocked` with cause
      in `bd update --notes`
  [test](at_or_above_max_applies_blocked_with_retry_exhausted_cause)
- Iteration count is **work-epic-level** state (cached in
      `work_epics.iteration_count`, not on individual beads) and
      survives `retry → [running]` round-trips; every promoted
      remediation pass consumes one slot of `[loop] max_iterations`
  [test](iteration_counter_round_trips_through_cache_db)
- Agent event-stream failures are classified by `first_event_seen`:
      EOF before the first canonical `source = "agent"` event is retryable
      `infra-preflight`; EOF after one or more agent-sourced events but before
      `session_complete` is retryable `infra-interrupted`; an explicit
      worker `LOOM_BLOCKED` remains semantic `loom:blocked`
  [test](agent_stream_failure_classifier_distinguishes_preflight_interrupted_and_blocked)
- Retryable infra failures use a per-bead, per-`loom loop` budget from
      `[loop.infra] max_attempts` (default 3), move failed beads to the
      tail of an in-memory retry queue, continue other ready work, and
      retry without wall-clock cooldown/backoff while attempts remain
  [test](infra_failures_round_robin_per_bead_without_cooldown)
- EOF before the first agent event retries under the infra budget; after
      exhaustion the bead is paused as `status=blocked` + `loom:infra`
      and never labelled `loom:blocked`
  [test](preflight_eof_retries_then_surfaces_infra_not_semantic_blocked)
- Partial event stream followed by EOF routes to `infra-interrupted`,
      includes `first_event_seen=true`, and follows the same infra retry
      budget instead of semantic worker recovery
  [test](partial_stream_eof_classifies_interrupted_infra)
- Driver infra-failure events include phase, first-event-seen,
      attempt/max attempts, infra class/cause, agent/container exit
      status when known, and stderr tail or spawn error when available
  [test](infra_failure_driver_event_payload_carries_stream_diagnostics)
- Static dispatch diagnostics such as undeclared `profile:X`, missing
      runtime for a declared profile, invalid spawn config, missing agent
      binary, or `workspace-recovery-failed` skip transport retry and
      surface immediately as `status=blocked` + `loom:infra` with notes
      naming the requested value, declared/available set, or preserved
      workspace-recovery failure detail. Missing or malformed
      `LOOM_PROFILES_MANIFEST` remains a startup/global error before bead
      selection.
  [test](static_dispatch_failures_surface_as_infra_without_retry)
- A prior attempt that reached `session_complete` is not overwritten by
      a later retry's pre-stream EOF; the later failure records infra
      diagnostics without converting the bead to semantic `loom:blocked`
  [test](prior_session_complete_not_overwritten_by_later_preflight_eof)
- Infra retry budget is driver-memory only; a fresh `loom loop`
      invocation gets a fresh per-bead budget and proactively retries
      selected work-root beads labelled `loom:infra`, clearing stale infra
      state when redispatching
  [test](fresh_loop_retries_loom_infra_beads_with_fresh_budget)
- The push gate refuses to push while any bead in the molecule carries
      `loom:blocked`, `loom:clarify`, or `loom:infra`
  [test](clarify_or_infra_present_stops_without_pushing)
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
  [test](run_creates_config_and_cache_db)
- `loom init --rebuild` drops and repopulates `.loom/cache.db` from
      durable sources: the spec index, `specs/*.md`, bd spec/work
      epics, and each spec's `## Companions` section. It also folds
      gate criterion-status storage into the unified cache; there is no
      `.loom/gate-cache.sqlite`
  [test](rebuild_drops_and_repopulates_cache_db)
- `loom status` prints the active work epic, any pending `loom:todo`
      work epic, cached iteration counts, and cache health; no active
      spec/current-spec value is displayed or read
  [test](status_reports_active_work_epic_not_current_spec)
The `loom logs` inspection surface is owned by [events.md](events.md).

- `loom sync` remains absent, but `loom tune` is present as the manual
      SkillOpt-style proposal command; the surface-conformance walk rejects
      any reintroduction of sync and validates the tune subcommand shape
  [check](cargo run -p loom-walk -- tune_surface_conformance)

### Cache database

- `CacheDb::open` creates `.loom/cache.db` tables on first open
      (`specs`, `spec_epics`, `work_epics`, `companions`, `notes`,
      `criterion_status`, and `meta`)
  [test](cache_db_init_creates_tables)
- `CacheDb::rebuild` populates `specs` from `docs/README.md`'s spec
      index and cross-checks `specs/*.md`; unindexed spec files,
      missing indexed files, duplicate labels, and label/path mismatches
      fail loud
  [test](cache_rebuild_cross_checks_spec_index_and_files)
- `CacheDb::rebuild` mirrors exactly one `loom:spec spec:<label>` spec
      epic per indexed spec, regardless of epic status; duplicates fail
      with conflicting IDs
  [test](cache_rebuild_requires_one_spec_epic_per_indexed_spec)
- `loom todo` creates a missing spec epic during preflight, treats the
      spec as uninitialized/changed, and blocks when an existing spec
      epic lacks `loom.todo_cursor` metadata
  [test](todo_missing_spec_epic_initializes_existing_missing_cursor_blocks)
- `loom todo` closes driver-created or already-open spec metadata epics
      with reason `spec metadata carrier`, so spec epics do not remain
      open solely because they carry metadata
  [test](todo_preflight_closes_spec_metadata_epics)
- `loom todo` rejects malformed, missing, non-ancestor, or unknown
      `loom.todo_cursor` SHAs with diagnostics that name the spec epic
      and repair surface
  [test](todo_invalid_spec_cursor_blocks_loudly)
- `loom todo` discovers changed specs by comparing each spec/index row
      at `HEAD` against the spec epic's durable cursor; it includes
      inactive/stale specs and brand-new indexed specs regardless of
      `loom:active`
  [test](todo_preflight_discovers_active_inactive_and_new_specs)
- `loom todo` creates one `loom:todo` work epic with a placeholder title
      before rendering the agent prompt, records `loom.todo_head`,
      `loom.todo_fingerprint`, and changed spec labels on it, and does
      not add `loom:active` until validation succeeds
  [test](todo_creates_pending_work_epic_before_agent_prompt)
- A pre-existing open `loom:todo` work epic with matching head and
      `TodoFingerprint` is reused; multiple matches or non-matching
      pending work epics block with an Options-format diagnostic
  [test](todo_reuses_matching_pending_work_epic_else_blocks)
- `loom-protocol::todo::parse_todo_success` accepts exactly
      `LOOM_TODO: <json>` final lines and returns typed `TodoSuccess`;
      malformed JSON, missing fields, empty `title`, empty
      `Decomposed.beads`, empty `NoWork.reason`, or wrong prefix fail
      parse
  [test](todo_success_marker_parses_to_typed_protocol)
- `loom todo` validates `TodoSuccess.head`, `TodoFingerprint`, work
      epic id, final title, exact changed-spec coverage, bead existence,
      and bead parentage under the work epic before finalization
  [test](todo_success_validation_rejects_missing_extra_or_misparented_beads)
- Validated `NoWork` outcomes advance the spec cursor just like
      `Decomposed` outcomes; no-work rows require a non-empty reason
  [test](todo_no_work_outcome_advances_cursor_with_reason)
- Failed todo validation leaves the work epic labelled `loom:todo`,
      writes diagnostics to it, advances no spec cursor, and does not
      change `loom:active`
  [test](todo_validation_failure_leaves_pending_without_advancing)
- Validated or blocked `loom todo` output prints a driver-authored
      per-spec summary covering every changed spec and its outcome; a
      changed spec missing from the summary is a validation failure
  [test](todo_output_summarizes_every_changed_spec_outcome)
- Validated todo finalization removes `loom:todo`, applies the sole
      `loom:active` label to the work epic, clears any previous active
      epic, and advances every changed spec epic's `loom.todo_cursor`
      to the preflight HEAD all-or-nothing
  [test](todo_finalization_sets_active_and_advances_all_cursors)
- `criterion_status` cache rows join to current criteria by typed
      `(SpecLabel, CriterionId)`; stale annotation evidence renders as
      `EvidenceState::StaleAnnotation`, absent rows as
      `EvidenceState::Missing`
  [test](todo_missing_criterion_cache_rows_are_missing_evidence)
- `CacheDb::rebuild` parses each spec's `## Companions` section and
      writes one `companions` row per listed path; specs without the
      section contribute zero rows (not an error)
  [test](cache_db_rebuild_companions)
- `CacheDb::rebuild` resets work-epic iteration counters to 0
  [test](cache_rebuild_resets_work_epic_counters)
- Corrupted cache file → `loom init --rebuild` recovers from durable
      sources or reports the exact durable inconsistency; it never
      treats cache loss as clean todo state
  [test](cache_corruption_recovery_never_implies_clean_todo)
- `loom plan [labels...]` does NOT create epics and does NOT write to
      bd; plan sessions edit specs/index/notes only
  [test](plan_does_not_create_epic_or_touch_bd)
- `loom plan [labels...]` reads existing implementation notes for
      anchor/touched specs and writes back merged arrays via
      `loom note set` (interview-driven keep/drop/add — not blind
      append, not blind replace)
  [judge](../tests/judges/loom.sh#judge_plan_merges_notes)
- `loom todo` renders implementation notes for each changed spec into
      the relevant work beads and deletes those notes only after the
      spec cursor advances during validated finalization
  [test](todo_consumes_notes_only_after_validated_finalization)
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
      `prompt.txt`, `scratch.md`, `repin.sh`, and any materialized built-in
      `skills/` for every agent-bearing phase command (plan, todo, loop,
      gate review, inbox chat)
  [test](open_creates_layout_and_drop_removes_it)
- `<key>` is the joined anchor-label set (or `plan`) for `loom plan`,
      the work epic id for `loom todo`, the bead id for loop/gate worker
      sessions, and the addressed item/filter key for inbox chat
  [test](resolve_scratch_key_uses_plan_anchors_work_epic_or_bead)
- Running `repin.sh` emits a valid `SessionStart[compact]` JSON
      envelope containing banner + `prompt.txt` + `scratch.md` contents
  [test](repin_script_runs_jq_envelope_against_files)
- Running `repin.sh` preserves the full `prompt.txt` bytes in the
      post-compaction envelope before appending `scratch.md`; compacted
      summaries are not accepted as substitutes for the pinned prompt
  [test](repin_script_preserves_full_prompt_verbatim)
- A simulated planning compaction with a fixture `Interview Modes`
      section defining `polish` / `do a polish` as report-only, with no
      edits applied unless explicitly asked, resumes with that definition
      still present
  [test](compacted_resume_preserves_polish_mode_definition)
- The context-assembly unit canary rejects a vague compacted summary as a
      substitute for the full `polish` report-only mode definition
  [test](post_compaction_polish_canary_requires_full_mode_definition)
- A production-path behavioral canary for a planning session forces or
      simulates compaction, asks `do a polish` after compaction, and fails
      unless the post-compaction answer preserves both the full report-only,
      propose-edits/no-file-edits-unless-asked semantics and a test-only
      nonce from the initial rendered prompt
  [test](loom_plan_compaction_repin_polish_canary)
- A simulated planning compaction with a fixture `Interview Modes`
      section defining `one by one` as one design question per turn
      resumes with that definition still present
  [test](compacted_resume_preserves_one_by_one_mode_definition)
- Any backend-specific hard-limit fallback preserves instruction,
      protocol, and mode sections verbatim and removes ordinary history
      before pinned instruction text
  [test](hard_limit_fallback_preserves_pinned_instruction_sections)
- Interactive `loom plan` shell-outs for Claude and Pi, plus Claude-backed
      `loom inbox chat`, install the backend-specific compaction re-pin
      delivery surface before the prompt is accepted; an integration test may
      use a mock launcher, but merely writing an unused scratch file or hook
      fragment does not satisfy this criterion
  [test](interactive_shell_out_installs_compaction_repin_delivery)
- The Pi-backed `loom inbox chat` native-TUI path launches `wrix run ... pi`
      with inherited stdio, a scratch-local session directory, and re-pin
      extension instead of the raw RPC renderer
  [test](inbox_chat_pi_tty_uses_native_wrix_run_with_inherited_stdio)
- The Pi-backed `loom inbox chat` RPC bridge sends the backend-specific
      compaction re-pin after observing `compaction_start`; merely queuing an
      unused steer payload does not satisfy this criterion
  [test](inbox_chat_pi_bridge_repins_on_compaction_start)
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
- Loom binary is available in the hook-free CI devShell
  [system](nix develop .#ci -c loom --version)

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
     accepts success only via a validated `LOOM_TODO:` payload carrying
     a final work-epic title and covering exactly that roster.
     Finalization applies the title, removes `loom:todo`, applies
     `loom:active`, and advances every changed spec cursor to the
     preflight HEAD all-or-nothing.
   - `loom loop [OPTIONS] [BEAD_OR_EPIC_ID ...]` — execute work. With
     no ids, runs the sole `loom:active` work epic. With ids, each
     positional may be a task bead (run exactly that bead) or an epic
     (run ready child work under that epic/molecule). Options may
     appear before, between, or after ids. Repository keys are mandatory
     at startup unless `--host-key` explicitly opts into ambient host Git
     policy. The loop pulls ready child
     beads filtered to exclude semantic `loom:blocked` / `loom:clarify`
     beads; `loom:infra` diagnostic beads are a driver-owned retry queue
     and are retried under the infra policy before a work root is
     considered fully stuck. An epic positional is a work root, never a
     worker task itself.
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
     LLM rubric). The gate command table, subcommand meanings, scope
     flags, and removed gate selector surface are owned by
     [gate.md § Commands](gate.md#commands) and
     [gate.md § Scope flags](gate.md#scope-flags); this harness spec
     owns only that `loom gate` is the workflow command in this slot and
     that the surface-conformance walk is dispatched by `loom gate check`.
   - `loom inbox` — human decision and operator diagnostic queue for
     clarifies, semantic blocked beads, infra diagnostics, and tune proposals.
     Bare `loom inbox` and `loom inbox list` are read-only;
     `loom inbox view` renders a numbered, bead-addressed, or proposal-addressed
     item; and `loom inbox chat` launches the interactive resolution agent.
     There is no host-side pick/reply/resolve/apply path in v1.
   - `loom tune` — manual SkillOpt-style tuning surface. The command and
     proposal creation/isolation contracts are owned by
     [skills.md § Tune Command Surface](skills.md#tune-command-surface) and
     [skills.md § Tune Proposal Worktrees and
     Beads](skills.md#tune-proposal-worktrees-and-beads); this harness spec owns
     its workflow placement and integration with `loom inbox`.

   **Inspection** — read-only views over cache, bd state, and logs:
   - `loom status` — print the active work epic, any pending
     `loom:todo` work epic, cached iteration counts, and cache health;
     it does not report or depend on an active-spec pointer
   - `loom logs` — inspect, render, or tail persisted event logs;
     the event-log surface and flags are owned by [events.md](events.md).
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
   | `loom todo --since` | deterministic todo discovery uses durable spec cursors rather than a caller-supplied base override |
   | `loom sync` | Askama-compiled workflow templates make per-project sync unnecessary |
   | `loom msg` | renamed/replaced by `loom inbox`; no compatibility alias |
   | `loom inbox -c` / `loom inbox --chat` | chat is the `loom inbox chat` subcommand |
   | `loom inbox -d` / `loom inbox --dismiss` | resolution happens through `loom inbox chat`; no host-side dismissal path |
   | `loom inbox pick` / `loom inbox reply` / `loom inbox resolve` | options are chat context, not a host-side executable menu |
   | `loom inbox apply` | tune proposals may be applied only through `LOOM_APPLY` emitted by `loom inbox chat` and executed by the trusted driver |

2. **Compiled templates with consumer-composable typed building blocks** —
   Askama engine, per-phase templates, partials, and per-phase pinning
   policy live in [templates.md](templates.md). The crate that
   builds them (`loom-templates`) is one of the workspace crates enumerated above.
   `loom-templates` is **public-contract**: it exposes its typed context
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
   labels or missing runtime variants fail at dispatch as static
   `loom:infra` diagnostics (no silent default, no transport retry).
   `--profile` overrides bead labels.
6. **Bead dispatch** — `loom loop --parallel N` (alias `-p N`) dispatches
   up to N ready beads, each in its own clone of the loom workspace
   under `.loom/beads/<id>/` on a per-bead branch. The operator's
   `/workspace` is never the bead's workdir. `--parallel 1` (default)
   runs one bead at a time; `--parallel N > 1` runs N concurrently.
   Before each bead-worker dispatch, Loom preserves any dirty bead
   workspace work in an unapplied recovery stash, rebases committed bead
   work onto the current integration tip when possible, and exposes the
   stash/alignment result through loop-only `workspace_recovery` prompt
   context. After workers finish, the driver fetches each bead branch
   from its bead workspace path into the loom workspace, then rebases +
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
   verdict gate after the agent emits its terminal marker. Per-bead
   `loop` workers use the decision table above; review uses the
   review-specific terminal handling documented there. For implementation
   beads, the driver then runs deterministic post-integration verification
   (`loom gate verify --diff <pre-integration-head>..HEAD`) before the
   bead's integration is durable. LLM review is not part of the default
   per-bead hot path;
   the worker prompt requires self-review, and authoritative LLM review
   runs at molecule completion over the actual push range. See
   [Verdict Gate](#verdict-gate) for the execution layer (decision table,
   recovery mechanics, markers, labels) and [gate.md](gate.md) for the
   review rubric. Driver-detected gate failures and `LOOM_RETRY` self-
   reports enter a bounded recovery loop; `LOOM_BLOCKED` and direct
   loop/todo `LOOM_CLARIFY` self-reports escalate directly to the human
   via `loom inbox`. The verdict gate applies to **worker sessions only**
   (`loop`, `todo`, `review`); interactive sessions (`plan`, `inbox`)
   are agent-and-human authoritative — the driver does not mutate bd
   state as a consequence of an interactive session. See [Verdict Gate §
   Interactive vs worker sessions](#verdict-gate) for the full
   no-reconciliation contract.
9. **Push gate — consume the gate-owned receipt.** Harness owns the
   molecule-completion orchestration: require resolved molecule state,
   synchronize the integration branch with origin, resolve the actual
   push range, execute deterministic pre-push verification followed by
   review, and route integrity findings through molecule remediation.
   It then supplies the resulting handoff evidence to `GateSuccess::new`.
   The receipt's evidence set, matching rules, structural seal, and
   rejection conditions are owned only by
   [gate.md § Gate success receipt](gate.md#gate-success-receipt).
   A rejected receipt becomes `GateOutcome::Fail` and refuses the push;
   an accepted receipt is the only path to marker minting and the clean
   push branch. Together with FR1's epic worker filter, this keeps a
   clean epic close unreachable without a gate-authorized receipt.

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
    per-command tables (e.g. *Inbox Modes*, *Tune Modes*, the
    event-log flags in [events.md](events.md), FR1 scope-flag lines) ↔
    declared `#[arg(...)]`; (3) **Removed
    surface** — the `Removed` table is absent from the binary; (4) **Grouping
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
15. **`loom-llm` public-contract crate** — typed multi-provider
    LLM primitives + `Conversation` with built-in tool-use loop +
    agent-loop observers. Surface, dependency graph constraints,
    and observer behavior owned by [llm.md](llm.md).
    Loom-harness's role is the crate-graph placement
    (public-contract leaf, dep floor) — see *Crate Layout* and
    *Dependency Graph* above.
16. **`EventSink` trait and composition** — owned by
    [events.md](events.md). Sinks compose via chainable
    `.tee(other)`; the driver applies `react()` after every
    non-streaming event and processes returned
    `SessionCommand`s with `Abort` as terminal priority. The
    `EventSink` trait lives in `loom-events` so any AgentEvent
    consumer (Loom binary, external `loom-llm` `Conversation`
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
    `loom-protocol::todo` and validated for a final work-epic title plus
    the preflight roster.
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
   `packages.loom` consumes `.bin`; the test-tier and full-suite composition
   is owned by [tests.md — Nix Integration](tests.md#nix-integration). Binary
   is included in the devShell.

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
- **Runtime override of Loom's workflow templates** — Loom's `plan` / `todo`
  / `loop` / `review` / `inbox` templates are Askama, compiled into the
  binary. `loom tune phase` / `loom tune partial` may propose source edits in an
  isolated worktree, but there is no per-project runtime template-fetch or
  hot-override mechanism for Loom's own workflow templates. Project-specific
  prompt tweaks to Loom's workflow happen via `pinned_context`, `style_rules`,
  skills, and per-spec implementation notes. Consumers writing their *own*
  templates (for their own LLM calls via `loom-llm`) compose them from
  `loom-templates`' exposed typed building blocks — that path is supported and is *not* what this
  exclusion covers.
- **Runtime template engine for consumer overrides of Loom's
  workflow templates** — adding a runtime engine (e.g. `minijinja`)
  to allow consumers to drop in replacements for Loom's compiled
  Askama templates is bolt-on-able after the typed-context public
  surface lands and is deferred until a concrete consumer asks.
- **Prompt-size tuning for oversized initial prompts** — if a rendered
  phase prompt grows too large to re-pin after compaction, the fix is
  to tune the prompt/template/pinning surface separately. The compaction
  recovery path does not silently drop pinned instruction context to
  make room.
- **Observation daemon** — a polling monitor that spawns short-lived
  agent sessions to observe tmux / browser logs and create beads for
  detected issues. Independent of the workflow phase set; deferred to
  a follow-up spec if and when the use case re-emerges.
- **Preserve-on-GC for dirty closed bead workspaces** — routine closed-bead
  workspace cleanup stays simple. Loom preserves dirty work before worker
  dispatch via recovery stashes, but it does not add a special move-aside
  branch for the unusual case where a bead is already closed/reapable while
  its workspace still contains useful uncommitted work.
- **Session persistence across container restarts** — each container starts a
  fresh agent session.
- **Wall-clock infra cooldown/backoff** — v1 infra resilience uses
  round-robin retry with an attempt cap, not timer-based sleeps. Adding
  exponential backoff later is an additive scheduler policy if evidence shows
  it is useful.
