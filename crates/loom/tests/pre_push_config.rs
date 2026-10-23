#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    workspace: PathBuf,
    tools: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = root.path().join("workspace");
        let tools = root.path().join("tools");
        let log = root.path().join("pre-push.log");
        std::fs::create_dir_all(workspace.join("bin")).expect("create workspace bin");
        std::fs::create_dir_all(workspace.join("scripts")).expect("create workspace scripts");
        std::fs::create_dir_all(&tools).expect("create tools");

        let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::fs::create_dir_all(workspace.join("tests/verify"))
            .expect("native preflight directory");
        std::fs::copy(
            source_root.join("tests/verify/native-resources.py"),
            workspace.join("tests/verify/native-resources.py"),
        )
        .expect("copy actual native source preflight");
        std::fs::write(workspace.join("flake.nix"), "{ outputs = { self }: {}; }\n")
            .expect("empty native output fixture");
        // These tests exercise the retained Gate hook behavior explicitly,
        // independently of the temporary automatic-stage suspension.
        let config = std::fs::read_to_string(source_root.join(".pre-commit-config.yaml"))
            .expect("read pre-commit config");
        let config = config.replacen("stages: [manual]", "stages: [pre-commit]", 1);
        let config = config.replace("stages: [manual]", "stages: [pre-push]");
        std::fs::write(workspace.join(".pre-commit-config.yaml"), config)
            .expect("write explicitly enabled Gate fixture");
        std::fs::copy(
            source_root.join("bin/pre-push-checks"),
            workspace.join("bin/pre-push-checks"),
        )
        .expect("copy pre-push wrapper");

        for script in ["full-test.sh", "required-test.sh"] {
            std::fs::copy(
                source_root.join("scripts").join(script),
                workspace.join("scripts").join(script),
            )
            .expect("copy test script");
        }

        install_recording_command(&tools, "cargo");
        install_recording_command(&tools, "loom");

        git(&workspace, &["init", "-q", "-b", "main"]);
        git(&workspace, &["config", "user.email", "test@example.com"]);
        git(&workspace, &["config", "user.name", "Test"]);
        git(&workspace, &["config", "commit.gpgsign", "false"]);
        std::fs::write(workspace.join("README.md"), "fixture\n").expect("write seed");
        git(&workspace, &["add", "."]);
        git(&workspace, &["commit", "-q", "-m", "Seed fixture"]);

        Self {
            _root: root,
            workspace,
            tools,
            log,
        }
    }

    fn head(&self) -> String {
        git_stdout(&self.workspace, &["rev-parse", "HEAD"])
    }

    fn commit(&self, path: &str, body: &str, message: &str) -> String {
        let path = self.workspace.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create file parent");
        }
        std::fs::write(&path, body).expect("write committed file");
        let relative = path
            .strip_prefix(&self.workspace)
            .expect("workspace-relative path");
        let output = git_command()
            .arg("-C")
            .arg(&self.workspace)
            .arg("add")
            .arg(relative)
            .output()
            .expect("spawn git add");
        assert_success(&output, "git add");
        git(&self.workspace, &["commit", "-q", "-m", message]);
        self.head()
    }

    fn recording_command(&self, executable: &str) -> Command {
        std::fs::write(&self.log, "").expect("clear invocation log");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut entries = vec![self.tools.clone()];
        entries.extend(std::env::split_paths(&path));
        let path = std::env::join_paths(entries).expect("join PATH");

        let mut command = Command::new(executable);
        loom_test_support::scrub_git_local_env(&mut command);
        command
            .current_dir(&self.workspace)
            .env("PATH", path)
            .env("PRE_PUSH_TEST_LOG", &self.log)
            .env_remove("LOOM_VERIFY_TIERS");
        command
    }

    fn run_hooks_output(&self, hooks: &[&str], from_ref: &str, to_ref: &str) -> Output {
        self.recording_command("prek")
            .args(["run"])
            .args(hooks)
            .args([
                "--stage",
                "pre-push",
                "--from-ref",
                from_ref,
                "--to-ref",
                to_ref,
            ])
            .output()
            .expect("spawn prek")
    }

    fn run_hooks(&self, hooks: &[&str], from_ref: &str, to_ref: &str) -> Vec<String> {
        let output = self.run_hooks_output(hooks, from_ref, to_ref);
        assert_success(&output, "prek pre-push");

        std::fs::read_to_string(&self.log)
            .expect("read invocation log")
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "The external Nix fixture contains literal Bash parameter expansions, not Rust formatting placeholders."
)]
fn write_nix_fixture(path: impl AsRef<Path>, body: &str) -> std::io::Result<()> {
    // Only external Nix metadata is modeled; the real Python preflight runs.
    let prefix = r#"
case "${1:-} ${2:-}" in
  'flake metadata')
    if [[ "$3" == git+file://* ]]; then printf 'native-resource-preflight\ttiers=<unset>\n' >> "$PRE_PUSH_TEST_LOG"; fi
    if [[ "${PRE_PUSH_INVALID_NATIVE_METADATA:-0}" == 1 ]]; then printf '{}\n'; else printf '{"path":"%s","url":"path:%s"}\n' "$PWD" "$PWD"; fi
    exit 0 ;;
  'config show') printf '{"system":{"value":"__SYSTEM__"}}\n'; exit 0 ;;
  'eval --json') [[ "$*" == *'collect "packages" ++ collect "checks"'* ]] || exit 64; printf '[]\n'; exit 0 ;;
