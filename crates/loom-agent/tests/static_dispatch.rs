//! Integration tests for `agent` backend dispatch + startup probe.
//!
//! Two complementary surfaces:
//!
//! 1. **Compile-only static dispatch** — `PiBackend`, `ClaudeBackend`, and
//!    `DirectBackend` instantiate a generic `<B: AgentBackend>` helper. The
//!    verify shell function runs `cargo build --workspace --tests`; this file
//!    failing to compile is the assertion. The local `run_agent` helper mirrors the
//!    dispatch shape that lives in `loom-workflow` (and the spec's
//!    Architecture section): a generic free function over `<B: AgentBackend>`
//!    that the binary monomorphizes once per concrete backend.
//!
//! 2. **Startup probe round-trip** (spec Functional #4 first bullet) —
//!    drives the pi handshake against `mock-pi.sh` in valid `get_state`
//!    and malformed-state modes. The first must hand back an `Idle`
//!    session; the second must surface [`ProtocolError::Unsupported`] (the
//!    version-mismatch sentinel) before any conversation begins.
//!
//! The probe round-trip cannot be exercised in-process via
//! `LineParse + tokio::io::duplex`: the round-trip is the kernel-level
//! pipe + child-stdio plumbing between the pi handshake driver and the pi
//! subprocess. Replacing it with `tokio::io::duplex` would skip the very
//! lifecycle the contract pins (process spawn, JSONL framing across a real
//! pipe, EOF semantics on launcher exit). Per spec NFR #8.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::time::Duration;

use loom_agent::pi::backend::spawn_with_handshake;
use loom_agent::{ClaudeBackend, DirectBackend, PiBackend};
use loom_driver::agent::{AgentBackend, ProtocolError, RePinContent, SessionOutcome, SpawnConfig};
use loom_driver::clock::SystemClock;
use loom_events::ParsedAgentEvent;
use tokio::process::Command;

async fn run_agent<B: AgentBackend>(config: &SpawnConfig) -> Result<SessionOutcome, ProtocolError> {
    let _session = B::spawn(config).await?;
    Err(ProtocolError::Unsupported)
}

#[test]
fn all_backends_dispatch_through_run_agent() {
    // The bound `B: AgentBackend` is the dispatch contract — instantiating
    // it at every concrete type is what monomorphizes `run_agent` and proves
    // the trait surface accepts each backend.
    fn assert_backend<B: AgentBackend>() {}
    assert_backend::<PiBackend>();
    assert_backend::<ClaudeBackend>();
    assert_backend::<DirectBackend>();

    // Reference the generic function at each backend so the test binary
    // pulls in `run_agent::<PiBackend>`, `run_agent::<ClaudeBackend>`, and
    // `run_agent::<DirectBackend>` monomorphizations rather than only the
    // trait-bound check above.
    let _pi_fut = async {
        let cfg = sample_config();
        run_agent::<PiBackend>(&cfg).await
    };
    let _claude_fut = async {
        let cfg = sample_config();
        run_agent::<ClaudeBackend>(&cfg).await
    };
    let _direct_fut = async {
        let cfg = sample_config();
        run_agent::<DirectBackend>(&cfg).await
    };
}

fn sample_config() -> SpawnConfig {
    SpawnConfig {
        image_ref: String::new(),
        image_source: PathBuf::new(),
        image_source_kind: None,
        wrix_launcher: None,
        profile_config: None,
        workspace: PathBuf::new(),
        env: Vec::new(),
        mounts: Vec::new(),
        initial_prompt: String::new(),
        agent_args: Vec::new(),
        repin: RePinContent {
            orientation: String::new(),
            pinned_context: String::new(),
            partial_bodies: Vec::new(),
        },
        skills: None,
        scratch_dir: PathBuf::new(),
        model_id: None,
        model: None,
        thinking_level: None,
        output_limits: None,
        shutdown_grace: None,
        denied_tools: Vec::new(),
        handshake_timeout: None,
        stall_warn_interval: None,
        launcher_env: Vec::new(),
    }
}

//---------------------------------------------------------------------------
// Startup probe round-trip
//---------------------------------------------------------------------------

/// Locate `tests/mock-pi/pi.sh` by walking ancestors of the crate
/// manifest dir — handles both the dev tree (`repo/crates/loom-agent/`
/// → `repo/tests/...`) and the crane build sandbox (the loom workspace
/// is the staged root, `tests/...` is staged next to it).
fn mock_pi_path() -> PathBuf {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for ancestor in manifest_dir.ancestors() {
        let candidate = ancestor.join("tests/mock-pi/pi.sh");
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!(
        "could not locate tests/mock-pi/pi.sh above {} — neither \
         dev-tree nor nix-sandbox layout matched.",
        manifest_dir.display(),
    );
}

/// Build a `Command` that exec's `bash mock-pi.sh <mode>`. Used as a
/// drop-in for the production launcher (`wrix spawn --spawn-config
/// <file> --stdio`); the argv contract for that path is exercised by
/// `loom/tests/spawn_dispatch.rs`. Here the test cares only about the
/// handshake round-trip, so we bypass the wrix shim.
fn mock_command(mode: &str) -> Command {
    let mut cmd = Command::new("bash");
    cmd.arg(mock_pi_path()).arg(mode);
    cmd
}

/// `mock-pi happy-path` returns a valid `get_state` payload; the backend
/// handshake completes and yields an `Idle` session. Driving a single
/// prompt through to `SessionComplete` verifies the session is wired and
/// the launcher's stdin/stdout pipes round-trip JSONL frames.
#[tokio::test]
async fn pi_startup_probe_succeeds_with_valid_get_state() {
    let session = spawn_with_handshake(
        mock_command("happy-path"),
        None,
        None,
        Duration::from_secs(5),
        &SystemClock::new(),
    )
    .await
    .expect("get_state handshake should succeed");

    // Run one prompt round-trip to confirm the session is alive past the
    // handshake. `happy-path` sends one message_delta then `agent_end`.
    let mut session = session.prompt("ping").await.expect("prompt ok");
    loop {
        match session.next_event().await.expect("event ok") {
            Some(ParsedAgentEvent::SessionComplete { .. }) => return,
            Some(_) => continue,
            None => panic!("unexpected EOF before SessionComplete"),
        }
    }
}

/// `mock-pi probe-bad-state` returns a malformed `get_state` payload.
/// The handshake must short circuit with `ProtocolError::Unsupported`
/// *before* any prompt is sent — the version-mismatch contract that keeps
/// Loom from running against an incompatible pi build.
#[tokio::test]
async fn pi_startup_probe_fails_with_bad_get_state_shape() {
    let result = spawn_with_handshake(
        mock_command("probe-bad-state"),
        None,
        None,
        Duration::from_secs(5),
        &SystemClock::new(),
    )
    .await;
    match result {
        Err(ProtocolError::Unsupported) => {}
        Err(other) => panic!("expected ProtocolError::Unsupported, got {other:?}"),
        Ok(_) => panic!("probe should have failed on malformed get_state data"),
    }
}
