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
          db.rs               # CacheDb impl + inline #[cfg(test)] mod tests
          rebuild.rs          # rebuild logic + inline tests
          companions.rs       # `## Companions` parser + inline tests
        bd/
          client.rs           # bd CLI wrapper + inline tests
          label.rs            # Label newtype + inline tests
        agent/
          repin.rs            # RePinContent + inline tests (doc-tested)
          ...
      tests/
        cache_db.rs           # Integration: CacheDb across rebuild + queries
        lock_manager.rs       # Integration: per-spec advisory locking
        git_client.rs         # Integration: GitClient against a temp repo
        logging.rs            # Integration: shared renderer + log channel
        properties.rs         # proptest invariants for cache DB rebuild
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
        static_dispatch.rs    # Compile-time check: all concrete backends impl AgentBackend
        properties.rs         # proptest invariants for subprocess protocol parsers
    loom-workflow/
      src/
        loop/
          mod.rs              # loop orchestration + inline tests for unit-level helpers
        gate/
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
        loop_smoke.rs         # Integration: CLI subcommand surface
        agent_flag.rs         # Integration: --agent flag parsing/validation
        spawn_dispatch.rs     # Integration: shim-based wrix spawn argv
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
    default.nix               # Nix derivation: explicit `loom gate` tiers
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

Mock pi is a shell script that frames pi-mono's RPC protocol as JSONL
on stdin/stdout. Its job is to exercise *process-level* paths the
parser unit tests cannot reach — round-tripping through real pipes,
stdin write-back from `ParsedLine::response`, and child reaping. Each
mode is shaped to exactly one Rust test that drives it; the script is
not a general-purpose pi emulator.

Modes (selected via `argv[1]`):

| Mode | Used by | Wire behavior |
|------|---------|---------------|
| `probe-ok` | startup probe round-trip test | Replies to `get_state` with a valid state object |
| `probe-bad-state` | startup probe failure test | Replies to `get_state` with malformed state data |
| `echo-prompt` | wire-shape assertion test | Probe ok, then echoes the prompt payload as a `message_delta` |
| `steering` | mid-session steer test | Probe ok, prompt → first turn, then echoes the steer payload on the next turn |
| `compaction` | re-pin-via-steer test | Probe ok, emits `compaction_start`, expects the re-pin steer, echoes it back, emits `compaction_end` |
| `interactive-compaction-canary` | interactive re-pin behavioral canary | After forced compaction, answers the `do a polish` probe correctly only when the delivered re-pin contains the full interview-mode definition and a test-only nonce. Pi plan uses the native-TUI extension path; Pi inbox non-TTY coverage uses the controlled bridge. |
| `set-model` | per-phase model override test | Probe ok, expects `set_model { provider, modelId }`, echoes the pair into a later `message_delta` |
| `set-model-reject` | model override failure test | Probe ok, rejects `set_model` so the backend hard-fails the handshake |
| `happy-path` | container smoke | Probe ok, prompt → `message_delta` → `agent_end` |

Each mode is single-shot: the script runs until the conversation it
encodes completes, then exits. The Rust test owns the assertions; the
mock owns the wire framing.

### Inbox Bridge Fixture

The Pi inbox bridge follow-up fixture is separate from the mock-pi mode table.
Its whole contract is one startup probe, one initial prompt that completes
without a terminal marker, one human reply encoded as a fresh `prompt`, and
then a terminal marker. It exists only because the bridge keeps the same
process pipes alive across a post-completion human reply; parser unit tests do
not exercise that process lifecycle. A conformance test drives this exact JSONL
exchange so the fixture does not grow into another Pi protocol emulator.

### Mock Claude Design

Mock claude follows the same pattern as mock pi but speaks Claude
Code's stream-json framing (also JSONL) on stdin/stdout.

