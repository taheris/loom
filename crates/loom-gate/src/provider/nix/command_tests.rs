#![cfg(test)]

use super::*;

fn isolated(command: &mut Command, root: &Path) {
    command
        .env("HOME", root.join("home"))
        .env("NIX_CONF_DIR", root.join("conf"))
        .env("XDG_CONFIG_HOME", root.join("xdg"));
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for directory in ["home", "conf", "xdg", "project"] {
        std::fs::create_dir(root.path().join(directory)).unwrap();
    }
    root
}

#[test]
fn canonical_nextest_command_uses_scheduler_default_without_narrowing_test_scope() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = fixture();
    let bin = root.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let cargo = bin.join("cargo");
    let search = std::env::var_os("PATH").unwrap();
    let bash = std::env::split_paths(&search)
        .map(|directory| directory.join("bash"))
        .find(|program| program.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    std::fs::write(
        &cargo,
        format!("#!{}\nprintf '%s\\n' \"$@\"\n", bash.display()),
    )
    .unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o700)).unwrap();
    let scripts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../nix");
    let report = root.path().join("report");
    let search =
        std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&search))).unwrap();
    let output = Command::new(bash)
        .args(["-euc", CargoOperation::Test.command()])
        .env("PATH", search)
        .env("CARGO_PROFILE", "release")
        .env(
            "LOOM_PRIVATE_FIXTURE_RUNNER",
            scripts.join("private-fixture-tests.sh"),
        )
        .env(
            "LOOM_REPORT_SUPERVISOR",
            scripts.join("report-supervisor.sh"),
        )
        .env("out", &report)
        .env_remove("LOOM_NATIVE_REPORT")
        .env_remove("LOOM_TEST_NIX_REGISTRATION")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let arguments = std::fs::read_to_string(report.join("loom-report/stdout")).unwrap();
    assert_eq!(
        arguments.lines().collect::<Vec<_>>(),
        [
            "nextest",
            "run",
            "--offline",
            "--locked",
            "--workspace",
            "--all-targets",
            "--cargo-profile",
            "release",
            "--ignore-default-filter",
            "--no-fail-fast",
            "--message-format",
            "libtest-json-plus",
        ]
    );
    assert_eq!(
        std::fs::read_to_string(report.join("loom-report/status")).unwrap(),
        "0\n"
    );
}

#[test]
fn native_adapter_declares_features_after_removing_ambient_config_without_relaxing_policy() {
    let root = fixture();
    let tool = locate_tool().unwrap();
    let mut command = native_command(
        &tool,
        &[
            "config",
            "show",
            "--json",
            "--option",
            "sandbox",
            "true",
            "--option",
            "sandbox-fallback",
            "false",
            "--option",
            "allow-import-from-derivation",
            "false",
        ],
    );
    let removed: BTreeSet<_> = command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(name, _)| name.to_owned())
        .collect();
    assert!(removed.contains(std::ffi::OsStr::new("NIX_CONFIG")));
    assert!(removed.contains(std::ffi::OsStr::new("NIX_PATH")));
    isolated(&mut command, root.path());
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let settings: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let features = settings["experimental-features"]["value"]
        .as_array()
        .unwrap();
    assert!(features.contains(&serde_json::json!("nix-command")));
    assert!(features.contains(&serde_json::json!("flakes")));
    assert_eq!(settings["sandbox"]["value"], true);
    assert_eq!(settings["sandbox-fallback"]["value"], false);
    assert_eq!(settings["allow-import-from-derivation"]["value"], false);
    assert_eq!(settings["accept-flake-config"]["value"], false);
}

#[test]
fn native_adapter_evaluates_a_real_offline_flake_without_user_or_system_feature_config() {
    let root = fixture();
    std::fs::write(
        root.path().join("project/flake.nix"),
        "{ outputs = { self }: { fixtureValue = \"checked-native-cli-ready\"; }; }\n",
    )
    .unwrap();
    let reference = format!(
        "path:{}#fixtureValue",
        root.path().join("project").display()
    );
    let mut command = native_command(
        &locate_tool().unwrap(),
        &[
            "eval",
            "--offline",
            "--raw",
            "--no-write-lock-file",
            &reference,
            "--option",
            "allow-import-from-derivation",
            "false",
        ],
    );
    isolated(&mut command, root.path());
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"checked-native-cli-ready");
}
