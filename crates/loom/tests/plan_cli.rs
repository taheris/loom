//! Process-level contracts for the interactive `loom plan` boundary.

#![allow(clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use loom_driver::identifier::SpecLabel;
use loom_driver::state::CacheDb;

fn install_bd_shim(root: &Path) -> PathBuf {
    let bin_dir = root.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create shim directory");
    let destination = bin_dir.join("bd");
    let source = PathBuf::from(env!("CARGO_BIN_EXE_bd-shim"));
    if std::os::unix::fs::symlink(&source, &destination).is_err() {
        std::fs::copy(&source, &destination).expect("copy bd shim");
        let mut permissions = std::fs::metadata(&destination)
            .expect("stat bd shim")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&destination, permissions).expect("chmod bd shim");
    }
    bin_dir
}

fn seed_workspace(root: &Path) -> PathBuf {
    std::fs::create_dir_all(root.join(".loom")).expect("create loom directory");
    std::fs::create_dir_all(root.join("docs")).expect("create docs directory");
    std::fs::create_dir_all(root.join("specs")).expect("create specs directory");
    std::fs::write(
        root.join("docs/README.md"),
        "# Loom Docs\n\n- [agent](../specs/agent.md)\n",
    )
    .expect("write spec index");
    std::fs::write(root.join("specs/agent.md"), "# Agent\n\nBefore plan.\n")
        .expect("write agent spec");

    let db = CacheDb::open(root.join(".loom/cache.db")).expect("open cache");
    db.notes_set(
        &SpecLabel::new("agent"),
        "implementation",
        &["old implementation note".to_owned()],
        1,
    )
    .expect("seed implementation note");
    drop(db);

    let manifest = root.join("profile-images.json");
    let body = serde_json::json!({
        "base": {
            "claude": {
                "ref": "localhost/wrix-base-claude:plan-contract",
                "source": "/nix/store/plan-contract-image",
                "source_kind": "nix-descriptor"
            }
        }
    });
    std::fs::write(
        &manifest,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&body).expect("serialize manifest")
        ),
    )
    .expect("write profile manifest");
    manifest
}

fn install_wrix_shim(root: &Path) -> PathBuf {
    let path = root.join("wrix-shim");
    loom_test_support::write_executable_bash_script(
        &path,
        r#"set -euo pipefail
printf 'subcommand=%s\nworkspace=%s\nagent=%s\n' "$1" "$2" "$3" > "$WRIX_LOG"
if [[ "$1" != "run" ]]; then
    printf 'expected wrix run, got %s\n' "$1" >&2
    exit 2
fi
workspace="$2"
printf '# Agent\n\nUpdated by plan.\n' > "$workspace/specs/agent.md"
printf '\n- planning-session-update\n' >> "$workspace/docs/README.md"
"${LOOM_TEST_BIN:?}" --workspace "$workspace" note set agent --kind implementation --json '["merged implementation note"]'
bd list --json > "$BD_READ_LOG"
"#,
    )
    .expect("write wrix shim");
    path
}

#[test]
fn plan_does_not_create_epic_or_touch_bd() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let manifest = seed_workspace(&workspace);
    let wrix = install_wrix_shim(dir.path());
    let bd_bin = install_bd_shim(dir.path());
    let bd_state = dir.path().join("bd-state");
    std::fs::create_dir_all(&bd_state).expect("create bd state");
    let wrix_log = dir.path().join("wrix.log");
    let bd_read_log = dir.path().join("bd-read.json");

    let mut path_entries = vec![bd_bin];
    path_entries.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH"),
    ));
    let shimmed_path = std::env::join_paths(path_entries).expect("join PATH");

    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .arg("--workspace")
        .arg(&workspace)
        .arg("--host-key")
        .args(["plan", "agent"])
        .env("LOOM_PROFILES_MANIFEST", manifest)
        .env("LOOM_WRIX_BIN", wrix)
        .env("LOOM_TEST_BIN", env!("CARGO_BIN_EXE_loom"))
        .env("WRIX_LOG", &wrix_log)
        .env("BD_READ_LOG", &bd_read_log)
        .env("BD_STATE_DIR", &bd_state)
        .env("PATH", shimmed_path)
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom plan");

    assert!(
        output.status.success(),
        "loom plan failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let launch = std::fs::read_to_string(&wrix_log).expect("read wrix log");
    assert!(launch.contains("subcommand=run"), "{launch}");
    assert!(launch.contains("agent=claude"), "{launch}");
    assert_eq!(
        std::fs::read_to_string(workspace.join("specs/agent.md")).expect("read updated spec"),
        "# Agent\n\nUpdated by plan.\n",
    );
    assert!(
        std::fs::read_to_string(workspace.join("docs/README.md"))
            .expect("read updated index")
            .contains("planning-session-update"),
    );
    let db = CacheDb::open(workspace.join(".loom/cache.db")).expect("reopen cache");
    let notes = db
        .notes_list(Some(&SpecLabel::new("agent")), Some("implementation"))
        .expect("list implementation notes");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].text, "merged implementation note");

    assert_eq!(
        std::fs::read_to_string(&bd_read_log).expect("read bd list output"),
        "[]\n",
    );
    let bd_invocations =
        std::fs::read_to_string(bd_state.join(".invocations.log")).expect("read bd invocation log");
    assert_eq!(bd_invocations, "list --json\n");
    let bead_directories = std::fs::read_dir(&bd_state)
        .expect("read bd state")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .count();
    assert_eq!(bead_directories, 0, "plan must not create an epic");
    assert!(
        !bd_invocations.lines().any(|line| {
            matches!(
                line.split_whitespace().next(),
                Some("create" | "update" | "close")
            )
        }),
        "plan invoked a bd mutation: {bd_invocations}",
    );
}
