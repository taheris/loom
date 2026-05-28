//! Live-path test for `loom gate audit --tree --spec <X>` refusing when
//! more than one open epic exists for `<X>`.
//!
//! `specs/gate.md` § *Standing-safety-net bonding* lists three branches
//! for the single-tier resolution: zero results mint, one result bonds,
//! and more-than-one results refuse with the conflicting epic IDs. The
//! zero-result branch's `auto_creates_epics_*` test lives next to the
//! resolver helper in `loom-workflow`; this file pins the refuse-side
//! observable: the operator must see both conflicting IDs in stderr and
//! the binary must NOT spawn the reviewer agent.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn find_bash() -> PathBuf {
    let path_var = std::env::var_os("PATH").expect("PATH must be set");
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("bash");
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!("bash not found in PATH");
}

/// Install a `bd` shim that returns a two-epic body for every JSON list
/// query — driving the resolver's `InvariantViolation` branch — and
/// exits 0 silently for every other invocation.
fn install_bd_conflict_stub(dir: &Path) -> PathBuf {
    let bin_dir = dir.join("bd-bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bd = bin_dir.join("bd");
    let bash = find_bash();
    let body = format!(
        "#!{bash}\n\
         set -euo pipefail\n\
         for arg in \"$@\"; do\n\
             if [ \"$arg\" = '--json' ]; then\n\
                 cat <<'__BD_BEAD_JSON__'\n\
[\
{{\"id\":\"lm-aaa\",\"title\":\"acme\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"epic\",\"labels\":[\"spec:acme\"],\"metadata\":{{}}}}\
,\
{{\"id\":\"lm-bbb\",\"title\":\"acme\",\"status\":\"open\",\"priority\":2,\"issue_type\":\"epic\",\"labels\":[\"spec:acme\"],\"metadata\":{{}}}}\
]\n\
__BD_BEAD_JSON__\n\
                 exit 0\n\
             fi\n\
         done\n\
         exit 0\n",
        bash = bash.display(),
    );
    std::fs::write(&bd, body).unwrap();
    let mut perm = std::fs::metadata(&bd).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&bd, perm).unwrap();
    bin_dir
}

fn pinned_path(workspace: &Path) -> std::ffi::OsString {
    let bd_bin_dir = install_bd_conflict_stub(workspace);
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![bd_bin_dir];
    entries.extend(std::env::split_paths(&path_var));
    std::env::join_paths(entries).expect("join PATH")
}

fn run_loom_gate_audit(workspace: &Path, label: &str) -> std::process::Output {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .args(["gate", "audit", "--tree", "--spec", label])
        .env_remove("LOOM_INSIDE")
        .env("PATH", pinned_path(workspace))
        .output()
        .expect("spawn loom")
}

#[test]
fn tree_scope_refuses_when_more_than_one_open_epic_for_spec() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();

    let output = run_loom_gate_audit(workspace, "acme");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "loom gate audit --tree --spec acme must exit non-zero when more \
         than one open epic exists for acme. status={:?}\nstdout={stdout}\nstderr={stderr}",
        output.status,
    );
    assert!(
        stderr.contains("acme"),
        "stderr must name the spec. stderr:\n{stderr}",
    );
    assert!(
        stderr.contains("lm-aaa") && stderr.contains("lm-bbb"),
        "stderr must surface both conflicting epic IDs. stderr:\n{stderr}",
    );

    let logs_dir = workspace.join(".wrapix/loom/logs");
    let log_entries: Vec<_> = std::fs::read_dir(&logs_dir)
        .map(|it| it.filter_map(Result::ok).collect())
        .unwrap_or_default();
    assert!(
        log_entries.is_empty(),
        "no agent log must be written when the >1-epic refuse path fires. \
         entries={log_entries:?}",
    );
}
