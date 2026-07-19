//! End-to-end `loom tune` CLI surface tests.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn find_bash() -> PathBuf {
    let path_var = std::env::var_os("PATH").expect("PATH");
    std::env::split_paths(&path_var)
        .map(|directory| directory.join("bash"))
        .find(|candidate| candidate.is_file())
        .expect("bash on PATH")
}

fn mock_pi_path() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .map(|ancestor| ancestor.join("tests/mock-pi/pi.sh"))
        .find(|candidate| candidate.is_file())
        .expect("mock pi fixture")
}

fn install_tune_review_wrix(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let shim = root.join("tune-review-wrix");
    let count = root.join("tune-review-count");
    let bash = find_bash();
    let mock = mock_pi_path();
    write_file(
        &shim,
        &format!(
            "#!{}\nset -euo pipefail\nprintf 'x\\n' >> '{}'\necho '[wrix] Starting container (mock)...' >&2\nexec '{}' '{}' tune-review\n",
            bash.display(),
            count.display(),
            bash.display(),
            mock.display(),
        ),
    );
    let mut permissions = std::fs::metadata(&shim)
        .expect("stat wrix shim")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&shim, permissions).expect("chmod wrix shim");

    let profile_config = root.join("profile-config.json");
    write_file(&profile_config, "{}\n");
    let manifest = root.join("profile-images.json");
    let body = serde_json::json!({
        "base": {
            "pi": {
                "ref": "localhost/wrix-base-pi:tune-test",
                "source": "/nix/store/tune-test-image",
                "source_kind": "nix-descriptor",
                "launcher": shim,
                "profile_config": profile_config,
            }
        }
    });
    write_file(
        &manifest,
        &format!(
            "{}\n",
            serde_json::to_string_pretty(&body).expect("manifest json")
        ),
    );
    (shim, manifest, count)
}

fn git_command() -> Command {
    let mut command = Command::new("git");
    loom_test_support::scrub_git_local_env(&mut command);
    loom_test_support::configure_hermetic_git(&mut command);
    command
}

fn install_bd_shim(dir: &Path) -> PathBuf {
    let bin_dir = dir.join("bd-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bd-bin");
    let bd_path = bin_dir.join("bd");
    let source = PathBuf::from(env!("CARGO_BIN_EXE_bd-shim"));
    match std::os::unix::fs::symlink(&source, &bd_path) {
        Ok(()) => {}
        Err(_) => {
            std::fs::copy(&source, &bd_path).expect("copy bd-shim into bin dir");
            let mut perm = std::fs::metadata(&bd_path).expect("stat bd").permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&bd_path, perm).expect("chmod bd");
        }
    }
    bin_dir
}

fn install_template_validation_cargo(bin_dir: &Path, log: &Path) {
    let cargo = bin_dir.join("cargo");
    let bash = find_bash();
    write_file(
        &cargo,
        &format!(
            "#!{}\nset -euo pipefail\nprintf '%s|%s\\n' \"$PWD\" \"$*\" >> '{}'\nif [[ \"${{FAIL_TEMPLATE_CHECK-}}\" == \"representative-renders\" && \"${{1-}}\" == \"test\" ]]; then\n  echo 'representative render failure' >&2\n  exit 42\nfi\n",
            bash.display(),
            log.display(),
        ),
    );
    let mut permissions = std::fs::metadata(&cargo)
        .expect("stat cargo shim")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&cargo, permissions).expect("chmod cargo shim");
}

fn init_workspace(root: &Path) {
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Tune Test"]);
    git(root, &["config", "user.email", "tune-test@example.invalid"]);
    std::fs::write(
        root.join(".gitignore"),
        ".loom/\n.loom-state/\nbd-bin/\nbd-state/\n",
    )
    .expect("write gitignore");
    std::fs::write(root.join("README.md"), "# Tune fixture\n").expect("write readme");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "seed workspace"]);
}

fn write_file(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, body).expect("write file");
}

