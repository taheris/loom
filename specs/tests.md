# Loom Tests

Test strategy and infrastructure for the Loom agent driver.

## Problem Statement

Loom is a Rust binary that orchestrates per-bead agent sessions
across a multi-crate workspace. Testing has to cover three things at
once: protocol parsing across two agent backends (pi-mono RPC, Claude
stream-json), workflow orchestration (state DB, locking, worktree
parallelism, push gate), and host↔container plumbing (entrypoint
branching, bind mounts, profile selection). All three need
first-class coverage in a Rust-native test framework with explicit
state-DB and protocol-parser tests.

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

### Test File Layout

Each crate uses two complementary Rust test homes:

- **Inline `#[cfg(test)] mod tests { … }`** at the bottom of each source file
  — white-box tests with access to private impl details, kept next to the
  code they exercise so changes land together.
- **Cargo integration tests** under `crates/<crate>/tests/*.rs` —
  black-box tests that import the crate by its public API, exercising
  cross-module behaviour and the surfaces that downstream crates also see.

```
loom/
  crates/
    loom-driver/
      src/
        state/
          db.rs               # StateDb impl + inline #[cfg(test)] mod tests
          rebuild.rs          # rebuild logic + inline tests
          companions.rs       # `## Companions` parser + inline tests
        bd/
          client.rs           # bd CLI wrapper + inline tests
          label.rs            # Label newtype + inline tests
        agent/
          repin.rs            # RePinContent + inline tests (doc-tested)
          ...
      tests/
        state_db.rs           # Integration: StateDb across rebuild + queries
        lock_manager.rs       # Integration: per-spec advisory locking
        git_client.rs         # Integration: GitClient against a temp repo
        logging.rs            # Integration: shared renderer + log channel
        properties.rs         # proptest invariants for state DB rebuild
    loom-events/              # Public contract leaf crate — `AgentEvent`,
      src/                    #   identifier newtypes, `Session` trait. Tiny
        identifier/           #   dep surface (serde, futures-core, thiserror).
          bead.rs             # BeadId + inline tests (validation, serde)
          ...                 # one file per id newtype, all with inline tests
        event.rs              # AgentEvent + envelope types
        lib.rs                # Session / EventSink contract + inline tests
    loom-agent/
      src/
        pi/
          mod.rs
          parser.rs           # JSONL parsing + inline tests (string literals)
          backend.rs          # spawn / lifecycle + inline tests driving mock-pi
          messages.rs
        claude/
          mod.rs
          parser.rs           # stream-json parsing + inline tests (string literals)
          backend.rs          # spawn / lifecycle + inline tests driving mock-claude
          messages.rs
      tests/
        static_dispatch.rs    # Compile-time check: both backends impl AgentBackend
        properties.rs         # proptest invariants for pi/claude protocol parsers
    loom-workflow/
      src/
        run/
          mod.rs              # run loop + inline tests for unit-level helpers
        check/
          mod.rs              # push gate + inline tests
        ...
      tests/
        parallel.rs           # Integration: --parallel N bead-clone dispatch
    loom-templates/
      src/
        ...                   # per-template module + inline rendering tests
      tests/
        render.rs             # Integration: every template renders with partials
    loom/
      tests/
        run_smoke.rs          # Integration: CLI subcommand surface
        agent_flag.rs         # Integration: --agent flag parsing/validation
        spawn_dispatch.rs     # Integration: shim-based wrapix spawn argv
                              #   contract + stdin-pipe-not-tty assertion
                              # properties.rs is reserved here for cross-crate
                              #   invariants; per-crate properties live in
                              #   each crate's own tests/properties.rs
    loom-walk/                # [check]-tier verifier binary — takes named
      src/                    #   walks as positional args. Annotations point
        main.rs               #   at it: [check](cargo run -p loom-walk -- <name>)
        walk/
          mod.rs              # name → walk fn dispatch
          no_gix_outside_git_client.rs
          no_types_files.rs
          template_ctx.rs
          newtype_identifiers.rs
          no_hardcoded_tmp_paths.rs
          ...                 # one walk per file
      tests/
        fixture.rs            # per-walk pass/fail fixtures
    loom-gate/                # The gate runner. Owns annotation dispatch,
      src/                    #   status cache, integrity gate. See gate.md.
        annotation.rs         # [tier](target) parser
        dispatch.rs           # per-tier dispatch (subprocess, batched, LLM)
        runner.rs             # toolchain detection + <workspace>/loom.toml [runner.*]
        cache.rs              # status cache schema + reads/writes
        integrity.rs          # integrity gate (itself a [check] walk)
      tests/
        annotation_parse.rs   # Integration: spec walking + annotation extract
        dispatch.rs           # Integration: per-tier dispatch contract
        cache.rs              # Integration: status cache round-trip
        integrity.rs          # Integration: forward + atomic-acceptance

tests/
  loom/
    default.nix               # Nix derivation: `loom gate verify`
    run-tests.sh              # Container smoke harness (single happy-path)
    mock-pi/pi.sh             # Mock pi (scoped scenario modes)
    mock-claude/claude.sh     # Mock claude (scoped scenario modes)
