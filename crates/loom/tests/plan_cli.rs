//! Process-level contracts for the interactive `loom plan` boundary.

#![allow(
    clippy::unwrap_used,
    reason = "validated identifier literals are fixed integration-test fixtures"
)]
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
        &SpecLabel::new("agent").unwrap(),
        "implementation",
        &["old implementation note".to_owned()],
        1,
    )
    .expect("seed implementation note");
    drop(db);

    let manifest = root.join("profile-images.json");
    let mut body = serde_json::json!({});
    for profile in ["base", "rust"] {
        for runtime in ["claude", "pi"] {
            body[profile][runtime] = serde_json::json!({
                "ref": format!("localhost/wrix-{profile}-{runtime}:plan-contract"),
                "source": "/nix/store/plan-contract-image",
                "source_kind": "nix-descriptor"
            });
        }
    }
    loom_test_support::profile_manifest::write(
        &manifest,
        body.to_string(),
        &root.parent().unwrap().join("wrix-shim"),
    )
    .expect("write profile manifest");
    manifest
}

#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "the braces are Bash parameter expansion syntax in the generated test script"
)]
fn install_wrix_shim(root: &Path) -> PathBuf {
    let path = root.join("wrix-shim");
    loom_test_support::write_executable_bash_script(
        &path,
        r#"set -euo pipefail
printf 'launcher=%s\n' "$0" > "$WRIX_LOG"
for arg in "$@"; do printf '%s\n' "$arg" >> "$WRIX_LOG"; done
[[ "$1" == --profile-config ]]
jq -r '"image_ref=\(.image.ref)\nprofile_agent=\(.agent.kind)"' "$2" >> "$WRIX_LOG"
shift 2
for arg in "$@"; do [[ "$arg" != --profile-config ]]; done
printf 'subcommand=%s\nworkspace=%s\nagent=%s\n' "$1" "$2" "$3" >> "$WRIX_LOG"
printf 'deploy=%s\nsigning=%s\n' "${WRIX_DEPLOY_KEY:-}" "${WRIX_SIGNING_KEY:-}" >> "$WRIX_LOG"
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

struct LaunchFixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    manifest: PathBuf,
    launcher: PathBuf,
    wrapper: PathBuf,
}

impl LaunchFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let manifest = seed_workspace(&workspace);
        let launcher = root.path().join("wrix-shim");
        loom_test_support::write_executable_bash_script(&launcher, r#"set -euo pipefail
[[ "$1" == --profile-config ]]
[[ -f "$2" ]]
[[ "$3" == run ]]
for arg in "${@:3}"; do [[ "$arg" != --profile-config ]]; done
jq -n --arg launcher "$0" --args '{launcher:$launcher, argv:$ARGS.positional}' -- "$@" > "$LAUNCH_LOG"
if [[ "${READ_INPUT:-0}" == 1 ]]; then
    read -r input
    printf 'stdout:%s\n' "$input"
    printf 'stderr:%s\n' "$input" >&2
fi
exit "${LAUNCH_EXIT:-0}"
"#).unwrap();
        let mut matrix: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        for profile in ["base", "rust"] {
            for runtime in ["claude", "pi"] {
                if (profile, runtime) != ("base", "claude") {
                    let raw = root.path().join(format!("raw-wrix-{profile}-{runtime}"));
                    std::fs::copy(&launcher, &raw).unwrap();
                    matrix[profile][runtime]["launcher"] = serde_json::json!(raw);
                }
            }
        }
        std::fs::write(&manifest, matrix.to_string()).unwrap();
        let wrapper = root.path().join("wrix-rust-pi");
        loom_test_support::write_executable_bash_script(
            &wrapper,
            &format!(
                "set -euo pipefail\nexec \"{}\" --profile-config /baked-rust-pi.json \"$@\"\n",
                launcher.display()
            ),
        )
        .unwrap();
        Self {
            root,
            workspace,
            manifest,
            launcher,
            wrapper,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loom"));
        command
            .arg("--workspace")
            .arg(&self.workspace)
            .args(["plan", "flake", "--host-key"])
            .env("LOOM_PROFILES_MANIFEST", &self.manifest)
            .env("LOOM_WRIX_BIN", &self.wrapper)
            .env_remove("LOOM_WRIX_SPAWN_BIN")
            .env("LAUNCH_LOG", self.root.path().join("launch.json"))
            .env_remove("LOOM_INSIDE");
        command
    }

    fn launch(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.root.path().join("launch.json")).unwrap())
            .unwrap()
    }
}

