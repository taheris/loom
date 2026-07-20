# Loom Tests

Test strategy and infrastructure for the Loom agent driver.

## Problem Statement

Loom is a Rust binary that orchestrates per-bead agent sessions
across a multi-crate workspace. Testing has to cover three things at
once: protocol parsing across subprocess agent protocols (pi-mono RPC,
Claude stream-json, plus Direct runner JSONL compatibility), workflow
orchestration (cache DB, locking, bead-clone parallelism, push gate), and
host↔container plumbing (entrypoint branching, bind mounts,
profile/runtime selection). All three need
first-class coverage in a Rust-native test framework with explicit
cache-DB and protocol-parser tests.

This spec designs the test strategy across three levels — unit,
integration, container smoke — and the design rules that make tests
deterministic and findable: per-tier annotations on acceptance
criteria (`[check]` / `[test]` / `[system]` / `[judge]`, syntax owned
by [`docs/spec-conventions.md`](../docs/spec-conventions.md), dispatch
owned by [gate.md](gate.md)), a `Clock` trait that
eliminates real-time waits, AST-based style enforcement, snapshot
testing for contract surfaces, property-based testing for protocol
parsers, and Nix-pinned protocol versions to catch upstream drift.

## Architecture

### Test Homes and Boundaries

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

Internal per-crate file and module organization remains an implementation
choice rather than part of this spec's contract.

### Annotation Contract

Annotation syntax (`[check]` / `[test]` / `[system]` / `[judge]`),
cardinality rules (atomic acceptance, N→1 sharing, cross-spec
sharing), and the deterministic-vs-stochastic partition are defined
in [`docs/spec-conventions.md`](../docs/spec-conventions.md). The
gate's resolution mechanics — per-tier dispatch, batching for
`[test]` and `[judge]`, runner discovery, the `--files` scope model
— live in [gate.md](gate.md). This spec does not duplicate
those definitions.

What loom-tests owns: the **classification policy** for tests in
this repo — which tier each kind of test belongs to:

- Static analysis of Rust source (presence, absence, structural
  property across files) → `[check]`. The verifier is a Rust binary
  in `loom-walk` (or an analogous walk crate) invoked via
  `cargo run -p loom-walk -- <walk-name>`.
- Running Rust code in isolation (unit, integration, property,
  snapshot) → `[test]`. The verifier is a `#[test]` / `#[tokio::test]`
  / proptest function; the gate batches all `[test]` targets into one
  `cargo nextest run` invocation.
- Container smoke / nix-driven end-to-end → `[system]`.
- Code-quality dimensions requiring LLM evaluation (error-message
  clarity, naming consistency, doc-comment usefulness) → `[judge]`.

### Annotation Integrity Gate

The gate that verifies annotations themselves resolve is defined in
[gate.md](gate.md) (Integrity gate section). It runs as
part of `loom gate check`. Loom-tests has the acceptance criterion
that the gate is self-checking (its own annotation points at its own
implementation); the mechanism lives in gate.md.

### Determinism Through Clock Injection

Time-dependent components — lock acquisition timeout, shutdown
watchdog grace, JSONL read-line timeout, log retention sweep, bd /
git subprocess timeouts — make tests flaky when ordinary logic tests touch real
wall time on a loaded CI runner. The design routes their timer logic through an
injected clock.

**`Clock` trait in `loom-driver`** with `now()`, `sleep(Duration)`,
`timeout(Duration, Future)` async surface. Two implementations:

- `SystemClock` — production. Wraps tokio's real timers.
- `MockClock` — tests. Deterministic advance under
  `#[tokio::test(start_paused = true)]`.

Components touching time take `&dyn Clock` or `<C: Clock>`. Functions
comparing against external timestamps (e.g., the log retention sweep
comparing against filesystem mtime) take `now: Instant` as a
parameter. Tests pass synthetic `now` values to age files; production
passes `clock.now()`.

**Filesystem mtime in tests** is set via the `filetime` crate. Real wall time
stays zero for tests of time-dependent logic: tests can express "this file is
15 days old" without sleeping.

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
test. The audit registry names the exact file, function, operation, and call-site
count, plus its boundary justification, finite upper deadline, cleanup strategy,
and deterministic companion coverage for underlying timer logic. Durations stay
at the smallest practical value. Every unenumerated call and every extra call in
an enumerated function fails the audit; there is no directory-wide test
exemption.

### Style Enforcement

[`docs/style-rules.md`](../docs/style-rules.md) and the workspace lint
configuration own the style rules and exact Clippy policy. This spec owns only
test-tier classification: compiler and source-walking style checks are
`[check]` verifiers, while tests that execute behavior remain `[test]`
verifiers. `loom-walk` provides the source-analysis runner for checks that
Clippy cannot express; sibling component specs bind architectural walks to the
contracts they own.

Walk output follows the verifier-runner contract in [gate.md](gate.md), so a
failure identifies the source location and applicable rule.

### Property-Based Testing

`proptest` for invariants on four targets:

| Target | Invariants |
|--------|------------|
| JSONL line parser | never panics on arbitrary bytes; respects `MAX_LINE_BYTES`; never emits `AgentEvent` from a malformed line |
| Pi protocol parser | round-trip identity for known shapes; unknown shapes map to `ProtocolError::UnknownMessageType`; never panics |
| Claude protocol parser | round-trip identity for known shapes; unknown shapes map to the `Unknown` variant via `#[serde(other)]`; never panics |
| Cache DB rebuild | never panics on arbitrary spec/index content; schema invariants always hold; corrupted cache recovers via `recreate` or reports durable-source inconsistency |

**Convention.** Parsers and codecs ship with a proptest invariant —
minimally no-panic-on-arbitrary-input and (where applicable) round-trip
identity. State machines lean on typestate (per RS-12 / RS-7 in
[`docs/style-rules.md`](../docs/style-rules.md)) to make invalid
transitions unrepresentable at compile time; proptests on
state-transition logic are redundant when the type system already
enforces them. Parsers and codecs without proptest coverage are
flagged at `loom gate review`.

