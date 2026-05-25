//! CLI surface tests for the global `--agent` flag.
//!
//! - `test_backend_selection_flag` — `--agent pi` parses; `--help` lists it
//! - `test_backend_invalid_name` — `--agent unknown` fails with a clear error
//!
//! Default-resolution and per-phase config behaviour live in
//! `loom-driver/src/config/mod.rs::tests` so they can assert on the typed
//! `AgentSelection` directly. This file pins the binary's clap surface.

use std::process::Command;

#[test]
fn loom_help_lists_agent_global_flag() {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--help")
        .output()
        .expect("spawn loom");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom --help must exit zero. stdout={stdout} stderr={stderr}",
    );
    assert!(
        stdout.contains("--agent"),
        "loom --help must list --agent. stdout={stdout}",
    );
    assert!(
        stdout.contains("claude") && stdout.contains("pi"),
        "loom --help must list backend choices. stdout={stdout}",
    );
}

#[test]
fn loom_run_help_includes_agent_flag() {
    // --agent is a global flag, so it must appear under every subcommand's
    // --help output. Pinning `run` covers the subcommand surface most users
    // interact with.
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("run")
        .arg("--help")
        .output()
        .expect("spawn loom");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--agent"),
        "loom run --help must list --agent. stdout={stdout}",
    );
}

#[test]
fn loom_rejects_unknown_agent_value() {
    // Clap's value-enum validation rejects the value before any workflow
    // code runs. The error message names the offending input and lists the
    // valid choices so users can self-correct. We pass `status` (a
    // subcommand that does no IO besides the state DB) so clap reaches
    // value-enum validation instead of short-circuiting on `--help`.
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--agent")
        .arg("unknown")
        .arg("status")
        .output()
        .expect("spawn loom");
    assert!(
        !output.status.success(),
        "loom --agent unknown must fail. status={:?}",
        output.status,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown") || stderr.contains("invalid"),
        "stderr should name the offending value. stderr={stderr}",
    );
    assert!(
        stderr.contains("claude") && stderr.contains("pi"),
        "stderr should list valid backends. stderr={stderr}",
    );
}

#[test]
fn loom_accepts_agent_pi() {
    // Smoke: `loom --agent pi run --help` reaches subcommand help, proving
    // the value-enum accepts `pi`.
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--agent")
        .arg("pi")
        .arg("run")
        .arg("--help")
        .output()
        .expect("spawn loom");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom --agent pi run --help must succeed. stdout={stdout} stderr={stderr}",
    );
}

#[test]
fn loom_accepts_agent_claude() {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .arg("--agent")
        .arg("claude")
        .arg("run")
        .arg("--help")
        .output()
        .expect("spawn loom");
    assert!(
        output.status.success(),
        "loom --agent claude run --help must succeed",
    );
}
