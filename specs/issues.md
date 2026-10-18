# Issues

Defines backend-neutral task operations and adapters in an extraction-ready
crate, without owning Loom's workflow policy.

## Problem Statement

Workflow behavior and most tests must not depend on starting a native issue
tracker database. Loom needs interchangeable tracking backends, neutral worker
operations, and a lightweight test implementation without losing real backend
semantics or durable workflow attribution.

## Architecture

A separate public-contract workspace crate, `loom-issues`, owns the typed
tracking boundary and its adapters. It is designed for eventual extraction into
its own repository: its public contract and adapter implementations do not
import Loom's workflow, driver, gate, templates, or disposable cache. Consumers
supply configuration and transport dependencies without requiring a Loom
checkout. Public dependencies must retain that extraction boundary.

Beads is the production backend. A lightweight non-Beads implementation supplies
fast isolated tests without a database service or subprocess per fixture. Other
production adapters may follow later; interchangeability includes production
composition and worker access, not just substituting a test runner behind bd
argv.

[Harness](harness.md#crate-layout) owns workspace composition;
[Loop](loop.md#task-acceptance-at-dispatch), [Todo](todo.md),
[Findings](findings.md), and [Inbox](inbox.md) retain acceptance, scheduling,
remediation, and human authority. The tracker stores their records without
interpreting them as permission. [Workspaces](workspaces.md#bead-dispatch) owns
sandbox access and launch mounts; [Tests](tests.md#agent-first-execution) owns
execution locality and suite composition.

Acceptance: [boundary](#boundary-and-adapters),
[conformance](#backend-conformance), and [workflow use](#workflow-consumption).

### Tracking contract

[Acceptance](#boundary-and-adapters).

The public boundary uses checked opaque identities, typed task records and
updates, explicit relationships, and reason-carrying errors. It covers creation,
lookup/query, targeted updates to content/status/labels/metadata, closure,
parentage, bonds, dependency graphs, readiness, progress, and the persistence
and synchronization operations needed by consumers. Stable identities and
relationships survive adapter round trips; spelling does not infer ancestry.
Unknown or unsupported states and relationships fail explicitly rather than
silently becoming another status, losing an edge, or selecting ready work.

Updates preserve fields outside their declared scope. Typed error/outcome
surfaces distinguish rejection/conflict, unsupported guarantees, transport
failure, and an uncertain mutation outcome. Cancellation or timeout does not
assert rollback. Backend durability, visibility, conditional-update and
synchronization guarantees are explicit; an unsupported transaction or revision
guard cannot masquerade as success. Unsupported capabilities do not silently
weaken a consumer's required safety condition.

Raw backend bytes and argv stay in adapters. Domain consumers do not match bd
stderr, assemble bd commands, or reach a concrete client through an escape
hatch. Backend-specific operator tools may remain available outside that
portable surface. Workflow-owned metadata stays candidate data until its owning
authority resolves/adopts it; storing a value does not make it trusted
attribution.

### API development policy

The public Rust API is intentionally unstable during initial development.
Callers and external adapter implementations may need coordinated changes;
[stable compatibility commitments](#out-of-scope) are deferred until interfaces
settle. This does not relax the [current tracking contract](#tracking-contract)
or workflow authority boundaries.

### Workflow and worker use

[Acceptance](#workflow-consumption).

Production workflows and their tests consume the same tracking contract,
including worker-facing task tools, prompts/guides, queue queries, graph and
readiness checks, recovery, and publication synchronization. Worker operations
resolve the configured backend without teaching native bd commands as the
portable workflow interface. Access remains limited by phase and task authority;
backend substitution grants no additional mounts, credentials, host execution,
or admission rights. Existing Beads-specific operator usage need not acquire a
bd-compatible facade for other backends.

Acceptance resolution, ownership, scheduling, decision waits, deduplication,
replay, and publication policy remain in their real Loom consumers above this
boundary. Tests substitute storage/transport, not canned admission or scheduling
answers. Canonical workflow journals and protected attribution remain distinct
from backend projections and disposable indexes. The adapter reports actual
backend limits; consumers reconcile or refuse work where those limits prevent a
safe transition, rather than assuming all writers honor an advisory lock.

Synchronization is explicit and backend-selected. With Beads, workers and the
driver continue using shared authoritative state; there is no per-task Dolt
push/pull. Publication retains the ordering and failure handling owned by Loop,
without hard-coding a Beads subprocess into backend-neutral orchestration.

### Lightweight implementation and conformance

[Acceptance](#backend-conformance).

The lightweight implementation is in-process, isolated per fixture, and usable
without Beads, Dolt, or their binaries/services. Ordinary workflow fixtures do
not initialize a native database merely to test policy. The implementation
supports actual task/graph/metadata behavior, deterministic faults and
before/after-write interruption, including uncertain outcomes and stale writers.
It does not invent stronger transactions, visibility, revision checks, or
readiness rules to make tests pass. Tests of guarantees specific to an adapter
exercise that adapter rather than generalizing from the lightweight backend.

One shared conformance suite exercises the common contract against the
lightweight implementation and isolated pinned-Beads fixtures. Adapter-specific
tests retain native graph restrictions, readiness, status/metadata behavior,
error translation, timeouts and uncertain writes. Keep a small real-backend
assembly/synchronization smoke surface; the lightweight implementation cannot
prove bd interoperability. Local isolated native tests should also be
agent-runnable wherever their required capabilities can be supplied safely;
using a database or subprocess does not itself make a test host-only.

## Success Criteria

### Boundary and adapters

- The dedicated public loom-issues crate and adapters compile for an external
  fixture consumer without Loom runtime/orchestration dependencies or a Loom
  checkout; production composition selects Beads through this boundary.
  [test?](issues_external_consumer_preserves_extraction_boundary)

- Both implementations round-trip identities, records, targeted field updates,
  parentage, bonds, queries, dependency graphs, readiness, progress, closure,
  and declared synchronization behavior through the same typed contract.
  Unknown/unsupported values fail without lossy mapping or invented authority.
  [test?](issues_shared_contract_round_trips_operations)

- Actual adapter failures distinguish rejected/conflicting, unsupported,
  transport, and uncertain outcomes; timeout/cancellation and partial mutations
  never imply rollback or stronger atomic/revision guarantees.
  [test?](issues_adapters_preserve_failure_and_guarantee_semantics)

### Backend conformance

- Lightweight fixtures use no Beads/Dolt binary, database startup or subprocess,
  and exercise real state/graph operations with isolated deterministic state.
  [test?](lightweight_tracking_fixtures_need_no_native_database)

- Shared conformance runs against both lightweight and isolated pinned-Beads
  backends, including targeted metadata preservation, graph/readiness rules,
  stale writes, interrupted operations and uncertain outcomes. Backend-specific
  differences remain explicit rather than silently strengthened by the fake.
  [test?](tracking_conformance_covers_lightweight_and_pinned_beads)

- Isolated Beads adapter and assembly tests exercise real argv/JSON, native
  graph guards, shared-state visibility and synchronization failures; they do
  not infer production interoperability from in-process conformance alone.
  [test?](beads_tracking_adapter_retains_native_interoperability_coverage)

### Workflow consumption

- Production Loop, Todo, Findings and Inbox paths, including restart/recovery,
  use interchangeable tracking operations while retaining their real admission,
  ownership, dedup, scheduling and publication decisions above the adapter. The
  lightweight backend exercises those paths, not replacement policy stubs.
  [test?](workflow_tracking_substitution_retains_production_policy)

- Real worker tool/prompt composition uses backend-neutral task operations;
  substituting a backend does not require bd compatibility or change phase,
  sandbox, protected-attribution or human-authorization boundaries.
  [test?](worker_tracking_tools_are_backend_neutral_and_authority_scoped)

- Production publication uses backend-selected synchronization with existing
  ordering/error behavior; Beads retains shared authoritative worker state and
  no per-task Dolt push/pull, including interrupted/uncertain synchronization.
  [test?](tracking_publication_sync_preserves_backend_and_workflow_boundaries)

## Out of Scope

- Stable API compatibility or release-policy commitments during initial
  development. Those policies will be planned once interfaces settle; current
  backend correctness and workflow authority requirements remain in force.
- Implementing a second production backend or performing repository extraction
  now. The crate and production consumers must nevertheless be interchangeable
  and extraction-ready; these are not deferred to a hypothetical adapter.
- A bd compatibility layer for other backends, a repair DSL, or workflow policy
  embedded in a generic tracker.
- Claiming native transactions, compare-and-swap, rollback, cross-record
  atomicity or cooperating writers where the backend does not provide those
  guarantees.
- Granting execution, acceptance or publication authority from task storage,
  labels, parentage, a successful transport call or an in-memory test result.
- A broad benchmarking program, mandatory before/after report, initial host-test
  inventory, or numerical speed quota. Focused implementation-time measurements
  relevant to fixture/test speed are appropriate; expensive measurements remain
  scoped/on-demand rather than routine verification obligations.
