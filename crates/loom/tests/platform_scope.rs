#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::{Command, Output};

#[path = "common/native.rs"]
mod native;

fn foreign_systems() -> Vec<&'static str> {
    let current = loom_gate::runner::result::current_platform();
    ["aarch64-linux", "aarch64-darwin", "x86_64-linux"]
        .into_iter()
        .filter(|system| *system != current)
        .take(2)
        .collect()
}

fn fixture() -> tempfile::TempDir {
    let current = loom_gate::runner::result::current_platform();
    let dir = native::fixture(&current);
    // Keep an actual native producer but no annotations: these are independently
    // discovered automatic baselines, as in the Weld flake.lock regression.
    std::fs::remove_file(dir.path().join("specs/native.md")).unwrap();
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    let foreign = foreign_systems()
        .iter()
        .map(|system| {
            format!(
                r#"checks.{system}.cli-boundaries = checked.check {{
                  resources = [ source ];
                  evaluation = map (origin: checked.resource {{ root = ./.; inherit origin; }}) [ "flake.nix" "verification.nix" ];
                  build = sources: builtins.derivation {{
                    name = "foreign-cli-boundaries";
                    system = "{system}";
                    inherit (self.checks.{current}.heading) builder args;
                    src = builtins.head sources;
                  }};
                }};"#
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        dir.path().join("flake.nix"),
        flake
            .replace("in { checks.", &format!("in {{ {foreign}\n checks."))
            .replace(
                "\"verification.nix\" ];",
                "\"verification.nix\" \"flake.lock\" ];",
            ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("flake.lock"),
        "{\"nodes\":{\"root\":{}},\"root\":\"root\",\"version\":7}\n",
    )
    .unwrap();
    dir
}

fn verify(root: &Path, flags: &[&str]) -> Output {
    command(root, flags).output().unwrap()
}

fn command(root: &Path, flags: &[&str]) -> Command {
    let config = root.join(".loom/platform-test.nix.conf");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "extra-platforms =\n").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_loom"));
    command
        .current_dir(root)
        .arg("--workspace")
        .arg(root)
        .args(["gate", "verify"])
        .args(flags)
        .env("NIX_USER_CONF_FILES", config);
    command
}

fn diagnostic(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn native_default_excludes_ownerless_foreign_baselines_without_claiming_passes() {
    let dir = fixture();
    for flags in [
        vec!["--files", "flake.lock"],
        vec!["--platform", "native", "--files", "flake.lock"],
        vec!["--tree"],
    ] {
        let output = verify(dir.path(), &flags);
        let stderr = diagnostic(&output);
        assert!(output.status.success(), "{stderr}");
        assert!(stderr.contains("\"platform\":\"native\""), "{stderr}");
        assert!(stderr.contains("\"verdict\":\"pass\""), "{stderr}");
        assert!(stderr.contains("outside-platform-scope"), "{stderr}");
        assert!(
            stderr.contains("\"unverified_foreign\":[\"nix build"),
            "{stderr}"
        );
        for system in foreign_systems() {
            assert!(
                stderr.contains(&format!("checks.{system}.cli-boundaries")),
                "{stderr}"
            );
            assert!(
                !stderr
                    .lines()
                    .any(|line| { line.starts_with("loom gate [") && line.contains(system) }),
                "foreign runtime was observed: {stderr}"
            );
        }
        assert!(!dir.path().join(".loom/marker.json").exists());
    }
}

#[test]
fn upstream_lockfile_diff_uses_native_platform_by_default() {
    let dir = fixture();
    std::fs::write(
        dir.path().join(".pre-commit-config.yaml"),
        "repos:\n  - repo: local\n    hooks:\n      - id: lockfile-present\n        name: Validate actual lockfile presence\n        language: system\n        entry: bash -c 'test -s flake.lock'\n        pass_filenames: false\n",
    ).unwrap();
    let git = |args: &[&str]| {
        let mut command = Command::new("git");
        loom_test_support::scrub_git_local_env(&mut command);
        let output = command
            .current_dir(dir.path())
            .args([
                "-c",
                "user.name=Platform fixture",
                "-c",
                "user.email=platform@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", diagnostic(&output));
    };
    git(&["add", "."]);
    git(&["commit", "-qm", "Seal platform fixture"]);
    git(&["branch", "-M", "platform-base"]);
    git(&["checkout", "-qb", "platform-change"]);
    git(&["branch", "--set-upstream-to=platform-base"]);
    let lock = dir.path().join("flake.lock");
    let source = std::fs::read_to_string(&lock).unwrap();
    std::fs::write(lock, format!("{source}\n")).unwrap();
    git(&["add", "flake.lock"]);
    git(&["commit", "-qm", "Change lockfile"]);
    let output = verify(dir.path(), &["--diff", "@{upstream}..HEAD"]);
    let stderr = diagnostic(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("\"platform\":\"native\""), "{stderr}");
    assert!(
        stderr.contains("\"unverified_foreign\":[\"nix build"),
        "{stderr}"
    );
    assert!(stderr.contains("\"verdict\":\"pass\""), "{stderr}");
    assert!(!stderr.contains("\"verdict\":\"inapplicable\""), "{stderr}");
}

#[test]
fn all_platform_request_retains_nonpassing_foreign_applicability() {
    let dir = fixture();
    let output = verify(dir.path(), &["--platform", "all", "--tree"]);
    let stderr = diagnostic(&output);
    assert_eq!(output.status.code(), Some(78), "{stderr}");
    assert!(stderr.contains("\"platform\":\"all\""), "{stderr}");
    assert!(stderr.contains("\"verdict\":\"inapplicable\""), "{stderr}");
    assert!(!stderr.contains("outside-platform-scope"), "{stderr}");
}

#[test]
fn explicit_foreign_owner_is_not_erased_by_native_default() {
    let dir = fixture();
    let foreign = foreign_systems()[0];
    std::fs::write(
        dir.path().join("specs/native.md"),
        format!("## Success Criteria\n- Explicit foreign duty [system](nix build .#checks.{foreign}.cli-boundaries)\n"),
    )
    .unwrap();
    let output = verify(dir.path(), &["--tree"]);
    let stderr = diagnostic(&output);
    assert_eq!(output.status.code(), Some(78), "{stderr}");
    assert!(stderr.contains("\"verdict\":\"inapplicable\""), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "\"target\":\"nix build .#checks.{foreign}.cli-boundaries\""
        )),
        "{stderr}"
    );
}