#[test]
fn plan_cli_overrides_select_matching_profile_configs() {
    for (args, profile, runtime) in [
        (vec![], "rust", "pi"),
        (vec!["--profile", "base"], "base", "pi"),
        (vec!["--agent", "claude"], "rust", "claude"),
        (
            vec!["--profile", "base", "--agent", "claude"],
            "base",
            "claude",
        ),
    ] {
        let fixture = LaunchFixture::new();
        std::fs::write(
            fixture.workspace.join("loom.toml"),
            "[phase.plan]\nprofile = 'rust'\nagent.backend = 'pi'\n",
        )
        .unwrap();
        let output = fixture.command().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let launch = fixture.launch();
        let expected_launcher = if (profile, runtime) == ("base", "claude") {
            fixture.launcher.clone()
        } else {
            fixture
                .root
                .path()
                .join(format!("raw-wrix-{profile}-{runtime}"))
        };
        assert_eq!(launch["launcher"], expected_launcher.to_str().unwrap());
        let argv = launch["argv"].as_array().unwrap();
        assert_eq!(argv[0], "--profile-config");
        assert_eq!(
            argv[1],
            fixture
                .workspace
                .join(format!("{profile}-{runtime}-profile.json"))
                .to_str()
                .unwrap()
        );
        assert_eq!(argv[2], "run");
        assert_eq!(argv[3], fixture.workspace.to_str().unwrap());
        assert_eq!(argv[4], runtime);
        assert_eq!(
            argv.iter().filter(|arg| *arg == "--profile-config").count(),
            1
        );
        if runtime == "pi" {
            assert!(argv.iter().any(|arg| arg == "-e"));
            assert!(
                argv.last()
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .starts_with("@/workspace/")
            );
        }
    }
}

#[test]
fn plan_raw_launcher_override_changes_executable_not_profile_config() {
    let fixture = LaunchFixture::new();
    let override_bin = fixture.root.path().join("override-wrix");
    std::fs::copy(&fixture.launcher, &override_bin).unwrap();
    let output = fixture
        .command()
        .env("LOOM_WRIX_SPAWN_BIN", &override_bin)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let launch = fixture.launch();
    assert_eq!(launch["launcher"], override_bin.to_str().unwrap());
    assert_eq!(
        launch["argv"][1],
        fixture
            .workspace
            .join("base-claude-profile.json")
            .to_str()
            .unwrap()
    );
}

#[test]
fn plan_missing_raw_override_fails_without_wrapper_fallback() {
    let fixture = LaunchFixture::new();
    let output = fixture
        .command()
        .env(
            "LOOM_WRIX_SPAWN_BIN",
            fixture.root.path().join("missing-wrix"),
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!fixture.root.path().join("launch.json").exists());
}

#[test]
fn plan_does_not_unwrap_a_configured_launcher_override() {
    let fixture = LaunchFixture::new();
    let output = fixture
        .command()
        .env("LOOM_WRIX_SPAWN_BIN", &fixture.wrapper)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!fixture.root.path().join("launch.json").exists());
}

#[test]
fn plan_inherits_stdio_and_reports_nonzero_launcher_status() {
    use std::io::Write;
    use std::process::Stdio;
    let fixture = LaunchFixture::new();
    let mut child = fixture
        .command()
        .env("READ_INPUT", "1")
        .env("LAUNCH_EXIT", "42")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"inherited-input\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("stdout:inherited-input"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("stderr:inherited-input"));
    assert!(stderr.contains("exit status: 42"), "{stderr}");
}

#[test]
fn plan_does_not_create_epic_or_touch_bd() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let manifest = seed_workspace(&workspace);
    let wrix = install_wrix_shim(dir.path());
    let default_wrapper = dir.path().join("wrix-rust-pi");
    loom_test_support::write_executable_bash_script(
        &default_wrapper,
        &format!(
            "set -euo pipefail\nexec \"{}\" --profile-config /baked-rust-pi.json \"$@\"\n",
            wrix.display()
        ),
    )
    .unwrap();
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
        .env("LOOM_WRIX_BIN", default_wrapper)
        .env_remove("LOOM_WRIX_SPAWN_BIN")
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
    let argv: Vec<_> = launch.lines().collect();
    assert_eq!(argv[0], format!("launcher={}", wrix.display()));
    assert_eq!(argv[1], "--profile-config");
    assert_eq!(
        argv[2],
        workspace.join("base-claude-profile.json").to_string_lossy()
    );
    assert_eq!(argv[3], "run");
    assert_eq!(argv[4], workspace.to_string_lossy());
    assert_eq!(argv[5], "claude");
    assert_eq!(
        argv.iter()
            .filter(|arg| **arg == "--profile-config")
            .count(),
        1
    );
    assert!(launch.contains("image_ref=localhost/wrix-base-claude:plan-contract"));
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
        .notes_list(
            Some(&SpecLabel::new("agent").unwrap()),
            Some("implementation"),
        )
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
