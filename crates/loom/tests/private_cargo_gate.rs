//! Native verifier commands preserve private output; Gate refuses unregistered scripts.

#![allow(
    clippy::unwrap_used,
    reason = "disposable integration fixture assertions"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Output};

const CHILD_ROOT: &str = "LOOM_PRIVATE_CARGO_TEST_ROOT";
const NATIVE_CASE: &str =
    "native_verifier_creates_reuses_and_refuses_private_cargo_under_both_umasks";

fn policy(state: &Path) {
    let root = state.join("loom/trusted-cargo");
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("policy.json");
    fs::write(
        &path,
        "{\"reservation_bytes\":1048576,\"min_free_bytes\":0}",
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn source(workspace: &Path) {
    fs::create_dir_all(workspace.join("specs")).unwrap();
    fs::create_dir(workspace.join("src")).unwrap();
    fs::create_dir(workspace.join(".loom")).unwrap();
    fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname='private-gate-fixture'\nversion='0.1.0'\nedition='2024'\n",
    )
    .unwrap();
    fs::write(workspace.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    assert!(
        Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(workspace)
            .status()
            .unwrap()
            .success()
    );
}

fn native_child(workspace: &Path) {
    let mut command = Command::new("bash");
    command.args([
        "-c",
        "set -euo pipefail; cargo build --offline --locked --quiet; printf '%s' \"$CARGO_TARGET_DIR\" > .loom/observed-target",
    ]);
    command.current_dir(workspace);
    let _execution =
        loom_driver::workspace::cargo::verifier_command(&mut command, workspace).unwrap();
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn native(workspace: &Path, state: &Path, mask: &str) -> Output {
    Command::new("bash")
        .args(["-c", "umask \"$1\"; shift; exec \"$@\"", "bash", mask])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", NATIVE_CASE, "--nocapture"])
        .env(CHILD_ROOT, workspace)
        .env("XDG_STATE_HOME", state)
        .env("LOOM_TEST_STORAGE", "1")
        .env_remove("LOOM_INSIDE")
        .output()
        .unwrap()
}

#[test]
fn native_verifier_creates_reuses_and_refuses_private_cargo_under_both_umasks() {
    if let Some(workspace) = std::env::var_os(CHILD_ROOT) {
        native_child(Path::new(&workspace));
        return;
    }
    for mask in ["022", "077"] {
        let fixture = tempfile::tempdir().unwrap();
        let workspace = fixture.path().join("source");
        let state = fixture.path().join("state");
        source(&workspace);
        policy(&state);
        let first = native(&workspace, &state, mask);
        assert!(
            first.status.success(),
            "umask {mask}: {} {}",
            String::from_utf8_lossy(&first.stdout),
            String::from_utf8_lossy(&first.stderr)
        );
        let target = fs::read_to_string(workspace.join(".loom/observed-target")).unwrap();
        let target = Path::new(&target);
        let namespace = target.parent().unwrap();
        for path in [target, namespace] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o7777,
                0o700
            );
        }
        fs::write(target.join("sentinel"), b"retained").unwrap();
        let second = native(&workspace, &state, mask);
        assert!(
            second.status.success(),
            "{} {}",
            String::from_utf8_lossy(&second.stdout),
            String::from_utf8_lossy(&second.stderr)
        );
        assert_eq!(
            fs::read_to_string(workspace.join(".loom/observed-target")).unwrap(),
            target.to_str().unwrap()
        );
        fs::set_permissions(namespace, fs::Permissions::from_mode(0o755)).unwrap();
        let rejected = native(&workspace, &state, mask);
        assert!(!rejected.status.success());
        let diagnostic = format!(
            "{} {}",
            String::from_utf8_lossy(&rejected.stdout),
            String::from_utf8_lossy(&rejected.stderr)
        );
        assert!(
            diagnostic.contains(namespace.to_str().unwrap()),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("0755") && diagnostic.contains("0700"),
            "{diagnostic}"
        );
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"retained");
        assert_eq!(
            fs::metadata(namespace).unwrap().permissions().mode() & 0o7777,
            0o755
        );
    }
}

#[test]
fn native_gate_refuses_unregistered_cargo_scripts_before_execution() {
    let fixture = tempfile::tempdir().unwrap();
    let workspace = fixture.path().join("source");
    let state = fixture.path().join("state");
    source(&workspace);
    policy(&state);
    fs::write(
        workspace.join("specs/fixture.md"),
        "## Success Criteria\n- Unregistered Cargo [check](sh check.sh)\n",
    )
    .unwrap();
    fs::write(workspace.join("check.sh"), "touch executed\n").unwrap();
    let mut git = Command::new("git");
    loom_test_support::scrub_git_local_env(&mut git);
    assert!(
        git.args(["init", "-q"])
            .arg(&workspace)
            .status()
            .unwrap()
            .success()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .arg("--workspace")
        .arg(&workspace)
        .args(["gate", "verify", "--tree"])
        .env("XDG_STATE_HOME", &state)
        .env_remove("LOOM_INSIDE")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostic.contains("no actual checked definition"),
        "{diagnostic}"
    );
    assert!(!workspace.join("executed").exists());
    assert!(!state.join("loom/trusted-cargo/native").exists());
}
