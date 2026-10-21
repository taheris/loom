//! Native Gate must not depend on its caller's umask or repair retained output.

#![allow(
    clippy::unwrap_used,
    reason = "disposable integration fixture assertions"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Output};

fn gate(workspace: &Path, state: &Path, mask: &str) -> Output {
    Command::new("bash")
        .args(["-c", "umask \"$1\"; shift; exec \"$@\"", "bash", mask])
        .arg(env!("CARGO_BIN_EXE_loom"))
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "verify", "--tree"])
        .env("XDG_STATE_HOME", state)
        .env_remove("LOOM_INSIDE")
        .output()
        .unwrap()
}

#[test]
fn native_gate_creates_reuses_and_refuses_private_cargo_under_both_umasks() {
    for mask in ["022", "077"] {
        let fixture = tempfile::tempdir().unwrap();
        let workspace = fixture.path().join("source");
        let state = fixture.path().join("state");
        fs::create_dir_all(workspace.join("specs")).unwrap();
        fs::create_dir(workspace.join("src")).unwrap();
        fs::create_dir_all(state.join("loom/trusted-cargo")).unwrap();
        fs::set_permissions(
            state.join("loom/trusted-cargo"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::write(
            state.join("loom/trusted-cargo/policy.json"),
            "{\"reservation_bytes\":1048576,\"min_free_bytes\":0}",
        )
        .unwrap();
        fs::set_permissions(
            state.join("loom/trusted-cargo/policy.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::write(
            workspace.join("Cargo.toml"),
            "[package]\nname='private-gate-fixture'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        fs::write(
            workspace.join("Cargo.lock"),
            "version=4\n[[package]]\nname='private-gate-fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::write(workspace.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
        fs::write(
            workspace.join("specs/fixture.md"),
            "# Fixture\n\n## Success Criteria\n\n- Native Cargo runs [check](sh check.sh)\n",
        )
        .unwrap();
        fs::write(workspace.join("check.sh"), "set -eu\ncargo build --offline --quiet\nprintf '%s' \"$CARGO_TARGET_DIR\" > .loom/observed-target\n").unwrap();
        assert!(
            Command::new("cargo")
                .args(["generate-lockfile", "--offline"])
                .current_dir(&workspace)
                .status()
                .unwrap()
                .success()
        );
        let mut git = Command::new("git");
        loom_test_support::scrub_git_local_env(&mut git);
        assert!(
            git.arg("init")
                .arg("-q")
                .arg(&workspace)
                .status()
                .unwrap()
                .success()
        );
        let first = gate(&workspace, &state, mask);
        assert!(
            first.status.success(),
            "umask {mask}: {}",
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
        let second = gate(&workspace, &state, mask);
        assert!(
            second.status.success(),
            "{}",
            String::from_utf8_lossy(&second.stderr)
        );
        assert_eq!(
            fs::read_to_string(workspace.join(".loom/observed-target")).unwrap(),
            target.to_str().unwrap()
        );
        fs::set_permissions(namespace, fs::Permissions::from_mode(0o755)).unwrap();
        let rejected = gate(&workspace, &state, mask);
        assert!(!rejected.status.success());
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert!(stderr.contains(namespace.to_str().unwrap()), "{stderr}");
        assert!(
            stderr.contains("0755") && stderr.contains("0700"),
            "{stderr}"
        );
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"retained");
        assert_eq!(
            fs::metadata(namespace).unwrap().permissions().mode() & 0o7777,
            0o755
        );
    }
}