fn git(root: &Path, args: &[&str]) {
    let output = git_command()
        .args(args)
        .current_dir(root)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn git_stdout(root: &Path, args: &[&str]) -> String {
    let output = git_command()
        .args(args)
        .current_dir(root)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("utf-8")
}

fn run_loom(workspace: &Path, bin_dir: &Path, state_dir: &Path, args: &[&str]) -> Output {
    run_loom_with_env(workspace, bin_dir, state_dir, args, &[])
}

fn run_loom_with_env(
    workspace: &Path,
    bin_dir: &Path,
    state_dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Output {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut entries = vec![bin_dir.to_path_buf()];
    entries.extend(std::env::split_paths(&path_var));
    let new_path = std::env::join_paths(entries).expect("join PATH");
    let mut command = Command::new(env!("CARGO_BIN_EXE_loom"));
    loom_test_support::configure_hermetic_git(&mut command);
    command
        .arg("--workspace")
        .arg(workspace)
        .args(args)
        .env("PATH", new_path)
        .env("BD_STATE_DIR", state_dir)
        .env("XDG_STATE_HOME", state_dir.join("state-home"))
        .env_remove("LOOM_CONFIG")
        .env_remove("LOOM_INSIDE");
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().expect("spawn loom")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("utf-8 stdout")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("utf-8 stderr")
}

fn assert_success(output: &Output, args: &[&str]) {
    assert!(
        output.status.success(),
        "loom {args:?} failed\nstdout={}\nstderr={}",
        stdout(output),
        stderr(output),
    );
}

fn assert_failure(output: &Output, args: &[&str]) {
    assert!(
        !output.status.success(),
        "loom {args:?} unexpectedly succeeded\nstdout={}\nstderr={}",
        stdout(output),
        stderr(output),
    );
}

#[test]
fn loom_tune_bare_prints_help_without_proposal() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    init_workspace(tmp.path());
    let bin_dir = install_bd_shim(tmp.path());
    let state_dir = tmp.path().join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir state");

    let output = run_loom(tmp.path(), &bin_dir, &state_dir, &["tune"]);

    assert_success(&output, &["tune"]);
    let out = stdout(&output);
    assert!(out.contains("Usage: loom tune"), "{out}");
    assert!(out.contains("skill"), "{out}");
    assert!(!tmp.path().join(".loom/tune").exists());
}

#[test]
fn loom_tune_listings_are_read_only() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    init_workspace(tmp.path());
    let bin_dir = install_bd_shim(tmp.path());
    let state_dir = tmp.path().join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir state");

    for args in [
        vec!["tune", "skill"],
        vec!["tune", "phase"],
        vec!["tune", "partial"],
        vec!["tune", "checker"],
        vec!["tune", "all"],
    ] {
        let output = run_loom(tmp.path(), &bin_dir, &state_dir, &args);
        assert_success(&output, &args);
    }

    assert!(!tmp.path().join(".loom/tune").exists());
    assert_eq!(git_stdout(tmp.path(), &["status", "--short"]), "");
}

#[test]
fn loom_tune_cli_surface() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    init_workspace(tmp.path());
    let bin_dir = install_bd_shim(tmp.path());
    let state_dir = tmp.path().join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir state");

    for args in [
        vec!["tune", "skills"],
        vec!["tune", "template"],
        vec!["tune", "skill", "slow"],
        vec!["tune", "skill", "--dry-run"],
        vec!["tune", "checker", "--seed", "7"],
        vec!["tune", "all", "fast", "extra-target"],
    ] {
        let output = run_loom(tmp.path(), &bin_dir, &state_dir, &args);
        assert_failure(&output, &args);
    }
}

#[test]
fn loom_tune_level_seed_dry_run_shape_plan() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    init_workspace(tmp.path());
    let bin_dir = install_bd_shim(tmp.path());
    let state_dir = tmp.path().join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir state");

    let args = [
        "tune",
        "skill",
        "fast",
        "--dry-run",
        "--seed",
        "42",
        "loom-inbox-resolution",
    ];
    let output = run_loom(tmp.path(), &bin_dir, &state_dir, &args);

    assert_success(&output, &args);
    let out = stdout(&output);
    for needle in [
        "loom tune dry-run",
        "loaded tuning docs:",
        "evidence roots:",
        "seed: 42",
        "case pool:",
        "selected cases:",
        "skipped cases:",
        "frozen checker plan:",
        "candidate generation: skipped",
    ] {
        assert!(out.contains(needle), "missing {needle}: {out}");
    }
    assert!(!tmp.path().join(".loom/tune").exists());
}