**CI configuration**: `PROPTEST_CASES=32` for `nix flake check`,
overridable via env var to `2048+` for local exhaustive runs.

**Discoverability.** The CI cap is a single named constant in a
shared test-support module, not a scattered `with_cases(32)` literal:

```rust
// loom-test-support/src/lib.rs (or equivalent)
pub const CI_PROPTEST_CASES: u32 = 32;
```

Every proptest call site imports the constant. One place to bump;
one place to grep; no chance of drift between blocks. The env-var
override behaviour is documented next to the constant — single
source of truth.

Property tests live in `crates/<crate>/tests/properties.rs` —
each crate owns the invariants for the types it defines. The binary
crate's `crates/loom/tests/properties.rs` is reserved for
cross-crate invariants if any arise.

**No `cargo fuzz` under `nix flake check`.** If a fuzz target later
proves valuable for byte-level edge cases proptest misses (e.g.,
JSONL framing under adversarial input), it's exposed as
`nix run .#fuzz-loom` for on-demand or nightly use, never gating PRs.

### Snapshot Testing

`insta` snapshots for **contract surfaces** — outputs whose shape is
the contract:

- Templates (`loom-templates`) — every Askama template × representative
  input set produces a `.snap` checked into
  `crates/loom-templates/tests/snapshots/`. Reviewers see the
  rendered diff in PRs.
- CLI help text (`loom --help`, `loom loop --help`, etc.) — `--help`
  output *is* the user contract.

Substring + structural assertions for **flexibility surfaces** —
outputs with intentional cosmetic latitude:

- Loop renderer (terminal tool-call lines, status colors,
  truncation). Tests assert bullet count, presence of key markers,
  and color-disabled-when-NO_COLOR; layout decisions remain free to
  evolve without churning a snapshot.

