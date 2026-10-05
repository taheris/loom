# Loom platform

Defines shared crate layering, typed configuration and process boundaries,
bootstrap, and reconstructable platform state.

## Problem Statement

Defines shared crate layering, typed configuration and process boundaries,
bootstrap, and reconstructable platform state. This package is one contract
owner, not a new crate or command tree.

## Architecture

Inputs, outputs, and trust boundaries are stated in the contracts below. Related
owners: [workspaces](workspaces.md), [specs](specs.md), [loop](loop.md),
[todo](todo.md), [events](events.md), [templates](templates.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Process Architecture

[Acceptance](#process-architecture-1).

Loom is a host-side orchestrator. Agent-bearing worker phases receive an
isolated container for each dispatch, selected from the bead's workspace profile
and the phase's agent runtime. Loom delegates sandbox construction to
`wrix spawn`; it supplies typed launch configuration and consumes canonical
agent events over piped JSONL. It does not construct containers directly.

The trust boundary is stable: Loom, Git integration, gate decisions, and pushes
run on the host; agent tools run in the sandbox. Non-interactive worker sessions
use `wrix spawn --stdio`. Interactive `plan` and `inbox chat` follow
[Agent — Interactive Shell-Out](agent.md#interactive-shell-out): native terminal
paths use `wrix run`, while non-TTY Pi inbox chat uses the controlled
`wrix spawn --stdio` RPC bridge. Direct is rejected for interactive phases.

### Crate Layout

[Acceptance](#crate-layout-1).

The fixed workspace separates public contracts from internal orchestration:

- `loom` — CLI parsing and dispatch (internal).
- `loom-driver` — host configuration, state, locking, Git, Beads, and session
  support (internal).
- `loom-events` — canonical events, identifiers, `Session`, and `EventSink`
  (public contract).
- `loom-llm` — provider-neutral LLM and conversation primitives (public
  contract).
- `loom-skill` — skill artifacts, discovery, resolution, and materialization
  (public contract).
- `loom-tune` — tuning cases, evidence, scoring, and proposal metadata
  (internal).
- `loom-render` — event renderers and log sink (internal).
- `loom-agent` — Pi, Claude, and Direct backend adapters (internal).
- `loom-direct-runner` — sandboxed Direct conversation entrypoint (internal).
- `loom-gate` — verifier, review, marker, and finding implementation (internal).
- `loom-protocol` — shared subprocess wire protocols (public contract).
- `loom-workflow` — phase orchestration and lifecycle state machines (internal).
- `loom-templates` — typed contexts, partials, and compiled prompts (public
  contract).
- `loom-test-support` — shared test fixtures (internal, test-only).
- `loom-walk` — deterministic source/spec conformance walks (internal).

### Dependency Graph

[Acceptance](#dependency-graph-1).

Public contracts stay at the bottom of the graph. `loom-events` is the base
contract and imports no internal crate. `loom-protocol`, `loom-llm`, and
`loom-skill` build only on their documented public-contract dependencies;
`loom-templates` builds on `loom-events` and `loom-protocol`. They do not import
runtime orchestration.

`loom-render`'s dependency direction is owned by
[Events — Crate Boundaries](events.md#crate-boundaries). `loom-agent` consumes
events, LLM primitives, and materialized skills; `loom-direct-runner` composes
agent tools with the LLM conversation. `loom-tune` consumes events and skills.
`loom-gate` consumes events and protocol values. `loom-workflow` is the
orchestration top and may compose the internal crates. Runtime crates do not
depend on `loom-test-support` or `loom-walk`.

### Workspace Dependencies

[Acceptance](#dependency-graph-1).

Third-party versions are pinned once at workspace scope and inherited by member
crates. Public-contract crates retain narrow dependency floors so they can
version without importing host orchestration.

### Workspace Lints

[Acceptance](#crate-structure).

Lint policy is workspace-owned and inherited by every crate. The enforceable
rule set and override policy live only in
[`docs/style-rules.md`](../docs/style-rules.md).

### Parse, Don't Validate

[Acceptance](#configuration-and-beads-domains).

Raw CLI values, manifest JSON, Beads JSON, cache rows, and backend protocol
frames are parsed once at their boundaries. Downstream workflow code receives
typed domain values and canonical events rather than re-parsing strings.
Syntactic parsing and context-dependent resolution are distinct typed
transitions: well-formed identifiers alone do not establish that their
references resolve. Checked construction returns the richer immutable domain
value or a typed error, not a validation boolean leaving raw or partially
resolved data downstream. Snapshot-dependent facts are bound to that snapshot; a
type is not a claim that mutable external state remains valid forever.

`BeadId`, `SpecLabel`, `MoleculeId`, `ProfileName`, `SessionId`, `ToolCallId`,
and `RequestId` are transparent newtypes. `BeadId` validates its canonical shape
on construction and deserialization. `AgentRuntime` is a closed enum. Typestate
for subprocess sessions and the non-optional loop/gate outcome shapes make
invalid lifecycle transitions unrepresentable.

### Askama Template System

[Acceptance](#success-criteria).

[Templates](templates.md) owns the template engine, typed contexts, partials,
pinning policy, and public composition surface.

### Beads CLI Wrapper

[Acceptance](#beads-cli-wrapper-1).

The host interacts with Beads through a typed subprocess boundary. JSON output
is parsed into typed beads, molecules, labels, and progress; arguments are
passed without shell interpolation; errors and timeouts remain typed. The
workflow uses create, show, close, update, list, dependency, bond, and progress
operations against the shared Dolt service.

Beads and Git subprocess timeouts terminate and reap the direct child before
returning. On Unix, termination includes descendants in the command's process
group; deliberately detached processes are outside that boundary. Cancelling the
operation kills that group and delegates direct-child reaping to Tokio. Neither
timeout nor cancellation proves that earlier mutations were rolled back; effects
completed before termination may still be durable.

### SQLite Cache Store

[Acceptance](#cache-database).

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

## Loom-LLM

[Acceptance](#success-criteria).

[Llm](llm.md) owns `LlmClient`, typed completion and cache controls,
`Conversation`, tools, usage, and observers. Harness owns only crate placement,
configuration composition, and routing observer aborts into typed recovery.

## Configuration

[Acceptance](#configuration-and-beads-domains).

Loom reads `<workspace>/loom.toml`; `LOOM_CONFIG` may select another path.
Missing files and fields use typed defaults. The root file is the sole
configuration source, including gate runner blocks; `.loom/` contains runtime
state rather than hidden configuration.

Harness owns ingestion and precedence, not every setting's domain semantics.
Workspaces owns integration/profile/mount settings; Loop owns retry/iteration
budgets; Events owns log retention; Agent/LLM, Skills/Tuning, and Verify own
their respective backend, registry, and execution settings. Beads creation
defaults and the shared typed subprocess boundary remain here.

Phase values resolve from `[phase.<name>]`, then `[phase.default]`, then
built-in defaults. A loop bead's `profile:X` label precedes phase profile
defaults, while the CLI profile override has highest precedence. Default
configuration selects the base profile, Claude backend, ten work-epic
iterations, two worker retries, and three infra attempts. Beads creation
defaults remain priority 2 and type `task`.

Phase names (`default`, `plan`, `todo`, `loop`, `inbox`, `gate.review`) and
phase/agent fields are closed: misspellings fail ingestion, even in an unused
phase. Nested `[phase.gate.review]` and literal `[phase.'gate.review']` are
equivalent, but declaring both is an error. Profile, backend, thinking-level,
model, and provider values are checked during TOML or direct serde ingestion;
phase selection then only applies fallback. Backend-specific settings travel
with the selected backend, including CLI overrides. Model/provider registries
remain open, but names must be nonempty and contain no whitespace or control
characters.

A suppression contains exactly one nonblank `id` or `hash` and a nonblank
`reason`. Its checked Rust value cannot be mutated into an invalid combination.

Beads statuses are `open`, `in_progress`, `blocked`, `deferred`, `closed`,
`pinned`, or `tombstone`; issue types are `task`, `bug`, `feature`, `epic`, or
`chore`. Unknown external status/type values are rejected, not collapsed into
`other` or silently treated as an actionable state. Priorities are integers in
`0..=4`. Reads and create/update/list options use these same domain values;
string conversion happens at the subprocess boundary. Multi-status filters
retain their comma-separated wire representation; an empty filter means no
restriction. Extra fields in Beads responses remain forward-compatible. The
partial bead projection defaults omitted priority/type fields to P0/task; status
is required.

## Success Criteria

### `loom-protocol` crate

- The `loom-protocol` crate exists as a leaf workspace member with the `gate`
  module carrying every type listed above. The crate's dependencies are limited
  to `serde`, `serde_json`, `thiserror` / `displaydoc`, `blake3` (finding-hash
  crate — algorithm is implementer's choice per _Finding id, finding hash,
  suppression, and dedup_, but the dep set is closed), and `loom-events` (for
  `SpecLabel`); no transitive dependency on `loom-templates`, `loom-workflow`,
  or `loom-gate` [test](loom_protocol_crate_has_minimal_leaf_dependency_set)

- The crate's MAJOR version is the wire-format protocol version. A breaking wire
  change (renamed token, retyped target shape, removed enum variant) requires a
  major bump. Additive changes (new `ConcernToken` variant, new `FindingTarget`
  variant, new fields with `#[serde(default)]`) are minor bumps. No
  `"protocol": <n>` field appears on `LOOM_FINDING:` / `LOOM_CONCERN:` payloads
  — the typed parse errors give loud structural breakage on version skew
  [test](loom_protocol_wire_format_does_not_carry_protocol_version_field)

### Configuration and Beads domains

- Phase keys and known phase values reject typos at direct serde ingestion, not
  when a particular phase is selected
  [test](direct_serde_rejects_phase_typos_and_invalid_known_values)

- Nested and literal review tables resolve the same typed fallback and cannot
  both be declared
  [test](nested_and_literal_review_tables_have_identical_typed_fallback)

- Backend overrides carry only settings belonging to the selected backend
  [test](backend_override_carries_only_its_own_settings)

- Suppressions require one nonblank selector and a nonblank reason through
  direct serde as well as explicit construction
  [test](direct_serde_cannot_bypass_exclusive_nonempty_suppression)

- Beads statuses/types preserve recognized wire values
  [test](beads_domains_preserve_recognized_wire_values)

- Unknown Beads statuses/types are rejected rather than collapsed into another
  state [test](unknown_beads_statuses_and_types_are_rejected_not_collapsed)

- Beads and configuration priorities share the checked 0–4 domain while creation
  defaults remain P2/task
  [test](beads_and_configuration_priorities_share_the_checked_boundary)

- Typed list filters retain comma-separated and empty-filter semantics
  [test](typed_list_statuses_keep_csv_and_empty_filter_semantics)

- Model/provider names reject empty, whitespace-bearing, and control-bearing
  values [test](external_names_reject_empty_tokens_at_all_boundaries)

- Valid model/provider tokens preserve their wire spelling without closing
  external registries
  [test](external_names_preserve_forward_compatible_provider_tokens)

### Crate structure

<!-- prettier-ignore -->
- Workspace builds with `cargo build` from `loom/` root [check](cargo build --workspace)

<!-- prettier-ignore -->
- Target v1 crate set matches the fixed workspace members exactly: loom,
  loom-driver, loom-events, loom-llm, loom-skill, loom-tune, loom-render,
  loom-agent, loom-direct-runner, loom-gate, loom-protocol, loom-workflow,
  loom-templates, loom-test-support, and loom-walk [check](cargo run -p loom-walk -- crate_structure_includes_loom_tune)

<!-- prettier-ignore -->
- Five public-contract crates declared in workspace manifest metadata:
  loom-events, loom-protocol, loom-llm, loom-templates, loom-skill; no other
  crate declares the marker [check](cargo run -p loom-walk -- public_contract_crates)

<!-- prettier-ignore -->
- Workspace uses edition 2024 and resolver "3" [check](cargo run -p loom-walk -- workspace_edition)

<!-- prettier-ignore -->
- All dependencies pinned under `[workspace.dependencies]` [check](cargo run -p loom-walk -- workspace_deps_pinned)

<!-- prettier-ignore -->
- All crates declare `[lints] workspace = true` [check](cargo run -p loom-walk -- workspace_lints)

<!-- prettier-ignore -->
- No `types.rs` or `error.rs` files at crate roots [check](cargo run -p loom-walk -- no_types_or_error_files)

<!-- prettier-ignore -->
- Domain identifiers use newtypes (BeadId, SpecLabel, MoleculeId, etc.)
  [check](cargo run -p loom-walk -- newtype_identifiers)

<!-- prettier-ignore -->
- No `unwrap()`, `todo!()`, `panic!()`, `unimplemented!()`, `unreachable!()` in
  non-test code [check](cargo run -p loom-walk -- no_panics_in_production)

<!-- prettier-ignore -->
- No `#[allow(dead_code)]` in non-test code [check](cargo run -p loom-walk -- no_allow_dead_code)

<!-- prettier-ignore -->
- Event identities expose no sentinel `placeholder` constructors or
  `EventEnvelope::default` [check](cargo run -p loom-walk -- no_event_sentinels)

<!-- prettier-ignore -->
- No `derive(From)` or `derive(Into)` on newtype structs [check](cargo run -p loom-walk -- no_derive_from_on_newtypes)

### Process architecture

<!-- prettier-ignore -->
- Loom never invokes `podman run` directly (grep `crates/` for `podman` finds
  only documentation references) [check](cargo run -p loom-walk -- loom_does_not_invoke_podman)

- `SpawnConfig` JSON shape is stable: serialization round-trip preserves
  documented per-launch fields and key names, including `image_ref`,
  `image_source`, and `image_source_kind`, while omitting ProfileConfig-only
  host fields
  [test](spawn_config_omits_profile_manifest_host_only_fields_from_wrix_json)

<!-- prettier-ignore -->
- `wrix spawn` installs from `image_source` (a Nix store path) before invoking
  podman with `image_ref` as the ref; the selected ProfileConfig image digest
  lets wrix skip reloading bytes if the same content already exists in the local
  image store [system](nix run .#smoke)

- Per-bead profile/runtime selection: two beads with different profile labels or
  backend runtimes result in `wrix spawn` invocations with the matching
  `image_ref`, `image_source`, `image_source_kind`, and ProfileConfig
  [test](per_bead_profile_runtime_dispatch_produces_distinct_image_refs)

- Loom reads `LOOM_PROFILES_MANIFEST` at startup and parses it into
  `BTreeMap<ProfileName, BTreeMap<AgentRuntime, ImageEntry>>`; missing env var
  or missing file errors before any bead spawn
  [test](from_path_missing_file_returns_manifest_not_found)

- A bead with `profile:X` where `X` is not in the manifest fails with a typed
  `ProfileError::UnknownProfile` naming the missing profile
  [test](lookup_unknown_profile_carries_manifest_path)

- A resolved backend runtime missing under an existing profile fails with a
  typed profile-manifest error naming the profile and runtime
  [test](lookup_missing_runtime_for_profile_carries_profile_and_runtime)

- `--profile` CLI override takes precedence over bead labels
  [test](cli_override_swaps_resolved_image)

- `loom plan` shells out to interactive `wrix run` with inherited stdio, and
  surfaces unsuccessful launcher status
  [test](plan_inherits_stdio_and_reports_nonzero_launcher_status)

### Dependency graph

<!-- prettier-ignore -->
- `loom-events` is a leaf crate — no internal deps on `loom-driver` /
  `loom-render` / `loom-workflow` / `loom-templates` / `loom-llm` / `loom-agent`
  / `loom-skill` / `loom-tune` [check](cargo run -p loom-walk -- loom_events_is_leaf)

<!-- prettier-ignore -->
- `loom-llm` depends on `loom-events` only (no `loom-driver` / `loom-agent` /
  `loom-workflow` / `loom-skill` / `loom-tune` import) [check](cargo run -p loom-walk -- loom_llm_deps)

<!-- prettier-ignore -->
- `loom-templates` depends on `loom-events` and `loom-protocol` only among
  internal crates (no `loom-driver` / `loom-llm` / `loom-agent` /
  `loom-workflow` / `loom-skill` / `loom-tune` import) [check](cargo run -p loom-walk -- loom_templates_deps)

<!-- prettier-ignore -->
- `loom-skill` depends on `loom-events` but not `loom-driver` / `loom-agent` /
  `loom-templates` / `loom-tune` / `loom-workflow` [check](cargo run -p loom-walk -- loom_skill_deps)

<!-- prettier-ignore -->
- `loom-tune` depends on `loom-events` and `loom-skill`, but not `loom-driver` /
  `loom-agent` / `loom-workflow` [check](cargo run -p loom-walk -- loom_tune_deps)

<!-- prettier-ignore -->
- `loom-agent` depends on `loom-llm`, `loom-events`, and `loom-skill`; its
  `direct` backend wraps `loom-llm::Conversation` [check](cargo run -p loom-walk -- loom_agent_deps)

### Workflow commands

<!-- prettier-ignore -->
- The surface-conformance walk hard-fails when the binary's surface drifts from
  FR1 (command set, flag set, removed surface, grouping order) and exits 0 when
  spec and binary agree. Wired as a `[check]`-tier verifier under
  `loom gate check` [check](cargo run -p loom-walk -- surface_conformance)

- Bare `loom` (no args) renders the same Workflow / Inspection / State grouped
  sections (in spec order) as `loom --help`, `loom -h`, and `loom help` — clap's
  flat default-help fallback is not produced for any top-level invocation
  [test](loom_help_groups_workflow_inspection_state_in_order)

### Auxiliary commands

- `loom init` creates `<workspace>/loom.toml` (or `$LOOM_CONFIG` when set) and
  `.loom/cache.db` with the default cache schema
  [test](run_creates_config_and_cache_db)

- `loom init --rebuild` drops and repopulates `.loom/cache.db` from durable
  sources: the spec index, canonical spec packages, bd spec/work epics, and each
  package contract's `## Companions` section. It also folds gate
  criterion-status storage into the unified cache; there is no
  `.loom/gate-cache.sqlite`. Gate evidence views remain derived from
  [Evidence-owned durable records](evidence.md#retained-safety-evidence);
  rebuilding the cache cannot erase or reset that safety history.
  [test?](rebuild_drops_and_repopulates_cache_db_from_packages)

### Cache database

- `CacheDb::open` creates `.loom/cache.db` tables on first open (`specs`,
  `spec_epics`, `work_epics`, `companions`, `notes`, `criterion_status`, and
  `meta`) [test](cache_db_init_creates_tables)

- Cache reconstruction keeps spec cursors separate from work-epic range anchors:
  todo batches use `loom.todo_head`, and pending batches retain their
  fingerprint independently of the closed spec metadata carriers.
  [test](rebuild_discovers_closed_spec_carriers_and_independent_work_batches)

- Cache reconstruction uses a remediation batch's own `loom.base_commit` without
  fabricating spec metadata from the work epic.
  [test](rebuild_preserves_remediation_anchor_without_fabricating_spec_epic)

- Malformed durable metadata fails reconstruction without inheritance or Beads
  writes. [test](rebuild_rejects_malformed_metadata_instead_of_inheriting_it)

- `CacheDb::rebuild` resets work-epic iteration counters to 0
  [test](cache_rebuild_resets_work_epic_counters)

- Corrupted cache file → `loom init --rebuild` recovers from durable sources or
  reports the exact durable inconsistency; it never treats cache loss as clean
  todo state [test](cache_corruption_recovery_never_implies_clean_todo)

- `loom note set <label> --kind <k> --json '[…]'` is atomic —
  `DELETE WHERE spec_label=? AND kind=?` plus N `INSERT`s in one transaction;
  partial failure leaves the prior set intact
  [test](notes_set_replaces_atomically)

- `loom note add <label> --kind <k> --text "…"` appends a single row to `notes`
  [test](notes_add_then_list_chronological)

- `loom note rm <id>` deletes by primary key
  [test](notes_rm_removes_one_row_by_id)

- `loom note list [<label>]` returns rows for the spec/kind pair (default kind:
  `implementation`) ordered by `id` ascending (chronological); `--all-kinds`
  widens to every kind and includes the `kind` column in output
  [test](notes_add_then_list_chronological)

- `loom note clear <label>` deletes rows for the spec/kind pair (default kind:
  `implementation`); `--all-kinds` wipes every kind for the spec in one
  statement [test](notes_clear_kind_only_or_all_kinds)

- `--kind` defaults to `implementation` on every subcommand that accepts it, so
  `loom note add my-spec --text "…"` is the common-case shorthand
  [test](notes_kind_defaults_implementation)

- `loom init --rebuild` drops and recreates the `notes` table — no notes survive
  a rebuild, regardless of `kind` [test](rebuild_drops_all_notes)

- `notes.spec_label` is declared with `ON DELETE CASCADE`; an explicit
  `DELETE FROM specs WHERE label = ?` removes the notes in the same statement.
  No routine command takes that path today — this verifies the FK clause itself
  [test](notes_cascade_on_spec_delete)

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

<!-- prettier-ignore -->
- Loom binary builds via `nix build` [system](nix build .#loom)

<!-- prettier-ignore -->
- Loom binary is available in the hook-free CI devShell [system](nix develop .#ci -c loom --version)

### Unit tests

- Newtype serde round-trip tests cover all ID types (`BeadId`, `SpecLabel`,
  `MoleculeId`, `ProfileName`, `SessionId`, `ToolCallId`, `RequestId`)
  [test](serde_round_trips_as_plain_string)

- Closed-set enum tests cover `AgentRuntime` parse/serde and reject unknown
  runtime strings before manifest lookup or Wrix spawn
  [test](agent_runtime_parse_serde_rejects_unknown_values)

- Cache database round-trip tests cover spec rows, spec epics, work epics,
  notes, and criterion evidence without any `current_spec` operation
  [test](cache_db_round_trips_specs_epics_notes_and_criteria)

### Property-based testing

- Arbitrary spec bodies do not corrupt the cache schema during rebuild
  [test](rebuild_never_corrupts_schema)

- Arbitrary corrupt cache bytes recover through `recreate`
  [test](recreate_recovers_from_arbitrary_bytes)

- Cache rebuild round-trips generated durable source shapes
  [test](rebuild_round_trips_known_shapes)

### Architecture

- Interactive plan and inbox launches reject the Direct backend before dispatch.
  [test](interactive_shell_out_rejects_direct_backend)

- TTY Pi inbox chat uses native wrix run with inherited terminal streams rather
  than the non-TTY RPC bridge.
  [test](inbox_chat_pi_tty_uses_native_wrix_run_with_inherited_stdio)

- Non-TTY Pi inbox chat uses the controlled wrix spawn RPC bridge.
  [test](inbox_chat_runs_pi_backend_through_controlled_bridge)

### Crate Layout

- Cancelling a subprocess operation kills its Unix process group and eventually
  reaps the direct child; it does not imply rollback of completed mutations.
  [test](subprocess_cancellation_kills_group_and_eventually_reaps_child)

- A subprocess timeout kills the Unix process group and reaps the direct child
  before returning; deliberately detached processes are outside that boundary.
  [test](subprocess_timeout_kills_group_and_reaps_child)

- Git subprocess timeouts terminate descendants in the process group before
  returning. [test](git_timeout_terminates_descendants_before_returning)

- Beads subprocess timeouts terminate descendants in the process group before
  returning. [test](bd_timeout_terminates_descendants_before_returning)

## Requirements

### Canonical contract location

[Acceptance](#loom-protocol-crate).

**Crate scope.** `loom-protocol` is single-purpose: cross-crate wire protocols
Loom emits or consumes. Its `gate` and `todo` protocols are owned by
[Findings](findings.md#canonical-contract-location-1) and
[Todo](todo.md#todo-success-marker). Future protocols may use sibling modules
without importing runtime orchestration. Domain specs own their wire shapes and
anti-drift contracts; Harness owns this shared dependency and versioning policy.

**Dependency direction.** Leaf crate. Depends on `serde` + `serde_json` (JSON
wire), `thiserror` / `displaydoc` (error types), `blake3` (the finding-hash
crate; see _Finding id, finding hash, suppression, and dedup_ — algorithm is
implementer's choice, but the dep set is closed), and `loom-events` for
`SpecLabel`. No Askama, no bd client, no template prose — those live one layer
up. `loom-templates`, `loom-workflow`, `loom-gate`, and the loom CLI all depend
on `loom-protocol`; `loom-templates` re-exports the `gate` module's public types
via `pub use` so existing `PreviousFailure::ReviewConcern { findings }`
construction works without consumers touching the dependency graph.

**SemVer = wire format stability.** The crate's MAJOR version is the protocol
version. A breaking wire change (renamed token, retyped target shape, removed
enum variant) requires a major bump; consumers opt in via Cargo. Additive
changes (new `ConcernToken` variant, new `FindingTarget` variant, new fields
with `#[serde(default)]`) are minor bumps. No `"protocol": <n>` field appears on
the wire — the existing typed `FindingParseError::Json` (which carries the serde
unknown-variant error verbatim) and `FindingParseError::TokenVariantMismatch`
give loud, structural breakage when a consumer's protocol-crate version doesn't
match the loom binary it spawns. Cargo + Cargo.lock pinning coordinates the two
halves of the pipeline; no per-line versioning needed.

### Functional

1. **Command set** — commands fall into three groups that MUST be rendered as
   separate sections under those headings in `loom --help` output (in this
   order). Order within each group is as listed.

   **Workflow** — in execution order; each linked owner defines its command
   arguments and behavior:

   - [`loom plan`](plan.md) — specification interview.
   - [`loom todo`](todo.md) — changed-spec decomposition.
   - [`loom loop`](loop.md) — worker execution and integration.
   - [`loom gate`](gate.md) — verification, review, and publication admission.
   - [`loom inbox`](inbox.md) — human resolution and diagnostics.
   - [`loom tune`](tuning.md) — manual artifact tuning.

   **Inspection** — read-only views over cache, bd state, and logs:
   - `loom status` — print the active work epic, any pending `loom:todo` work
     epic, cached iteration counts, and cache health; it does not report or
     depend on an active-spec pointer
   - `loom logs` — inspect, render, or tail persisted event logs; the event-log
     surface and flags are owned by [Events](events.md).
   - `loom spec` — query spec annotations; supports `--deps` to print nixpkgs
     required by the spec's `[check]` / `[test]` / `[system]` / `[judge]`
     verifier targets, and `loom spec <label> --targets` to print annotation
     targets (`--tier <tier>` narrows; `--plain` prints exact target strings for
     piping)

   **State** — workspace lifecycle and cached state:
   - `loom init` — create `.loom/` config + `.loom/cache.db`. `--rebuild` drops
     and repopulates the cache from the spec index, spec files, bd spec/work
     epics, and each spec's `## Companions` section. The cache is
     non-authoritative; hot correctness paths re-read durable Git/Beads inputs
     and, for verification admission,
     [Evidence-owned records](evidence.md#retained-safety-evidence).
   - `loom use <label>` — legacy active-spec selector retained for
     compatibility; deterministic `loom todo` does not read it, and `loom loop`
     defaults from `loom:active` work-epic state instead.
   - `loom note` — manage spec notes

   The single-line help text for every command follows CLI-1: one short sentence
   describing current behavior, no implementation details / migration history /
   decision references / bead ids. The binary has no `loom doctor` subcommand;
   its absence is part of the surface contract (the surface audit flags
   reintroduction).

   **Removed surface.** The table below lists user-facing surface explicitly
   removed from the binary — both top-level subcommands and flags on retained
   commands. The surface-conformance walk (registered under `loom gate check`)
   parses it and hard-fails if any listed surface element resurfaces.

   | Surface                                                       | Removed because                                                                                                         |
   | ------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
   | `loom doctor`                                                 | replaced by `loom gate <subcommand>` per-tier dispatch                                                                  |
   | `loom check`                                                  | renamed to `loom gate <subcommand>` per [Gate](gate.md)                                                                 |
   | `loom run`                                                    | renamed to `loom loop` (current name describes the iteration shape)                                                     |
   | `loom todo --since`                                           | deterministic todo discovery uses durable spec cursors rather than a caller-supplied base override                      |
   | `loom sync`                                                   | Askama-compiled workflow templates make per-project sync unnecessary                                                    |
   | `loom msg`                                                    | renamed/replaced by `loom inbox`; no compatibility alias                                                                |
   | `loom inbox -c` / `loom inbox --chat`                         | chat is the `loom inbox chat` subcommand                                                                                |
   | `loom inbox -d` / `loom inbox --dismiss`                      | resolution happens through `loom inbox chat`; no host-side dismissal path                                               |
   | `loom inbox pick` / `loom inbox reply` / `loom inbox resolve` | options are chat context, not a host-side executable menu                                                               |
   | `loom inbox apply`                                            | tune proposals may be applied only through `LOOM_APPLY` emitted by `loom inbox chat` and executed by the trusted driver |

### Functional

3. **SQLite cache store** — workflow cache persisted in `.loom/cache.db`
   (renamed from `.loom/state.db`). Tracks indexed spec rows, spec/work epic
   mirrors, criterion evidence cache, companions, iteration counters, and
   implementation notes. It is reconstructable or disposable:
   correctness-sensitive decisions use Git + Beads/Dolt metadata + current spec
   files/index, with verification admission consulting
   [Evidence-owned records](evidence.md#retained-safety-evidence) rather than
   treating evidence-cache loss as empty safety history. There is no
   `current_spec` pointer. `loom:active` is a bd label on the default work epic
   for `loom loop`, not cache state and not a todo-discovery input.
4. **Beads integration** — interacts with beads via the `bd` CLI (subprocess
   calls). Bead operations: create, show, close, update, list, dep add, mol
   bond, mol progress. CLI output parsed into typed Rust structs.

### Functional

13. **Surface conformance** — the surface-conformance walk (registered as a
    `[check]`-tier verifier dispatched by `loom gate check`) audits the binary's
    user-facing surface against this spec, hard-failing on any drift across four
    dimensions: (1) **Command set** — FR1's commands ↔ the `Command` enum's
    variants; (2) **Flag set** — flags documented in the spec's indexed owners'
    per-command tables (including Inbox, Tuning, Gate, and Events), not a
    duplicated Harness-only flag registry ↔ declared `#[arg(...)]`; (3)
    **Removed surface** — the `Removed` table is absent from the binary; (4)
    **Grouping order** — both `loom --help` AND bare `loom` render `Workflow:` /
    `Inspection:` / `State:` in FR1's declared order. Help-text wording is _not_
    a dimension — CLI-1 style is enforced by `loom gate review`'s style-rule
    walk. The audit exists because an earlier multi-bead molecule closed despite
    cross-component drift that the success-criteria walk did not catch.

### Functional

15. **`loom-llm` public-contract crate** — typed multi-provider LLM primitives +
    `Conversation` with built-in tool-use loop + agent-loop observers. Surface,
    dependency graph constraints, and observer behavior owned by [Llm](llm.md).
    Loom-harness's role is the crate-graph placement (public-contract leaf, dep
    floor) — see _Crate Layout_ and _Dependency Graph_ above.

### Non-Functional

1. **Style.** All loom crates follow
   [`docs/style-rules.md`](../docs/style-rules.md). The architectural
   commitments specific to loom — newtype IDs at parse boundaries,
   parser-to-stamper split, `Session` trait as public surface (with
   subprocess-driving backends keeping their typestate as internal mechanic),
   workspace-scope lints, single-source-of-truth verdict gate function — are
   described in the _Architecture_ sections above; this NFR commits to the
   team-wide style rules as a whole.
2. **Required newtypes** — `BeadId`, `SpecLabel`, `MoleculeId`, `ProfileName`
   for domain identifiers; `SessionId`, `ToolCallId`, `RequestId` for protocol
   identifiers. No bare `String` for typed IDs. `AgentRuntime` is an enum (`Pi`,
   `Claude`, `Direct`), not a newtype.
3. **Nix integration** — built via the wrix Rust package builder.
   `packages.loom` consumes `.bin`; the test-tier and full-suite composition is
   owned by [Tests — Nix Integration](tests.md#nix-integration). Binary is
   included in the devShell.

#### loom-driver

[Acceptance](#success-criteria).

- Newtype construction and serde round-trips (`BeadId`, `SpecLabel`,
  `MoleculeId`, `ProfileName`)
- `CacheDb` schema creation on first open
- `CacheDb` query methods return typed rows (`SpecRow`, `SpecEpicRow`,
  `WorkEpicRow`, criterion-evidence rows)
- `CacheDb::rebuild` populates from the spec index, spec files, and mock `bd`
  output
- Companion-section parser: spec with a `## Companions` section containing two
  backtick-delimited paths yields two `companions` rows; spec without the
  section yields zero
- Parser ignores: text outside backticks on a bullet line, blank bullets,
  multi-path bullets (skipped with warn, not error)
- Parser is case-sensitive on the heading: `## companions` (lowercase) and
  `## Companion paths` are not recognized
- No `current_spec` / `set_current_spec` API exists
- `increment_iteration` for a work epic returns updated count, starts at 0
- `bd` CLI output parsing (JSON → typed structs)
- `bd` CLI error mapping (exit codes → error variants)
- `bd` CLI wrapper passes every argument via `Command::arg()` — never shell
  interpolation. Tests inject values containing shell metacharacters
  (`; rm -rf /`, `` `id` ``, `$(whoami)`) and assert they reach `bd` literally
  as one argv element each, never expanded
- Config file loading (TOML parsing into `LoomConfig`), defaults when file is
  absent or fields are missing

#### Auxiliary commands (loom-workflow)

[Acceptance](#success-criteria).

- `loom init` writes a default `loom.toml` and creates `.loom/cache.db` with the
  expected schema (specs, spec epics, work epics, companions, notes, criterion
  status, meta tables)
- `loom init` is idempotent: running twice does not clobber existing notes or
  cache rows that can still be validated against durable state
- `loom init --rebuild` drops and repopulates cache rows from the spec index,
  canonical spec packages, mock bd spec/work epics, and companions; iteration
  counters reset to 0

### Functional

- CLI surface: `loom --help` lists every v1 command (`plan`, `todo`, `loop`,
  `gate`, `inbox`, `tune`, `spec`, `init`, `status`, `logs`, `note`)

## Out of Scope

- **Agent backend implementations** — defined in [Agent](agent.md).
- **Parallelism beyond clone-per-bead** — `loom loop --parallel N` dispatches
  one bead clone per bead in parallel. New parallelism strategies (cross-spec,
  distributed, scheduler-aware) are future work.
- **Hidden specs (`-h` flag)** — scratch / private specs are not a first-class
  concept. The use case — keeping a spec out of git — is covered by
  `.git/info/exclude` on `specs/<label>/`. Eliminating the flag keeps `plan` /
  `todo` / `loop` path-resolution single-shaped. Reintroducing it later is a
  non-breaking additive change if the workflow asks for it.
- **Workflow-template override policy** — owned by
  [Templates — Out of Scope](templates.md#out-of-scope).
- **Prompt-size tuning for oversized initial prompts** — if a rendered phase
  prompt grows too large to re-pin after compaction, the fix is to tune the
  prompt/template/pinning surface separately. The compaction recovery path does
  not silently drop pinned instruction context to make room.
- **Observation daemon** — a polling monitor that spawns short-lived agent
  sessions to observe tmux / browser logs and create beads for detected issues.
  Independent of the workflow phase set; deferred to a follow-up spec if and
  when the use case re-emerges.
- **Preserve-on-GC for dirty closed bead workspaces** — routine closed-bead
  workspace cleanup stays simple. Loom preserves dirty work before worker
  dispatch via recovery stashes, but it does not add a special move-aside branch
  for the unusual case where a bead is already closed/reapable while its
  workspace still contains useful uncommitted work.
- **Session persistence across container restarts** — each container starts a
  fresh agent session.
- **Wall-clock infra cooldown/backoff** — v1 infra resilience uses round-robin
  retry with an attempt cap, not timer-based sleeps. Adding exponential backoff
  later is an additive scheduler policy if evidence shows it is useful.
