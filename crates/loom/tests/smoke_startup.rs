//! Smoke startup exercises the real readiness helper with an external Wrix fixture.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn wait_for_dolt(root: &Path, failures: usize) -> std::io::Result<(Output, String)> {
    let wrix = root.join("wrix");
    let calls = root.join("calls");
    let attempts = root.join("attempts");
    std::fs::write(
        &wrix,
        loom_test_support::bash_script(&format!(
            r#"set -euo pipefail
printf '%s\n' "$*" >> "$LOOM_TEST_DOLT_CALLS"
case "$*" in
    'service dolt wait')
        count=0
        if [[ -f "$LOOM_TEST_DOLT_ATTEMPTS" ]]; then read -r count < "$LOOM_TEST_DOLT_ATTEMPTS"; fi
        count=$((count + 1))
        printf '%s\n' "$count" > "$LOOM_TEST_DOLT_ATTEMPTS"
        if [[ "$count" -le {failures} ]]; then
            printf 'Dolt endpoint did not become ready within 6 seconds\n' >&2
            exit 1
        fi
        ;;
    'service status') printf 'runtime: Stopped\n' ;;
    'service logs') printf 'Dolt startup diagnostic\n' ;;
    *) printf 'unexpected Wrix command: %s\n' "$*" >&2; exit 2 ;;
esac
"#,
        )),
    )?;
    std::fs::set_permissions(&wrix, std::fs::Permissions::from_mode(0o755))?;
    let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/smoke/wait-dolt.sh");
    let output = Command::new("bash")
        .arg(helper)
        .arg(wrix)
        .env("LOOM_TEST_DOLT_CALLS", &calls)
        .env("LOOM_TEST_DOLT_ATTEMPTS", &attempts)
        .output()?;
    Ok((output, std::fs::read_to_string(calls)?))
}

#[test]
fn smoke_dolt_ready_service_needs_one_probe() {
    let dir = tempfile::tempdir().unwrap();
    let (output, calls) = wait_for_dolt(dir.path(), 0).unwrap();
    assert!(output.status.success());
    assert_eq!(calls, "service dolt wait\n");
}

#[test]
fn smoke_dolt_delayed_readiness_does_not_restart_service() {
    let dir = tempfile::tempdir().unwrap();
    let (output, calls) = wait_for_dolt(dir.path(), 1).unwrap();
    assert!(output.status.success());
    assert_eq!(calls, "service dolt wait\nservice dolt wait\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("startup probe 1/5 failed"), "{stderr}");
    assert!(
        stderr.contains("Dolt ready (startup probe 2/5)"),
        "{stderr}"
    );
}

#[test]
fn smoke_dolt_failed_startup_retains_service_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let (output, calls) = wait_for_dolt(dir.path(), 5).unwrap();
    assert!(!output.status.success());
    assert_eq!(
        calls,
        format!(
            "{}service status\nservice logs\n",
            "service dolt wait\n".repeat(5)
        ),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("30-second startup budget"), "{stderr}");
    assert!(stderr.contains("runtime: Stopped"), "{stderr}");
    assert!(stderr.contains("Dolt startup diagnostic"), "{stderr}");
}