**Snapshot update policy**: a snapshot diff in a PR requires explicit
acknowledgment in the PR description ("snapshot updated because:
..."). Forces intentional regression vs. accidental drift.

### Judge Mechanism

`[judge]` annotations are reserved for criteria that genuinely require
LLM evaluation — code-quality dimensions that AST walks can't capture:

- "error messages are clear and actionable"
- "doc comments explain *why* non-obviously"
- "API surface is ergonomic for typical call patterns"
- "naming is consistent with codebase conventions"

**Runner**: `loom gate judge` (or `loom gate review` for both
criterion-attached judges and the rubric walk together). See
[gate.md](gate.md). The runner sends the named source
files plus the criterion text to the LLM via the existing agent
abstraction and captures a structured verdict per the
verifier-runner contract.

**Cost class.** Judges are non-deterministic, paid, and
network-dependent. They do NOT run under `nix flake check`; they run
on demand, on bead completion, or in scheduled jobs. A `[judge]`
verdict that disagrees with human judgement is a prompt to either
rewrite the criterion as one of `[check]` / `[test]` / `[system]`
(if the property is reducible to a deterministic check) or accept
the disagreement (if the property is genuinely subjective).

### Test Patterns

Concrete patterns for writing tests against the design rules above. Each
pattern is one short example; the verify-runner and integration tests
own the full coverage.

#### Parse, Don't Validate boundaries

Each boundary layer pins the parse-once-use-everywhere contract with
a dedicated test. Three illustrative examples below — newtype
construction, two-phase envelope parsing, and `#[serde(other)]`
catchall behavior. JSONL framing and SQLite row mapping have
analogous tests in `loom-driver/src/{agent,state}` that follow the
same shape:

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

Round-trip and corruption-recovery tests use real on-disk SQLite
files inside `tempfile::tempdir`. The `:memory:` mode is deliberately
not used — it skips the file-IO codepaths that production runs hit
(open, fsync, corruption recovery), so an in-memory test passing
gives false confidence.

```rust
#[test]
fn cache_db_rebuild() {
    let dir = tempdir().unwrap();

    // Seed spec files
    std::fs::create_dir_all(dir.path().join("specs")).unwrap();
    std::fs::write(dir.path().join("specs/auth.md"), "# Auth\n").unwrap();
    std::fs::write(dir.path().join("specs/api.md"), "# API\n").unwrap();

    let db = CacheDb::open(&dir.path().join("cache.db")).unwrap();
    let report = db.rebuild(dir.path(), &mock_bd_client()).unwrap();

    assert_eq!(report.specs_found, 2);
    assert!(report.counters_reset);

    let spec = db.spec(&SpecLabel::new("auth")).unwrap();
    assert_eq!(spec.spec_path, "specs/auth.md");
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

Render tests assert on the contract (partials included, agent content
wrapped, truncation applied) rather than full string parity. Contract
shape comes from the typed `LoopContext` struct; layout regressions are
caught by `insta` snapshots (see *Snapshot Testing*).

```rust
#[test]
fn run_wraps_agent_supplied_fields_in_agent_output() -> Result<()> {
    let ctx = LoopContext {
        pinned_context: PINNED_CONTEXT_BODY.into(),
        label: SpecLabel::new("harness"),
        spec_path: "specs/harness.md".into(),
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

Pi handshake timeout and workflow stall-heartbeat coverage depend on a
pending real pipe plus the outer timeout or watchdog. They use separate,
no-selector scripts under `tests/fixtures/agent/` rather than modes in the
general mock-pi table. Each script encodes only the pending lifecycle needed by
one integration test. A host-side upper deadline kills the process group, reaps
the child, and joins pipe readers if the production deadline regresses.
Malformed output does not require process lifecycle coverage and remains a Pi
parser unit test.

### Inbox Bridge Fixture

The Pi inbox bridge follow-up fixture is separate from the mock-pi mode table.
Its whole contract is one startup probe, one initial prompt that completes
without a terminal marker, one human reply encoded as a fresh `prompt`, and
then a terminal marker. It exists only because the bridge keeps the same
process pipes alive across a post-completion human reply; parser unit tests do
not exercise that process lifecycle. A conformance test drives this exact JSONL
exchange so the fixture does not grow into another Pi protocol emulator.

### Mock Claude Design

Mock Claude follows the same narrow, single-shot fixture pattern while speaking
Claude Code's stream-json framing. Its scenarios cover steering, shutdown
escalation, interactive compaction-hook delivery, and the smoke lifecycle.
Conformance comes from production launch paths that execute the mock; invoking
the script directly with a test-synthesized success payload is not evidence.

### Nix Integration

```nix
# tests/loom/default.nix
{ pkgs, loomPackage, ... }:
let
  inherit (loomPackage) craneLib;
in
{
  # Deterministic verifiers — invokes explicit tier subcommands:
  # `[check]` (one subprocess per `cargo run -p loom-walk -- …` annotation)
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
`packages.loom-tests` in `nix/flake/tests.nix`; it is not part of the
flake `checks` set. The fast `nix flake check` surface stays limited to
non-workspace-compile derivations. The full required suite is the
`nix run .#test` app in `nix/flake/apps.nix`: it runs the fast flake
tier, workspace clippy, full workspace nextest, and `loom gate system
--tree`. Grep-tier `[check]` annotations across specs use paths relative
to the staged-source root (which mirrors the `loom/` workspace flattened
to `$out/` plus host files like `lib/sandbox/linux/entrypoint.sh`
mirrored under their host paths), so the explicit tier commands run at
tree scope with no `--spec` filter. `loom-smoke` is exposed as
`nix run .#smoke` on Linux only.

## Success Criteria

### Unit tests

- Newtype serde round-trip tests cover all ID types (`BeadId`,
      `SpecLabel`, `MoleculeId`, `ProfileName`, `SessionId`,
      `ToolCallId`, `RequestId`)
  [test](serde_round_trips_as_plain_string)
- Closed-set enum tests cover `AgentRuntime` parse/serde and reject
      unknown runtime strings before manifest lookup or Wrix spawn
  [test](agent_runtime_parse_serde_rejects_unknown_values)
- Cache database round-trip tests cover spec rows, spec epics,
      work epics, notes, and criterion evidence without any
      `current_spec` operation
  [test](cache_db_round_trips_specs_epics_notes_and_criteria)
- Pi successful response envelopes preserve correlation id, command,
      success, data, and absent-error fields
  [test](pi_response_success_populates_data_field)
- Pi failure response envelopes preserve command failure and error fields
  [test](pi_response_failure_populates_error_field)
- Pi prompt, steer, and abort command serializers preserve their wire fields
  [test](command_structs_serialize_to_expected_type_field)
- Pi follow-up encoding preserves the idle prompt-cycle wire shape
  [test](encode_follow_up_emits_idle_prompt_command)
- Pi extension auto-cancel responses preserve type, id, and cancellation fields
  [test](extension_ui_response_serializes_with_all_fields)
- Claude `result` messages preserve every documented result field
  [test](result_message_round_trips_every_documented_field)
- Claude `system` messages preserve subtype and session id
  [test](system_message_maps_subtype_and_session_id)
- Claude assistant blocks preserve text and tool-use fields
  [test](assistant_block_text_and_tool_use_field_mapping)
- Claude user blocks preserve tool-result fields
  [test](user_block_tool_result_field_mapping)
- Claude control requests preserve id, tool, and input
  [test](control_request_message_round_trips_all_fields)
- Unknown Claude message types resolve through the forward-compatible
      `Unknown` variant
  [test](unknown_event_type_does_not_error)
- Template rendering tests cover every Askama template with
      representative inputs
  [test](template_renders_are_byte_stable_across_runs)

### Integration tests

These criteria cover the load-bearing flows in Functional #4 and the two
narrow process-lifecycle boundaries that parser unit tests cannot exercise.

- Startup probe round-trip: mock pi with valid `get_state` data
      → loom proceeds
  [test](pi_startup_probe_succeeds_with_valid_get_state)
- Startup probe malformed-state guard: mock pi with malformed `get_state`
      data → loom fails fast with a version-mismatch error
  [test](pi_startup_probe_fails_with_bad_get_state_shape)
- A dedicated no-selector Pi fixture leaves the startup probe unanswered so
      the assembled todo path surfaces its handshake timeout without adding a
      timeout mode to the general mock-pi table
  [test](loom_todo_pi_hang_probe_surfaces_handshake_timeout)
- A dedicated no-selector Pi fixture stalls after one prompt event so the
      assembled todo path emits its workflow stall warning without adding a
      stall mode to the general mock-pi table
  [test](loom_todo_pi_stall_mid_session_emits_stall_warning)
- Todo validation rejects an agent `LOOM_TODO` payload that omits any
      changed spec; no spec cursor advances and no work epic becomes
      `loom:active`
  [test](todo_success_missing_changed_spec_fails_without_advancing)
- The dedicated Pi inbox-bridge follow-up fixture stays outside the mock-pi
      mode table and covers only the probe → prompt → one human follow-up
      prompt → terminal-marker exchange
  [test](inbox_bridge_pi_followup_fixture_accepts_one_prompt_reply)

### Assembled-system checks

- `nix run .#smoke` follows the [agent-owned container launch
      boundary](agent.md#container-integration) to run a Pi-backed mock-agent
      bead against live bd, exiting 0 with the bead closed
  [system](nix run .#smoke)
- `nix run .#test-sandbox` launches the selected packaged Pi agent only for an
      offline `--version` health check: container networking is disabled, no
      provider credentials are supplied, and no prompt, protocol turn, or LLM
      API request occurs
  [system](nix run .#test-sandbox)

### Test-source portability

Style-rule and component-architecture criteria are owned by
[`docs/style-rules.md`](../docs/style-rules.md) and the relevant component
specs. This test-strategy spec retains only the test portability outcome it
owns:

- Rust tests use isolated temporary directories rather than hardcoded host
      temporary paths
  [check](cargo run -p loom-walk -- no_hardcoded_tmp_paths)

### Determinism

- `std::thread::sleep` is absent from production and unenumerated tests; each
      test exception is an exact bounded process-lifecycle or elapsed-performance
      registry entry
  [check](cargo run -p loom-walk -- no_thread_sleep)
- `tokio::time::sleep` appears only in clock implementations, synthetic paused
      tests, and exact bounded registry entries across production and test sources
  [check](cargo run -p loom-walk -- no_tokio_sleep_outside_clock)
- `tokio::time::timeout` appears only in clock implementations, synthetic paused
      tests, and exact bounded registry entries across production and test sources
  [check](cargo run -p loom-walk -- no_tokio_timeout_outside_clock)
- Real-clock reads appear only in clock implementations and exact bounded
      registry entries across production and test sources
  [check](cargo run -p loom-walk -- no_real_clock_outside_system_clock)
- `#[ignore]` never hides flaky, optional, or otherwise omitted coverage;
      each exception is an enumerated child-process entry point invoked by a
      non-ignored parent during the ordinary test suite and carries a
      process-boundary justification
  [test](no_ignore_for_flake)

### Annotation gate

- Every `[check]` / `[test]` / `[system]` / `[judge]` annotation in
      `specs/*.md` resolves to a valid verifier for its tier
  [test](end_to_end_specs_dir_check_combines_both_directions)

### Property-based testing

- JSONL backend parsers never panic on arbitrary bounded text
  [test](jsonl_arbitrary_bytes_never_panic)
- Malformed JSONL never emits an agent event or protocol response
  [test](jsonl_malformed_line_emits_no_events)
- The JSONL framing cap remains ten mebibytes
  [test](max_line_bytes_is_ten_megabytes)
- Pi prompt encoding round-trips arbitrary message text
  [test](pi_encode_prompt_round_trips)
- Pi steer encoding round-trips arbitrary message text
  [test](pi_encode_steer_round_trips)
- Unknown correlated Pi message types return a typed protocol error
  [test](pi_unknown_message_type_surfaces_typed_error)
- Pi parsing never panics on arbitrary bytes
  [test](pi_arbitrary_bytes_never_panic)
- Claude system messages preserve generated known-shape fields
  [test](claude_system_round_trips)
- Claude result messages preserve generated result fields
  [test](claude_result_round_trips)
- Unknown Claude types resolve through the serde fallback
  [test](claude_unknown_type_falls_through_serde_other)
- Claude parsing never panics on arbitrary bytes
  [test](claude_arbitrary_bytes_never_panic)
- Arbitrary spec bodies do not corrupt the cache schema during rebuild
  [test](rebuild_never_corrupts_schema)
- Arbitrary corrupt cache bytes recover through `recreate`
  [test](recreate_recovers_from_arbitrary_bytes)
- Cache rebuild round-trips generated durable source shapes
  [test](rebuild_round_trips_known_shapes)
- `PROPTEST_CASES=32` for CI; overridable via env var
  [check](grep -q 'pub const CI_PROPTEST_CASES: u32 = 32' crates/loom-test-support/src/lib.rs)

### Snapshot testing

- Every Askama workflow template has a representative `insta` snapshot
  [test](every_askama_template_has_snapshot)
- `loom --help` and every subcommand `--help` have `insta` snapshots
  [test](all_cli_help_snapshots)
- Loop renderer uses substring + structural assertions, not
      `insta` (ensures terminal-output flexibility)
  [check](cargo run -p loom-walk -- renderer_no_insta_dependency)

### Cross-platform

- The flake source declares shared `loom-tests` package wiring for its four
      configured Linux and Darwin system identifiers
  [check](cargo run -p loom-walk -- test_nix_surface_contract)
- The smoke app selects the real image-backed implementation only on Linux
  [check](cargo run -p loom-walk -- test_nix_surface_contract)
- The Darwin smoke branch is an explicit successful unavailable-platform stub
  [check](cargo run -p loom-walk -- test_nix_surface_contract)

### CI integration

- The `loom-tests` package invokes both deterministic gate tiers and remains
      outside the flake `checks` set
  [check](cargo run -p loom-walk -- test_nix_surface_contract)
- `nix run .#test` composes the fast flake tier, workspace Clippy, full
      nextest, and system verifiers
  [check](cargo run -p loom-walk -- workspace_compile_checks_are_full_test_app_only)
- `nix run .#smoke` carries a concrete mock-Pi image, immutable ProfileConfig,
      Linux implementation, and Darwin stub
  [check](cargo run -p loom-walk -- test_nix_surface_contract)
- `nix run .#fuzz-loom` is on-demand and absent from flake checks
  [check](cargo run -p loom-walk -- test_nix_surface_contract)
- Container smoke enforces a 30-second wall-time budget
  [check](cargo run -p loom-walk -- test_nix_surface_contract)

## Requirements

### Functional

1. **Three test levels** with complementary scope (each level is
   addressed by one or more annotation tiers; the levels here are the
   *test-design* axis, not the annotation-tier axis):
   - **Unit tests** — per-crate, fast, no external dependencies.
     Inline `#[cfg(test)] mod tests` blocks. Annotated `[test]`; run
     via `loom gate test`, which dispatches to `cargo nextest`.
   - **Integration tests** — cross-crate, use mock agent processes
     over real pipes, no containers. Live in
     `crates/<crate>/tests/*.rs`. Annotated `[test]`; run via
     `loom gate test`.
   - **Container smoke** — one happy-path scenario that uses the
     [agent-owned container launch boundary](agent.md#container-integration),
     runs a mock agent *inside* the container, drives `loom loop <bead-id>`,
     and asserts the bead closes. It validates host↔container assembly and
     teardown — *not* protocol depth, which the integration level already
     covers. Annotated `[system](nix run .#smoke)`; run
     via `loom gate system`. Linux-only (no podman in Darwin CI).

2. **Mock agent processes** — process-level fixtures driven over real pipes
   from cargo integration tests, plus the in-container smoke:
   - **Mock pi** (`tests/mock-pi/pi.sh`) — narrowly scoped scenario
     modes that exercise the *pipe-level* paths the parser unit tests
     can't reach (probe round-trip, prompt ack, mid-session steer,
     compaction re-pin via steer, interactive compaction canary,
     `set_model` from phase config, plus `happy-path` for the container
     smoke).
   - **Mock claude** (`tests/mock-claude/claude.sh`) — modes for
     mid-session steering via stream-json user message, the shutdown
     watchdog SIGTERM→SIGKILL escalation, interactive compaction canary,
     plus `happy-path` for the container smoke.
   - **Out of scope for mock mode tables**: tool-call simulation,
     malformed-JSONL injection, hang/timeout modes, and general multi-turn
     behavior. The narrow interactive compaction canary is the only scripted
     multi-turn exception because the bug is visible only after a
     post-compaction probe. Parser unit tests cover malformed protocol input
     with inline string literals. Named process-lifecycle fixtures may hold a
     real pipe pending only for the handshake-timeout and workflow-stall
     assertions, and carry no mode selector or broader protocol behavior.

3. **Rust test coverage by component** — private behavior is exercised
   inline where white-box access is useful; public, cross-module, and process
   boundaries use Cargo integration tests where that boundary adds signal.
   Leaf crates are not required to create an integration-test file solely for
   layout symmetry. The lists below describe coverage areas rather than
   internal file organization.

   #### loom-driver
   - Newtype construction and serde round-trips (`BeadId`, `SpecLabel`,
     `MoleculeId`, `ProfileName`)
   - `CacheDb` schema creation on first open
   - `CacheDb` query methods return typed rows (`SpecRow`, `SpecEpicRow`,
     `WorkEpicRow`, criterion-evidence rows)
   - `CacheDb::rebuild` populates from the spec index, spec files, and mock
     `bd` output
   - Companion-section parser: spec with a `## Companions` section
     containing two backtick-delimited paths yields two `companions` rows;
     spec without the section yields zero
   - Parser ignores: text outside backticks on a bullet line, blank
     bullets, multi-path bullets (skipped with warn, not error)
   - Parser is case-sensitive on the heading: `## companions` (lowercase)
     and `## Companion paths` are not recognized
   - No `current_spec` / `set_current_spec` API exists
   - `increment_iteration` for a work epic returns updated count, starts at 0
   - `bd` CLI output parsing (JSON → typed structs)
   - `bd` CLI error mapping (exit codes → error variants)
   - `bd` CLI wrapper passes every argument via `Command::arg()` — never
     shell interpolation. Tests inject values containing shell
     metacharacters (`; rm -rf /`, `` `id` ``, `$(whoami)`) and assert
     they reach `bd` literally as one argv element each, never expanded
   - Config file loading (TOML parsing into `LoomConfig`), defaults when
     file is absent or fields are missing
   - `SpawnConfig` JSON serialization round-trips with stable field ordering
     and key names at the [agent-owned launch boundary](agent.md#spawnconfig).
     Adding a field is non-breaking; renaming or removing one is — the test
     pins the on-disk shape so changes surface as test failures, not silent
     wire-format drift. Includes the optional
     `model: Option<ModelSelection>` field with
     `#[serde(skip_serializing_if = "Option::is_none")]` so the on-disk
     shape is stable whether the field is present or absent

   #### loom-agent
   - Pi RPC command serialization (Rust struct → JSONL line)
   - Pi RPC event deserialization via two-phase strategy:
     - Envelope parse (`PiEnvelope` with `type` + `id`) classifies the line
     - Full parse into `PiResponse`, `PiEvent`, or `PiUiRequest`
     - Test that envelope-only parse does not fail on unknown fields
   - `PiResponse` success/failure discrimination: `success: true` extracts
     `data`, `success: false` extracts `error` message; idless prompt
     acknowledgements parse and are ignored mid-session
   - `message_update` nested delta dispatch: `text_delta` →
     `AgentEvent::TextDelta`, `thinking_delta` →
     `AgentEvent::ThinkingDelta`, idful `toolcall_delta` →
     `AgentEvent::ToolcallDelta`, idless `toolcall_delta` → skipped,
     `error` → `AgentEvent::Error`, `done` → skipped (empty events)
   - Pi `tool_execution_start` field mapping: `toolCallId` → `ToolCallId`,
     `toolName` → `tool`, `args` → `params`
   - Pi `tool_execution_end` field mapping: `result` → `output`,
     `isError` → `is_error`
   - Claude stream-json event deserialization (`#[serde(tag = "type")]` →
     `ClaudeMessage`)
   - Claude `#[serde(other)]` catches unknown event types without error
   - Per-phase backend resolution (`[phase.todo].agent.backend` overrides
     `[phase.default].agent.backend`, `--agent` flag overrides all phases)
   - Backend launch tests execute the runtime-to-child-environment contract
     owned by [agent.md § Container integration](agent.md#container-integration)
   - Malformed JSONL handling — specific test cases:
     - Truncated JSON (`{"type": "message_del`) → `ProtocolError::InvalidJson`
     - Valid JSON, wrong shape (`{"foo": 42}`) → `ProtocolError::UnknownMessageType`
     - Empty line between objects → silently skipped
     - Line containing only whitespace → silently skipped
     - Escaped `\n` inside a JSON string value (e.g. `{"text":"line1\nline2"}`)
       → parsed as a single line, string value contains literal newline
     - U+2028/U+2029 inside JSON string → passes through, not treated as
       line terminator
     - Trailing `\r\n` → `\r` stripped, parsed normally
     - Line exceeding `MAX_LINE_BYTES` (10 MB) → `ProtocolError::LineTooLong`
   - `ParsedLine::response` populated for Claude `control_request` (parser
     returns auto-approve JSON string, `events` is empty)
   - `ParsedLine::events` contains two events for Claude `result/success`
     (`TurnEnd` + `SessionComplete`); two events for `result/error`
     (`Error` + `SessionComplete`); Pi's `turn_end` and `agent_end` each
     map to a single event
   - `ParsedLine::response` is `None` for Pi events and Claude non-control
     events
   - Event normalization (all backends produce identical `AgentEvent`
     sequences for equivalent agent behavior)
   - Timeout behavior: no JSONL line for 5+ minutes → warning logged, no
     abort

   #### loom-templates
   - All templates compile — Askama enforces this at build time; an
     explicit `cargo nextest run -p loom-templates` is the regression
     gate
   - Template rendering with representative inputs produces output
     containing required partials, agent-output wrapping, and applied
     truncation (see *Architecture / Test Patterns / Template render
     contract*)
   - Layout regressions caught by `insta` snapshots (see
     *Architecture / Snapshot Testing*)
   - Partial inclusion works (context pinning, exit signals, spec
     header, companions, implementation notes)

   #### loom-workflow
   - `loom plan [SPEC_LABEL ...]` anchor parsing, including zero anchors,
     multiple anchors, missing-label-as-new-spec, and interspersed options
   - Todo preflight, terminal validation, and lifecycle tests execute the
     [harness-owned Todo contract](harness.md#functional), including production CLI
     routing where cursor discovery is the behavior under test
   - Profile/runtime selection from bead labels plus resolved backend
     (parse, fallback to base, flag override, missing runtime failure)
   - Interactive `plan` / `inbox chat` command-construction tests cover the
     backend-specific launch matrix owned by
     [agent.md § Interactive Shell-Out](agent.md#interactive-shell-out)
   - Retry logic (failure count tracking, `loom:clarify` label after max
     retries)
   - Push gate logic (clean completion, fix-up beads, iteration cap)
   - No per-bead `bd dolt push/pull` is invoked: assert `BdClient` exposes
     no `dolt_push`/`dolt_pull` methods and the workflow paths do not
     spawn `bd dolt …` subprocess calls (containers reach the authoritative
     state via the bind-mounted Dolt socket)
   - Parallel dispatch and integration tests execute the
     [harness-owned bead lifecycle](harness.md#bead-dispatch) through public
     seams; production CLI coverage is used when command routing is the
     behavior under test

   #### Concurrency & locking (loom-driver)
   - Lock tests execute the [harness-owned locking
     contract](harness.md#concurrency--locking). In-process tests cover typed
     selection and error mapping; process tests are reserved for actual flock
     contention and crash release.

   #### Auxiliary commands (loom-workflow)
   - `loom init` writes a default `loom.toml` and creates `.loom/cache.db`
     with the expected schema (specs, spec epics, work epics, companions,
     notes, criterion status, meta tables)
   - `loom init` is idempotent: running twice does not clobber existing
     notes or cache rows that can still be validated against durable state
   - `loom init --rebuild` drops and repopulates cache rows from the spec
     index, `specs/*.md`, mock bd spec/work epics, and companions;
     iteration counters reset to 0
   - `loom status` prints active work epic, pending `loom:todo` work epic,
     iteration count, and cache health in a stable parseable format
   - `loom status` with no active work epic exits 0 with a clear message,
     not an error
   - `loom logs` rendering, replay, follow, raw, and path-selection
     cases are owned by [events.md](events.md)
   - `loom spec <label> --deps` parses the named spec's `[check]` /
     `[test]` / `[system]` / `[judge]` annotations, opens each referenced
     verifier source, and prints the deduplicated set of nixpkgs needed
   - CLI surface: `loom --help` lists every v1 command (`plan`,
     `todo`, `loop`, `gate`, `inbox`, `tune`, `spec`, `init`, `status`,
     `logs`, `note`)

   #### Events and rendering
   - Event schema, live rendering, replay rendering, log persistence,
     retention, and tracing-boundary tests are owned by [events.md](events.md)

   #### GitClient (loom-driver)
   - Git integration tests execute the [harness-owned typed Git
     contract](harness.md#bead-dispatch) against temporary repositories.
     Process tests cover only behavior that cannot be exercised through an
     in-memory seam.

   #### loom-walk
   - Walk dispatch: `loom-walk <name>` invokes the named walk; an
     unknown name exits non-zero with a clear error naming the
     available walks
   - Each walk reads `LOOM_FILES` (colon-separated paths) if set and
     filters its input set; absent means scan the walk's declared
     scope
   - Output conforms to the verifier-runner contract: one JSON line
     on stdout (`{"pass": bool, "evidence": "<path>:<line> <rule>"}`),
     exit code mirrors `pass`
   - Per-walk fixtures: each walk has a `#[test]` exercising both pass
     and fail cases against synthetic source under `tempfile::tempdir`

   #### loom-gate
   - Annotation parser: walks `specs/*.md`, regex-extracts
     `[tier](target)` annotations, returns typed `Annotation` records
     (tier, target, source spec, line)
   - Per-tier dispatcher: `[check]` and `[system]` route to one
     subprocess per annotation; `[test]` and `[judge]` collect targets
     for batched invocations
   - Toolchain detection: `Cargo.toml` at root → cargo nextest runner
     template; `pyproject.toml` → pytest; `go.mod` → go test
   - `<workspace>/loom.toml` loading: `[runner.<tier>.<name>]`
     tables parse into per-tier runners with `match`/`command`/
     `target`/`join`/`parse`/`cwd` fields; missing file falls back to
     detected defaults
   - Status cache schema: per-criterion row with annotation target,
     last-run timestamp, commit hash, verdict (pass / fail / skipped),
     evidence string
   - Status cache writes on every verifier invocation; reads on plain
     `loom gate` for the report
   - Integrity gate forward direction: every annotation's target is
     valid for its tier (resolves on PATH for `[check]` / `[system]`;
     resolves to a `#[test]` function via cargo metadata for `[test]`;
     resolves to a file on disk for `[judge]`)
   - Integrity gate atomic acceptance: each criterion carries exactly
     one annotation
   - Integrity gate self-test: its own criterion in `gate.md`
     annotates back to its implementation
   - `--files` scope filtering for `[test]`: cargo metadata computes
     scope per annotation (files in crate(test) ∪ transitive deps);
     intersection with input file set determines which tests batch
   - Test-tier silent-zero-match sniffing: cargo / nextest / pytest
     stdout post-processed to detect zero-match cases and fail loud

4. **Integration test coverage** — load-bearing tests execute public
   cross-crate or operating-system process seams. Backend launch and protocol
   behavior is owned by [agent.md](agent.md); cache, Git, todo, parallel
   dispatch, and locking behavior is owned by [harness.md](harness.md); event
   fan-out and persistence behavior is owned by [events.md](events.md). This
   spec owns the classification and fixture discipline: parser-only shape
   checks stay in unit tests, while startup handshakes, pending pipes, child
   reaping, and production CLI routing use integration tests.

5. **Container smoke coverage** — one happy-path scenario validates
   host↔container plumbing that the integration tier cannot reach. A temporary
   workspace is seeded with one ready `profile:base` bead, and a concrete test
   image carries mock Pi through the [agent-owned container launch
   boundary](agent.md#container-integration). The smoke asserts the container
   exits cleanly and the bead closes.
   Workflow-level coverage (plan/todo/loop/gate/inbox/tune, profile/runtime
   selection, agent switching) lives in inline
   `#[cfg(test)] mod tests` blocks under `loom-workflow/src/` — those
   are exercised via `cargo nextest run`, not the smoke.

6. **Rust style enforcement** — [`docs/style-rules.md`](../docs/style-rules.md)
   and the workspace configuration own the exact lint policy. Clippy-backed and
   source-walking assertions are `[check]` verifiers; component specs own the
   architectural facts those walks inspect.

7. **Annotation contract** — every acceptance criterion in any spec
   under `specs/` carries a `[check]`, `[test]`, `[system]`, or
   `[judge]` annotation that must resolve to an existing verifier.
   The full rules (syntax, cardinality, classification, cross-spec
   sharing) live in [`docs/spec-conventions.md`](../docs/spec-conventions.md);
   the integrity gate that enforces them lives in
   [gate.md](gate.md).

8. **Property-based testing** — `proptest` for invariants on four
   targets: JSONL line parser, Pi protocol parser, Claude protocol
   parser, cache DB rebuild. Properties target invariants ("never
   panics on arbitrary input", "round-trip is identity for known
   shapes", "unknown shapes map to typed errors") rather than
   specific input/output pairs. CI runs each property at
   `PROPTEST_CASES=32`; local exhaustive runs use `PROPTEST_CASES=2048+`
   via env var. No `cargo fuzz` under `nix flake check` — exposed
   separately as `nix run .#fuzz-loom` for on-demand or nightly use.

9. **Snapshot testing** — `insta` snapshots for templates and CLI help
    output (contract surfaces where layout regressions matter).
    Substring + structural assertions for the loop renderer
    (terminal tool-call lines, status colors — surfaces with
    intentional flexibility). Snapshot updates require explicit
    acknowledgment in the PR description ("snapshot updated because:
    ...") to surface accidental drift.

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
   defined in *Architecture / Determinism Through Clock Injection*.
2. **Fast** — soft targets per gate command, warm cache:
   - `loom gate` (status, no verifiers): <100 ms (and a hard <500 ms
     ceiling, asserted by a self-test on the cache implementation).
   - `loom gate check`: <5 s aggregate across all `[check]` walks.
   - `loom gate test`: <30 s aggregate (one batched cargo-nextest
     invocation; nextest's internal parallelism does the heavy
     lifting).
   - `loom gate system`: <60 s per verifier; container smoke targets
     <30 s.
   - `loom gate judge`: no fixed target; bounded by LLM API
     concurrency.

   All except the `loom gate` status ceiling are *soft* — they guide
   design (host waits require audited boundaries, subprocess tests need
   justification, proptest case count bounded) but the gate doesn't fail when a
   budget is exceeded; humans review timing in PRs.
3. **Isolated** — each test uses its own temp directory and beads database
   prefix. No shared mutable state between tests.
4. **Parallel-safe** — unit and integration tests run in parallel
   under `cargo nextest`'s process-per-test model. Each test gets a
   fresh process, so global state (env vars, working directory,
   process-level locks) doesn't leak between tests. The container
   smoke (single scenario) gets its own pre-seeded `.beads/` snapshot
   in a tempdir, fully isolated from any concurrent peers running
   against the workspace.
5. **Push-friendly full suite** — `nix flake check` runs the fast
   deterministic derivations that stay inside the interactive push
   budget. Full workspace nextest plus `[system]`/container verifiers
   live in the explicit `nix run .#test` full-suite app, which pre-push
   invokes because this repository has no separate CI safety net. The
   container smoke remains exposed as `nix run .#smoke` because it needs
   podman at runtime; its acceptance criterion is annotated
   `[system](nix run .#smoke)`. Pre-push also runs clippy plus targeted
   `loom gate verify --diff`; scope-derived gate policy excludes
   `[system]` from finite diff verification while project-specific hook
   composition, stage budgets, and lock semantics live in
   [pre-commit.md](pre-commit.md).
6. **Real bd** — the container smoke runs against live `bd` (not a
   mock). The integration tier may mock `bd` where the test concern
   is orthogonal to the issue tracker, but the smoke validates that
   loom and `bd` interact correctly under realistic conditions.
7. **Cross-platform source composition** — the flake declares shared test
   package wiring for its configured Linux and Darwin system identifiers.
   The source-level verifier checks that composition; platform-native builds
   remain the authority for whether a package builds on that host and are not
   inferred from a foreign-system source grep. The container smoke is
   Linux-only (podman dependency); on Darwin the `smoke` app exits 0 with a
   clear "container smoke not available on Darwin" message. Tests use
   `tempfile::tempdir` rather than hardcoded `/tmp/...` paths so the source is
   compatible with Darwin's build sandbox. Darwin smoke support is a
   follow-up.
8. **Subprocess-spawning tests are exceptional** — each subprocess test
   (mock-pi, mock-claude, real `git`) costs 50-200ms; ten of them blow
   the 5s soft target alone. A test that spawns a subprocess includes a short
   comment or doc string explaining why an in-process equivalent (via
   `LineParse` + `tokio::io::duplex`) is not feasible. Any direct host-clock
   read or sleep also appears in the exact audited exception registry with a
   finite upper deadline, reliable child cleanup, the smallest practical
   duration, and deterministic companion coverage for timer logic.
9. **Upstream protocol versioning** — Pi Coding Agent and Claude Code
   versions are pinned by the repo's nixpkgs input. Bumps are deliberate PRs
   accompanied by a protocol-bump checklist (re-run parser tests, scan
   upstream changelog for new event types, add `Unknown` coverage if
   any new types lack typed variants, update mock scripts if new types
   reach pipe-level paths). No live wire tests run against real binaries; the
   Functional #10 exception proves only that the selected package launches.
   Detection coverage: silent breaks in *exercised* fields surface as
   `serde_json` errors in parser tests when the pinned version is
   bumped. Fields not exercised by any test could still drift silently
   — parser tests must therefore touch every field of every documented
   message type for the pinned version, not just every type.
10. **No `#[ignore]` for skipped coverage** — `#[ignore]` cannot hide a
    flaky, optional, or otherwise omitted test; fix the root cause or delete
    the test. The only exceptions are enumerated child-process entry points
    that a non-ignored parent invokes during the ordinary test suite because
    process isolation is itself part of the behavior under test. Each exception
    carries a process-boundary justification in the audited allowlist, and
    every unenumerated marker fails the audit. A CI flake opens a `loom-flake`
    P1 bead naming the failing test; the test is fixed before any further work
    on the affected crate.

## Out of Scope

- **Real-binary behavioral tests** — no test invokes real Claude Code or uses
  real Pi for a conversation, prompt, protocol turn, or LLM API request. Mock
  pi and mock claude scripts cover the protocol surface (parser tests use
  inline strings; mocks cover pipe-level paths; smoke runs mock pi inside the
  container). The sole real-agent exception is the offline packaged-Pi
  `--version` health check in Functional #10; it detects image/runtime
  packaging drift but does not validate conversational or protocol behavior.
  The pinned nixpkgs input plus parser tests with field-level coverage catch
  silent protocol drift on bumps.
- **macOS container smoke** — the smoke requires `podman` (Linux). Darwin
  container testing is a follow-up.
- **Mocking `bd`** — the container smoke uses live `bd` (see NFR #6).
- **Broader system-tier scenario library** — `tests/loom/scenarios/` with
  steering, compaction, error-recovery scripts. The integration tier
  already covers these flows via shim-based mocks; repeating them with
  podman adds CI time without catching new failure modes. One happy-path
  smoke is sufficient to validate host↔container plumbing.
- **Captured JSONL fixtures** — `loom-agent/src/{pi,claude}/fixtures/`
  with replay scripts. Parser tests use inline string literals, which are
  easier to read in PR diffs and don't bit-rot when pi/claude release new
  event shapes.
- **External-template parity fixtures** — any compatibility-fixture
  set tied to a predecessor templating system that is itself
  scheduled for removal. Such fixtures become irrelevant the moment
  the predecessor is removed; capturing them is wasted work.
- **Pi cost capture** — deferred to loom-agent. When pi's
  `get_session_stats` is wired up after the startup probe, loom-tests
  gains one acceptance criterion: a round-trip test asserting that
  `SessionOutcome.cost_usd` is populated for pi sessions, parallel to
  the existing claude `result/total_cost_usd` extraction.
- **General mock-script protocol breadth** — tool-call simulation,
  malformed-JSONL injection, hang/timeout modes, and general multi-turn
  conversations do not belong in the general mock-pi/mock-claude scripts.
  Parser unit tests own malformed protocol input. The interactive compaction
  canary is the only multi-turn exception in those mode tables, because it
  verifies a post-compaction turn rather than protocol breadth. Dedicated
  bridge and process-lifecycle fixtures remain single-purpose, carry their own
  conformance tests, and are not folded into the general mock-agent mode
  tables.
- **Per-repo verifier registry separate from `loom.toml`** —
  annotations carry the verifier directly (target name for `[test]`
  / `[judge]`, command for `[check]` / `[system]`); no separate
  config maps names to commands. Toolchain detection (`Cargo.toml`
  at repo root → cargo nextest, etc.) supplies defaults for
  batched-tier runners; `<workspace>/loom.toml` is the override
  path when defaults don't fit, not a per-verifier registry.
- **`cargo fuzz` under `nix flake check`** — exposed as `nix run .#fuzz-loom`
  for on-demand or nightly runs only. proptest covers invariants in CI.
- **Hard CI-time NFR for the verify path** — the per-tier budgets
  (Non-Functional #2) are soft design targets, not CI failure
  thresholds. They guide decisions (no real sleeps, subprocess tests
  need justification, proptest case count bounded) but the gate
  doesn't fail when a budget is exceeded; humans review timing in
  PRs. Exception: `loom gate` status has a hard <500ms ceiling with a
  self-test — that one is a regression of the cache implementation,
  not of the corpus.