esac
"#.replace("__SYSTEM__", &loom_gate::runner::result::current_platform());
    loom_test_support::write_executable_bash_script(path, &(prefix + body))
}

fn install_recording_command(tools: &Path, name: &str) {
    let body = format!(
        "set -euo pipefail\n\
         {{\n\
             printf '{name}'\n\
             for arg in \"$@\"; do\n\
                 printf '\\t%s' \"$arg\"\n\
             done\n\
             if [[ -v LOOM_VERIFY_TIERS ]]; then\n\
                 printf '\\ttiers=%s\\n' \"$LOOM_VERIFY_TIERS\"\n\
             else\n\
                 printf '\\ttiers=<unset>\\n'\n\
             fi\n\
         }} >> \"$PRE_PUSH_TEST_LOG\"\n",
    );
    if name == "nix" {
        write_nix_fixture(tools.join(name), &body).expect("write native metadata recorder");
    } else {
        loom_test_support::write_executable_bash_script(tools.join(name), &body)
            .expect("write recording command");
    }
}

#[test]
fn marker_free_hook_rejects_absent_required_tool_before_skip_wrapper() {
    let fixture = Fixture::new();
    let mut command = fixture.recording_command("bash");
    let output = command
        .args([
            "bin/pre-push-checks",
            "--hook-id",
            "required-tool",
            "--hook-entry",
            "skip-if-missing loom-fixture-tool-not-installed -- true",
            "--",
            "skip-if-missing",
            "loom-fixture-tool-not-installed",
            "--",
            "true",
        ])
        .output()
        .expect("execute actual hook wrapper");
    assert_eq!(
        output.status.code(),
        Some(78),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("coverage remains pending"));
    assert_eq!(
        std::fs::read_to_string(&fixture.log).expect("invocation log"),
        ""
    );
}

#[test]
fn configured_worker_wrapper_initializes_patches_before_cargo() {
    let fixture = Fixture::new();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bin/loom");
    std::fs::copy(source, fixture.workspace.join("bin/loom")).expect("configured wrapper");
    std::fs::write(
        fixture.workspace.join("scripts/configure-cargo-patches.sh"),
        "set -euo pipefail\nprintf 'initialized\\n' > .patches-ready\n",
    )
    .expect("external patch initializer");
    loom_test_support::write_executable_bash_script(fixture.tools.join("cargo"),
        "set -euo pipefail\n[[ \"$(cat .patches-ready)\" == initialized ]]\n[[ \"$1\" == run && \"$2\" == --bin && \"$3\" == loom ]]\nprintf 'cargo-after-patches\\n'\n"
    ).expect("external Cargo");
    let output = fixture
        .recording_command("bash")
        .args(["bin/loom", "--help"])
        .env("LOOM_INSIDE", "1")
        .output()
        .expect("configured worker startup");
    assert_success(&output, "configured worker wrapper");
    assert_eq!(
        String::from_utf8(output.stdout).expect("startup output"),
        "cargo-after-patches\n"
    );
}

fn git(workspace: &Path, args: &[&str]) {
    let output = git_command()
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .expect("spawn git");
    assert_success(&output, &format!("git {}", args.join(" ")));
}

fn git_stdout(workspace: &Path, args: &[&str]) -> String {
    let output = git_command()
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .expect("spawn git");
    assert_success(&output, &format!("git {}", args.join(" ")));
    String::from_utf8(output.stdout)
        .expect("git stdout is UTF-8")
        .trim()
        .to_owned()
}

