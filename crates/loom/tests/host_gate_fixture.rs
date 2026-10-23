#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

fn writable_directories(root: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(());
    }
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o200);
    std::fs::set_permissions(root, permissions)?;
    for entry in std::fs::read_dir(root)? {
        writable_directories(&entry?.path())?;
    }
    Ok(())
}

#[test]
fn host_gate_setup_keeps_executed_harness_outside_inner_project_inventory() {
    let root = tempfile::tempdir().unwrap();
    let tmp = root.path().join("tmp");
    let native = root.path().join("native");
    std::fs::create_dir(&tmp).unwrap();
    for directory in ["docs", "specs"] {
        std::fs::create_dir_all(native.join(directory)).unwrap();
    }
    for path in [
        "verification.nix",
        "result-check.sh",
        "flake.nix",
        "loom-smoke-result.txt",
        "docs/README.md",
        "specs/smoke.md",
    ] {
        std::fs::write(native.join(path), "fixture\n").unwrap();
    }
    std::fs::write(native.join(".gitignore"), "/.loom/\n/target/\n").unwrap();
    let wrapper = root.path().join("pre-push-checks");
    std::fs::write(&wrapper, include_str!("../../../bin/pre-push-checks")).unwrap();
    let launcher = root.path().join("setup-only-wrix");
    std::fs::write(&launcher, "#!/usr/bin/env bash\nexit 0\n").unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let prefix = include_str!("../../../tests/smoke/host-gate-test.sh")
        .split_once("git add .gitignore")
        .unwrap()
        .0;
    let script = format!("{prefix}\nexit 73\n");
    let output = Command::new("bash")
        .args(["-c", &script, "setup-unit"])
        .arg(&launcher)
        .arg(&wrapper)
        .arg(&native)
        .arg("unused-registration")
        .env("TMPDIR", &tmp)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(73),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fixture = std::fs::read_dir(&tmp)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let workspace = fixture.join("workspace");
    let harness = fixture.join("harness");
    let snapshot = loom_gate::inventory::snapshot::Snapshot::discover(&workspace).unwrap();
    assert!(
        snapshot
            .files()
            .keys()
            .any(|path| path.as_path() == Path::new("result-check.sh")),
        "actual project resources must remain inventoried"
    );
    assert!(!snapshot.files().keys().any(|path| matches!(
        path.as_path().file_name().and_then(|name| name.to_str()),
        Some("host-gate-hook.sh" | "pre-push-checks")
    )));
    assert_eq!(
        std::fs::read(harness.join("pre-push-checks")).unwrap(),
        std::fs::read(&wrapper).unwrap()
    );
    let hook = std::fs::read_to_string(harness.join("host-gate-hook.sh")).unwrap();
    assert!(hook.contains("loom gate check --tree"));
    let config = snapshot
        .text(
            &loom_gate::inventory::snapshot::ResourcePath::try_from(std::path::PathBuf::from(
                ".pre-commit-config.yaml",
            ))
            .unwrap(),
        )
        .unwrap();
    assert!(config.contains(harness.join("pre-push-checks").to_str().unwrap()));
    assert!(config.contains(harness.join("host-gate-hook.sh").to_str().unwrap()));
    let tools = root.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let loom = tools.join("loom");
    std::fs::write(&loom, "#!/usr/bin/env bash\n[[ \"$*\" == 'gate check --tree' ]] || exit 2\nprintf dispatched > \"$DISPATCH_RECORD\"\nexit 79\n").unwrap();
    std::fs::set_permissions(&loom, std::fs::Permissions::from_mode(0o755)).unwrap();
    let dispatch_record = root.path().join("dispatch");
    let mut search = vec![tools];
    search.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let status = Command::new("bash")
        .arg(harness.join("pre-push-checks"))
        .args(["--hook-id", "smoke-host-gate", "--hook-entry"])
        .arg(format!(
            "bash \"{}\"",
            harness.join("host-gate-hook.sh").display()
        ))
        .args(["--", "bash"])
        .arg(harness.join("host-gate-hook.sh"))
        .env("PATH", std::env::join_paths(search).unwrap())
        .env("DISPATCH_RECORD", &dispatch_record)
        .current_dir(&workspace)
        .status()
        .unwrap();
    assert_eq!(
        status.code(),
        Some(79),
        "the real wrapper and generated hook must propagate the verifier result"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join(".loom/host-hook-ran")).unwrap(),
        "ran\n"
    );
    assert_eq!(
        std::fs::read_to_string(dispatch_record).unwrap(),
        "dispatched"
    );
    writable_directories(root.path()).unwrap();
}

