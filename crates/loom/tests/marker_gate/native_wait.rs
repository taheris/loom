//! The native-wait CLI regression uses isolated real Beads, not the legacy shim.

use std::fmt::Write as _;

use super::*;

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub fn assert_native_wait_parking() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path();
    init_workspace_repo(workspace);
    let state = workspace.join("native-bd-state");
    std::fs::create_dir_all(state.join("home")).unwrap();
    let bin = workspace.join("native-bd-bin");
    std::fs::create_dir(&bin).unwrap();
    let native = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("bd"))
        .find(|path| path.is_file())
        .expect("native Beads must be provisioned for the native CLI regression");
    let mut script = String::from("set -euo pipefail\n");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("BEADS_") {
            writeln!(script, "unset {}", quote(&name.to_string_lossy())).unwrap();
        }
    }
    write!(script,
        "export HOME={}\nprintf '%s ' \"$@\" >> {}\nprintf '\\n' >> {}\ncd {}\nexec {} --sandbox --dolt-auto-commit off \"$@\"\n",
        quote(&state.join("home").to_string_lossy()),
        quote(&state.join(".invocations.log").to_string_lossy()),
        quote(&state.join(".invocations.log").to_string_lossy()),
        quote(&workspace.to_string_lossy()), quote(&native.to_string_lossy()),
    ).unwrap();
    let bd = bin.join("bd");
    loom_test_support::write_executable_bash_script(&bd, &script).unwrap();
    let run = |args: &[&str]| {
        let output = Command::new(&bd).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "native bd {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    run(&[
        "init",
        "--prefix",
        "lm",
        "--skip-agents",
        "--skip-hooks",
        "--non-interactive",
    ]);
    let metadata = serde_json::json!({loom_protocol::acceptance::METADATA_KEY: loom_test_support::acceptance::metadata("harness").unwrap()}).to_string();
    run(&[
        "create",
        "--id",
        "lm-waiting",
        "--title",
        "waiting implementation",
        "--labels",
        "spec:agent,profile:base",
        "--metadata",
        &metadata,
    ]);
    run(&[
        "create",
        "--id",
        "lm-blocker",
        "--title",
        "real prerequisite",
    ]);
    run(&["dep", "add", "lm-waiting", "lm-blocker"]);
    let before = loom_driver::git::sync_head_commit_sha(workspace).unwrap();
    let manifest = write_minimal_manifest(workspace);
    let output = run_loom_loop_bead(
        workspace,
        &bin,
        &state,
        &manifest,
        "waiting-marker",
        "lm-waiting",
    );
    let log = read_invocation_log(&state);
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}\nnative bd log={log}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let current: serde_json::Value =
        serde_json::from_slice(&run(&["show", "lm-waiting", "--json"])).unwrap();
    assert_eq!(current[0]["status"], "blocked");
    assert!(current[0]["metadata"]["loom.dependency_wait"].is_string());
    assert!(
        current[0]["labels"]
            .as_array()
            .unwrap()
            .iter()
            .all(|label| !label.as_str().unwrap().starts_with("loom:"))
    );
    assert!(!driver_closed_bead(&log, "lm-waiting"));
    assert!(workspace.join(".loom/beads/lm-waiting").is_dir());
    assert_eq!(
        loom_driver::git::sync_head_commit_sha(workspace).unwrap(),
        before
    );
    assert!(workspace.join(".loom/dependency-waits").is_dir());
    assert!(
        log.lines()
            .any(|line| line.starts_with("update lm-waiting ")
                && line.contains("--if-status open")
                && line.contains("--set-metadata loom.dependency_wait="))
    );
}