```

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
git subprocess timeouts — make tests flaky when they touch real wall
time on a loaded CI runner. The design eliminates real-time waits.

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

**Filesystem mtime in tests** is set via the `filetime` crate. Real
wall time stays zero; tests can express "this file is 15 days old"
without sleeping.

**Banned patterns** (enforced by walks in `loom-walk`):

- `std::thread::sleep` — anywhere, no exceptions.
- `tokio::time::sleep` outside `SystemClock::sleep`'s implementation.
- `tokio::time::timeout` outside `SystemClock::timeout`'s
  implementation.
- `Instant::now()` / `SystemTime::now()` outside `SystemClock::now()`.

Tests that need to advance time construct a `MockClock` directly
(`MockClock::new()` or via a small `with_mock_clock` helper in
`loom-driver::testing`) and pass it as `&dyn Clock` into whatever
component is under test. There is no other opt-out path; the bans
apply uniformly across `src/` and `tests/`.

### Style Enforcement

Two complementary mechanisms:

**Workspace clippy lints** for what clippy supports natively
(`unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`,
`allow_attributes`). The full configuration is the contract in
[`docs/style-rules.md`](../docs/style-rules.md) under RS-3
(*Workspace lint configuration*); this spec does not duplicate the
rule list. Tests opt out via per-file
`#![allow(clippy::unwrap_used, ...)]` at the top of
`crates/*/tests/*.rs` and inside `#[cfg(test)] mod tests`
blocks.

**Source-walking checks** for rules clippy can't express. Each walk
is a `[check]`-tier verifier in `loom-walk`. The rule set is owned
by [`docs/style-rules.md`](../docs/style-rules.md) (RS-5, RS-7, RS-8,
RS-16, RS-18, and the test-discipline rules TST-*); this spec lists
the walks the repo ships, not the rules they enforce:

| Walk | Enforces |
|------|----------|
| `no_derive_from_on_newtypes` | RS-8 |
| `no_types_or_error_files` | RS-5 |
| `git_client_encapsulation` | architectural — `GitClient` is the only `gix` / `git` CLI site |
| `single_event_channel` | architectural — renderer + log writer subscribe to one `AgentEvent` sender |
| `newtype_identifiers` | RS-7 |
| `template_context_structs` | architectural — each Askama template has a typed context |
| `no_hardcoded_tmp_paths` | NFR #7 (Darwin sandbox compatibility) |

`syn` and `walkdir` are `[dev-dependencies]` of `loom-walk`. Output
on failure follows the verifier-runner contract in gate.md:
JSON-line stdout `{"pass": false, "evidence": "<path>:<line> <rule>"}`
so reviewers can click directly into the violation.

### Property-Based Testing

`proptest` for invariants on four targets:

| Target | Invariants |
|--------|------------|
| JSONL line parser | never panics on arbitrary bytes; respects `MAX_LINE_BYTES`; never emits `AgentEvent` from a malformed line |
| Pi protocol parser | round-trip identity for known shapes; unknown shapes map to `ProtocolError::UnknownMessageType`; never panics |
| Claude protocol parser | round-trip identity for known shapes; unknown shapes map to the `Unknown` variant via `#[serde(other)]`; never panics |
| State DB rebuild | never panics on arbitrary spec file content; schema invariants always hold; corrupted DB always recovers via `recreate` |

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

- Templates (`templates`) — every Askama template × representative
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
fn state_db_rebuild() {
    let dir = tempdir().unwrap();

    // Seed spec files
    std::fs::create_dir_all(dir.path().join("specs")).unwrap();
    std::fs::write(dir.path().join("specs/auth.md"), "# Auth\n").unwrap();
    std::fs::write(dir.path().join("specs/api.md"), "# API\n").unwrap();

    let db = StateDb::open(&dir.path().join("state.db")).unwrap();
    let report = db.rebuild(dir.path(), &mock_bd_client()).unwrap();

    assert_eq!(report.specs_found, 2);
    assert!(report.counters_reset);

    let spec = db.spec(&SpecLabel::new("auth")).unwrap();
    assert_eq!(spec.spec_path, "specs/auth.md");
}

#[test]
fn state_db_corruption_recovery() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("state.db");

    // Write garbage to the DB file
    std::fs::write(&db_path, b"not a sqlite db").unwrap();

    // open detects corruption, rebuild recovers
    let db = StateDb::open(&db_path).unwrap();
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

Mock pi is a shell script that frames pi-mono's RPC protocol as JSONL
on stdin/stdout. Its job is to exercise *process-level* paths the
parser unit tests cannot reach — round-tripping through real pipes,
stdin write-back from `ParsedLine::response`, and child reaping. Each
mode is shaped to exactly one Rust test that drives it; the script is
not a general-purpose pi emulator.

Modes (selected via `argv[1]`):