fn git_command() -> Command {
    let mut command = Command::new("git");
    loom_test_support::scrub_git_local_env(&mut command);
    command
}

fn assert_success(output: &Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// The checker is a process-level hook, so its exit status and diagnostic are
/// observable only by invoking the shipped script against a real file.
fn run_shell_reexec_check(source: &str) -> Output {
    let fixture = tempfile::tempdir().expect("tempdir");
    let script = fixture.path().join("fixture.sh");
    std::fs::write(&script, source).expect("write shell fixture");
    Command::new("bash")
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join("scripts/check-shell-reexec"),
        )
        .arg(&script)
        .output()
        .expect("spawn shell re-exec checker")
}

#[test]
fn automatic_push_temporarily_omits_gate_verify_and_required_suite() {
    let fixture = Fixture::new();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::copy(
        source.join(".pre-commit-config.yaml"),
        fixture.workspace.join(".pre-commit-config.yaml"),
    )
    .expect("use actual suspended hook policy");
    git(&fixture.workspace, &["add", ".pre-commit-config.yaml"]);
    install_recording_command(&fixture.tools, "nix");
    let base = fixture.head();
    let head = fixture.commit("notes.txt", "changed\n", "Change pushed file");
    let lines = fixture.run_hooks(&[], &base, &head);
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("nix\tflake\tcheck"))
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("nix\trun\t.#test-required"))
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("loom\tgate\tverify"))
    );
}

#[test]
fn pre_commit_gate_receives_complete_file_set_in_one_invocation() {
    let fixture = Fixture::new();
    let base = fixture.head();
    let expected: Vec<_> = (0..64)
        .map(|index| format!("subject-{index:02}.rs"))
        .collect();
    for path in &expected {
        std::fs::write(fixture.workspace.join(path), "pub fn subject() {}\n")
            .expect("write changed subject");
    }
    git(&fixture.workspace, &["add", "--all"]);
    git(
        &fixture.workspace,
        &["commit", "-q", "-m", "Add affected subjects"],
    );
    let head = fixture.head();
    let output = fixture
        .recording_command("prek")
        .args([
            "run",
            "loom-gate-verify-files",
            "--stage",
            "pre-commit",
            "--from-ref",
            &base,
            "--to-ref",
            &head,
        ])
        .output()
        .expect("run affected pre-commit gate");
    assert_success(&output, "prek pre-commit");
    let log = std::fs::read_to_string(&fixture.log).expect("read invocation log");
    let lines: Vec<_> = log.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "affected verification must not fan out: {log}"
    );
    let fields: Vec<_> = lines[0].split('\t').collect();
    assert_eq!(&fields[..4], ["loom", "gate", "verify", "--files"]);
    assert_eq!(fields.last(), Some(&"tiers=<unset>"));
    let mut received = fields[4..fields.len() - 1].to_vec();
    received.sort_unstable();
    assert_eq!(received, expected);
}

#[test]
fn pre_push_config_runs_clippy_and_verify_diff_without_loom_verify_tiers() {
    let fixture = Fixture::new();
    let text_base = fixture.head();
    let text_head = fixture.commit("notes.txt", "text only\n", "Add text fixture");

    let text_lines = fixture.run_hooks(
        &[
            "cargo-clippy-production",
            "cargo-clippy",
            "loom-gate-verify-diff",
        ],
        &text_base,
        &text_head,
    );
    assert_eq!(
        text_lines,
        vec![format!(
            "loom\tgate\tverify\t--diff\t{text_base}..{text_head}\ttiers=<unset>"
        )],
        "a non-Rust push must skip clippy while the always-run gate hook stays active",
    );

    let rust_base = text_head;
    let rust_head = fixture.commit("src/lib.rs", "pub fn live() {}\n", "Add Rust fixture");
    let rust_lines = fixture.run_hooks(
        &[
            "cargo-clippy-production",
            "cargo-clippy",
            "loom-gate-verify-diff",
        ],
        &rust_base,
        &rust_head,
    );
    assert_eq!(
        rust_lines.len(),
        3,
        "Rust pushes must check both feature configurations"
    );
    assert_eq!(
        rust_lines[0],
        "cargo\tclippy\t--workspace\t--\t-D\twarnings\ttiers=<unset>",
    );
    assert_eq!(
        rust_lines[1],
        "cargo\tclippy\t--workspace\t--all-targets\t--\t-D\twarnings\ttiers=<unset>",
    );
    assert_eq!(
        rust_lines[2],
        format!("loom\tgate\tverify\t--diff\t{rust_base}..{rust_head}\ttiers=<unset>"),
    );
}