#[test]
fn package_tuning_docs_load_only_for_tuned_skill_target() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    init_workspace(tmp.path());
    let bin_dir = install_bd_shim(tmp.path());
    let state_dir = tmp.path().join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir state");
    write_file(
        &tmp.path().join("skills/target/skill.md"),
        "---\nname: repo-target\ndescription: Use when tuning the selected package skill.\n---\nBody\n",
    );
    write_file(
        &tmp.path().join("skills/other/skill.md"),
        "---\nname: repo-other\ndescription: Use when testing non-target package tuning isolation.\n---\nBody\n",
    );
    write_file(
        &tmp.path().join("skills/other/tuning.md"),
        r#"```loom-case
id = "other-package-case"
checker = "behavior.review.finding-recall"
targets = ["skill:repo-target"]
```
"#,
    );
    git(tmp.path(), &["add", "."]);
    git(tmp.path(), &["commit", "-q", "-m", "add package skills"]);

    let args = ["tune", "skill", "fast", "--dry-run", "repo-target"];
    let output = run_loom(tmp.path(), &bin_dir, &state_dir, &args);

    assert_success(&output, &args);
    let out = stdout(&output);
    assert!(!out.contains("skills/other/tuning.md"), "{out}");
}

#[test]
fn skill_tune_evidence_roots_and_gate() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let external = tempfile::tempdir().expect("external evidence root");
    init_workspace(tmp.path());
    let bin_dir = install_bd_shim(tmp.path());
    let state_dir = tmp.path().join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir state");
    write_file(
        &external.path().join("agent-session.jsonl"),
        "{\"type\":\"review\",\"result\":\"external correction\"}\n",
    );
    write_file(
        &tmp.path().join(".loom/logs/skills/review.jsonl"),
        "{\"type\":\"review\",\"result\":\"workspace finding\"}\n",
    );
    write_file(
        &tmp.path().join("loom.toml"),
        &format!(
            "[phase.gate.review]\nagent.backend = \"pi\"\n\n[tune.checks]\nmax_behavior_cases = 1\n\n[tune.evidence]\nexternal_roots = [\"{}\"]\n",
            external.path().display(),
        ),
    );
    write_file(
        &tmp.path().join("docs/tuning.md"),
        r#"# Fixture tuning guidance

Use concrete review inputs when evaluating candidate guidance.

```loom-case
id = "review-recall-case"
checker = "behavior.review.finding-recall"
targets = ["skill:loom-context-before-edit"]

[input]
patch = "cases/review.diff"

[expected]
max_extra_findings = 1

[[expected.findings]]
contains = ["missing test from replay input"]
```
"#,
    );
    write_file(
        &tmp.path().join("docs/cases/review.diff"),
        "diff --git a/src/lib.rs b/src/lib.rs\n+TUNE_REVIEW_INPUT_CANARY\n",
    );
    git(tmp.path(), &["add", "."]);
    git(tmp.path(), &["commit", "-q", "-m", "add tuning case"]);
    let (_wrix, profile_manifest, replay_count) = install_tune_review_wrix(tmp.path());
    let manifest_path = profile_manifest.to_string_lossy();

    let dry_args = [
        "tune",
        "skill",
        "run",
        "--dry-run",
        "--seed",
        "7",
        "loom-context-before-edit",
    ];
    let dry = run_loom_with_env(
        tmp.path(),
        &bin_dir,
        &state_dir,
        &dry_args,
        &[("LOOM_PROFILES_MANIFEST", manifest_path.as_ref())],
    );
    assert_success(&dry, &dry_args);
    let dry_out = stdout(&dry);
    let roots_at = dry_out.find("evidence roots:").expect("root report");
    let plan_at = dry_out.find("loom tune dry-run").expect("dry-run report");
    assert!(
        roots_at < plan_at,
        "roots must print before harvesting: {dry_out}"
    );
    assert!(dry_out.contains("workspace:"), "{dry_out}");
    assert!(
        dry_out.contains(&format!("external: {}", external.path().display())),
        "{dry_out}"
    );
    assert!(!dry_out.contains(".claude"), "{dry_out}");
    assert!(dry_out.contains("evidence split:"), "{dry_out}");
    assert!(dry_out.contains("salt id: repo-sha256-v1:"), "{dry_out}");
    assert!(!dry_out.contains("salt material"), "{dry_out}");
    assert!(dry_out.contains("declared:review-recall-case"), "{dry_out}");

    let args = [
        "tune",
        "skill",
        "run",
        "--seed",
        "7",
        "loom-context-before-edit",
    ];
    let output = run_loom_with_env(
        tmp.path(),
        &bin_dir,
        &state_dir,
        &args,
        &[
            ("BD_CREATE_ID", "lm-tune.2"),
            ("LOOM_PROFILES_MANIFEST", manifest_path.as_ref()),
        ],
    );

    assert_success(&output, &args);
    assert_eq!(
        std::fs::read_to_string(&replay_count)
            .expect("replay count")
            .lines()
            .count(),
        2,
        "current and candidate must each run through the real Pi spawn path",
    );
    let envelope = tmp.path().join(".loom/tune/lm-tune.2");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(envelope.join("manifest.json")).expect("read manifest"),
    )
    .expect("manifest json");
    assert_eq!(manifest["state"], "pending");
    assert_eq!(manifest["evidence_split"]["algorithm"], "sha256-salt-v1");
    assert!(
        manifest["evidence_split"]["salt_id"]
            .as_str()
            .expect("salt id")
            .starts_with("repo-sha256-v1:"),
        "evidence_split={}",
        manifest["evidence_split"],
    );
    assert_eq!(
        manifest["evidence_split"]["selection_fraction"],
        serde_json::json!(0.34),
    );
    let case_counts = &manifest["case_counts"];
    let mined = case_counts["mined_train"].as_u64().expect("train count")
        + case_counts["mined_selection"]
            .as_u64()
            .expect("selection count");
    assert_eq!(
        mined, 3,
        "workspace log, tuning doc, and external transcript"
    );
    assert!(
        manifest["validation"]
            .as_array()
            .expect("validation rows")
            .iter()
            .any(|row| row["check"] == "behavioral-cases"
                && row["status"] == "passed"
                && row["detail"]
                    .as_str()
                    .expect("detail")
                    .contains("selected behavioral case(s) evaluated")),
        "validation={}",
        manifest["validation"],
    );
    let candidate = std::fs::read_to_string(
        envelope.join("repo/.loom-override/skills/loom-context-before-edit/skill.md"),
    )
    .expect("candidate skill");
    assert!(candidate.contains("concrete review inputs"), "{candidate}");
    assert!(
        !candidate.contains("missing test from replay input"),
        "candidate generation must not copy held-out expected predicates: {candidate}",
    );
}

#[test]
fn template_tune_candidate_validation() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    init_workspace(tmp.path());
    write_file(
        &tmp.path().join("crates/loom-templates/Cargo.toml"),
        "[package]\nname = \"loom-templates\"\nversion = \"0.0.0\"\n",
    );
    write_file(
        &tmp.path().join("crates/loom-templates/templates/loop.md"),
        "# Loop fixture\n",
    );
    write_file(
        &tmp.path()
            .join("crates/loom-templates/templates/partial/skill_index.md"),
        "# Skill index fixture\n",
    );
    git(tmp.path(), &["add", "."]);
    git(tmp.path(), &["commit", "-q", "-m", "add template fixture"]);
    let bin_dir = install_bd_shim(tmp.path());
    let state_dir = tmp.path().join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir state");
    let cargo_log = state_dir.join("cargo-invocations.log");
    install_template_validation_cargo(&bin_dir, &cargo_log);

    let passing_args = ["tune", "phase", "fast", "--seed", "7", "loop"];
    let passing = run_loom_with_env(
        tmp.path(),
        &bin_dir,
        &state_dir,
        &passing_args,
        &[("BD_CREATE_ID", "lm-tune.3")],
    );
    assert_success(&passing, &passing_args);
    let passing_manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(".loom/tune/lm-tune.3/manifest.json"))
            .expect("passing manifest"),
    )
    .expect("passing manifest json");
    assert_eq!(passing_manifest["state"], "pending");

    let failing_args = ["tune", "partial", "fast", "--seed", "8", "skill_index"];
    let failing = run_loom_with_env(
        tmp.path(),
        &bin_dir,
        &state_dir,
        &failing_args,
        &[
            ("BD_CREATE_ID", "lm-tune.4"),
            ("FAIL_TEMPLATE_CHECK", "representative-renders"),
        ],
    );
    assert_success(&failing, &failing_args);
    let failing_manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(".loom/tune/lm-tune.4/manifest.json"))
            .expect("failing manifest"),
    )
    .expect("failing manifest json");
    assert_eq!(failing_manifest["state"], "blocked");
    assert_eq!(
        std::fs::read_to_string(state_dir.join("lm-tune.4/status"))
            .expect("blocked bead status")
            .trim(),
        "blocked",
    );

    let cargo_invocations = std::fs::read_to_string(&cargo_log).expect("cargo invocations");
    let lines = cargo_invocations.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 6, "{cargo_invocations}");
    let expected_commands = [
        "check -p loom-templates --quiet",
        "test -p loom-templates --test snapshots --quiet",
        "run -p loom-walk --quiet -- template_pinning_matrix template_wire_format_restatement templates_no_removed_surface",
    ];
    for (proposal_id, chunk) in ["lm-tune.3", "lm-tune.4"]
        .into_iter()
        .zip(lines.chunks_exact(3))
    {
        let proposal_repo = tmp.path().join(".loom/tune").join(proposal_id).join("repo");
        for (line, expected) in chunk.iter().zip(expected_commands) {
            assert_eq!(*line, format!("{}|{expected}", proposal_repo.display()),);
        }
    }

    let bd_invocations =
        std::fs::read_to_string(state_dir.join(".invocations.log")).expect("bd invocations");
    for line in bd_invocations
        .lines()
        .filter(|line| line.starts_with("create "))
    {
        assert!(!line.contains("loom:tune"), "{line}");
    }
    for (proposal_id, status) in [("lm-tune.3", "open"), ("lm-tune.4", "blocked")] {
        let publication =
            format!("update {proposal_id} --status {status} --add-label loom:tune --description");
        assert!(bd_invocations.contains(&publication), "{bd_invocations}");
    }
}

#[test]
fn loom_tune_subcommands_create_isolated_proposals() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    init_workspace(tmp.path());
    let bin_dir = install_bd_shim(tmp.path());
    let state_dir = tmp.path().join("bd-state");
    std::fs::create_dir_all(&state_dir).expect("mkdir state");

    let args = [
        "tune",
        "skill",
        "fast",
        "--seed",
        "7",
        "loom-inbox-resolution",
    ];
    let output = run_loom_with_env(
        tmp.path(),
        &bin_dir,
        &state_dir,
        &args,
        &[("BD_CREATE_ID", "lm-tune.1")],
    );

    assert_success(&output, &args);
    let out = stdout(&output);
    assert!(
        out.contains("loom tune proposal created: lm-tune.1"),
        "{out}"
    );
    let envelope = tmp.path().join(".loom/tune/lm-tune.1");
    for path in ["repo", "logs", "evidence"] {
        assert!(envelope.join(path).is_dir(), "missing dir {path}");
    }
    for path in ["manifest.json", "evidence.md"] {
        assert!(envelope.join(path).is_file(), "missing file {path}");
    }

    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(envelope.join("manifest.json")).expect("read manifest"),
    )
    .expect("manifest json");
    assert_eq!(manifest["proposal_id"], "lm-tune.1");
    assert_eq!(manifest["state"], "pending");
    assert_eq!(manifest["level"], "fast");
    assert_eq!(manifest["seed"], 7);
    assert_eq!(
        manifest["targets"],
        serde_json::json!(["skill:loom-inbox-resolution"])
    );
    assert!(
        manifest["target_files"]
            .as_array()
            .expect("target files")
            .iter()
            .any(|path| path == ".loom-override/skills/loom-inbox-resolution/skill.md"),
        "manifest target_files={}",
        manifest["target_files"],
    );

    let metadata: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(state_dir.join("lm-tune.1/metadata.json")).expect("metadata"),
    )
    .expect("metadata json");
    assert_eq!(metadata["loom.tune.id"], "lm-tune.1");
    assert_eq!(metadata["loom.tune.state"], "pending");
    assert_eq!(metadata["loom.tune.level"], "fast");
    assert_eq!(metadata["loom.tune.seed"], 7);
    assert_eq!(
        metadata["loom.tune.targets"],
        serde_json::json!(["skill:loom-inbox-resolution"])
    );

    let labels = std::fs::read_to_string(state_dir.join("lm-tune.1/labels")).expect("labels");
    assert!(labels.contains("loom:tune"), "{labels}");
    assert!(labels.contains("spec:skills"), "{labels}");
    let body = std::fs::read_to_string(state_dir.join("lm-tune.1/description")).expect("body");
    assert!(body.contains("State: `pending`"), "{body}");
    assert!(body.contains("Proposal repo:"), "{body}");

    let base = git_stdout(tmp.path(), &["rev-parse", "HEAD"]);
    let head = git_stdout(&envelope.join("repo"), &["rev-parse", "HEAD"]);
    assert_ne!(base.trim(), head.trim());
    assert_eq!(
        git_stdout(tmp.path(), &["status", "--short", "--untracked-files=all"]),
        ""
    );
}