#[test]
fn host_gate_result_writers_agree_with_the_actual_native_predicate() {
    let root = tempfile::tempdir().unwrap();
    let writers: Vec<_> = include_str!("../../../tests/smoke/host-gate-test.sh")
        .lines()
        .filter(|line| {
            line.trim_start().starts_with("printf '") && line.ends_with("> loom-smoke-result.txt")
        })
        .collect();
    assert_eq!(writers.len(), 3);
    for writer in writers {
        let status = Command::new("bash")
            .args(["-c", writer])
            .current_dir(root.path())
            .status()
            .unwrap();
        assert!(status.success());
        let out = root.path().join("predicate-output");
        if out.exists() {
            std::fs::remove_file(&out).unwrap();
        }
        let result = Command::new("bash")
            .args([
                "-c",
                include_str!("../../../tests/smoke/result-check.sh"),
                "result-check",
                "loom-smoke-result.txt",
            ])
            .env("out", &out)
            .current_dir(root.path())
            .output()
            .unwrap();
        let expected = !writer.contains("not implemented");
        assert_eq!(
            result.status.success(),
            expected,
            "writer {writer}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        if expected {
            assert_eq!(std::fs::read_to_string(out).unwrap(), "verified\n");
        }
    }
}

#[test]
fn host_gate_registers_declared_native_inputs_in_its_own_store_database() {
    let root = tempfile::tempdir().unwrap();
    let tool = Command::new("bash")
        .args(["-c", "command -v nix"])
        .output()
        .unwrap();
    assert!(tool.status.success());
    let tool = std::fs::canonicalize(String::from_utf8(tool.stdout).unwrap().trim()).unwrap();
    let bash = Command::new("bash")
        .args(["-c", "printf '%s' \"$BASH\""])
        .output()
        .unwrap();
    let bash = std::fs::canonicalize(String::from_utf8(bash.stdout).unwrap()).unwrap();
    let roots = [
        tool.parent().unwrap().parent().unwrap(),
        bash.parent().unwrap().parent().unwrap(),
    ];
    let closure = Command::new("nix-store")
        .args(["--query", "--requisites"])
        .args(roots)
        .output()
        .unwrap();
    assert!(
        closure.status.success(),
        "{}",
        String::from_utf8_lossy(&closure.stderr)
    );
    let closure = String::from_utf8(closure.stdout).unwrap();
    let registration = Command::new("nix-store")
        .arg("--dump-db")
        .args(closure.lines())
        .output()
        .unwrap();
    assert!(registration.status.success());
    let registration_path = root.path().join("registration");
    std::fs::write(&registration_path, registration.stdout).unwrap();
    let args = [
        "path-info",
        "--offline",
        "--json",
        "--json-format",
        "1",
        roots[1].to_str().unwrap(),
        "--extra-experimental-features",
        "nix-command flakes",
    ];
    let original = Command::new(&tool).args(args).output().unwrap();
    assert!(original.status.success());
    let original: serde_json::Value = serde_json::from_slice(&original.stdout).unwrap();
    let before = Command::new(&tool)
        .args(args)
        .env("NIX_REMOTE", "local")
        .env("NIX_STATE_DIR", root.path().join("nix-state"))
        .env("NIX_CONF_DIR", root.path().join("nix-conf"))
        .output()
        .unwrap();
    let before: serde_json::Value = serde_json::from_slice(&before.stdout).unwrap();
    assert!(before[roots[1].to_str().unwrap()].is_null());
    let source = include_str!("../../../tests/smoke/host-gate-test.sh");
    let bootstrap = source
        .split_once("export NIX_REMOTE=local")
        .unwrap()
        .1
        .split_once("test_smoke_runtime_executes_host_hook() {")
        .unwrap()
        .0;
    let script = format!(
        "set -euo pipefail\nexport NIX_REMOTE=local\n{bootstrap}\nnix path-info --offline --json --json-format 1 \"$BASH_ROOT\"\n"
    );
    let observed = Command::new("bash")
        .args(["-c", &script])
        .env("fixture", root.path())
        .env("store_registration", &registration_path)
        .env("BASH_ROOT", roots[1])
        .output()
        .unwrap();
    assert!(
        observed.status.success(),
        "{}",
        String::from_utf8_lossy(&observed.stderr)
    );
    let observed: serde_json::Value = serde_json::from_slice(&observed.stdout).unwrap();
    for field in ["narHash", "narSize", "references"] {
        assert_eq!(
            observed[roots[1].to_str().unwrap()][field],
            original[roots[1].to_str().unwrap()][field]
        );
    }
    writable_directories(root.path()).unwrap();
}