#[test]
fn clippy_hooks_run_for_manifest_and_toolchain_changes() {
    let fixture = Fixture::new();
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "crates/example/Cargo.toml",
        "rust-toolchain.toml",
        "clippy.toml",
    ] {
        let base = fixture.head();
        let head = fixture.commit(
            path,
            "# changed configuration\n",
            "Change Rust configuration",
        );
        let lines = fixture.run_hooks(&["cargo-clippy-production", "cargo-clippy"], &base, &head);
        assert_eq!(
            lines,
            [
                "cargo\tclippy\t--workspace\t--\t-D\twarnings\ttiers=<unset>",
                "cargo\tclippy\t--workspace\t--all-targets\t--\t-D\twarnings\ttiers=<unset>",
            ],
            "{path} must trigger both Clippy configurations",
        );
    }
}

#[test]
fn suites_reject_malformed_native_metadata_before_building_or_claiming_system_credit() {
    for script in ["scripts/full-test.sh", "scripts/required-test.sh"] {
        let fixture = Fixture::new();
        install_recording_command(&fixture.tools, "nix");
        let output = fixture
            .recording_command("bash")
            .arg(script)
            .env("PRE_PUSH_INVALID_NATIVE_METADATA", "1")
            .output()
            .expect("run malformed native metadata control");
        assert!(!output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("KeyError: 'path'"),
            "{output:?}"
        );
        let log = std::fs::read_to_string(&fixture.log).expect("native preflight log");
        assert!(log.contains("native-resource-preflight"));
        assert!(
            !log.contains("nix\tbuild")
                && !log.contains("cargo\t")
                && !log.contains("loom\tgate\tsystem"),
            "{log}"
        );
    }
}

#[test]
fn full_suite_checks_production_and_test_feature_configurations() {
    let fixture = Fixture::new();
    install_recording_command(&fixture.tools, "nix");
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/full-test.sh");
    let output = fixture
        .recording_command("bash")
        .arg(script)
        .output()
        .expect("run full suite");
    assert_success(&output, "full suite");
    let log = std::fs::read_to_string(&fixture.log).expect("invocation log");
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        [
            "nix\tfmt\t--\t--fail-on-change\ttiers=<unset>",
            "native-resource-preflight\ttiers=<unset>",
            "nix\tbuild\ttiers=<unset>",
            "nix\tflake\tcheck\t--no-warn-dirty\ttiers=<unset>",
            "cargo\tbuild\t--workspace\ttiers=<unset>",
            "cargo\tclippy\t--workspace\t--\t-D\twarnings\ttiers=<unset>",
            "cargo\tclippy\t--workspace\t--all-targets\t--\t-D\twarnings\ttiers=<unset>",
            "cargo\tnextest\trun\t--workspace\t--ignore-default-filter\ttiers=<unset>",
            "nix\trun\t.#test-native-units\ttiers=<unset>",
        ],
    );
}

/// Execute the configured stage and real suite script; only external tools are recorded.
#[test]
fn pre_push_stage_runs_each_required_verifier_once_without_repeating_lint_hooks() {
    let fixture = Fixture::new();
    let nix_body = r#"set -euo pipefail
printf 'nix' >> "$PRE_PUSH_TEST_LOG"
for arg in "$@"; do printf '\t%s' "$arg" >> "$PRE_PUSH_TEST_LOG"; done
printf '\ttiers=<unset>\n' >> "$PRE_PUSH_TEST_LOG"
if [[ "$1" == run ]]; then
    case "$2" in
        '.#test') exec bash scripts/full-test.sh ;;
        '.#test-required') exec bash scripts/required-test.sh ;;
        '.#test-native-units') exit 0 ;;
        *) exit 2 ;;
    esac
