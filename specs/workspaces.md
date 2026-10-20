# Workspace isolation

Defines checkout isolation, repository Git authority, launch profiles, mounts,
locking, and preserved worker work.

## Problem Statement

Concurrent agents must not corrupt integration state, lose dirty work, or write
host-owned safety evidence. Workspace isolation provides the checkout, launch,
and locking boundaries that workflow phases rely on.

## Architecture

Operator, integration, bead, and tune checkouts have distinct roles; launch and
mount policy preserves their authority boundaries. Related owners:
[harness](harness.md), [agent](agent.md), [loop](loop.md), [tuning](tuning.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Bead Dispatch

Rust source-feedback readiness is checked before agent startup. Required
verification remains owned by the trusted driver.

[Acceptance](#bead-dispatch-1).

Loom operates from a dedicated integration clone rather than the operator's
checkout:

```text
<workspace>/                    operator checkout
<workspace>/.loom/integration/  host-side integration clone
<workspace>/.loom/beads/<id>/   persistent per-bead clone
```

Each bead clone has a self-contained Git directory and branch `loom/<id>`, so
its `/workspace` container mount remains usable without exposing the integration
clone. A clone persists across retries and loop invocations until trusted
[cleanup admission](#cleanup-admission), not merely Beads closure. Startup and
post-attempt cleanup use the same admission boundary for clones owned by the
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
`loom/<id>` ref is removed on every exit path. Integration conflicts receive one
worker recovery attempt before clarification.

A bead container mounts its clone at `/workspace`. Tracking access is selected
by the configured [Issues adapter](issues.md#workflow-and-worker-use), not an
unconditional Beads dependency. The production Beads adapter mounts its
authoritative Dolt socket at `/workspace/.wrix/dolt.sock`; non-Beads test use
requires no native socket/database. A configured shared sccache directory is an
additional mount. Compatible beads share that worker cache, not one cache per
bead; their Cargo targets remain separate. Cache keys retain source, toolchain
and configuration distinctions: a shared pool does not promise hits for every
unit or authorize consumption by trusted builds. Backend selection and
acceptance admission cannot expand mounts, credentials or execution authority;
proposed host/expanded-authority experiments require
[Inbox approval](inbox.md#acceptance-repair-and-execution-authority).
`SpawnConfig` owns these per-launch mounts;
[Agent — SpawnConfig](agent.md#spawnconfig) owns their wire shape.

### Cargo target seeding

[Acceptance](#cargo-target-seeding-1).

Loom alone owns bead-target seeding, implemented by the internal `loom-shared`
crate before worker launch. `[loom] cargo_target_reuse` is an enum:

- `"off"` disables reuse and donor registration.
- `"copy-on-write"` (default) reuses dependency libraries only through
  independent filesystem clones; unavailable CoW falls back to ordinary Cargo.
- `"allow-hardlinks"` prefers CoW but explicitly permits writable dependency
  hard links as fallback. Both the recipient and donor's recorded initialization
  policy must permit hard links; otherwise only CoW may reuse that donor.

Unknown values and booleans fail configuration parsing. No Wrix API, service,
shared writable target, compiler wrapper or external seeding tool is introduced.
Private tool-cache policy (`[tool_caches] shared = false` in the bead's
`wrix.toml`) disables seeding and donor use regardless of the enum. Missing Rust
manifests/lockfiles or unsupported layouts leave normal Cargo behavior
available.

The source-local seeding API initializes only an absent ordinary `target/` in a
direct bead clone under this operator's `.loom/beads/`. Native worker dispatch
uses the same transfer policy with private generated targets under the
operator's `.loom/cargo-artifacts/worker/<bead>/<context>/<generation>/target`.
Existing targets, including empty directories and symlinks, are never replaced
or merged. New dispatches allocate independent generations; prior output remains
available for compatible dependency seeding and is never silently discarded.
Donors are other direct bead clones with matching Loom-recorded worker image
identity, Cargo.lock, root manifest, toolchain and Cargo configuration.
Compatibility records are cache hints, not provenance or verifier results; donor
inputs are rechecked before use. Operator, integration, recovery, verification
and installed runtime storage are never donors. Native Darwin artifacts must not
seed Linux workers.

Only dependency units identified by external registry/git packages in Cargo.lock
and their matching Cargo fingerprints are seeded. Ambiguous names shared with
local/path packages are excluded. Mutable fingerprints, dep-info and external
build-script outputs are copied privately with timestamps preserved. External
`.rlib`/`.rmeta` libraries use filesystem cloning when supported (including APFS
on Darwin), or explicitly permitted same-filesystem hard links on ext4. Without
hard-link permission, a failed clone discards staging and continues with another
donor or a cold target; libraries are never silently hard-linked or privately
copied. Other dependency files are private copies; workspace/path artifacts,
top-level executables and incremental caches are omitted. CoW clones have
independently writable inodes. Hard links deliberately allow participating
workers to modify each other's linked library bytes, even without donor mounts;
this is not isolation against a malicious worker. All modes still reuse
worker-produced bytes, never verifier evidence. Trusted host builds and
canonical executables must never consume these worker-writable artifacts.

Seeding holds the donor's recognized Cargo profile locks nonblockingly; busy,
missing or unknown locks refuse reuse. Symlinks and special files in selected
input trees refuse reuse rather than redirecting reads or writes. A unique
staging directory is published atomically without replacing a concurrently
created target. Failed staging is cleaned only by its owner; process death may
leave inert staging directories which are neither targets nor donors. No
collector or new checkout-disposal authority is added. Missing/incompatible/busy
donors and seeding failures produce diagnostics and fall back to normal Cargo;
retries preserve their existing target. Cargo still checks freshness and every
required verifier still runs.

Policy, failure, locking, recovery and actual multi-checkout Cargo behavior run
inside Rust/Nextest without a host bridge. The native clone syscall is a narrow
filesystem-capability boundary: deterministic fixtures cover supported,
unsupported and failed operations without claiming physical CoW sharing. Only
actual APFS/shared-mount behavior through the Darwin Wrix launch substrate
requires that platform; ordinary policy tests are not deferred to it. Focused
on-demand measurements report Cargo freshness, advisory build time and
inode-aware allocated library bytes; ordinary acceptance has no speed quota and
does not infer CoW physical sharing from file sizes or allocated block sums.

### Cargo execution storage

[Acceptance](#cargo-storage-isolation).

Loom assigns generated Cargo targets by trust domain, checkout identity,
immutable worker profile, toolchain and Cargo configuration, with a driver-owned
generation. Worker target projections never expose trusted driver/verification
targets or installed runtime roots. Legacy or unknown targets remain untouched;
a changed context allocates another namespace and retry preserves existing owned
output. Worker dependency seeding retains its existing policy and bilateral
hard-link permission, not verifier authority.

Compatible workers share the configured bounded sccache pool across beads.
Private Wrix tool-cache policy disables shared-cache projection and dependency
seeding. The selected profile's effective wrapper and cache destination must
agree with the admitted mount. Trusted commands use separate targets and either
a separate trusted compiler-cache backend or explicitly disabled compiler
caching; ambient worker cache directories, sockets and wrappers cannot select
trusted execution inputs. Trusted output resides under the existing per-user
driver state root (`loom/trusted-cargo`), outside source checkouts and worker
projections. The source checkout's canonical executables are not ordinary worker
output.

### Generated Cargo retention

[Acceptance](#generated-cargo-retention-1).

Only driver-created generated namespaces are collection candidates. The
controlling transient state database records their current filesystem identity,
context, generation and trusted lifecycle ownership; missing or conflicting
records confer no disposal authority. Source/Git/ref/stash/log/note/dependency
state, runtime roots and admitted evidence are never candidates. Neither Beads
closure, a worker-written flag, age alone nor an absent/reused PID proves
inactivity.

Admission accounts generated bytes and outstanding reservations with
configurable size/age/free-space thresholds. Active, held, pinned or unknown
generations remain protected under pressure. An unavailable large reservation
returns infrastructure unavailability while independently admissible work may
continue. Accounting failures and ambiguous ownership are diagnostics, not zero
usage.

A trusted lifecycle lease is necessary but insufficient after process loss.
Restart reconciles owned process/cgroup generations before making output idle;
an unavailable substrate observation retains the generation, even if its lease
is free. No backend may infer container death merely from launcher exit. Only
confirmed inactive, unheld, unpinned namespaces may enter quarantine. Collection
fences allocation, records intent, rechecks identity and protection before
unlinking, refuses symlink/path substitution, and resumes interrupted quarantine
conservatively. It never removes a whole source clone or resets safety state.

### Cleanup admission

[Acceptance](#bead-dispatch-1).

Removing a selected work-root's closed source clone requires either trusted
acceptance of its current work or explicit human-authorized disposal of that
workspace. Accepted `Complete` includes successful integration verification;
accepted `Noop` includes Loop's intentional no-work reconciliation. A marker,
agent-written metadata, or `status=closed` alone is not cleanup authority.

Provisional, rejected, or unreconciled closure leaves the clone, branch, dirty
work and recovery stashes intact. Independent human holds prevent automatic
cleanup; cancellation or decision closure alone does not authorize discarding
work. Recheck current ownership, holds and disposition before removal. Explicit
disposal is a separate authorized action, not a workaround that completes work,
closes decisions, or rewrites their relationships.

Startup reconstructs eligibility from trusted current state/evidence, not an
in-memory result or disposable cache assertion. Missing, stale, conflicting or
ambiguous authority preserves the clone with a visible diagnostic and unchanged
human state. Restart/GC neither reopens a held source nor turns rejected
completion into acceptance. Eligible cleanup leaves independently queued
decision beads and their history intact.

### Gate safety-history isolation

[Acceptance](#safety-history-isolation).

Workspace preparation resolves the operator/integration checkout association
used by
[Evidence's local safety history](evidence.md#local-workspace-safety-history).
Host Gate invocations through either checkout reach the controlling transient
`.loom/state.db` through the same evidence-admission boundary. Host evidence
records are not worker-writable through container mounts or direct write
interfaces; a worker cannot replace, erase or append trusted observations by
editing its clone or reporting another workspace identity. Deterministic worker
feedback remains available without host-state write authority. There is no
external safety registry or separate canonical witness store; cold recovery
invalidates missing evidence and requires fresh current checks. This association
does not expose the integration checkout or its marker to workers; Gate retains
publication authority and Evidence owns history admission.

### External workspace membership

[Acceptance](#external-membership).

An optional host-side membership adapter groups the operator, integration and
execution checkouts for an external consumer. It is not execution, evidence or
cleanup authority: native checkout checks, the controlling `.loom/state.db`,
worker isolation and invocation-local inode rechecks remain authoritative. There
is no persistent native association registry or external safety-history store.

`loom init --workspace-associator <absolute-program>` explicitly selects or
replaces the adapter. `--no-workspace-associator` explicitly disables it.
Omission retains the previous selection; genuine absence preserves standalone
behavior. Selection belongs to the operator's direct private Git storage, not
tracked configuration, Git configuration or worker-provided environment.
Integration invocations locate the controlling operator's selection. Workers
cannot select or execute host adapters. Unsafe objects, malformed/unreadable
selection, unavailable programs and interrupted selection staging fail closed.
An accepted selection is durably published under the workspace initialization
lock before admission; failed initialization does not undo it or silently become
standalone. Interrupted staging is retained for explicit inspection.

Loom directly executes the selected program as the host user, from the operator
checkout, with `--operator <absolute-path> --checkout <absolute-path>`. Stdin is
closed, runtime is bounded to 30 seconds and each captured output pipe to 16
KiB. Nonzero exit, signal, timeout, excess output or uncertain process cleanup
stops preparation. On timeout or capture failure, owned process groups are
terminated and the direct child reaped. The adapter must leave no background
work on ordinary exit; deliberately detached descendants remain its
responsibility. Diagnostics include bounded adapter stderr, which must not
contain secrets.

The adapter confirms an already registered operator, derives its external
identity from that registration, and atomically associates an unregistered
target or confirms matching membership. It refuses unknown operators and
conflicting membership without enrollment or rebind. Operator-equals-target is
confirmation-only, although supported location refresh may write existing
registration. Paths locate participants; path, remote or readable-name equality
cannot establish membership. The consumer owns membership serialization,
participant validation around writes, stable host-user store/tool selection and
idempotent retry. Exit zero is the only response; no identity payload or receipt
is execution authority. A timeout may follow committed membership, which Loom
does not roll back. No transaction across independent stores is claimed.

Operator confirmation precedes integrated initialization. Existing integration
admission precedes native-state initialization/rebuild and Git reconciliation;
fresh integration admission follows necessary cloning but precedes rerere, hooks
and native-state creation. Fresh/reused bead admission precedes identity,
policy, hooks, mount preparation, dirty-work stashing and fetch/alignment. Tune
clone admission precedes checkout and identity configuration. Rejection retains
clones, dirty work and recovery evidence; directory existence never establishes
successful admission. Native participant and selection observations are checked
around adapter execution. Cleanup does not call the adapter or acquire external
authority.

An intact operator tree may move with its nested integration, bead clones and
native state. New invocations resolve the current layout; old invocation
capabilities fail recheck. Reused bead preparation repairs the retained absolute
`origin` to the current integration location after membership admission. Moving
integration independently outside the required layout is not supported. Native
checks do not prove cross-invocation continuity against copies or replacements;
external continuity remains the consumer's responsibility. Host association and
configured selection require direct Git directories, not `.git` pointer
interpretation. Existing bead linked-worktree/indirect/shared-storage refusal is
unchanged. See
[the integration guide](../docs/external-workspace-membership.md).

### Repository Git Isolation

[Acceptance](#bead-dispatch-1).

Every Loom command that launches Wrix defaults to repository-scoped Git
authority: `plan`, `loop`, `todo`, `inbox chat`, proposal tuning, and the
review, audit, judge, rubric, and mint gate paths. Before selecting beads or
launching `wrix run` / `wrix spawn`, each entry point resolves both deploy and
signing keys using Wrix precedence: an explicit `WRIX_DEPLOY_KEY` or
`WRIX_SIGNING_KEY` must be an absolute path to an existing file; otherwise each
key falls back to `$HOME/.ssh/deploy_keys/<repo>-<host>` with the signing-key
suffix. If either key remains unresolved, Loom fails before querying Beads or
spawning an agent. Repository mode also rejects ambient `GIT_SSH_COMMAND` and
`GIT_SSH` overrides. Diagnostics identify `--host-key` as the sole ambient-host
opt-in.

After resolution, Loom runs
`wrix init repo --offline --no-hooks --key <key-name>` in the selected checkout,
exposing key paths only to that child. Loop startup selects `.loom/integration`;
other Wrix-bearing commands select their active workflow checkout. Wrix installs
context-stable repository-local transport and SSH-signing policy, including
`.git/wrix/allowed_signers`; Loom validates the expected configuration and
policy files and fails closed on a no-op or partial result.

New and reused bead clones receive the same policy before dirty-work stashing,
origin fetch, or host-side rebase. Context-stable helpers let the host and the
container use the repository key without persisting a host private-key path.
Repository mode treats a missing allowed-signers file as an error and verifies
both fetched and rebased commits.

The global `--host-key` option removes managed Wrix and legacy signing/transport
configuration from the selected checkout and permits ambient host Git policy for
all Wrix-bearing commands. Without that flag, Loom never falls back to the host
GPG key.

Resolved host key paths are applied to every `wrix run` child and travel to
`wrix spawn` only through `SpawnConfig.launcher_env` as `WRIX_DEPLOY_KEY` and
`WRIX_SIGNING_KEY`. That map is excluded from serialized spawn JSON; Wrix stages
fixed in-container paths. Independently, `loom init` enables Git rerere for
integration-conflict replay.

### Profile-Image Manifest

[Acceptance](#bead-dispatch-1).

The Nix-produced profile-image manifest maps `(ProfileName, AgentRuntime)` to an
image entry. Each entry carries the podman reference, immutable image source,
source kind, required host-only raw launcher and immutable Wrix profile
configuration paths, and an optional digest path. Loom parses the manifest once
from `LOOM_PROFILES_MANIFEST` and has no implicit search fallback.

Dispatch resolves the CLI profile override or bead `profile:X` label together
with the phase backend, then looks up the pair. Missing profiles or runtimes are
static `loom:infra` diagnostics that name the requested and available values.
Non-interactive launches place the selected values in `SpawnConfig`; interactive
launches exec the selected raw launcher with
`--profile-config <selected-profile-config> run <workspace> <agent-command>`.
Configured wrappers are not an image-selection mechanism.

Worker profiles retain their tool packages, including Nix. Nix evaluation and
additive builds use Wrix's private single-user container store, without a host
Nix daemon socket or host-execution bridge. The Rust profile also supplies Cargo
and rustc.

Every manifest entry requires `launcher` and `profile_config`; older/custom
manifests missing either field fail ingestion with regeneration guidance.

### Concurrency & Locking

[Acceptance](#concurrency--locking-1).

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

[Acceptance](#concurrency--locking-1).

Every managed container receives `LOOM_INSIDE=1`. Under that marker, Loom
rejects container-spawning, workspace-mutating, LLM-review, mint, inbox, and
tune commands before dispatch. Read-only `status`, `logs`, and `spec` commands,
plus deterministic gate inspection such as `loom gate verify`, remain available
for worker feedback.

## Success Criteria

### Cargo storage isolation

- Generated worker targets have private context/generation ownership while
  preserving legacy targets and retry output. Profiles cannot project another
  worker's target namespace or trusted targets/runtime roots.
  [test](worker_cargo_storage_isolates_contexts_and_preserves_retry)

- Actual worker preparation projects the owned Cargo generation and the
  configured shared compiler cache, omits shared-cache projection and seeding
  under private tool-cache policy, and retains dependency-reuse permissions.
  [test](worker_cargo_target_reuse_honors_policy_before_spawn)

- Trusted subprocess configuration selects separate output roots and disables
  compiler caching. Worker cache directories, server endpoints and wrapper
  overrides cannot select trusted build inputs.
  [test](trusted_cargo_execution_rejects_worker_cache_inputs)

- Actual native Cargo builds in independent worker and trusted sources produce
  distinct application results without overwriting a canonical executable.
  [test](trusted_and_worker_cargo_builds_cannot_overwrite_canonical_outputs)

- Real Wrix profile projection exposes only the admitted generated target and
  routes the configured compiler cache to its bounded backend. Private profiles
  cannot reach forbidden shared pools, and no worker mount exposes trusted
  output/runtime roots. This is runtime mount/backend substrate conformance, not
  a repeat of portable Cargo correctness or policy scenarios.
  [test?](wrix_cargo_storage_projection_matches_admitted_profile)

### Generated Cargo retention

- Accounting and reservation admission cover only recorded generated namespaces;
  size, age and free-space policy protects active, held, pinned and unknown
  generations, preserves every source/evidence/runtime root, diagnoses failed
  accounting and permits independently admissible smaller work.
  [test?](cargo_retention_bounds_protect_independent_work)

- Process death and restart never convert a free lease or missing/reused PID
  into collection authority. Real surviving build processes retain their
  namespaces until their exact owned process/cgroup generation is confirmed
  inactive; unknown platform observations retain with diagnostics.
  [test?](cargo_retention_restart_preserves_surviving_processes)

- Quarantine and removal fence allocation and recheck trusted ownership,
  filesystem identity, holds and pins. Actual generated fixtures exercise
  adversarial paths/symlinks, interrupted collection, conservative recovery and
  protected source/evidence/runtime sentinels through Loop/Gate lifecycle entry
  points, without collector flags authored by workers.
  [test?](cargo_retention_quarantine_rechecks_ownership)

### Concurrency & locking

- `plan.lock`, `todo.lock`, and `<bead-or-epic-id>.lock` files are created
  outside the workspace and released on process exit
  [test](phase_and_work_root_locks_create_expected_files)

- Two mutating commands for the same phase/work root serialize: the second waits
  up to 5s, then errors clearly naming the held root
  [test](second_acquire_times_out_with_work_root_busy)

- Independent work-root commands run concurrently when they address different
  bead/epic ids [test](different_work_root_locks_do_not_block)

- Read-only commands (`status`, `logs`, `spec`) acquire no lock and run during
  an active `loom loop` [test](readonly_paths_unaffected_by_work_root_lock)

- `loom init` and `loom init --rebuild` acquire the workspace lock and error
  immediately if any plan/todo/work-root lock is held
  [test](acquire_workspace_errors_when_phase_or_work_root_lock_held)

- Crashed loom process leaves no stale lock (kernel releases flock on exit; new
  invocation acquires immediately) [test](crash_releases_work_root_lock)

- Lock files live under `$XDG_STATE_HOME/loom/locks/<workspace-     basename>/`
  (default `~/.local/state/loom/locks/<basename>/`); no lock files are created
  inside the workspace bind-mount [test](locks_outside_workspace)

- Removing the lock file from inside the bead container does not break mutual
  exclusion on the host (locks live outside the bind-mount; agent has no path to
  them) [test](container_cannot_rm_host_lock)

- Driver sets `LOOM_INSIDE=1` in every bead container's env via the
  `SpawnConfig.env` allowlist
  [test](spawn_config_env_includes_loom_inside_marker)

- With `LOOM_INSIDE=1`, driver/workspace-mutating or LLM-spawning subcommands
  (`loop`, `init`, `plan`, `todo`, `inbox`, `tune`, `loom gate mint`,
  `loom gate review`, `loom gate judge`, `loom gate rubric`, and
  `loom gate audit`) refuse with a clear error
  [test](mutating_and_llm_spawning_subcommands_refuse_with_loom_inside_set)

- With `LOOM_INSIDE=1`, read-only/deterministic inspection subcommands
  (`status`, `logs`, `spec`, and deterministic `loom gate` subcommands such as
  `verify`) still run normally
  [test](readonly_and_deterministic_gate_subcommands_run_under_loom_inside_set)

### Safety-history isolation

<!-- prettier-ignore -->
- Production workspace resolution associates the operator and integration
  checkouts with the shared host-owned safety history while the actual worker
  launch keeps canonical records and replay artifacts non-writable. Worker
  file edits, reports, or claimed workspace identities cannot mutate that
  history; ordinary deterministic worker feedback still works without exposing
  the integration checkout or transferring marker authority.
  [system](nix run .#test-quint -- workspace-safety-history)

### External membership

- Public initialization confirms operator and fresh integration membership
  before native state creation.
  [test](external_membership_cli_selected_before_initialization_confirms_operator_and_integration)

- Unknown-operator refusal does not enroll it, and failed initialization retains
  the selected adapter.
  [test](external_membership_cli_unknown_operator_retains_selection_without_initializing_native_state)

- Public rebuild tracker failure retains explicitly selected integration rather
  than allowing the next invocation to fall back to standalone.
  [test](external_membership_cli_rebuild_tracker_failure_cannot_erase_explicit_selection)

- Selection after standalone initialization preserves existing native records.
  [test](external_membership_cli_selecting_after_initialization_preserves_native_records)

- Rejected integration admission precedes configuration/state changes and
  preserves dirty work.
  [test](external_membership_cli_rejected_integration_precedes_configuration_and_native_state)

- Malformed, unavailable and interrupted selections fail closed without
  consuming staging evidence.
  [test](external_membership_malformed_unavailable_and_interrupted_selection_fail_closed)

- Unsafe selection objects and configuration under a foreign workspace lock are
  refused.
  [test](external_membership_selection_refuses_unsafe_objects_and_foreign_lock)

- Public reinitialization cannot hide malformed selection; explicit disabling
  permits standalone initialization.
  [test](external_membership_cli_malformed_selection_cannot_be_hidden_by_reinit)

- Explicit disabling restores standalone execution preparation without running
  an adapter.
  [test](external_membership_explicit_disable_restores_standalone_preparation)

- Fresh/reused bead admission preserves dirty work through production recovery
  stashing.
  [test](external_membership_admits_fresh_and_reused_clones_without_losing_dirty_work)

- Conflicting independent membership is refused before configuration or
  stashing, without rebinding.
  [test](external_membership_refuses_independent_clone_before_configuration_or_stashing)

- Fresh clone rejection precedes identity, hooks and mount setup while retaining
  the clone for retry.
  [test](external_membership_rejects_fresh_clone_before_identity_hooks_or_mount_setup)

- Tune clone rejection precedes checkout and identity configuration.
  [test](external_membership_tune_clone_admission_precedes_checkout_and_identity_configuration)

- Public initialization of an intact moved tree retains native state and
  membership.
  [test](external_membership_cli_moved_operator_tree_retains_initialization_and_membership)

- Moved trees support reused/fresh bead preparation, with current origin repair
  and stale capability refusal.
  [test](external_membership_moved_pair_prepares_reused_and_fresh_clones_and_invalidates_old_capability)

- Linked-worktree refusal remains intact and occurs before target admission.
  [test](external_membership_preserves_linked_worktree_refusal_before_target_admission)

- The public worker guard prevents host selection configuration or execution.
  [test](external_membership_cli_worker_guard_prevents_selection_execution)

- Integration resolves controlling selection; workers cannot supply it and Git
  replacement rejects admission.
  [test](external_membership_checks_controlling_selection_and_rejects_git_replacement)

- Selection replacement during adapter execution rejects admission.
  [test](external_membership_detects_selection_replacement_during_admission)

- Native checkout cleanup neither invokes nor requires external membership
  policy.
  [test](external_membership_does_not_participate_in_native_checkout_cleanup)

- Adapter timeout terminates/reaps the owned group without undoing committed
  membership, allowing idempotent retry.
  [test](external_membership_timeout_reaps_group_preserves_committed_effect_and_allows_retry)

- Both adapter output pipes are bounded and interactive stdin is closed.
  [test](external_membership_bounds_both_output_pipes_and_closes_interactive_stdin)

- The fixture independently demonstrates unknown-operator/conflict refusal and
  retry semantics, not production identity security.
  [test](fixture_adapter_confirms_operator_refuses_conflicts_and_retries_idempotently)

### Cargo target seeding

- Cargo target reuse parses `off`, `copy-on-write` (default) and
  `allow-hardlinks` as a closed enum, rejecting booleans and unknown values.
  [test](cargo_target_reuse_parses_permissions_and_defaults_to_copy_on_write)

- The shared preparation path invokes Loom-owned seeding after workspace
  admission and profile resolution, before spawn; `off` and private-cache policy
  leave targets absent under either enabled policy.
  [test](worker_cargo_target_reuse_honors_policy_before_spawn)

- Disabled reuse does not initialize or register a target.
  [test](target_reuse_off_does_not_initialize_or_register_a_target)

- Default reuse never hard-links libraries: it creates independent CoW clones
  when supported or discards staging and initializes a cold target without
  privately copying libraries when CoW is unavailable.
  [test](target_reuse_defaults_to_independent_clones_or_cold_builds)

- A recipient's hard-link opt-in cannot make a CoW-only donor's libraries
  writable through shared inodes; such donors can only supply CoW clones.
  [test](target_reuse_hardlinks_require_donor_permission)

- Supported cloning is preferred under both enabled policies, creating
  independently writable recipient files.
  [test](target_reuse_prefers_independent_clones_even_with_hardlink_permission)

- Unsupported cloning permits library hard links only when both workers allow
  them; the full permission matrix runs independently of filesystem support.
  [test](target_reuse_unsupported_clone_respects_both_workers_permissions)

- Permission, interruption and other clone I/O errors cannot authorize a
  hard-link fallback merely because hard links were allowed.
  [test](target_reuse_clone_io_errors_never_authorize_hardlink_fallback)

- The fixture clone adapter and native syscall adapter preserve bytes and
  independent writes on success; native unavailability is explicit, not evidence
  of physical sharing.
  [test](clone_adapter_contract_keeps_successful_files_independently_writable)

- Interrupted clone operations discard staging and cannot publish partial files.
  [test](target_reuse_interrupted_clone_discards_staging_without_partial_publication)

- Actual process death before publication leaves inert staging, never a partial
  target or donor; an independent retry works without deleting the orphan.
  [test](target_reuse_killed_preparation_leaves_inert_staging_and_retry_works)

- A concurrently created destination wins over a completed seed without any file
  merge or loss of the destination's contents.
  [test](target_reuse_concurrent_destination_wins_without_partial_merge)

- Private-cache donors are excluded under both enabled reuse policies.
  [test](target_reuse_private_donors_are_not_used_by_either_enabled_policy)

- A bounded native fixture reports actual external-dependency freshness and
  inode-aware library accounting for off, CoW-only and hard-link-permitted
  modes, without requiring host access or asserting timing quotas.
  [test](target_reuse_measurement_reports_freshness_and_inode_aware_storage)

- Existing targets are preserved, and only compatible direct worker siblings are
  reused, excluding operator/integration/recovery storage and symlinked
  checkouts; changed image, lockfile or configuration selects a cold target.
  [test](target_seeding_preserves_existing_and_rejects_incompatible_donors)

- External dependency libraries share storage on ext4 while metadata remains
  private; workspace/path artifacts, executables, incremental state and
  ambiguous package names are omitted. Supported filesystem clones preserve
  independent writable inodes.
  [test](target_seeding_shares_only_external_libraries)

- Busy/unknown Cargo locks, symlinks, malformed input and interrupted staging
  cannot publish a partial target; publication preserves a concurrently created
  destination, and seeding failures leave normal Cargo available.
  [test](target_seeding_refuses_unsafe_donors_and_preserves_publication_races)

- Actual Cargo builds across independently writable checkouts execute divergent
  code, rebuild changed dependencies correctly, and continue after donor
  deletion, without modifying source donor library bytes through normal
  rebuilds. The same native fixture exercises APFS cloning on Darwin and hard
  links on ext4; unavailable platform runs are reported rather than counted as
  passes. [test](target_seeding_real_cargo_rebuilds_divergent_checkouts)

### Bead dispatch

- Production worker preparation selects backend-neutral tracking access with
  only the configured adapter's required environment/mounts; lightweight test
  use needs no Beads socket/service. Backend/task admission grants no extra host
  execution, mounts or credentials and preserves independent holds.
  [test?](worker_tracking_access_follows_adapter_without_authority_expansion)

- Shared worker preparation threads prompt context and owns scratch cleanup for
  every supported agent runtime
  [test](shared_worker_preparation_threads_context_and_owns_scratch_for_every_backend)

- Parallel redispatch preserves dirty tracked and untracked work in a recovery
  stash before cleanup, just like sequential dispatch
  [test](parallel_preparation_preserves_dirty_work_in_recovery_stash)

- `loom init` materializes the loom workspace at `.loom/integration/` (one-shot
  clone from origin) — the workspace is separate from the operator's
  `/workspace` [test](loom_init_materializes_loom_workspace)

- `loom init` configures `.loom/integration` with the canonical `wrix.prekHooks`
  `core.hooksPath`; it does not rely on the operator checkout's `.git/config`
  hook path
  [test](loom_init_configures_integration_hooks_path_from_wrix_prekhooks)

- `loom loop` never touches the operator's working tree at `/workspace`; all
  dispatch runs against the loom workspace
  [test](loom_loop_does_not_touch_operator_workspace)

- The integration branch is settable via `[loom] integration_branch` in
  `loom.toml` (default `main`); the loom workspace has that branch checked out
  and never switches [test](integration_branch_setting_honored_by_loop)

- `loom loop --parallel N` creates one bead workspace per dispatched bead under
  `.loom/beads/<id>/`, derived from the loom workspace via `git clone --local`;
  bead ids are globally unique so no spec partition appears in the path
  [test](bead_dispatch_creates_clone_under_loom_beads)

- Every created bead workspace is configured with the canonical `wrix.prekHooks`
  `core.hooksPath` before the agent receives it; if the hook path is missing or
  drifted on redispatch, the driver repairs it before spawning the container
  [test](bead_workspace_configures_and_repairs_hooks_path)

- Bead workspaces persist across attempts, recovery and loop invocations until
  trusted cleanup admission; provisional, rejected or unreconciled closure
  retains the clone, branch, dirty work and recovery stashes.
  [test?](bead_workspace_survives_until_trusted_cleanup_admission)

- Post-attempt cleanup requires a closed source with trusted accepted completion
  or explicit authorized workspace disposal. Current ownership/holds are
  rechecked; markers, agent-written metadata, cancellation or decision closure
  alone cannot authorize removal or alter decision relationships.
  [test?](post_attempt_cleanup_requires_trusted_completion_or_explicit_disposal)

- Pre-dispatch dirty bead workspaces are preserved before destructive cleanup:
  tracked modifications, staged changes, and untracked files outside the ignore
  set are saved with a named `git stash push     --include-untracked`; the
  driver records pre-stash status, stash selector/message, stash commit, and
  target integration tip
  [test](pre_dispatch_dirty_workspace_creates_recovery_stash)

- After a recovery stash is created, the driver leaves it unapplied, rebases
  committed bead work onto the current integration tip (or fast-forwards when no
  local commits exist), and injects `workspace_recovery` prompt context without
  consuming `[loop] max_retries`
  [test](workspace_recovery_stash_left_unapplied_and_context_injected)

- Clean pre-dispatch workspaces, and dirty workspaces after successful recovery
  stashing/alignment, still run the reset/clean path that preserves `target/`,
  `.git/`, and `.wrix/`
  [test](bead_workspace_prepare_preserves_target_and_dotwrix)

- If branch alignment conflicts after recovery stashing, the worker is
  dispatched in the conflict state with stash/conflict context rather than
  immediately routing to `loom:clarify` or `loom:blocked`
  [test](workspace_recovery_rebase_conflict_dispatches_agent_with_context)

- `LOOM_COMPLETE` is not rejected solely because a recovery stash still exists;
  stash relevance is judged by review/gate evidence rather than a hard driver
  state machine [test](loop_complete_does_not_require_recovery_stash_removed)

- Recovery-stash preflight emits `DriverKind::WorkspaceRecovery` with bead id,
  pre-stash status, stash selector/message, stash commit, integration tip,
  alignment outcome, and conflict files when present
  [test](workspace_recovery_event_records_stash_and_alignment)

- Fresh `loom loop` / `loom init` startup fast-forwards the loom workspace's
  integration branch to `origin/<integration-branch>` before a new bead clone is
  materialized, so fresh work branches off published HEAD
  [test](loop_start_fast_forwards_integration_to_origin_main)

- Unattributed local commits or remote divergence fail startup naming the
  divergent commits instead of branching fresh work off a stale base
  [test](loop_start_fails_loud_when_integration_diverged_from_origin)

- Loop restart retains a clean unpublished tip only when a protected pending
  continuation pins that exact tip, its source checkout identity is unchanged,
  and origin remains its ancestor. Ordinary signature verification and all
  required gates still run; the recovery record is not accepted coverage.
  [test](production_scheduler_resumes_exhausted_verification_after_restart_without_source_dispatch)

- After the startup fast-forward, a bead clone forks from published HEAD,
  carrying commits that landed on `origin/<integration-branch>` rather than the
  pre-reconciliation local base
  [test](bead_clone_branches_off_published_head_not_stale_base)

- Under the work-root advisory lock, startup GC removes only closed source
  clones owned by the selected molecule with reconstructed trusted cleanup
  admission, using the same eligibility boundary as post-attempt cleanup.
  [test?](startup_gc_requires_restart_safe_cleanup_admission)

- Missing, stale, conflicting or ambiguous cleanup authority preserves the clone
  with a diagnostic, not a cache-derived acceptance or human-state rewrite.
  Independent holds survive; live provenance-only decision children remain
  queued and closed decisions retain history independently of eligible source
  cleanup.
  [test?](workspace_cleanup_admission_preserves_unaccepted_held_and_ambiguous_work)

<!-- prettier-ignore -->
- Actual Loop restart and startup GC preserve closed-but-unaccepted or held
  sources with their branch, dirty files and recovery stashes. Trusted accepted
  completion or explicit authorized disposal permits only eligible source
  cleanup; live decisions remain visible and unrelated clones/human state remain
  intact. Evidence exercises actual temporary Git clones, isolated pinned Beads
  and production entrypoints, not a fabricated cleanup-eligible flag.
  [system?](nix run .#test-decision-waits)

- `loom loop` startup leaves closed bead workspaces from other molecules alone
  [test](loop_startup_gc_skips_closed_bead_workspaces_from_other_molecules)

- `loom loop` startup leaves bead workspaces alone whose bead is in any
  non-closed state [test](loop_startup_gc_skips_open_bead_workspaces)

- Each bead workspace's dispatch spawns its own `wrix spawn`; spawns overlap in
  wall-clock under `--parallel N > 1`
  [test](concurrent_spawns_overlap_in_wall_clock)

- Successful bead branches are fetched by the driver from the bead workspace
  path into the loom workspace, then rebased + fast- forwarded into the
  integration branch (linear history, no merge commits); the worker never
  invokes `git push` [test](driver_fetches_bead_branch_from_workspace_path)

- The bead-branch ref `loom/<id>` in the loom workspace is deleted
  unconditionally at the end of the per-bead critical section — clean exit,
  audit-fail rollback, and rebase-conflict abort all delete the ref
  [test](bead_branch_ref_deleted_on_every_exit_path)

- The bead clone's `origin` remote remains pointing at the loom workspace path
  after `create_worktree` so host-side ahead/behind tracking works; the bead
  container has no path mount to the loom workspace and cannot push from inside
  [test](bead_clone_origin_unchanged_under_a3)

- Parallel dispatch's second-and-later beads rebase onto the moved
  integration-branch HEAD before fast-forwarding
  [test](merge_branch_rebases_bead_branch_onto_head_before_ff)

- Driver-side rebase that conflicts textually aborts (`git rebase     --abort`)
  and routes the bead to recovery with cause `integration-conflict` carrying the
  conflict files and the new integration tip SHA
  [test](rebase_conflict_routes_to_integration_conflict)

- `integration-conflict` recovery dispatches the agent at most once before human
  escalation. A second conflict creates or reuses a dedicated decision child of
  the source, with the same cause, conflict paths and integration base. The
  source depends on that decision and enters
  [Loop's attributed wait](loop.md#decision-batches-and-attributed-waits), not
  the human queue itself; its clone remains intact and no further automatic
  conflict retry runs while the decision is unresolved.
  [test?](integration_conflict_one_retry_then_admits_dedicated_decision_wait)

- The driver-authored integration decision has one canonical Options brief with
  resolve-in-bead-clone and abandon-the-bead paths. Inbox records the authorized
  outcome; decision closure alone neither completes nor abandons the source.
  Existing failed-integration recovery owns any restoration of provisional
  worker-closed state before parking; this is not permission to reopen unrelated
  closed or human-held work.
  [test?](integration_conflict_decision_preserves_options_source_and_recovery_ownership)

- `loom init` writes `[rerere] enabled = true` and
  `[rerere]     autoupdate = true` into the loom workspace's local `.git/config`
  so the driver-side rebase replays previously- recorded conflict resolutions
  before falling through to `integration-conflict` recovery
  [test](loom_init_enables_rerere_in_loom_workspace_gitconfig)

- The driver-side rebase drives a rerere-replayed resolution to completion: when
  `rerere.autoupdate` auto-stages a recorded resolution and the rebase pauses
  awaiting `--continue`, the rebase is carried through (no remaining unmerged
  paths) rather than aborted, so a recorded resolution lands instead of falling
  to `integration-conflict` recovery
  [test](merge_branch_replays_recorded_rerere_resolution)

- The driver-side rebase (`rebase_onto_integration`) does not advance the
  integration branch — the fast-forward is a separate step
  (`ff_merge_integration`), so pass-2 signature verification runs on the
  rewritten commits before anything lands durably and a pass-2 failure leaves
  the integration line untouched
  [test](rebase_onto_integration_leaves_integration_branch_unmoved)

- The cross-spec rebase + ff critical section in the shared loom workspace is
  serialized by git's `index.lock`; a peer holding the lock makes the losing
  `rebase_onto_integration` / `ff_merge_integration` retry from its current view
  of the integration tip rather than surface a spurious conflict
  [test](rebase_onto_integration_retries_through_index_lock_contention)

- A stale loom-workspace `index.lock` that never clears exhausts the bounded
  retry budget and surfaces a typed `GitError::IndexLocked` naming the workspace
  (distinct from a content failure), instead of looping forever
  [test](rebase_onto_integration_surfaces_index_locked_on_stale_lock)

- Origin push of the integration branch retries non-fast-forward errors by
  fetching and re-rebasing onto `origin/<integration-branch>`
  [test](clean_review_reruns_loop_when_origin_push_races)

- Rebase abort, audit-fail rollback, signature-verification failure, agent
  failure, retry, tree-not-clean recovery, semantic block, or attributed waiting
  preserves the bead workspace. Loop owns the corresponding recovery/wait
  outcome; decision reporting alone neither makes the source a clarify item nor
  changes its independent completion outcome.
  [test?](workspace_preservation_follows_recovery_wait_and_independent_decision_outcomes)

- Actual isolated pinned-Beads operations permit independent worker-owned source
  closure while its provenance-only decision children remain live and visible.
  Any needed closure workaround preserves those children, relationships and
  human state; it cannot bypass genuine own prerequisites/holds or use blanket
  force, reparenting or child closure. Post-agent reconciliation still rejects
  false completion even if a Beads operation bypassed a mechanical guard.
  [test?](independent_source_closure_preserves_decisions_and_real_prerequisites)

- With the Beads adapter, bead containers receive the host `wrix-beads` Dolt socket as a single-file
  bind mount at `/workspace/.wrix/dolt.sock` via `SpawnConfig.mounts`, replacing
  the host-side hardlink shim previously used in `GitClient::create_worktree`
  [test](bead_container_dolt_socket_via_mounts)

- When `[loom] sccache_dir` is configured, the directory is bind-mounted into
  the loom workspace and every bead container at the configured container path
  [test](sccache_mount_present_when_configured)

- When `[loom] sccache_dir` is unset, no sccache mount appears on the bead
  container spawn args [test](sccache_mount_omitted_when_unset)

- Two independently cloned Rust beads receive the same configured writable
  sccache source and effective cache path, with both Rust wrapper environment
  settings aligned. Actual offline Cargo builds with separate targets reuse a
  compatible external dependency through that pool; changing application
  features rebuilds the library and preserves each clone's correct output.
  This native test translates only the mount projection; it does not certify
  deployed Wrix mounts or trusted-build isolation.
  [test](sccache_shared_across_bead_clones)

- Cache hits are observable across beads in a multi-bead loop when
  `[loom] sccache_dir` is configured
  [judge](../tests/judges/loom.sh#sccache_hits_visible_across_beads)

<!-- prettier-ignore -->
- `GitClient` is the only module that imports `gix` or invokes the `git` CLI;
  callers see typed Rust methods [check](cargo run -p loom-walk -- git_client_encapsulation)

- Every non-loop Wrix-bearing command requires both repository deploy and
  signing keys by default and invokes
  `wrix init repo --offline --no-hooks     --key <key-name>` in its active checkout
  before selecting beads or launching Wrix
  [test](all_non_loop_wrix_launch_surfaces_preflight_repository_policy)

- Default `loom loop` startup applies that policy in `.loom/integration` with
  the exact resolved paths before selecting beads
  [test](loom_loop_startup_initializes_repository_git_policy)

- If either repository key is unresolved, `loom loop` exits non-zero before
  querying Beads or spawning an agent, and the error names `--host-key` as the
  only ambient-host opt-in
  [test](loom_loop_missing_repository_keys_fails_before_bead_selection)

- Repository mode rejects ambient `GIT_SSH_COMMAND` / `GIT_SSH` overrides that
  would outrank its repository deploy-key transport
  [test](repository_mode_rejects_ambient_git_transport_override)

- New and reused bead clones receive context-stable Wrix transport/signing
  config before host-side preflight; no host private-key path is persisted, and
  stale host signing config is repaired on redispatch
  [test](create_worktree_applies_context_stable_wrix_git_policy)

- Loom validates Wrix's expected local config and policy files after init and
  rejects a successful no-op or partial policy rather than failing open
  [test](repository_policy_rejects_success_without_wrix_config)

- Repository mode treats a missing allowed-signers file as an error, never as
  permission to skip signature verification
  [test](repository_policy_does_not_skip_when_allowed_signers_disappears)

- `loom loop --host-key` clears managed Wrix and legacy signing/transport config
  from Loom-owned clones before permitting ambient host Git policy
  [test](host_key_policy_clears_managed_repo_config)

- The fallback keyname is derived as `<repo>-<host>` where `<repo>` is parsed
  from the origin URL (`github.com[:/]<user>/<repo>`) and `<host>` is
  `hostname -s`, matching Wrix key provisioning
  [test](signing_key_fallback_uses_wrix_repo_host_derivation)

- Driver-side rebase in the loom workspace produces signed commits whose
  `gpgsig` header is present in the commit object, without prompting for a
  passphrase [test](driver_rebase_signs_with_wrix_key)

- `git log --show-signature` against a driver-rebased commit in the loom
  workspace prints `Good "git" signature` using the configured allowed-signers
  file [test](rebased_commits_verify_via_derived_allowed_signers)

- In repository mode, the per-bead integration step runs `git verify-commit`
  against fetched commits (pass 1) and rebased commits (pass 2); failures
  distinguish worker-side from driver-side
  [test](integration_step_verifies_signatures_in_two_passes)

- `GitClient::launcher_key_env` surfaces both startup-resolved keys as
  `WRIX_DEPLOY_KEY` / `WRIX_SIGNING_KEY` host-path pairs for Wrix launchers
  [test](loom_loop_startup_initializes_repository_git_policy)

- Interactive `wrix run` dispatch applies both resolved key paths to the child
  environment [test](plan_threads_repository_keys_to_wrix_run)

- Bead dispatch threads the resolved launcher keys onto
  `SpawnConfig.launcher_env` and keeps them out of the in-container
  `SpawnConfig.env` allowlist
  [test](launcher_env_threads_onto_spawn_config_not_container_env)

- Review dispatch threads the checkout-resolved launcher keys onto the reviewer
  `wrix spawn` child process so host deploy/signing keys are available before
  container setup resolves git auth and SSH signing
  [test](loom_gate_review_threads_launcher_keys_to_wrix_spawn) The host-only
  serialization boundary for `SpawnConfig.launcher_env` is owned by
  [Agent — SpawnConfig](agent.md#spawnconfig). This spec owns how dispatch
  populates that validated host-only state:

- Each backend applies `SpawnConfig.launcher_env` to the `wrix     spawn` child
  process environment before exec
  [test](apply_launcher_env_sets_child_process_env)

### Bead Dispatch

<!-- prettier-ignore -->
- Missing or legacy native profiles are provisioned automatically from the
  registered operator's clean, signature-verified published Git source through
  its fixed profile factory. Cold starts and legacy manifests recover without
  changing selection or granting coverage. Missing/corrupt authority is neither
  reconstructed nor bypassed. [system](nix run .#test-profile-bootstrap)

- Provisioned profile inputs survive trusted driver subprocesses, including
  the review handoff. [test](provisioned_profile_inputs_survive_actual_driver_subprocess_environment)

- Profile factory output must be complete and identify an immutable store
  manifest. [test](factory_output_rejects_partial_results_and_mutable_manifests)

- The configured native startup wrapper initializes pinned Cargo patches before
  invoking Cargo. [test](configured_native_wrapper_initializes_patches_before_cargo)

- A declared legacy native profile is upgraded from the installed driver's
  explicit pinned closure without introducing profiles or runtimes. Explicit
  native overrides remain selected; readiness still grants no required coverage.
  [test](installed_native_closure_repairs_legacy_metadata_without_changing_selection)

- Installed native provisioning preserves an explicitly selected native image.
  [test](installed_closure_does_not_replace_an_explicit_native_profile)

- Rust implementation work requires a provisioned `native_preflight = "rust"`
  profile. The existing driver derives this from the current tracked Cargo
  project before starting its agent. The pinned image initializes native Cargo
  patches and actually probes Cargo, rustc, Clippy, Nextest, Nix, native project
  metadata and private Nix-state ancestry. Profile absence or observed missing
  capability is an infrastructure diagnostic; timeout/interruption uses the
  existing bounded infrastructure budget. The purity guard is preserved.
  Availability establishes source-feedback readiness, never completed required
  coverage or publication authority.
  [test](native_worker_preparation_probes_before_agent_and_routes_missing_capability)

- Native preflight capability failure is infrastructure rather than source
  failure. [test](native_preflight_capability_exit_is_static_infrastructure)

- Native preflight exit after agent output remains an interruption.
  [test](native_preflight_exit_after_agent_output_is_interruption)

<!-- prettier-ignore -->
- A real Rust worker image can evaluate Nix and build a disposable non-Git
  derivation offline in its private single-user store, without a daemon socket
  or host-execution bridge; Cargo and rustc also launch successfully.
  [system](nix run .#test-sandbox)

<!-- prettier-ignore -->
- The isolated transport verifier provisions a pinned profile, checks private
  Nix-state ancestry, and completes normal signed substitution plus two
  independent cold imports of the pinned input with matching hashes and sizes.
  Its fixed harness has no repository keys, runtime secrets, host Nix/engine
  sockets or canonical Beads/safety authority; unavailable or timed-out input
  transport fails within a bounded deadline and preserves the purity guard.
  [system](nix run .#test-worker-transport)

- Profile manifest entries missing launcher or immutable profile configuration
  fail ingestion with regeneration guidance.
  [test](manifest_rejects_missing_launcher_or_profile_config)

- Interactive planning selects the manifest-provided raw launcher and profile
  configuration without creating epics or changing Beads state.
  [test](plan_does_not_create_epic_or_touch_bd)

## Requirements

### Functional

1. **Profile/runtime selection** — reads `profile:X` labels from beads, resolves
   the phase backend to an `AgentRuntime`, and resolves the pair via the
   [Profile-Image Manifest](#profile-image-manifest). Unknown labels or missing
   runtime variants fail at dispatch as static `loom:infra` diagnostics (no
   silent default, no transport retry). `--profile` overrides bead labels.

2. **Backend-selected task access** — workers use neutral operations through the
   configured adapter. With Beads, the host's `wrix-beads` Dolt socket is
   mounted at `/workspace/.wrix/dolt.sock` via `SpawnConfig.mounts`; worker and
   driver operations see shared authoritative state with no per-task Dolt
   push/pull. This is the Beads adapter's transport, not a required dependency
   of other backends or their fixtures. The legacy `.beads/issues.jsonl` path is
   not used.

#### Concurrency & locking (loom-driver)

[Acceptance](#success-criteria).

- Lock tests execute the
  [harness-owned locking contract](#concurrency--locking). In-process tests
  cover typed selection and error mapping; process tests are reserved for actual
  flock contention and crash release.

#### GitClient (loom-driver)

[Acceptance](#success-criteria).

- Git integration tests execute the
  [harness-owned typed Git contract](#bead-dispatch) against temporary
  repositories. Process tests cover only behavior that cannot be exercised
  through an in-memory seam.

## Out of Scope

- Workflow retry/publication decisions belong to Loop and Gate. Agent session
  protocols belong to Agent; workspace isolation is not a new scheduler.