| Mode | Used by | Wire behavior |
|------|---------|---------------|
| `probe-ok` | startup probe round-trip test | Replies to `get_commands` with the full required set |
| `probe-missing-set-model` | startup probe failure test | Replies to `get_commands` omitting `set_model` |
| `echo-prompt` | wire-shape assertion test | Probe ok, then echoes the prompt payload as a `message_delta` |
| `steering` | mid-session steer test | Probe ok, prompt → first turn, then echoes the steer payload on the next turn |
| `compaction` | re-pin-via-steer test | Probe ok, emits `compaction_start`, expects the re-pin steer, echoes it back, emits `compaction_end` |
| `set-model` | per-phase model override test | Probe ok, expects `set_model { provider, modelId }`, echoes the pair into a later `message_delta` |
| `happy-path` | container smoke | Probe ok, prompt → `message_delta` → `agent_end` |

Each mode is single-shot: the script runs until the conversation it
encodes completes, then exits. The Rust test owns the assertions; the
mock owns the wire framing.

### Mock Claude Design

Mock claude follows the same pattern as mock pi but speaks Claude
Code's stream-json framing (also JSONL) on stdin/stdout.

| Mode | Used by | Wire behavior |
|------|---------|---------------|
| `steering` | mid-session steer test | Emits one assistant turn, waits for a stream-json user message on stdin, emits a second assistant turn echoing the steer payload, then `result/success` |
| `ignore-stdin` | shutdown watchdog test | Emits `result/success`, ignores SIGTERM and stdin close so the test exercises the SIGTERM → SIGKILL escalation |
| `happy-path` | container smoke | system → assistant → `result/success` |

### Nix Integration

```nix
# tests/loom/default.nix
{ pkgs, loomPackage, ... }:
let
  inherit (loomPackage) craneLib;
in
{
  # Deterministic verifiers — invokes `loom gate verify` which dispatches
  # `[check]` (one subprocess per `cargo run -p loom-walk -- …` annotation)
  # and `[test]` (one batched `cargo nextest run -E 'test(…)'` over every
  # annotated test path). `[system]` is excluded via LOOM_VERIFY_TIERS
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
      LOOM_VERIFY_TIERS=check,test loom gate verify
    '';
  };

  # Container smoke — invoked via `nix run .#test`. Excluded from
  # `flake check` because it needs podman at runtime. Annotated as
  # [system](nix run .#test) on its acceptance criterion.
  loom-smoke = pkgs.writeShellApplication {
    name = "test";
    runtimeInputs = [ loom bd pkgs.podman pkgs.jq ];
    text = builtins.readFile ./run-tests.sh;
  };
}
```

`loomTests` is exposed via `tests/default.nix` (as
`loom-tests = loomDeriv.loomTests` under `rustChecks`, joining the flake
`checks` set) and lifted to `packages.loom-tests` in
`nix/flake/tests.nix`. It runs alongside `loom-nextest` (the bare
`cargo nextest run` derivation): the gate-driven variant batches the
`[test]`-annotated targets and runs the `[check]` walks across every
spec under `specs/`, while `loom-nextest` continues to cover every
workspace test as a wide safety net. Grep-tier `[check]` annotations
across specs use paths relative to the staged-source root (which mirrors
the `loom/` workspace flattened to `$out/` plus host files like
`lib/sandbox/linux/entrypoint.sh` mirrored under their host paths), so
the verify loop runs unscoped — no `--spec` filter.
`loom-smoke` is exposed as an app on Linux only.

## Success Criteria

### Unit tests

- Newtype serde round-trip tests cover all ID types (`BeadId`,
      `SpecLabel`, `MoleculeId`, `ProfileName`, `SessionId`,
      `ToolCallId`, `RequestId`)
  [test](serde_round_trips_as_plain_string)
- State database round-trip tests cover spec, molecule, and meta
      operations
  [test](state_current_spec_round_trips)
- Pi RPC protocol tests cover every command and every event type
      in the pi v0.72 protocol table, asserting on every documented
      field (not just type discrimination) so a renamed field fails
      deserialization at test time
  [test](pi_response_success_populates_data_field)
- Claude stream-json protocol tests cover all `ClaudeMessage`
      variants including `Unknown` via `#[serde(other)]`, with
      field-level assertions on each variant
  [test](result_message_round_trips_every_documented_field)
- Template rendering tests cover every Askama template with
      representative inputs
  [test](template_renders_are_byte_stable_across_runs)

### Integration tests

Each criterion below corresponds to one of the seven load-bearing flows
in Functional #4.

- Startup probe round-trip: mock pi with full required command set
      → loom proceeds; mock pi missing `set_model` → loom fails fast
      with a version-mismatch error
  [test](pi_startup_probe_succeeds_with_required_commands)
- `wrapix spawn` argv contract: loom invokes
      `wrapix spawn --spawn-config <file> --stdio` with stdin
      attached as a pipe (not a TTY); recorded `SpawnConfig` JSON
      matches the on-disk shape
  [test](wrapix_spawn_invocation_records_correct_argv)
- Parallel run end-to-end: `loom loop --parallel 2` with two ready
      beads dispatches two mock-agent spawns concurrently, each in its
      own worktree, then merges both branches back to driver
  [test](parallel_run_two_beads_e2e)