#[test]
fn advertised_foreign_capability_does_not_prepare_foreign_graphs_in_native_mode() {
    // On x86 Linux this advertises ARM, regardless of actual emulator availability.
    // Other hosts exercise the same boundary without silently passing an empty test.
    let foreign = foreign_systems()[0];
    let dir = fixture();
    let path = dir.path().join("flake.nix");
    let flake = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        flake.replace(
            &format!("checks.{foreign}.cli-boundaries = checked.check"),
            &format!("checks.{foreign}.cli-boundaries = throw \"foreign-graph-preparation-requires-explicit-request\"; unused-foreign-construction = checked.check"),
        ),
    )
    .unwrap();
    // The provider deliberately strips NIX_CONFIG. Exercise actual backend
    // configuration without changing the SDK's isolated store or sandbox policy.
    let settings = tempfile::tempdir().unwrap();
    let config = settings.path().join("nix.conf");
    std::fs::write(&config, format!("extra-platforms = {foreign}\n")).unwrap();
    let advertised = Command::new("nix")
        .args(["config", "show", "--json"])
        .env_remove("NIX_CONFIG")
        .env("NIX_USER_CONF_FILES", &config)
        .output()
        .unwrap();
    assert!(advertised.status.success(), "{}", diagnostic(&advertised));
    let advertised: serde_json::Value = serde_json::from_slice(&advertised.stdout).unwrap();
    assert!(
        advertised["extra-platforms"]["value"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == foreign)
    );
    let output = command(dir.path(), &["--tree"])
        .env("NIX_USER_CONF_FILES", &config)
        .output()
        .unwrap();
    let stderr = diagnostic(&output);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!("checks.{foreign}.cli-boundaries")),
        "{stderr}"
    );
    assert!(
        !stderr.contains("foreign-graph-preparation-requires-explicit-request"),
        "{stderr}"
    );
    // Manual all-platform selection really does enter foreign construction.
    // The evaluator sentinel stops it before any foreign build or CPU execution.
    let output = command(dir.path(), &["--platform", "all", "--tree"])
        .env("NIX_USER_CONF_FILES", config)
        .output()
        .unwrap();
    let stderr = diagnostic(&output);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("foreign-graph-preparation-requires-explicit-request"),
        "{stderr}"
    );
}

#[test]
fn native_failure_is_not_masked_by_excluded_foreign_baselines() {
    let dir = fixture();
    std::fs::write(dir.path().join("README.md"), "not a heading\n").unwrap();
    let output = verify(dir.path(), &["--tree"]);
    let stderr = diagnostic(&output);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("\"verdict\":\"fail\""), "{stderr}");
    assert!(stderr.contains("\"unverified_foreign\""), "{stderr}");
}

#[test]
fn invalid_platform_policy_is_not_a_native_fallback() {
    let output = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["gate", "verify", "--platform", "aarch64-linux", "--tree"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(diagnostic(&output).contains("use native (default) or all"));
}