| Mode | Used by | Wire behavior |
|------|---------|---------------|
| `steering` | mid-session steer test | Emits one assistant turn, waits for a stream-json user message on stdin, emits a second assistant turn echoing the steer payload, then `result/success` |
| `ignore-stdin` | shutdown watchdog test | Emits `result/success`, ignores SIGTERM and stdin close so the test exercises the SIGTERM → SIGKILL escalation |
| `interactive-compaction-canary` / `interactive-bridge-canary` | plan/inbox re-pin behavioral canary | Simulates the launched interactive Claude process, native Pi extension hand-off, or controlled Pi inbox bridge: verifies the compact hook/config, extension, or re-pin steer is loaded through the production launch path, triggers compaction, then answers a post-compaction probe correctly only when the delivered context contains the full interview-mode definition and a test-only nonce |
| `happy-path` | container smoke | system → assistant → `result/success` |

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

- Startup probe round-trip: mock pi with valid `get_state` data
      → loom proceeds
  [test](pi_startup_probe_succeeds_with_valid_get_state)
- Startup probe malformed-state guard: mock pi with malformed `get_state`
      data → loom fails fast with a version-mismatch error
  [test](pi_startup_probe_fails_with_bad_get_state_shape)
- `wrix spawn` argv contract: loom invokes
      `wrix --profile-config <file> spawn --spawn-config <file> --stdio`
      with stdin attached as a pipe (not a TTY); recorded `SpawnConfig` JSON
      matches the on-disk shape
  [test](wrix_spawn_invocation_records_correct_argv)
- `WRIX_AGENT` launcher-env contract: command construction is exercised
      with the parent shell lacking `WRIX_AGENT` and with a conflicting
      parent value; the recorded `wrix spawn` child env contains the
      backend-derived runtime (`pi`, `claude`, or `direct`)
  [test](wrix_spawn_child_env_sets_backend_derived_wrix_agent)
- Parallel run end-to-end: `loom loop --parallel 2` with two ready
      beads dispatches two mock-agent spawns concurrently, each in its
      own bead clone, then integrates both branches back to the driver
  [test](parallel_run_two_beads_e2e)
- `GitClient` round-trip: create bead clone, status, rebase + fast-forward
      integration (clean / non-conflicting / conflict variants), remove — all
      against a temp repo via the typed Rust API
  [test](create_and_remove_worktree_round_trip)
- Cache DB lifecycle: `open` on fresh path creates schema; `rebuild`
      populates from the spec index, spec files, mock bd spec/work
      epics, and companions; `recreate` recovers from a corrupted file
      without treating cache loss as clean todo state
  [test](cache_db_rebuild_populates_specs_epics_and_companions)
- Todo multi-spec regression fixture: one commit modifies a spec already
      represented in the active work epic, an existing inactive/stale spec,
      and a brand-new spec added to `docs/README.md`; `loom todo`
      discovers all three from durable cursors regardless of `loom:active`
  [test](todo_preflight_discovers_active_inactive_and_new_specs)
- Todo validation rejects an agent `LOOM_TODO` payload that omits any
      changed spec; no spec cursor advances and no work epic becomes
      `loom:active`
  [test](todo_success_missing_changed_spec_fails_without_advancing)
- Missing criterion evidence rows in `.loom/cache.db` render as
      `EvidenceState::Missing` and never as no criteria/no work
  [test](todo_missing_criterion_cache_rows_are_missing_evidence)
- Phase/work-root advisory locking: two contending acquisitions on the
      same plan/todo/bead/epic root serialize via `flock`; the second
      waits via `MockClock` advance, then errors naming the held root.
      Crashed child releases lock immediately for parent
  [test](second_acquire_times_out_with_work_root_busy)
- Logging tee: renderer and on-disk `.jsonl` log subscribe to the
      same `AgentEvent` stream — capturing both yields line-for-line
      equality on the log side
  [test](run_single_event_sink_property)
- Mock-agent compaction canaries exercise post-compaction behavior, not just
      payload assembly: the fixture fails when the delivered context contains
      only a compacted summary and passes only when the post-compaction
      `do a polish` probe can rely on both the full report-only/no-edit mode
      definition and a test-only nonce
  [test](mock_agent_compaction_canary_requires_rehydrated_mode_definition)
