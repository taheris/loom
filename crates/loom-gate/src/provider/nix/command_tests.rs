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