fi
"#;
    write_nix_fixture(fixture.tools.join("nix"), nix_body).expect("write Nix recorder");
    for (path, expected_clippy) in [("src/lib.rs", 2), ("notes.txt", 0)] {
        let base = fixture.head();
        let head = fixture.commit(path, "changed\n", "Change pushed file");
        let lines = fixture.run_hooks(&[], &base, &head);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("nix\tflake\tcheck"))
                .count(),
            1
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("cargo\tclippy"))
                .count(),
            expected_clippy
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| line
                    .starts_with("cargo\tnextest\trun\t--workspace\t--ignore-default-filter"))
                .count(),
            1
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("loom\tgate\tsystem\t--tree"))
                .count(),
            0
        );
        assert!(lines.contains(&format!(
            "loom\tgate\tverify\t--diff\t{base}..{head}\ttiers=<unset>"
        )));
        assert!(lines.contains(&"nix\trun\t.#test-required\ttiers=<unset>".to_owned()));
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.as_str() == "nix\trun\t.#test-native-units\ttiers=<unset>")
                .count(),
            1,
            "pre-push must retain the native isolation verifier exactly once",
        );
    }
}

/// Real prek and repository policy must stop before launching any later verifier.
#[test]
fn pre_push_stage_stops_after_failed_flake_check() {
    let fixture = Fixture::new();
    write_nix_fixture(
        fixture.tools.join("nix"),
        "set -euo pipefail\nprintf 'nix\\t%s\\n' \"$*\" >> \"$PRE_PUSH_TEST_LOG\"\nexit 1\n",
    )
    .expect("write failing Nix command");
    let base = fixture.head();
    let head = fixture.commit("src/lib.rs", "changed\n", "Change pushed Rust file");

    let output = fixture.run_hooks_output(&[], &base, &head);

    assert_eq!(
        output.status.code(),
        Some(1),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        std::fs::read_to_string(&fixture.log).expect("invocation log"),
        "nix\tflake check\n",
        "a failed fast tier must not launch Clippy, Gate, or the required suite",
    );
}

#[test]
fn required_suite_keeps_all_native_tests_without_repeating_lint_or_nix_work() {
    let fixture = Fixture::new();
    install_recording_command(&fixture.tools, "nix");
    let output = fixture
        .recording_command("bash")
        .arg("scripts/required-test.sh")
        .output()
        .expect("run required suite");
    assert_success(&output, "required suite");
    let log = std::fs::read_to_string(&fixture.log).expect("invocation log");
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        [
            "native-resource-preflight\ttiers=<unset>",
            "cargo\tbuild\t--workspace\ttiers=<unset>",
            "cargo\tnextest\trun\t--workspace\t--ignore-default-filter\ttiers=<unset>",
            "nix\trun\t.#test-native-units\ttiers=<unset>",
        ]
    );
}

#[test]
fn suites_stop_without_system_credit_when_native_isolation_is_unavailable_or_failed() {
    for script in ["scripts/full-test.sh", "scripts/required-test.sh"] {
        for exit in [1, 78] {
            let fixture = Fixture::new();
            write_nix_fixture(
                fixture.tools.join("nix"),
                &format!(
                    "set -euo pipefail\nprintf 'nix' >> \"$PRE_PUSH_TEST_LOG\"\nfor arg in \"$@\"; do printf '\\t%s' \"$arg\" >> \"$PRE_PUSH_TEST_LOG\"; done\nprintf '\\n' >> \"$PRE_PUSH_TEST_LOG\"\nif [[ \"$*\" == 'run .#test-native-units' ]]; then exit {exit}; fi\n"
                ),
            )
            .expect("write native-unit failure control");
            let output = fixture
                .recording_command("bash")
                .arg(script)
                .output()
                .expect("execute real suite with native-unit failure control");
            assert_eq!(output.status.code(), Some(exit), "{script}: {output:?}");
            let log = std::fs::read_to_string(&fixture.log).expect("invocation log");
            assert!(log.contains("cargo\tnextest\trun\t--workspace\t--ignore-default-filter"));
            assert!(
                !log.contains("loom\tgate\tsystem"),
                "{script} must not continue past unavailable or failed native isolation: {log}",
            );
        }
    }
}