- `GitClient` round-trip: create worktree, list, status, merge
      (clean / non-conflicting / conflict variants), remove — all
      against a temp repo via the typed Rust API
  [test](create_and_remove_worktree_round_trip)
- State DB lifecycle: `open` on fresh path creates schema;
      `rebuild` populates from `specs/*.md` plus mock `bd` output,
      resetting iteration counters; `recreate` recovers from a
      corrupted file
  [test](state_db_rebuild_populates_specs_and_molecules)
- Per-spec advisory locking: two contending acquisitions on the
      same `<label>.lock` serialize via `flock`; the second waits
      via `MockClock` advance, then errors naming the held label.
      Crashed child releases lock immediately for parent
  [test](second_acquire_times_out_with_spec_busy)
- Logging tee: renderer and on-disk `.jsonl` log subscribe to the
      same `AgentEvent` stream — capturing both yields line-for-line
      equality on the log side
  [test](run_single_event_sink_property)

### Container smoke

- `nix run .#test` spawns a real podman container, runs
      `loom loop --once` against a bead with `WRAPIX_AGENT=pi`
      `MOCK_PI_SCENARIO=happy-path`, exits 0 with the bead closed
  [system](nix run .#test)

### Style enforcement

**Configuration** — these tests check that the rules are *declared*:

- `[workspace.lints.clippy]` denies `unwrap_used`, `expect_used`,
      `panic`, `todo`, `unimplemented`; warns `allow_attributes`
  [check](grep -q 'unwrap_used = "deny"' Cargo.toml)

**Outcome** — these tests check that the *codebase complies* with
the rules:

- `cargo clippy --workspace` is covered by the `loom-clippy` flake
      check (shared cargoArtifacts cache); see [profiles.md](profiles.md)
- No `derive(From)` / `derive(Into)` on tuple-struct newtypes
  [check](cargo run -p loom-walk -- no_derive_from_on_newtypes)
- No `crates/*/src/{types,error}.rs` files at crate roots
  [check](cargo run -p loom-walk -- no_types_or_error_files)
- `GitClient` is the only module importing `gix` or invoking the
      `git` CLI
  [check](cargo run -p loom-walk -- git_client_encapsulation)
- Renderer + log writer subscribe to the same `AgentEvent` channel
  [check](cargo run -p loom-walk -- single_event_channel)
- Domain identifiers are tuple-struct newtypes
  [check](cargo run -p loom-walk -- newtype_identifiers)
- Each Askama template has a typed context struct
  [check](cargo run -p loom-walk -- template_context_structs)
- Tests use `tempfile::tempdir`, never hardcoded `/tmp/...` paths
  [check](cargo run -p loom-walk -- no_hardcoded_tmp_paths)

### Determinism

- No `std::thread::sleep` in any source file
  [check](cargo run -p loom-walk -- no_thread_sleep)
- No `tokio::time::sleep` outside `SystemClock::sleep`
  [check](cargo run -p loom-walk -- no_tokio_sleep_outside_clock)
- No `tokio::time::timeout` outside `SystemClock::timeout`
  [check](cargo run -p loom-walk -- no_tokio_timeout_outside_clock)
- No `Instant::now()` / `SystemTime::now()` outside `SystemClock`
  [check](cargo run -p loom-walk -- no_real_clock_outside_system_clock)
- No `#[ignore]` outside the container smoke runner
  [test](no_ignore_for_flake)

### Annotation gate

- Every `[check]` / `[test]` / `[system]` / `[judge]` annotation in
      `specs/*.md` resolves to a valid verifier for its tier
  [test](end_to_end_specs_dir_check_combines_both_directions)

### Property-based testing

- JSONL line parser proptest: never panics on arbitrary bytes,
      respects `MAX_LINE_BYTES`, never emits `AgentEvent` from a
      malformed line
  [test](jsonl_arbitrary_bytes_never_panic)
- Pi protocol parser proptest: round-trip identity for known
      shapes, unknown shapes map to typed errors, never panics
  [test](pi_arbitrary_bytes_never_panic)
- Claude stream-json parser proptest: round-trip identity for
      known shapes, `Unknown` variant catches unknown types, never
      panics
  [test](claude_arbitrary_bytes_never_panic)
- State DB rebuild proptest: arbitrary spec content never
      corrupts schema; corrupted DB always recovers via `recreate`
  [test](rebuild_never_corrupts_schema)
- `PROPTEST_CASES=32` for CI; overridable via env var
  [check](grep -q 'pub const CI_PROPTEST_CASES: u32 = 32' crates/loom-test-support/src/lib.rs)

### Snapshot testing

- Every Askama template has at least one `insta` snapshot under
      `crates/loom-templates/tests/snapshots/`
  [test](run_snapshot)
- `loom --help` and every subcommand `--help` have `insta`
      snapshots
  [test](loom_help_snapshot)
- Loop renderer uses substring + structural assertions, not
      `insta` (ensures terminal-output flexibility)
  [check](cargo run -p loom-walk -- renderer_no_insta_dependency)

### Cross-platform

- `flake.nix` declares `tests` under `checks.<system>` for
      `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin`
  [check](grep -q 'loomTests = craneLib.mkCargoDerivation' tests/loom/default.nix)
- `nix run .#test` is exposed only on Linux systems
  [check](grep -q 'isLinux' tests/loom/default.nix)
- `nix run .#test` on Darwin exits 0 with a clear "not
      available on Darwin" message
  [check](grep -q 'container smoke not available on Darwin' tests/loom/default.nix)

### CI integration

- `tests` derivation is exposed for `nix build` and invokes
      `loom gate verify` (which batches `cargo nextest run` for the
      `[test]` tier and dispatches per-annotation subprocesses for
      `[check]`); it joins the flake `checks` set under `rustChecks`
  [check](grep -q 'loom-tests = loomDeriv.loomTests' tests/default.nix)
- `nix run .#test` exists as a `writeShellApplication` exposed
      on Linux platforms
  [check](grep -q 'name = "test"' tests/loom/default.nix)
- `nix run .#fuzz-loom` exists for on-demand `cargo fuzz` runs
      (not gated by `nix flake check`)
  [check](grep -q 'name = "fuzz-loom"' nix/flake/apps.nix)
- Container smoke enforces a <30s wall-time budget
  [check](grep -qE 'ELAPSED.*-gt 30' tests/run-tests.sh)
- pi-mono version pinned in `nix/pi-mono/package.json` (loom owns the
      build via `buildNpmPackage`); Claude Code tracked via nixpkgs
      (no local pin)
  [check](grep -q '@mariozechner/pi-coding-agent' nix/pi-mono/package.json)

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
   - **Container smoke** — one happy-path scenario that spawns a real
     podman container via `wrapix spawn`, runs a mock agent *inside*
     the container, drives `loom loop --once` against it, and asserts
     the bead closes. Validates host↔container plumbing
     (entrypoint.sh, bind mounts, `WRAPIX_AGENT` branching, container
     teardown) — *not* protocol depth, which the integration level
     already covers. Annotated `[system](nix run .#test)`; run
     via `loom gate system`. Linux-only (no podman in Darwin CI).

2. **Mock agent processes** — process-level fixtures driven over real pipes
   from cargo integration tests, plus the in-container smoke:
   - **Mock pi** (`tests/mock-pi/pi.sh`) — narrowly scoped scenario
     modes that exercise the *pipe-level* paths the parser unit tests
     can't reach (probe round-trip, prompt ack, mid-session steer,
     compaction re-pin via steer, `set_model` from phase config, plus
     `happy-path` for the container smoke).
   - **Mock claude** (`tests/mock-claude/claude.sh`) — modes for
     mid-session steering via stream-json user message, the shutdown
     watchdog SIGTERM→SIGKILL escalation, plus `happy-path` for the
     container smoke.
   - **Out of scope for mocks**: tool-call simulation, malformed-JSONL
     injection, hang/timeout simulation, multi-turn — the parser unit
     tests cover these with inline string literals, where regressions
     are easier to read in PR diffs and fixtures don't bit-rot when
     pi/claude release new event shapes.