#[test]
fn successful_host_gate_cleanup_unlinks_readonly_sources_without_following_external_symlinks() {
    let root = tempfile::tempdir().unwrap();
    let tmp = root.path().join("tmp");
    let outside = root.path().join("outside");
    std::fs::create_dir(&tmp).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("input"), "untouched").unwrap();
    std::fs::set_permissions(
        outside.join("input"),
        std::fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o555)).unwrap();
    let prefix = include_str!("../../../tests/smoke/host-gate-test.sh")
        .split_once("export HOME=")
        .unwrap()
        .0;
    let script = format!(
        "{prefix}\nprintf '%s' \"$fixture\" > \"$RECORD\"\nmkdir -p \"$fixture/store\"\nprintf readonly > \"$fixture/store/input\"\nchmod 0444 \"$fixture/store/input\"\nchmod 0555 \"$fixture/store\"\nln -s \"$OUTSIDE\" \"$fixture/external\"\n"
    );
    let record = root.path().join("record");
    let output = Command::new("bash")
        .args([
            "-c",
            &script,
            "cleanup-unit",
            "unused-wrix",
            "unused-pre-push",
            "unused-native-fixture",
            "unused-registration",
        ])
        .env("TMPDIR", &tmp)
        .env("RECORD", &record)
        .env("OUTSIDE", &outside)
        .output()
        .unwrap();
    let retained = std::fs::read_to_string(&record).unwrap();
    let unchanged = std::fs::metadata(&outside).unwrap().permissions().mode() & 0o222 == 0
        && std::fs::metadata(outside.join("input"))
            .unwrap()
            .permissions()
            .mode()
            & 0o222
            == 0;
    writable_directories(root.path()).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!Path::new(&retained).exists());
    assert!(unchanged);
    assert_eq!(
        std::fs::read_to_string(outside.join("input")).unwrap(),
        "untouched"
    );
}

#[test]
fn failed_host_gate_fixture_retains_readonly_store_and_original_exit_without_cleanup_noise() {
    let root = tempfile::tempdir().unwrap();
    let tmp = root.path().join("tmp");
    std::fs::create_dir(&tmp).unwrap();
    let launcher = root.path().join("failing-wrix");
    std::fs::write(&launcher, "#!/usr/bin/env bash\nset -euo pipefail\nstore=\"$HOME/.local/share/nix/root/nix/store/readonly-source\"\nmkdir -p \"$store\"\nprintf preserved > \"$store/input\"\nchmod 0444 \"$store/input\"\nchmod 0555 \"$store\"\nexit 73\n").unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new("bash")
        .arg("-c")
        .arg(include_str!("../../../tests/smoke/host-gate-test.sh"))
        .arg("host-gate-test.sh")
        .arg(&launcher)
        .arg("unused-pre-push")
        .arg("unused-native-fixture")
        .arg("unused-registration")
        .env("TMPDIR", &tmp)
        .output()
        .unwrap();
    let fixtures: Vec<_> = std::fs::read_dir(&tmp)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let retained = fixtures
        .first()
        .unwrap()
        .join("home/.local/share/nix/root/nix/store/readonly-source/input");
    let preserved = std::fs::read_to_string(&retained).unwrap();
    let readonly = std::fs::metadata(&retained).unwrap().permissions().mode() & 0o222 == 0;
    writable_directories(root.path()).unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(73), "{stderr}");
    assert!(
        stderr.contains("host gate fixture retained after failure (exit 73)"),
        "{stderr}"
    );
    assert!(!stderr.contains("Permission denied"), "{stderr}");
    assert_eq!(preserved, "preserved");
    assert!(
        readonly,
        "failure cleanup must not change immutable file permissions"
    );
}