- The dedicated Pi inbox-bridge follow-up fixture stays outside the mock-pi
      mode table and covers only the probe → prompt → one human follow-up
      prompt → terminal-marker exchange
  [test](inbox_bridge_pi_followup_fixture_accepts_one_prompt_reply)

### Container smoke

- `nix run .#smoke` spawns a real podman container, unsets the
      parent-shell `WRIX_AGENT`, runs `loom loop <bead-id>` against a
      Pi-backed bead with child env `WRIX_AGENT=pi` and
      `MOCK_PI_SCENARIO=happy-path`, exits 0 with the bead closed
  [system](nix run .#smoke)

### Style enforcement

**Configuration** — these tests check that the rules are *declared*:

- `[workspace.lints.clippy]` denies `unwrap_used`, `expect_used`,
      `panic`, `todo`, `unimplemented`; warns `allow_attributes`
  [check](grep -q 'unwrap_used = "deny"' Cargo.toml)

**Outcome** — these tests check that the *codebase complies* with
the rules:

- `cargo clippy --workspace` and full workspace nextest are covered by
      the `nix run .#test` full-suite app, while `nix flake check`
      omits them from the fast checks set
  [check](cargo run -p loom-walk -- workspace_compile_checks_are_full_test_app_only)
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
- Cache DB rebuild proptest: arbitrary spec/index content never
      corrupts schema; corrupted cache recovers via `recreate` or reports
      durable-source inconsistency
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

- `loom-tests` remains buildable as a package for
      `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin`
  [check](grep -q 'packages.loom-tests' nix/flake/tests.nix)
- `nix run .#smoke` selects the real podman smoke implementation only
      on Linux systems
  [check](grep -q 'isLinux' tests/loom/default.nix)
- `nix run .#smoke` on Darwin exits 0 with a clear "not
      available on Darwin" message
  [check](grep -q 'container smoke not available on Darwin' tests/loom/default.nix)

### CI integration

- `tests` derivation is exposed for `nix build` and invokes explicit
      deterministic gate tiers (batching `cargo nextest run` for
      `[test]` and dispatching per-annotation subprocesses for `[check]`);
      it is not part of the flake `checks` set
  [check](grep -q 'packages.loom-tests' nix/flake/tests.nix)
- `nix run .#test` exists as the full required suite app
  [check](grep -q 'name = "test"' nix/flake/apps.nix)
- `nix run .#smoke` exists as a `writeShellApplication` with a Linux
      implementation and Darwin stub
  [check](grep -q 'name = "smoke"' tests/loom/default.nix)
- `nix run .#fuzz-loom` exists for on-demand `cargo fuzz` runs
      (not gated by `nix flake check`)
  [check](grep -q 'name = "fuzz-loom"' nix/flake/apps.nix)
- Container smoke enforces a <30s wall-time budget
  [check](grep -qE 'ELAPSED.*-gt 30' tests/run-tests.sh)
- Pi Coding Agent and Claude Code are tracked via nixpkgs (no local
      language-package pins in this repo)
  [check](grep -q 'agentPkg = piCodingAgent' nix/flake/lib.nix)

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
     podman container via `wrix spawn`, runs a mock agent *inside*
     the container, drives `loom loop <bead-id>` against it, and asserts
     the bead closes. Validates host↔container plumbing
     (entrypoint.sh, bind mounts, `WRIX_AGENT` branching, container
     teardown) — *not* protocol depth, which the integration level
     already covers. Annotated `[system](nix run .#smoke)`; run
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
   - **Out of scope for mocks**: tool-call simulation, malformed-JSONL
     injection, hang/timeout simulation, and general multi-turn behavior.
     The narrow interactive compaction canary is the only scripted multi-turn
     exception because the bug is visible only after a post-compaction probe.
     Parser unit tests cover the broader cases with inline string literals,
     where regressions are easier to read in PR diffs and fixtures don't
     bit-rot when pi/claude release new event shapes.

3. **Unit test coverage by crate** — every crate has inline
   `#[cfg(test)] mod tests` blocks plus integration tests under
   `tests/*.rs`. The lists below are the contract surfaces, not an
   exhaustive enumeration; specific edge cases live in the test code.

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
     and key names (the contract with `wrix --profile-config <file> spawn --spawn-config`).
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
   - Backend runtime name mapping for Wrix launcher env: Pi → `pi`,
     Claude → `claude`, Direct → `direct`
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
   - `loom todo` changed-spec preflight from durable spec epic cursors,
     including active, inactive/stale, and brand-new indexed specs
   - `LOOM_TODO:` terminal parsing/validation against the preflight roster
     and work epic
   - Profile/runtime selection from bead labels plus resolved backend
     (parse, fallback to base, flag override, missing runtime failure)
   - Interactive `plan` / `inbox chat` command-construction tests cover the
     backend-specific launch matrix owned by
     [agent.md § Interactive Shell-Out](agent.md#interactive-shell-out)
   - Retry logic (failure count tracking, `loom:clarify` label after max
     retries)
   - Push gate logic (clean completion, fix-up beads, iteration cap)
   - Spec/work epic lifecycle: spec epic initialization, missing-cursor
     blocking, pending `loom:todo` reuse, and active work epic finalization
   - No per-bead `bd dolt push/pull` is invoked: assert `BdClient` exposes
     no `dolt_push`/`dolt_pull` methods and the workflow paths do not
     spawn `bd dolt …` subprocess calls (containers reach the authoritative
     state via the bind-mounted Dolt socket)
   - Parallel batch dispatch: given 3 ready beads and `--parallel 3`,
     the dispatcher creates 3 bead clones under `.loom/beads/`,
     spawns 3 `wrix spawn` futures concurrently, and reports all
     results before integration
   - Parallel batch with N=1 (the default): one bead clone is created
     per bead, same shape as N>1 (no special-case sequential path)
   - Integration ordering: branches rebase + fast-forward into the
     integration branch sequentially, not in parallel (avoids index
     lock races)
   - On worker failure, the bead clone persists (per-bead-close
     lifecycle) and the bead is queued for retry per the retry policy
   - On integration merge conflict, the bead clone is preserved and the
     verdict gate gives the agent one `integration-conflict` retry; a
     second conflict escalates to `loom:clarify` with the conflict files
     and new integration tip

   #### Concurrency & locking (loom-driver)
   - `flock` wrapper acquires/releases an exclusive lock on a file path;
     blocking variant returns when the lock is free, try-variant returns a
     typed error if held
   - Phase/work-root lock path resolution: `plan.lock`, `todo.lock`, and
     `<bead-or-epic-id>.lock` are created on first acquire, parent dirs
     created on demand
   - Workspace lock path: `$XDG_STATE_HOME/loom/locks/<workspace-basename>/workspace.lock`
   - Lock-class dispatch: `LockClass::None`, `LockClass::Plan`,
     `LockClass::Todo`, `LockClass::WorkRoot(id)`, `LockClass::Workspace`
     are derived from the parsed CLI command before any side effects
   - Two threads contending on the same phase/work-root lock: first wins,
     second waits up to 5s then errors with a clear message naming the held
     root
   - Crash test: spawn a child, have it acquire the lock, kill it; parent
     re-acquires immediately (kernel released the flock)

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
   - `GitClient::create_worktree(label, bead_id)` materializes a standalone
     bead clone under `.loom/beads/<bead-id>/` on branch
     `loom/<bead-id>` from the loom workspace
   - `GitClient::remove_worktree(path)` removes the bead clone directory;
     idempotent if already removed
   - `GitClient::merge_branch(branch)` rebases a bead branch onto the
     configured integration branch and fast-forwards it; returns a typed
     `MergeResult` distinguishing success and conflict
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
   - **Startup probe round-trip** — mock pi replies to `get_state` with
     a valid state object; loom proceeds. Mock pi replies with malformed
     state data; loom fails fast with a version-mismatch error.
   - **`wrix spawn` argv contract** — loom writes a `SpawnConfig`
     JSON, invokes a `wrix-spawn` shim that records the argv +
     stdin properties (TTY vs pipe), then exec's a mock agent. Asserts
     the JSON shape (including `image_source_kind` whenever `image_source`
     is emitted), the `--spawn-config <file> --stdio` argv, and that stdin is
     a pipe (not a TTY).
   - **`WRIX_AGENT` launcher-env contract** — the same command-construction
     shim records child environment for pi, claude, and direct backend
     selections. Parent-shell `WRIX_AGENT` is unset in one case and set
     to a conflicting value in another; the child env always matches the
     resolved backend runtime.
   - **Parallel run end-to-end** — `loom loop --parallel 2` with two
     ready beads dispatches two mock-agent spawns concurrently
     (overlapping spawn timestamps captured by the mock), each in its
     own bead clone under `.loom/beads/<id>/`, then rebases +
     fast-forwards both branches into the integration branch
     sequentially.
   - **`GitClient` round-trip** — create bead clone, list, status,
     rebase + ff (clean / non-conflicting / conflict variants),
     remove — all against a temp repo via the typed Rust API.
   - **Cache DB lifecycle** — `CacheDb::open` on a fresh path creates
     schema; `rebuild` populates from the spec index, `specs/*.md`, and
     mock bd spec/work epics; `recreate` recovers from a corrupted file.
   - **Todo changed-spec regression** — fixture repo with one commit
     modifying a spec already represented in the active work epic, an
     inactive/stale spec, and a brand-new indexed spec; verifies preflight
     includes all three, missing cache
     rows are `EvidenceState::Missing`, omitted `LOOM_TODO` rows fail,
     and valid finalization advances every cursor all-or-nothing.
   - **Phase/work-root advisory locking** — two contending acquisitions
     on the same plan/todo/bead/epic lock serialize via `flock`; the
     second waits up to 5s (driven by `MockClock`), then errors with a
     clear message naming the held root. Crash test: child acquires + is
     killed; parent re-acquires immediately.
   - **Logging tee** — renderer and on-disk `.jsonl` log subscribe to
     the same `AgentEvent` stream; assert line-for-line equality on
     the log side.

5. **Container smoke coverage** — one happy-path scenario validates
   host↔container plumbing that the integration tier cannot reach: a
   temp `.beads/` is seeded with one ready bead labelled
   `profile:base`; a test image bundles `mock-pi` at a known path
   inside the container; with parent-shell `WRIX_AGENT` unset, loom
   invokes `wrix spawn` with `WRIX_AGENT=pi` and
   `MOCK_PI_SCENARIO=happy-path`; the smoke asserts the container exits
   clean and the bead closes.
   Workflow-level coverage (plan/todo/loop/gate/inbox/tune, profile/runtime
   selection, agent switching) lives in inline
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
7. **Cross-platform** — unit and integration tests pass on Linux *and*
   Darwin (`x86_64`/`aarch64` for both). The container smoke is
   Linux-only (podman dependency); on Darwin the `smoke` app exits
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
9. **Upstream protocol versioning** — Pi Coding Agent and Claude Code
   versions are pinned by the repo's nixpkgs input. Bumps are deliberate PRs
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
  CI. The pinned nixpkgs input plus parser tests with field-level
  coverage catch silent protocol drift on bumps.
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
  injection, hang/timeout simulation, and general multi-turn conversations.
  These belong in parser unit tests with inline string literals, not in the
  general mock-pi/mock-claude scripts. The interactive compaction canary is
  the only multi-turn exception in those mode tables, because it verifies a
  post-compaction turn rather than protocol breadth. Dedicated bridge fixtures
  remain single-purpose and carry their own conformance tests instead of being
  folded into the general mock-agent mode tables.
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