#[test]
fn required_suite_retains_real_nextest_failures_despite_quiet_settings() {
    let fixture = Fixture::new();
    install_recording_command(&fixture.tools, "nix");
    std::fs::remove_file(fixture.tools.join("cargo")).expect("use real Cargo and Nextest");
    std::fs::create_dir(fixture.workspace.join("src")).expect("create Rust fixture");
    std::fs::write(
        fixture.workspace.join("Cargo.toml"),
        "[package]\nname = \"required-suite-negative-control\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n",
    )
    .expect("write Cargo manifest");
    std::fs::write(
        fixture.workspace.join("src/lib.rs"),
        r#"#[test]
fn retained_failure_control() {
    println!("retained stdout control");
    eprintln!("retained stderr control");
    assert_eq!(1, 2, "intentional required-suite failure");
}
"#,
    )
    .expect("write failing native test");

    for _ in 0..2 {
        let output = fixture
            .recording_command("bash")
            .arg("scripts/required-test.sh")
            .env("NEXTEST_FAILURE_OUTPUT", "never")
            .env("NEXTEST_STATUS_LEVEL", "none")
            .env("NEXTEST_FINAL_STATUS_LEVEL", "none")
            .env("NEXTEST_SHOW_PROGRESS", "only")
            .output()
            .expect("execute required suite with real failing Nextest");
        assert_eq!(output.status.code(), Some(100), "{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        for marker in [
            "retained_failure_control",
            "retained stdout control",
            "retained stderr control",
        ] {
            assert!(stdout.contains(marker), "{stdout}");
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("runner=100, recorder=0"), "{stderr}");
        let calls = std::fs::read_to_string(&fixture.log).expect("tool invocations");
        assert!(
            !calls.contains("loom"),
            "failed Nextest must stop system dispatch"
        );
    }
    let runs: Vec<_> = std::fs::read_dir(fixture.workspace.join(".loom/logs"))
        .expect("retained runs")
        .map(|entry| entry.expect("run directory").path())
        .collect();
    assert_eq!(
        runs.len(),
        2,
        "a retry must not overwrite the previous failure"
    );
    for run in runs {
        let transcript = std::fs::read_to_string(run.join("nextest.log"))
            .expect("read durable Nextest transcript");
        for marker in [
            "retained_failure_control",
            "retained stdout control",
            "retained stderr control",
        ] {
            assert!(transcript.contains(marker), "{transcript}");
        }
    }
}

#[test]
fn required_suite_fails_closed_when_nextest_transcript_cannot_be_recorded() {
    let fixture = Fixture::new();
    install_recording_command(&fixture.tools, "nix");
    loom_test_support::write_executable_bash_script(
        fixture.tools.join("tee"),
        "set -euo pipefail\nprintf 'recorder failure control\\n' >&2\nexit 1\n",
    )
    .expect("write failing external recorder");
    let output = fixture
        .recording_command("bash")
        .arg("scripts/required-test.sh")
        .output()
        .expect("execute required suite");
    assert_eq!(output.status.code(), Some(78), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("runner=0, recorder=1"), "{stderr}");
    let calls = std::fs::read_to_string(&fixture.log).expect("tool invocations");
    assert!(
        !calls.contains("loom"),
        "missing transcript must stop system dispatch"
    );
}

#[test]
fn pre_push_config_runs_verify_diff_for_pushed_range() {
    let fixture = Fixture::new();
    let pushed_base = fixture.head();

    git(&fixture.workspace, &["checkout", "-q", "-b", "stale"]);
    let stale_tip = fixture.commit("stale.txt", "not pushed\n", "Add stale fixture");
    git(&fixture.workspace, &["checkout", "-q", "main"]);
    let pushed_head = fixture.commit("pushed.txt", "pushed\n", "Add pushed fixture");
    git(
        &fixture.workspace,
        &["branch", "--set-upstream-to", "stale", "main"],
    );
    assert_eq!(
        git_stdout(&fixture.workspace, &["rev-parse", "@{u}"]),
        stale_tip
    );
    assert_ne!(
        stale_tip, pushed_base,
        "upstream must not be the pushed base"
    );

    let lines = fixture.run_hooks(&["loom-gate-verify-diff"], &pushed_base, &pushed_head);
    assert_eq!(
        lines,
        vec![format!(
            "loom\tgate\tverify\t--diff\t{pushed_base}..{pushed_head}\ttiers=<unset>"
        )],
        "the gate hook must use prek's pushed endpoints, not the branch upstream",
    );
}

#[test]
fn shell_reexec_check_accepts_explicit_interpreter() {
    let output = run_shell_reexec_check(
        "#!/usr/bin/env bash\nset -euo pipefail\nexec stdbuf -oL bash \"$0\" \"$@\"\n",
    );
    assert_success(&output, "check-shell-reexec explicit interpreter");
}

#[test]
fn shell_reexec_check_rejects_implicit_self_exec() {
    let output = run_shell_reexec_check(
        "#!/usr/bin/env bash\nset -euo pipefail\nexec stdbuf -oL \"$0\" \"$@\"\n",
    );
    assert!(!output.status.success(), "implicit self-exec must fail");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("explicit shell interpreter"),
        "checker must explain the interpreter requirement: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}
