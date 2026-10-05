# Test strategy

Defines shared test quality, deterministic fixtures, suite composition, and
assembled-system checks.

## Problem Statement

Defines shared test quality, deterministic fixtures, suite composition, and
assembled-system checks. This package is one contract owner, not a new crate or
command tree.

## Architecture

Inputs, outputs, and trust boundaries are stated in the contracts below. Related
owners: [verify](verify.md), [simulation](simulation.md), [agent](agent.md),
[harness](harness.md), [events](events.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Test Homes and Boundaries

[Acceptance](#success-criteria).

Rust tests use two complementary homes. Inline test modules cover private,
white-box behavior close to the owning code. Cargo integration tests exercise
public APIs, cross-module behavior, or real process boundaries when those
boundaries are load-bearing. A crate uses either or both homes according to the
surface it exposes; integration-test files are not required for leaf crates
whose contract is fully exercised inline.

The repository-level test infrastructure contains process fixtures, the Nix
verifier derivation, and the container smoke harness. Behavioral and protocol
tests use mock agents. The sole real-agent exception is an assembled-system
health check that launches the selected packaged Pi agent with `--version`
inside a network-disabled container without provider credentials; it sends no
prompt, performs no protocol turn, and makes no LLM API request.

Internal per-crate file and module organization remains an implementation choice
rather than part of this spec's contract.

### Annotation Contract

[Acceptance](#success-criteria).

Annotation syntax (`[check]` / `[test]` / `[system]` / `[judge]`), cardinality
rules (atomic acceptance, N→1 sharing, cross-spec sharing), and the
deterministic-vs-stochastic partition are defined in
[`docs/spec-conventions.md`](../docs/spec-conventions.md). The gate's resolution
mechanics — per-tier dispatch, batching for `[test]` and `[judge]`, runner
discovery, the `--files` scope model — live in [Gate](gate.md). This spec does
not duplicate those definitions.

What loom-tests owns: the **classification policy** for tests in this repo —
which tier each kind of test belongs to:

- Static analysis of Rust source (presence, absence, structural property across
  files) → `[check]`. The verifier is a Rust binary in `loom-walk` (or an
  analogous walk crate) invoked via `cargo run -p loom-walk -- <walk-name>`.
- Running Rust code in isolation (unit, integration, property, snapshot) →
  `[test]`. The verifier is a `#[test]` / `#[tokio::test]` / proptest function;
  the gate batches all `[test]` targets into one `cargo nextest run` invocation.
- Container smoke / nix-driven end-to-end → `[system]`.
- Code-quality dimensions requiring LLM evaluation (error-message clarity,
  naming consistency, doc-comment usefulness) → `[judge]`.

### Annotation Integrity Gate

[Acceptance](#success-criteria).

The gate that verifies annotations themselves resolve is defined in
[Gate](gate.md) (Integrity gate section). It runs as part of `loom gate check`.
Loom-tests has the acceptance criterion that the gate is self-checking (its own
annotation points at its own implementation); [Verify](verify.md#integrity-gate)
owns the mechanism.

### Determinism Through Clock Injection

[Acceptance](#success-criteria).

Time-dependent components — lock acquisition timeout, shutdown watchdog grace,
JSONL read-line timeout, log retention sweep, bd / git subprocess timeouts —
make tests flaky when ordinary logic tests touch real wall time on a loaded CI
runner. The design routes their timer logic through an injected clock.

**`Clock` trait in `loom-driver`** with `now()`, `sleep(Duration)`,
`timeout(Duration, Future)` async surface. Two implementations:

- `SystemClock` — production. Wraps tokio's real timers.
- `MockClock` — tests. Deterministic advance under
  `#[tokio::test(start_paused = true)]`.

Components touching time take `&dyn Clock` or `<C: Clock>`. Functions comparing
against external timestamps (e.g., the log retention sweep comparing against
filesystem mtime) take `now: Instant` as a parameter. Tests pass synthetic `now`
values to age files; production passes `clock.now()`.

**Filesystem mtime in tests** is set via the `filetime` crate. Real wall time
stays zero for tests of time-dependent logic: tests can express "this file is 15
days old" without sleeping.

**Clock-use audit** is enforced by walks in `loom-walk` over both production and
test Rust sources:

- `std::thread::sleep` is absent from production and from unenumerated tests.
- `tokio::time::sleep` appears only in clock implementations, tests using
  `#[tokio::test(start_paused = true)]`, and enumerated exceptions.
- `tokio::time::timeout` follows the same rule.
- `Instant::now()` / `SystemTime::now()` appears only in clock implementations
  and enumerated exceptions.

Unit tests for time-dependent components construct a `MockClock` and pass it
through the production clock boundary. Tokio paused time is synthetic and does
not consume a real-time exception.

A real-time exception qualifies only when an operating-system process or kernel
lock lifecycle, or actual elapsed performance, is itself the behavior under
test. The audit registry names the exact file, function, operation, and
call-site count, plus its boundary justification, finite upper deadline, cleanup
strategy, and deterministic companion coverage for underlying timer logic.
Durations stay at the smallest practical value. Every unenumerated call and
every extra call in an enumerated function fails the audit; there is no
directory-wide test exemption.

### Style Enforcement

[Acceptance](#success-criteria).

[`docs/style-rules.md`](../docs/style-rules.md) and the workspace lint
configuration own the style rules and exact Clippy policy. This spec owns only
test-tier classification: compiler and source-walking style checks are `[check]`
verifiers, while tests that execute behavior remain `[test]` verifiers.
`loom-walk` provides the source-analysis runner for checks that Clippy cannot
express; sibling component specs bind architectural walks to the contracts they
own.

Walk output follows the verifier-runner contract in [Gate](gate.md), so a
failure identifies the source location and applicable rule.

### Property-Based Testing

[Acceptance](#property-based-testing-1).

`proptest` for invariants on four targets:

| Target                 | Invariants                                                                                                                                                                          |
| ---------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| JSONL line parser      | never panics on arbitrary bytes; respects `MAX_LINE_BYTES`; never emits `AgentEvent` from a malformed line                                                                          |
| Pi protocol parser     | round-trip identity for known shapes; malformed input and unknown types follow the [agent-owned Pi classification and recovery policy](agent.md#pi-mono-rpc-protocol); never panics |
| Claude protocol parser | round-trip identity for known shapes; malformed input and unknown types follow the [agent-owned Claude protocol policy](agent.md#claude-stream-json-protocol); never panics         |
| Cache DB rebuild       | never panics on arbitrary spec/index content; schema invariants always hold; corrupted cache recovers via `recreate` or reports durable-source inconsistency                        |

**Convention.** Parsers and codecs ship with a proptest invariant — minimally
no-panic-on-arbitrary-input and (where applicable) round-trip identity. State
machines use typestate (per RS-12 / RS-7 in
[`docs/style-rules.md`](../docs/style-rules.md)) where it makes invalid
transitions unrepresentable. Tests need not duplicate a fact established solely
by type checking, but types do not establish temporal correctness, input
completeness, or evidence freshness. Consequential workflow behavior follows
[Simulation's model-admission and conformance contract](simulation.md#when-to-model).
Parsers and codecs without proptest coverage are flagged at `loom gate review`.

**Suite configuration**: property tests use 32 cases in the explicit full test
suite, overridable via `PROPTEST_CASES` to `2048+` for local exhaustive runs.
The full property suite is outside `nix flake check` and runs through the
workspace test app.

**Discoverability.** The CI cap is a single named constant in a shared
test-support module, not a scattered `with_cases(32)` literal:

```rust
// loom-test-support/src/lib.rs (or equivalent)
pub const CI_PROPTEST_CASES: u32 = 32;
```

Every proptest call site imports the constant. One place to bump; one place to
grep; no chance of drift between blocks. The env-var override behaviour is
documented next to the constant — single source of truth.

Each crate owns the property invariants for the types it defines. Exact
integration-test file and module organization remains an implementation choice;
cross-crate invariants belong to the narrowest test target that can exercise the
public seam.

**No `cargo fuzz` under `nix flake check`.** If a fuzz target later proves
valuable for byte-level edge cases proptest misses (e.g., JSONL framing under
adversarial input), it's exposed as `nix run .#fuzz-loom` for on-demand or
nightly use, never gating PRs.

### Snapshot Testing

[Acceptance](#snapshot-testing-1).

`insta` snapshots for **contract surfaces** — outputs whose shape is the
contract:

- Templates (`loom-templates`) — every Askama template × representative input
  set produces a `.snap` checked into `crates/loom-templates/tests/snapshots/`.
  Reviewers see the rendered diff in PRs.
- CLI help text (`loom --help`, `loom loop --help`, etc.) — `--help` output _is_
  the user contract.

Substring + structural assertions for **flexibility surfaces** — outputs with
intentional cosmetic latitude:

- Loop renderer (terminal tool-call lines, status colors, truncation). Tests
  assert bullet count, presence of key markers, and
  color-disabled-when-NO_COLOR; layout decisions remain free to evolve without
  churning a snapshot.

**Snapshot update policy**: a snapshot diff in a PR requires explicit
acknowledgment in the PR description ("snapshot updated because: ..."). Forces
intentional regression vs. accidental drift.

### Judge Mechanism

[Acceptance](#success-criteria).

`[judge]` annotations are reserved for criteria that genuinely require LLM
evaluation — code-quality dimensions that AST walks can't capture:

- "error messages are clear and actionable"
- "doc comments explain _why_ non-obviously"
- "API surface is ergonomic for typical call patterns"
- "naming is consistent with codebase conventions"

**Runner**: `loom gate judge` (or `loom gate review` for both criterion-attached
judges and the rubric walk together). See [Gate](gate.md). The runner sends the
named source files plus the criterion text to the LLM via the existing agent
abstraction and captures a structured verdict per the verifier-runner contract.

**Cost class.** Judges are non-deterministic, paid, and network-dependent. They
do NOT run under `nix flake check`; they run on demand, on bead completion, or
in scheduled jobs. A `[judge]` verdict that disagrees with human judgement is a
prompt to either rewrite the criterion as one of `[check]` / `[test]` /
`[system]` (if the property is reducible to a deterministic check) or accept the
disagreement (if the property is genuinely subjective).

### Test Patterns

[Acceptance](#success-criteria).

Concrete patterns for writing tests against the design rules above. Each pattern
is one short example; the verify-runner and integration tests own the full
coverage.

#### Parse, Don't Validate boundaries

[Acceptance](#success-criteria).

Each boundary layer pins the parse-once-use-everywhere contract with a dedicated
test. Three illustrative examples below — newtype construction, two-phase
envelope parsing, and `#[serde(other)]` catchall behavior. JSONL framing and
SQLite row mapping have analogous tests in `loom-driver/src/{agent,state}` that
follow the same shape:

```rust
#[test]
fn newtype_roundtrip() {
    let id = BeadId::new("lm-abc123").unwrap();
    assert_eq!(id.as_str(), "lm-abc123");
    assert_eq!(id.to_string(), "lm-abc123");

    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, r#""lm-abc123""#); // transparent, no wrapper
    let parsed: BeadId = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, id);

    // Deserialize validates the canonical shape.
    serde_json::from_str::<BeadId>(r#""not a bead""#).unwrap_err();
}

#[test]
fn pi_envelope_ignores_unknown_fields() {
    // Two-phase: envelope parse must succeed even with extra fields
    let line = r#"{"type":"response","id":"42","extra":"ignored"}"#;
    let env: PiEnvelope = serde_json::from_str(line).unwrap();
    assert_eq!(env.msg_type.as_deref(), Some("response"));
}

#[test]
fn claude_unknown_event_type_does_not_error() {
    // #[serde(other)] catches new event types from future Claude versions
    let line = r#"{"type":"new_feature_event","data":"something"}"#;
    let msg: ClaudeMessage = serde_json::from_str(line).unwrap();
    assert!(matches!(msg, ClaudeMessage::Unknown));
}
```

#### State database

[Acceptance](#success-criteria).

Round-trip and corruption-recovery tests use real on-disk SQLite files inside
`tempfile::tempdir`. The `:memory:` mode is deliberately not used — it skips the
file-IO codepaths that production runs hit (open, fsync, corruption recovery),
so an in-memory test passing gives false confidence.

```rust
#[test]
fn cache_db_rebuild() {
    let dir = tempdir().unwrap();

    // Seed complete indexed packages; the helper writes spec.md, tests.md,
    // their index rows, and the durable metadata expected by mock_bd_client.
    seed_indexed_spec_packages(dir.path(), &["auth", "api"]);

    let db = CacheDb::open(&dir.path().join("cache.db")).unwrap();
    let report = db.rebuild(dir.path(), &mock_bd_client()).unwrap();

    assert_eq!(report.specs_found, 2);
    assert!(report.counters_reset);

    let spec = db.spec(&SpecLabel::new("auth")).unwrap();
    assert_eq!(spec.spec_path, "specs/auth/spec.md");
}

#[test]
fn cache_db_corruption_recovery() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("cache.db");

    // Write garbage to the DB file
    std::fs::write(&db_path, b"not a sqlite db").unwrap();

    // open detects corruption, rebuild recovers
    let db = CacheDb::open(&db_path).unwrap();
    let report = db.rebuild(dir.path(), &mock_bd_client()).unwrap();
    assert_eq!(report.specs_found, 0); // no spec files in tempdir
}
```

#### Template render contract

[Acceptance](#success-criteria).

Render tests assert on the contract (partials included, agent content wrapped,
truncation applied) rather than full string parity. Contract shape comes from
the typed `LoopContext` struct; layout regressions are caught by `insta`
snapshots (see _Snapshot Testing_).

```rust
#[test]
fn run_wraps_agent_supplied_fields_in_agent_output() -> Result<()> {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.into(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness/spec.md".into(),
        issue_id: BeadId::new("lm-abc.1")?,
        title: "Implement parser".into(),
        description: "agent-supplied body".into(),
        previous_failure: None,
        // ...
    };
    let out = ctx.render()?;

    assert!(out.contains("<agent-output>"));
    assert!(out.contains("</agent-output>"));
    assert!(out.contains("agent-supplied body"));
    Ok(())
}
```

### Mock Pi Design

[Acceptance](#success-criteria).

Mock pi is a scenario-selectable shell fixture that frames pi-mono's RPC
protocol as JSONL on stdin/stdout. It exercises process paths parser unit tests
cannot reach: startup handshakes, real pipes, command write-back, compaction
re-pin delivery, and child reaping. Scenarios stay single-purpose and
single-shot, but one scenario may support multiple tests of the same observable
wire behavior. The fixture is not a general-purpose Pi emulator.

Conformance tests drive every retained scenario through its consuming backend,
workflow, or smoke path. Parser-only malformed-input cases remain inline Rust
fixtures rather than mock process modes.

### Process Lifecycle Fixtures

[Acceptance](#success-criteria).

Pi handshake timeout and workflow stall-heartbeat coverage depend on a pending
real pipe plus the outer timeout or watchdog. They use separate, no-selector
scripts under `tests/fixtures/agent/` rather than modes in the general mock-pi
table. Each script encodes only the pending lifecycle needed by one integration
test. A host-side upper deadline kills the process group, reaps the child, and
joins pipe readers if the production deadline regresses. Malformed output does
not require process lifecycle coverage and remains a Pi parser unit test.

### Inbox Bridge Fixture

[Acceptance](#success-criteria).

The Pi inbox bridge follow-up fixture is separate from the mock-pi mode table.
Its whole contract is one startup probe, one initial prompt that completes
without a terminal marker, one human reply encoded as a fresh `prompt`, and then
a terminal marker. It exists only because the bridge keeps the same process
pipes alive across a post-completion human reply; parser unit tests do not
exercise that process lifecycle. A conformance test drives this exact JSONL
exchange so the fixture does not grow into another Pi protocol emulator.

### Mock Claude Design

[Acceptance](#success-criteria).

Mock Claude follows the same narrow, single-shot fixture pattern while speaking
Claude Code's stream-json framing. Its scenarios cover steering, shutdown
escalation, interactive compaction-hook delivery, and the smoke lifecycle.
Conformance comes from production launch paths that execute the mock; invoking
the script directly with a test-synthesized success payload is not evidence.

### Nix Integration

[Acceptance](#success-criteria).

```nix
# tests/loom/default.nix
{ pkgs, loomPackage, ... }:
let
  inherit (loomPackage) craneLib;
in
{
  # Deterministic verifiers — invokes explicit tier subcommands:
  # `[check]` (batched `cargo run -p loom-walk -- …` annotations)
  # and `[test]` (one batched `cargo nextest run -E 'test(…)'` over every
  # annotated test path). `[system]` is excluded by composing explicit
  # tier subcommands (`loom gate check --tree` + `loom gate test --tree`)
  # because its verifiers shell out to `nix build`, `nix run`, and
  # `podman`, none of which exist inside the nix build sandbox. The
  # craneLib custom-derivation pattern threads cargoArtifacts, staged
  # source, and the pre-built loom binary into the sandbox.
  loomTests = craneLib.mkCargoDerivation {
    pname = "tests";
    src = stagedSrc;
    cargoLock = ../../loom/Cargo.lock;
    inherit (loomPackage) cargoArtifacts;
    doCheck = true;
    nativeBuildInputs = [ pkgs.git pkgs.cargo-nextest loomPackage.bin ];
    buildPhaseCargoCommand = ''
      cargo --version
      cargo nextest --version
      loom --version
    '';
    checkPhaseCargoCommand = ''
      loom gate check --tree
      loom gate test --tree
    '';
  };

  # Container smoke — invoked via `nix run .#smoke`. Excluded from
  # `flake check` because it needs podman at runtime. Annotated as
  # [system](nix run .#smoke) on its acceptance criterion.
  loom-smoke = pkgs.writeShellApplication {
    name = "smoke";
    runtimeInputs = [ loom bd pkgs.podman pkgs.jq ];
    text = builtins.readFile ./run-tests.sh;
  };
}
```

`loomTests` is exposed via `tests/default.nix` and lifted to
`packages.loom-tests` in `nix/flake/tests.nix`; it is not part of the flake
`checks` set. The fast `nix flake check` surface stays limited to
non-workspace-compile derivations. The full required suite is the
`nix run .#test` app in `nix/flake/apps.nix`: it runs the fast flake tier,
workspace clippy, full workspace nextest, and `loom gate system --tree`.
Pre-push composes those same required tiers without repetition: the standalone
fast and both Clippy hooks are followed by `nix run .#test-required`, which runs
full workspace nextest and `loom gate system --tree`. The standalone
`nix run .#test` remains complete. Grep-tier `[check]` annotations across specs
use paths relative to the staged-source root (which mirrors the `loom/`
workspace flattened to `$out/` plus host files like
`lib/sandbox/linux/entrypoint.sh` mirrored under their host paths), so the
explicit tier commands run at tree scope with no `--spec` filter. `loom-smoke`
is exposed as `nix run .#smoke` on Linux only.

## Success Criteria

### Assembled-system checks

<!-- prettier-ignore -->
- `nix run .#smoke` follows the
  [agent-owned container launch boundary](agent.md#container-integration) to run
  a Pi-backed mock-agent bead against live bd, exiting 0 with the bead closed
  [system](nix run .#smoke)

<!-- prettier-ignore -->
- `nix run .#test-sandbox` launches the selected packaged Pi agent only for an
  offline `--version` health check: container networking is disabled, no
  provider credentials are supplied, and no prompt, protocol turn, or LLM API
  request occurs [system](nix run .#test-sandbox)

### Test-source portability

<!-- prettier-ignore -->
- Rust tests use isolated temporary directories rather than hardcoded host
  temporary paths [check](cargo run -p loom-walk -- no_hardcoded_tmp_paths)

### Determinism

<!-- prettier-ignore -->
- `std::thread::sleep` is absent from production and unenumerated tests; each
  test exception is an exact bounded process-lifecycle or elapsed-performance
  registry entry [check](cargo run -p loom-walk -- no_thread_sleep)

<!-- prettier-ignore -->
- `tokio::time::sleep` appears only in clock implementations, synthetic paused
  tests, and exact bounded registry entries across production and test sources
  [check](cargo run -p loom-walk -- no_tokio_sleep_outside_clock)

<!-- prettier-ignore -->
- `tokio::time::timeout` appears only in clock implementations, synthetic paused
  tests, and exact bounded registry entries across production and test sources
  [check](cargo run -p loom-walk -- no_tokio_timeout_outside_clock)

<!-- prettier-ignore -->
- Real-clock reads appear only in clock implementations and exact bounded
  registry entries across production and test sources [check](cargo run -p loom-walk -- no_real_clock_outside_system_clock)

<!-- prettier-ignore -->
- `#[ignore]` never hides flaky, optional, or otherwise omitted coverage; each
  exception is an enumerated child-process entry point invoked by a non-ignored
  parent during the ordinary test suite and carries a process-boundary
  justification [check](cargo run -p loom-walk -- no_ignore_for_flake)

### Property-based testing

<!-- prettier-ignore -->
- Every property block consumes the shared property-test configuration rather
  than declaring a local case-count literal [check](cargo run -p loom-walk -- shared_proptest_config)

- The shared property-test configuration defaults to 32 cases and honors the
  `PROPTEST_CASES` environment override
  [test](proptest_case_configuration_honours_default_and_env_override)

### Snapshot testing

- `loom --help` and every subcommand `--help` have `insta` snapshots
  [test](all_cli_help_snapshots)

- Consolidated mechanical style walks retain real-workspace coverage
  [test](workspace_style_walks_pass)

### Cross-platform

<!-- prettier-ignore -->
- The flake source declares shared `loom-tests` package wiring for its four
  configured Linux and Darwin system identifiers [check](cargo run -p loom-walk -- test_nix_surface_contract)

<!-- prettier-ignore -->
- The smoke app selects the real image-backed implementation only on Linux
  [check](cargo run -p loom-walk -- test_nix_surface_contract)

<!-- prettier-ignore -->
- The Darwin smoke branch is an explicit successful unavailable-platform stub
  [check](cargo run -p loom-walk -- test_nix_surface_contract)

### CI integration

<!-- prettier-ignore -->
- The `loom-tests` package invokes both deterministic gate tiers and remains
  outside the flake `checks` set [check](cargo run -p loom-walk -- test_nix_surface_contract)

<!-- prettier-ignore -->
- `nix run .#test` composes the fast flake tier, workspace Clippy, full nextest,
  and system verifiers [check](cargo run -p loom-walk -- workspace_compile_checks_are_full_test_app_only)

<!-- prettier-ignore -->
- `nix run .#smoke` carries a concrete mock-Pi image, immutable ProfileConfig,
  canonical `wrix.prekHooks` directory, Linux implementation, and Darwin stub
  [check](cargo run -p loom-walk -- test_nix_surface_contract)

<!-- prettier-ignore -->
- `nix run .#fuzz-loom` is on-demand and absent from flake checks [check](cargo run -p loom-walk -- test_nix_surface_contract)

- Container smoke reports elapsed time and warns after the 30-second soft target
  without failing an otherwise successful run [test](smoke_timing_is_advisory)

- An unclosed smoke bead fails verification and reports its notes and metadata
  for diagnosis
  [test](smoke_unclosed_bead_reports_notes_and_metadata_for_diagnosis)

## Requirements

### Functional

1. **Three test levels** with complementary scope (each level is addressed by
   one or more annotation tiers; the levels here are the _test-design_ axis, not
   the annotation-tier axis):
   - **Unit tests** — per-crate, fast, no external dependencies. Inline
     `#[cfg(test)] mod tests` blocks. Annotated `[test]`; run via
     `loom gate test`, which dispatches to `cargo nextest`.
   - **Integration tests** — cross-crate, use mock agent processes over real
     pipes, no containers. Live in `crates/<crate>/tests/*.rs`. Annotated
     `[test]`; run via `loom gate test`.
   - **Container smoke** — one happy-path scenario that uses the
     [agent-owned container launch boundary](agent.md#container-integration),
     runs a mock agent _inside_ the container, drives `loom loop <bead-id>`, and
     asserts the bead closes. It validates host↔container assembly and teardown
     — _not_ protocol depth, which the integration level already covers.
     Annotated `[system](nix run .#smoke)`; run via `loom gate system`.
     Linux-only (no podman in Darwin CI).

2. **Mock agent processes** — process-level fixtures driven over real pipes from
   cargo integration tests, plus the in-container smoke:
   - **Mock pi** (`tests/mock-pi/pi.sh`) — narrowly scoped scenario modes that
     exercise the _pipe-level_ paths the parser unit tests can't reach (probe
     round-trip, prompt ack, mid-session steer, compaction re-pin via steer,
     interactive compaction canary, `set_model` from phase config, plus
     `happy-path` for the container smoke).
   - **Mock claude** (`tests/mock-claude/claude.sh`) — modes for mid-session
     steering via stream-json user message, the shutdown watchdog
     SIGTERM→SIGKILL escalation, interactive compaction canary, plus
     `happy-path` for the container smoke.
   - **Out of scope for mock mode tables**: tool-call simulation,
     malformed-JSONL injection, hang/timeout modes, and general multi-turn
     behavior. The narrow interactive compaction canary is the only scripted
     multi-turn exception because the bug is visible only after a
     post-compaction probe. Parser unit tests cover malformed protocol input
     with inline string literals. Named process-lifecycle fixtures may hold a
     real pipe pending only for the handshake-timeout and workflow-stall
     assertions, and carry no mode selector or broader protocol behavior.

3. **Rust test coverage by component** — private behavior is exercised inline
   where white-box access is useful; public, cross-module, and process
   boundaries use Cargo integration tests where that boundary adds signal. Leaf
   crates are not required to create an integration-test file solely for layout
   symmetry. The lists below describe coverage areas rather than internal file
   organization.

### Functional

4. **Integration test coverage** — load-bearing tests execute public cross-crate
   or operating-system process seams. Backend launch and protocol behavior is
   owned by [Agent](agent.md); cache, Git, todo, parallel dispatch, and locking
   behavior is owned by [Harness](harness.md); event fan-out and persistence
   behavior is owned by [Events](events.md). This spec owns the classification
   and fixture discipline: parser-only shape checks stay in unit tests, while
   startup handshakes, pending pipes, child reaping, and production CLI routing
   use integration tests.

5. **Container smoke coverage** — one happy-path scenario validates
   host↔container plumbing that the integration tier cannot reach. A temporary
   workspace is seeded with one ready `profile:base` bead, and a concrete test
   image carries mock Pi through the
   [agent-owned container launch boundary](agent.md#container-integration). The
   harness generates isolated ephemeral deploy and signing keys plus non-secret
   mock Pi auth, so it exercises repository-scoped Wrix spawn without ambient
   host credentials. The temporary repository carries an explicit no-op prek
   configuration for `pre-commit` and `pre-push`, so the canonical Wrix hook
   bundle is exercised without bypass flags. Its pre-push entry uses the real
   repository `bin/pre-push-checks` wrapper and required hook metadata. The
   fixture ignores Loom/Wrix-owned `.loom/` and `.wrix/` runtime state so
   cleanliness and marker minting cover only repository changes. The mock worker
   inherits Wrix's SSH signing policy, so its commit must pass Loom's
   worker-side signature verification. The smoke asserts the container exits
   cleanly and the bead closes. Workflow-level coverage
   (plan/todo/loop/gate/inbox/tune, profile/runtime selection, agent switching)
   lives in inline `#[cfg(test)] mod tests` blocks under `loom-workflow/src/` —
   those are exercised via `cargo nextest run`, not the smoke.

6. **Rust style enforcement** — [`docs/style-rules.md`](../docs/style-rules.md)
   and the workspace configuration own the exact lint policy. Clippy-backed and
   source-walking assertions are `[check]` verifiers; component specs own the
   architectural facts those walks inspect.

7. **Annotation contract** — every acceptance criterion in any spec under
   `specs/` carries a `[check]`, `[test]`, `[system]`, or `[judge]` annotation
   that must resolve to an existing verifier. The full rules (syntax,
   cardinality, classification, cross-spec sharing) live in
   [`docs/spec-conventions.md`](../docs/spec-conventions.md); the integrity gate
   that enforces them lives in [Gate](gate.md).

8. **Property-based testing** — `proptest` for invariants on four targets: JSONL
   line parser, Pi protocol parser, Claude protocol parser, cache DB rebuild.
   Properties target invariants ("never panics on arbitrary input", "round-trip
   is identity for known shapes", "unknown types follow backend classification
   policy") rather than specific input/output pairs. CI runs each property at
   `PROPTEST_CASES=32`; local exhaustive runs use `PROPTEST_CASES=2048+` via env
   var. No `cargo fuzz` under `nix flake check` — exposed separately as
   `nix run .#fuzz-loom` for on-demand or nightly use.

9. **Snapshot testing** — `insta` snapshots for templates and CLI help output
   (contract surfaces where layout regressions matter). Substring + structural
   assertions for the loop renderer (terminal tool-call lines, status colors —
   surfaces with intentional flexibility). Snapshot updates require explicit
   acknowledgment in the PR description ("snapshot updated because: ...") to
   surface accidental drift.

10. **Packaged-agent health check** — assembled-system verification may execute
    the selected real packaged agent only as a network-disabled `--version`
    launch without provider credentials. It does not send a prompt, exercise a
    protocol turn, or call an LLM API. All agent behavior and protocol coverage
    remains mock-driven.

### Non-Functional

1. **Deterministic** — no real LLM API calls and no ordinary real wall-clock
   waits. The packaged-agent exception in Functional #10 is an offline,
   non-conversational process-health check. Mock agents return canned responses.
   Time-dependent components take an injectable `Clock` trait; tests of their
   timer logic use a `MockClock` with controllable advance. Exact audited
   process-lifecycle and elapsed-performance exceptions use bounded host time as
   defined in _Architecture / Determinism Through Clock Injection_.
2. **Fast** — soft targets per gate command, warm cache:
   - `loom gate status` (cached status, no verifiers): <100 ms (and a hard <500
     ms ceiling, asserted by a self-test on the cache implementation).
   - `loom gate check`: <5 s aggregate across all `[check]` walks.
   - `loom gate test`: <30 s aggregate (one batched cargo-nextest invocation;
     nextest's internal parallelism does the heavy lifting).
   - `loom gate system`: <60 s per verifier; container smoke targets <30 s.
   - `loom gate judge`: no fixed target; bounded by LLM API concurrency.

   All except the `loom gate` status ceiling are _soft_ — they guide design
   (host waits require audited boundaries, subprocess tests need justification,
   proptest case count bounded) but the gate doesn't fail when a budget is
   exceeded; humans review timing in PRs.

3. **Isolated** — each test uses its own temp directory and beads database
   prefix. No shared mutable state between tests.
4. **Parallel-safe** — unit and integration tests run in parallel under
   `cargo nextest`'s process-per-test model. Each test gets a fresh process, so
   global state (env vars, working directory, process-level locks) doesn't leak
   between tests. The container smoke (single scenario) gets its own pre-seeded
   `.beads/` snapshot in a tempdir, fully isolated from any concurrent peers
   running against the workspace.
5. **Push-friendly full suite** — `nix flake check` runs the fast deterministic
   derivations that stay inside the interactive push budget. The standalone
   `nix run .#test` remains the complete suite. Pre-push runs the fast tier and
   both Clippy configurations through independent hooks, then full workspace
   nextest plus `[system]`/container verifiers through
   `nix run .#test-required`, without repeating lint hooks. This repository has
   no separate CI safety net. The container smoke remains exposed as
   `nix run .#smoke` because it needs podman at runtime; its acceptance
   criterion is annotated `[system](nix run .#smoke)`. Pre-push also runs clippy
   plus targeted `loom gate verify --diff` under
   [Gate's deterministic verify contract](gate.md#deterministic-verify-lanes).
   Project-specific hook composition, stage budgets, and lock semantics live in
   [Pre-Commit](pre-commit.md).
6. **Real bd** — the container smoke runs against live `bd` (not a mock). The
   integration tier may mock `bd` where the test concern is orthogonal to the
   issue tracker, but the smoke validates that loom and `bd` interact correctly
   under realistic conditions.
7. **Cross-platform source composition** — the flake declares shared test
   package wiring for its configured Linux and Darwin system identifiers. The
   source-level verifier checks that composition; platform-native builds remain
   the authority for whether a package builds on that host and are not inferred
   from a foreign-system source grep. The container smoke is Linux-only (podman
   dependency); on Darwin the `smoke` app exits 0 with a clear "container smoke
   not available on Darwin" message. Tests use `tempfile::tempdir` rather than
   hardcoded `/tmp/...` paths so the source is compatible with Darwin's build
   sandbox. Darwin smoke support is a follow-up.
8. **Subprocess-spawning tests are exceptional** — each subprocess test
   (mock-pi, mock-claude, real `git`) costs 50-200ms; ten of them blow the 5s
   soft target alone. A test that spawns a subprocess includes a short comment
   or doc string explaining why an in-process equivalent (via `LineParse` +
   `tokio::io::duplex`) is not feasible. Any direct host-clock read or sleep
   also appears in the exact audited exception registry with a finite upper
   deadline, reliable child cleanup, the smallest practical duration, and
   deterministic companion coverage for timer logic.
9. **Upstream protocol versioning** (Pi Coding Agent and Claude Code) — Agent
   versions are pinned by locked flake inputs and package overrides defined here
   or in those inputs. Bumps are deliberate PRs accompanied by a protocol-bump
   checklist (re-run parser tests, scan upstream changelog for new event types,
   add `Unknown` coverage if any new types lack typed variants, update mock
   scripts if new types reach pipe-level paths). No live wire tests run against
   real binaries; the Functional #10 exception proves only that the selected
   package launches. Detection coverage: silent breaks in _exercised_ fields
   surface as `serde_json` errors in parser tests when the pinned version is
   bumped. Fields not exercised by any test could still drift silently — parser
   tests must therefore touch every field of every documented message type for
   the pinned version, not just every type.
10. **No `#[ignore]` for skipped coverage** — `#[ignore]` cannot hide a flaky,
    optional, or otherwise omitted test; fix the root cause or delete the test.
    The only exceptions are enumerated child-process entry points that a
    non-ignored parent invokes during the ordinary test suite because process
    isolation is itself part of the behavior under test. Each exception carries
    a process-boundary justification in the audited allowlist, and every
    unenumerated marker fails the audit. A CI flake opens a `loom-flake` P1 bead
    naming the failing test; the test is fixed before any further work on the
    affected crate.

## Out of Scope

- Fine-grained selection of ordinary unit/integration suites, precise
  function-level Rust impact analysis, or forced crate/library splits for
  selective properties. Shared compilation is legitimate; Gate admits the
  dependency boundaries used for selection.
- Owning model-checking policy or a separate conformance execution route.
  [Simulation](simulation.md) owns modeling; Gate supplies the existing path.

- **Real-binary behavioral tests** — no test invokes real Claude Code or uses
  real Pi for a conversation, prompt, protocol turn, or LLM API request. Mock pi
  and mock claude scripts cover the protocol surface (parser tests use inline
  strings; mocks cover pipe-level paths; smoke runs mock pi inside the
  container). The sole real-agent exception is the offline packaged-Pi
  `--version` health check in Functional #10; it detects image/runtime packaging
  drift but does not validate conversational or protocol behavior. Locked flake
  inputs and package overrides defined here or in those inputs, plus parser
  tests with field-level coverage, catch silent protocol drift on bumps.
- **macOS container smoke** — the smoke requires `podman` (Linux). Darwin
  container testing is a follow-up.
- **Mocking `bd`** — the container smoke uses live `bd` (see NFR #6).
- **Broader system-tier scenario library** — `tests/loom/scenarios/` with
  steering, compaction, error-recovery scripts. The integration tier already
  covers these flows via shim-based mocks; repeating them with podman adds CI
  time without catching new failure modes. One happy-path smoke is sufficient to
  validate host↔container plumbing.
- **Captured JSONL fixtures** — `loom-agent/src/{pi,claude}/fixtures/` with
  replay scripts. Parser tests use inline string literals, which are easier to
  read in PR diffs and don't bit-rot when pi/claude release new event shapes.
- **External-template parity fixtures** — any compatibility-fixture set tied to
  a predecessor templating system that is itself scheduled for removal. Such
  fixtures become irrelevant the moment the predecessor is removed; capturing
  them is wasted work.
- **Pi cost capture** — deferred to loom-agent. When pi's `get_session_stats` is
  wired up after the startup probe, loom-tests gains one acceptance criterion: a
  round-trip test asserting that `SessionOutcome.cost_usd` is populated for pi
  sessions, parallel to the existing claude `result/total_cost_usd` extraction.
- **General mock-script protocol breadth** — tool-call simulation,
  malformed-JSONL injection, hang/timeout modes, and general multi-turn
  conversations do not belong in the general mock-pi/mock-claude scripts. Parser
  unit tests own malformed protocol input. The interactive compaction canary is
  the only multi-turn exception in those mode tables, because it verifies a
  post-compaction turn rather than protocol breadth. Dedicated bridge and
  process-lifecycle fixtures remain single-purpose, carry their own conformance
  tests, and are not folded into the general mock-agent mode tables.
- **Per-repo verifier registry separate from `loom.toml`** — annotations carry
  the verifier directly (target name for `[test]` / `[judge]`, command for
  `[check]` / `[system]`); no separate config maps names to commands. Toolchain
  detection (`Cargo.toml` at repo root → cargo nextest, etc.) supplies defaults
  for batched-tier runners; `<workspace>/loom.toml` is the override path when
  defaults don't fit, not a per-verifier registry.
- **`cargo fuzz` under `nix flake check`** — exposed as `nix run .#fuzz-loom`
  for on-demand or nightly runs only. proptest covers invariants in CI.
- **Hard CI-time NFR for the verify path** — the per-tier budgets
  (Non-Functional #2) are soft design targets, not CI failure thresholds. They
  guide decisions (no real sleeps, subprocess tests need justification, proptest
  case count bounded) but the gate doesn't fail when a budget is exceeded;
  humans review timing in PRs. Exception: `loom gate` status has a hard <500ms
  ceiling with a self-test — that one is a regression of the cache
  implementation, not of the corpus.
