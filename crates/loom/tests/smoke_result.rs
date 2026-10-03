//! Execute the smoke result stage in Bash: pipefail and subprocess exit status
//! are the boundary under test; only loom, bd, and the clock are fixtures.

use std::process::{Command, Output};

use anyhow::Context;

const CLOSED: &str = r#"[{"id":"smoke-test.1","title":"Smoke","status":"closed"}]"#;

fn run_smoke_loop(
    loop_exit: u8,
    status_json: &str,
    bd_exit: u8,
    elapsed: u32,
) -> anyhow::Result<Output> {
    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("smoke workspace");
    std::fs::create_dir(&workspace)?;
    for (name, body) in [
        (
            "loom",
            r#"[[ "$#" -eq 6 && "$1" == --workspace && "$2" == "$PWD" && "$3" == --agent && "$4" == pi && "$5" == loop && "$6" == smoke-test.1 ]]
[[ "$LOOM_PROFILES_MANIFEST" == "$PWD/profile-images.json" && -z "${WRIX_AGENT+x}" ]]
printf 'loop\n' > loop-called
exit "$LOOP_EXIT"
"#,
        ),
        (
            "bd",
            r#"[[ "$#" -eq 3 && "$1" == show && "$2" == smoke-test.1 && "$3" == --json ]]
printf 'show\n' > bd-called
printf '%s\n' "$STATUS_JSON"
exit "$BD_EXIT"
"#,
        ),
        (
            "date",
            r#"[[ "$#" -eq 1 && "$1" == +%s ]]
printf '%s\n' "$END_TS"
"#,
        ),
    ] {
        loom_test_support::write_executable_bash_script(
            fixture.path().join(name),
            &format!("set -euo pipefail\n{body}"),
        )?;
    }
    let path = std::env::var_os("PATH").context("Bash and jq must be on the test PATH")?;
    let path = std::env::join_paths(
        std::iter::once(fixture.path().to_path_buf()).chain(std::env::split_paths(&path)),
    )?;
    let output = Command::new(loom_test_support::bash_path())
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/smoke/loop.sh"
        ))
        .arg(fixture.path().join("loom"))
        .arg(&workspace)
        .args(["smoke-test.1", "100"])
        .current_dir(&workspace)
        .env("PATH", path)
        .env("LOOP_EXIT", loop_exit.to_string())
        .env("STATUS_JSON", status_json)
        .env("BD_EXIT", bd_exit.to_string())
        .env("END_TS", (100 + elapsed).to_string())
        .env_remove("WRIX_AGENT")
        .output()?;
    assert!(workspace.join("loop-called").exists(), "{output:?}");
    assert_eq!(workspace.join("bd-called").exists(), loop_exit == 0);
    Ok(output)
}

#[test]
fn smoke_timing_is_advisory() {
    for elapsed in [31, 44] {
        let output = run_smoke_loop(0, CLOSED, 0, elapsed).unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert!(stderr.contains("bead smoke-test.1 closed"), "{stderr}");
        assert!(stderr.contains(&format!("elapsed: {elapsed}s")), "{stderr}");
        assert!(stderr.contains("warning:"), "{stderr}");
        assert!(stderr.contains("30s soft target"), "{stderr}");
        assert!(stderr.ends_with("[smoke] ok\n"), "{stderr}");
    }
}

#[test]
fn smoke_within_target_does_not_warn() {
    for elapsed in [0, 30] {
        let output = run_smoke_loop(0, CLOSED, 0, elapsed).unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert!(!stderr.contains("warning:"), "{stderr}");
        assert!(stderr.ends_with("[smoke] ok\n"), "{stderr}");
    }
}

fn assert_failure(output: &Output, message: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(message), "{stderr}");
    assert!(!stderr.contains("[smoke] ok"), "{stderr}");
    assert!(!stderr.contains("warning:"), "{stderr}");
}

#[test]
fn smoke_loop_failure_is_fatal_even_when_bead_is_closed() {
    for exit in [1, 77] {
        let output = run_smoke_loop(exit, CLOSED, 0, 44).unwrap();
        assert_failure(&output, &format!("failed with exit {exit}"));
    }
}

#[test]
fn smoke_status_read_failure_is_fatal() {
    for (json, exit) in [(CLOSED, 1), ("invalid JSON", 0), ("[]", 0), ("{}", 0)] {
        let output = run_smoke_loop(0, json, exit, 44).unwrap();
        assert_failure(&output, "failed to read bead smoke-test.1 status");
    }
}

#[test]
fn smoke_unclosed_bead_is_fatal() {
    for status in ["open", "in_progress", "blocked"] {
        let json = format!(r#"[{{"status":"{status}"}}]"#);
        let output = run_smoke_loop(0, &json, 0, 44).unwrap();
        assert_failure(&output, &format!("did not close: status={status}"));
    }
}

#[test]
fn smoke_unclosed_bead_reports_notes_and_metadata_for_diagnosis() {
    for json in [
        r#"[{"status":"blocked","notes":"host gate rejected the worker","metadata":{"loom.infra.error":"signature verification failed"}}]"#,
        r#"{"status":"blocked","notes":"host gate rejected the worker","metadata":{"loom.infra.error":"signature verification failed"}}"#,
    ] {
        let output = run_smoke_loop(0, json, 0, 44).unwrap();
        assert_failure(&output, "did not close: status=blocked");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(json), "{stderr}");
    }
}

#[test]
fn smoke_status_fixture_matches_bd_response_contract() {
    let beads: Vec<loom_driver::bd::Bead> = serde_json::from_str(CLOSED).unwrap();
    assert_eq!(beads.len(), 1);
    assert_eq!(beads[0].id.as_str(), "smoke-test.1");
    assert_eq!(beads[0].status, loom_driver::bd::Status::Closed);
    let object = serde_json::to_string(&beads[0]).unwrap();
    let output = run_smoke_loop(0, &object, 0, 0).unwrap();
    assert!(output.status.success(), "{output:?}");
}