3. **Unit test coverage by crate** — every crate has inline
   `#[cfg(test)] mod tests` blocks plus integration tests under
   `tests/*.rs`. The lists below are the contract surfaces, not an
   exhaustive enumeration; specific edge cases live in the test code.

   #### loom-driver
   - Newtype construction and serde round-trips (`BeadId`, `SpecLabel`,
     `MoleculeId`, `ProfileName`)
   - `StateDb` schema creation on first open
   - `StateDb` query methods return typed rows (`SpecRow`, `MoleculeRow`)
   - `StateDb::rebuild` populates from spec files and mock `bd` output
   - Companion-section parser: spec with a `## Companions` section
     containing two backtick-delimited paths yields two `companions` rows;
     spec without the section yields zero
   - Parser ignores: text outside backticks on a bullet line, blank
     bullets, multi-path bullets (skipped with warn, not error)
   - Parser is case-sensitive on the heading: `## companions` (lowercase)
     and `## Companion paths` are not recognized
   - `current_spec` / `set_current_spec` round-trip
   - `increment_iteration` returns updated count, starts at 0
   - `bd` CLI output parsing (JSON → typed structs)
   - `bd` CLI error mapping (exit codes → error variants)
   - `bd` CLI wrapper passes every argument via `Command::arg()` — never
     shell interpolation. Tests inject values containing shell
     metacharacters (`; rm -rf /`, `` `id` ``, `$(whoami)`) and assert
     they reach `bd` literally as one argv element each, never expanded
   - Config file loading (TOML parsing into `LoomConfig`), defaults when
     file is absent or fields are missing
   - `SpawnConfig` JSON serialization round-trips with stable field ordering
     and key names (the contract with `wrapix spawn --spawn-config`).
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
     `data`, `success: false` extracts `error` message
   - `message_update` nested delta dispatch: `text_delta` →
     `AgentEvent::MessageDelta`, `error` → `AgentEvent::Error`,
     `thinking_delta` / `done` / toolcall deltas → skipped (empty events)
   - Pi `tool_execution_start` field mapping: `toolCallId` → `ToolCallId`,
     `toolName` → `tool`, `args` → `params`
   - Pi `tool_execution_end` field mapping: `result` → `output`,
     `isError` → `is_error`
   - Claude stream-json event deserialization (`#[serde(tag = "type")]` →
     `ClaudeMessage`)
   - Claude `#[serde(other)]` catches unknown event types without error
   - Per-phase backend resolution (`[phase.todo].agent.backend` overrides
     `[phase.default].agent.backend`, `--agent` flag overrides all phases)
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
   - Event normalization (both backends produce identical `AgentEvent`
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
   - Spec resolution logic (`--spec` flag, `current_spec` DB fallback, missing
     spec error)
   - Profile selection from bead labels (parse, fallback to base, flag
     override)
   - Retry logic (failure count tracking, `loom:clarify` label after max
     retries)
   - Push gate logic (clean completion, fix-up beads, iteration cap)
   - Four-tier detection (git diff, molecule-based, README discovery, new)
   - No per-bead `bd dolt push/pull` is invoked: assert `BdClient` exposes
     no `dolt_push`/`dolt_pull` methods and the workflow paths do not
     spawn `bd dolt …` subprocess calls (containers reach the authoritative
     state via the bind-mounted Dolt socket)
   - Parallel batch dispatch: given 3 ready beads and `--parallel 3`,
     the dispatcher creates 3 bead clones under `.loom/beads/`,
     spawns 3 `wrapix spawn` futures concurrently, and reports all
     results before integration
   - Parallel batch with N=1 (the default): one bead clone is created
     per bead, same shape as N>1 (no special-case sequential path)
   - Integration ordering: branches rebase + fast-forward into the
     integration branch sequentially, not in parallel (avoids index
     lock races)
   - On worker failure, the bead clone persists (per-bead-close
     lifecycle) and the bead is queued for retry per the retry policy
   - On merge conflict, the bead clone is preserved and the bead is
     routed to `Blocked` per the verdict gate (does not silently
     overwrite or auto-resolve)

   #### Concurrency & locking (loom-driver)
   - `flock` wrapper acquires/releases an exclusive lock on a file path;
     blocking variant returns when the lock is free, try-variant returns a
     typed error if held
   - Per-spec lock path resolution: `.loom/locks/<label>.lock` is
     created on first acquire, parent dirs created on demand
   - Workspace lock path: `.loom/locks/workspace.lock`
   - Lock-class dispatch: `LockClass::None`, `LockClass::Spec(label)`,
     `LockClass::Workspace` are derived from the parsed CLI command before
     any side effects
   - Two threads contending on the same per-spec lock: first wins, second
     waits up to 5s then errors with a clear message naming the held label
   - Crash test: spawn a child, have it acquire the lock, kill it; parent
     re-acquires immediately (kernel released the flock)

   #### Auxiliary commands (loom-workflow)
   - `loom init` writes a default `loom.toml` and creates `state.db` with
     the expected schema (specs, molecules, companions, meta tables)
   - `loom init` is idempotent: running twice does not clobber existing
     `current_spec` or molecule rows
   - `loom init --rebuild` drops and repopulates: spec rows from `specs/*.md`,
     molecule rows from mock `bd list --label=loom:active`, iteration
     counters reset to 0
   - `loom status` prints `current_spec`, active molecule id, iteration count
     in a stable format (parseable line-by-line)
   - `loom status` with no `current_spec` set exits 0 with a clear message,
     not an error
   - `loom use <label>` writes `current_spec` to the meta table; subsequent
     `loom status` reflects the change
   - `loom use <unknown>` errors when the spec row is missing
   - `loom logs` (no flags) prints the path of the most recent file
     under `.loom/logs/` and tails it
   - `loom logs --bead <id>` finds the most recent log for that bead;
     exits non-zero with a stderr message naming the bead id when no
     log exists
   - `loom spec --deps` parses the active spec's `[check]` / `[test]`
     / `[system]` / `[judge]` annotations, opens each referenced
     verifier source, and prints the deduplicated set of nixpkgs needed
   - CLI surface: `loom --help` lists every v1 command (`plan`,
     `todo`, `loop`, `gate`, `msg`, `spec`, `init`, `status`, `use`,
     `logs`, `note`)

   #### Loop UX renderer (loom-workflow)
   - Default mode: header line per bead, one line per tool call, no
     streamed assistant text deltas
   - `--verbose` / `-v` streams assistant text as it arrives
   - Tool call rendering: tool name + truncated single-line summary; lines
     longer than terminal width are clipped, never wrapped
   - Status colors: green for `✓ done`, red for `✗ failed`, yellow for
     retry; disabled when stdout is not a TTY (`NO_COLOR` honored)
   - Parallel mode: tool-call lines are bead-id-prefixed so interleaved
     output stays attributable

   #### Loop logger (loom-workflow)
   - Every bead spawn produces a file at
     `.loom/logs/<spec-label>/<bead-id>-<timestamp>.jsonl` containing
     the raw event stream as one JSON object per line
   - File is written for every spawn, regardless of terminal verbosity.
     The renderer and the on-disk logger both subscribe to the same
     `AgentEvent` channel; tests verify by routing the channel into
     two collectors and asserting line-for-line equality between the
     collected event sequences
   - With `--parallel N`, two concurrent spawns produce two distinct files;
     no shared writer, no interleaving
   - Log file path is logged at `info!` when the spawn starts so users can
     `tail -f` it
   - Retention sweep on `loom loop` startup: with `retention_days = 14`,
     files mtime'd 15+ days ago are deleted, files mtime'd today are kept
   - `retention_days = 0` is a no-op (no files deleted, no walk)
   - Best-effort: a single un-deletable file (read-only, locked) does not
     abort the sweep — remaining deletable files are still removed

   #### GitClient (loom-driver)
   - `GitClient::create_worktree(label, bead_id)` creates a worktree under
     `.wrapix/worktree/<label>/<bead-id>/` on a fresh branch
     `loom/<label>/<bead-id>` from HEAD
   - `GitClient::list_worktrees` returns worktrees registered with the
     repo (parity with `git worktree list --porcelain`)
   - `GitClient::remove_worktree(path)` removes the worktree directory
     and its registration; idempotent if already removed
   - `GitClient::merge(branch)` merges a bead branch into the current
     branch; returns a typed `MergeResult` distinguishing fast-forward,
     non-conflicting merge commit, and conflict
   - `GitClient::status` reports working-tree changes against HEAD
   - Hybrid implementation: callers see only the typed Rust API.
     Whichever path is used internally (gix vs `git` CLI) is
     encapsulated — no `gix::` or `tokio::process::Command::new("git")`
     references appear outside the `GitClient` module. Verified by a
     `[check]`-tier walk in `loom-walk` (`git_client_encapsulation`).

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

4. **Integration test coverage** — load-bearing flows that exercise
   cross-crate behavior or pipe-level orchestration. Protocol-level
   shape coverage (event mapping, malformed JSONL, control_request
   responses) belongs in parser unit tests; this list is the
   integration-tier contract.
   - **Startup probe round-trip** — mock pi replies to `get_commands`
     with the full required set; loom proceeds. Mock pi replies with a
     missing required command; loom fails fast with a version-mismatch
     error.
   - **`wrapix spawn` argv contract** — loom writes a `SpawnConfig`
     JSON, invokes a `wrapix-spawn` shim that records the argv +
     stdin properties (TTY vs pipe), then exec's a mock agent. Asserts
     the JSON shape, the `--spawn-config <file> --stdio` argv, and that
     stdin is a pipe (not a TTY).
   - **Parallel run end-to-end** — `loom loop --parallel 2` with two
     ready beads dispatches two mock-agent spawns concurrently
     (overlapping spawn timestamps captured by the mock), each in its
     own bead clone under `.loom/beads/<id>/`, then rebases +
     fast-forwards both branches into the integration branch
     sequentially.
   - **`GitClient` round-trip** — create bead clone, list, status,
     rebase + ff (clean / non-conflicting / conflict variants),
     remove — all against a temp repo via the typed Rust API.
   - **State DB lifecycle** — `StateDb::open` on a fresh path creates
     schema; `rebuild` populates from `specs/*.md` plus mock `bd`
     output; `recreate` recovers from a corrupted file.
   - **Per-spec advisory locking** — two contending acquisitions on
     the same `<label>.lock` serialize via `flock`; the second waits
     up to 5s (driven by `MockClock`), then errors with a clear
     message naming the held label. Crash test: child acquires + is
     killed; parent re-acquires immediately.
   - **Logging tee** — renderer and on-disk `.jsonl` log subscribe to
     the same `AgentEvent` stream; assert line-for-line equality on
     the log side.

5. **Container smoke coverage** — one happy-path scenario validates
   host↔container plumbing that the integration tier cannot reach: a
   temp `.beads/` is seeded with one ready bead labelled
   `profile:base`; a test image bundles `mock-pi` at a known path
   inside the container; loom invokes `wrapix spawn` with
   `WRAPIX_AGENT=pi` and `MOCK_PI_SCENARIO=happy-path`; the smoke
   asserts the container exits clean and the bead closes.
   Workflow-level coverage (plan/todo/run/gate/msg, profile selection,
   agent switching, runtime composition) lives in inline
   `#[cfg(test)] mod tests` blocks under `loom-workflow/src/` — those
   are exercised via `cargo nextest run`, not the smoke.

6. **Rust style enforcement** — two complementary mechanisms:
   - **Clippy lints** in `[workspace.lints.clippy]`: `unwrap_used = "deny"`,
     `expect_used = "deny"`, `panic = "deny"`, `todo = "deny"`,
     `unimplemented = "deny"`, `allow_attributes = "warn"`. Tests opt out
     via per-file `#![allow(clippy::unwrap_used, ...)]` at the top of
     `crates/*/tests/*.rs` and inside `#[cfg(test)] mod tests` blocks.
   - **Source-walking checks** in `loom-walk` for rules clippy can't
     express. Each walk is a `[check]`-tier verifier. Uses `syn` for
     AST patterns (no `derive(From)` / `derive(Into)` on tuple
     structs, `GitClient` encapsulation, single `AgentEvent` channel
     for renderer + log writer, newtype identifier shape, typed
     Askama context structs) and `walkdir` for filesystem-shape rules
     (no `crates/*/src/{types,error}.rs` at crate roots).

7. **Annotation contract** — every acceptance criterion in any spec
   under `specs/` carries a `[check]`, `[test]`, `[system]`, or
   `[judge]` annotation that must resolve to an existing verifier.
   The full rules (syntax, cardinality, classification, cross-spec
   sharing) live in [`docs/spec-conventions.md`](../docs/spec-conventions.md);
   the integrity gate that enforces them lives in
   [gate.md](gate.md).

8. **Property-based testing** — `proptest` for invariants on four
   targets: JSONL line parser, Pi protocol parser, Claude protocol
   parser, state DB rebuild. Properties target invariants ("never
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

### Non-Functional

1. **Deterministic** — no real LLM API calls; no real wall-clock waits.
   Mock agents return canned responses. Time-dependent components take
   an injectable `Clock` trait; tests use a `MockClock` with controllable
   advance (see *Architecture / Determinism Through Clock Injection*).
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
   design (no real sleeps, subprocess tests need justification,
   proptest case count bounded) but the gate doesn't fail when a
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
5. **CI-friendly** — `nix flake check` runs `loom gate verify`
   (deterministic verifiers: `[check]` + `[test]` + `[system]`) via a
   single Nix derivation. The container smoke is exposed as a
   separate `nix run .#test` app because it needs podman at
   runtime; its acceptance criterion is annotated
   `[system](nix run .#test)`. Both entry points are also wired
   into the prek pre-push hook; composition, stage budgets, and lock
   semantics live in [pre-commit.md](pre-commit.md).
6. **Real bd** — the container smoke runs against live `bd` (not a
   mock). The integration tier may mock `bd` where the test concern
   is orthogonal to the issue tracker, but the smoke validates that
   loom and `bd` interact correctly under realistic conditions.
7. **Cross-platform** — unit and integration tests pass on Linux *and*
   Darwin (`x86_64`/`aarch64` for both). The container smoke is
   Linux-only (podman dependency); on Darwin the `test` app exits
   0 with a clear "container smoke not available on Darwin" message.
   Tests use `tempfile::tempdir` exclusively, never hardcoded
   `/tmp/...` paths — Nix's Darwin build sandbox doesn't grant access
   to the host's `/tmp`, so any test that hardcodes one fails to even
   start under `nix flake check`. Darwin smoke support is a follow-up.
8. **Subprocess-spawning tests are exceptional** — each subprocess test
   (mock-pi, mock-claude, real `git`) costs 50-200ms; ten of them blow
   the 5s soft target alone. A test that spawns a subprocess must
   include a short comment or doc string explaining why an in-process
   equivalent (via `LineParse` + `tokio::io::duplex`) isn't feasible.
9. **Upstream protocol versioning** — pi-mono and Claude Code versions
   are pinned in `nix/flake/overlays.nix`. Bumps are deliberate PRs
   accompanied by a protocol-bump checklist (re-run parser tests, scan
   upstream changelog for new event types, add `Unknown` coverage if
   any new types lack typed variants, update mock scripts if new types
   reach pipe-level paths). No live wire tests against real binaries.
   Detection coverage: silent breaks in *exercised* fields surface as
   `serde_json` errors in parser tests when the pinned version is
   bumped. Fields not exercised by any test could still drift silently
   — parser tests must therefore touch every field of every documented
   message type for the pinned version, not just every type.
10. **No `#[ignore]` for flake mitigation** — a test marked `#[ignore]`
    because "it flakes sometimes" is forbidden. Either fix the root
    cause or delete the test. `#[ignore]` is reserved for tests that
    require explicit opt-in (e.g., the container smoke needing podman).
    A CI flake opens a `loom-flake` P1 bead naming the failing test;
    the test is fixed before any further work on the affected crate.

## Out of Scope

- **Real-binary tests at any tier** — no test invokes real pi-mono,
  real Claude Code, or any LLM API. Mock pi and mock claude scripts
  cover the protocol surface (parser tests use inline strings; mocks
  cover pipe-level paths; smoke runs mock pi inside the container).
  Validation against real binaries happens during development, outside
  CI. Pinned versions in `nix/flake/overlays.nix` plus parser
  tests with field-level coverage catch silent protocol drift on
  bumps.
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
- **Mock-script protocol breadth** — tool-call simulation, malformed-JSONL
  injection, hang/timeout simulation, multi-turn conversations. These
  belong in parser unit tests with inline string literals, not in mock
  scripts.
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
