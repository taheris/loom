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
fn paired_partition_evaluation_preserves_order_and_rejects_either_failure() {
    let root = fixture();
    let project = root.path().join("project");
    loom_driver::git::init_test_repo(&project).unwrap();
    let system = crate::runner::result::current_platform();
    let flake = |name: &str, loom: &str| {
        format!(
            r#"{{ outputs = {{ self }}: {{ checks.{system}.probe =
              (builtins.derivation {{ name = "{name}"; system = "{system}"; builder = "/bin/sh"; }})
              // {{ {loom} }};
            }}; }}"#
        )
    };
    std::fs::write(project.join("flake.nix"), flake("probe", "")).unwrap();
    let snapshot = Snapshot::discover(&project).unwrap();
    let inventory = Inventory::discover(&snapshot).unwrap();
    let subject = Name::try_from(format!("nix:check:flake.nix:{system}:probe")).unwrap();
    let targets = std::slice::from_ref(inventory.subjects()[&subject].nix_target().unwrap());
    let tool = locate_tool().unwrap();
    let prepare = |name: &str, loom: &str| {
        let path = root.path().join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("flake.nix"), flake(name, loom)).unwrap();
        read_metadata(&tool, &format!("path:{}", path.display())).unwrap()
    };
    let declaration = "loom = { version = 1; sources = []; evaluation = []; };";
    let first = prepare("paired-first", declaration);
    let second = prepare("paired-second", declaration);
    let invalid = prepare("paired-invalid", "");
    let first = [(&first, targets)];
    let second = [(&second, targets)];
    let invalid = [(&invalid, targets)];
    let evaluated = evaluate_partition_pair(&tool, &first, &second).unwrap();
    assert_eq!(evaluated.len(), 2);
    assert!(evaluated[0][0].root.as_str().ends_with("-paired-first.drv"));
    assert!(
        evaluated[1][0]
            .root
            .as_str()
            .ends_with("-paired-second.drv")
    );
    assert!(evaluate_partition_pair(&tool, &first, &invalid).is_err());
    assert!(evaluate_partition_pair(&tool, &invalid, &second).is_err());
}

#[test]
fn cargo_preparation_must_match_the_reporters_compilation_profile() {
    let mut root = Node {
        builder: String::new(),
        system: Name::try_from("aarch64-linux".to_owned()).unwrap(),
        args: Vec::new(),
        env: BTreeMap::new(),
        structured: None,
        fixed: false,
        sources: BTreeSet::new(),
        dependencies: BTreeMap::new(),
        outputs: BTreeMap::new(),
    };
    for profile in ["dev", "release"] {
        root.env.insert("CARGO_PROFILE".into(), profile.into());
        let mut prepared = root.clone();
        assert!(require_compilation_inputs(&root, &prepared).is_ok());
        prepared.env.insert(
            "CARGO_PROFILE".into(),
            if profile == "dev" { "release" } else { "dev" }.into(),
        );
        assert!(matches!(
            require_compilation_inputs(&root, &prepared),
            Err(Error::CargoExecution)
        ));
    }
}

#[test]
fn actual_catalogue_constructor_requires_owned_feature_graph_and_exact_remaps() {
    let workspace = loom_test_support::manifest_dir(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf();
    let reference = if workspace.join(".git").exists() {
        format!("git+file://{}", workspace.display())
    } else {
        assert!(workspace.starts_with("/nix/store"));
        assert!(!workspace.join("target").exists());
        format!("path:{}", workspace.display())
    };
    let expression = format!(
        "let flake = builtins.getFlake {}; in flake.checks.{}.catalogue-reuse.candidate.drvPath",
        serde_json::to_string(&reference).unwrap(),
        crate::runner::result::current_platform(),
    );
    let tool = locate_tool().unwrap();
    let path: StorePath = serde_json::from_slice(
        &query(
            &tool,
            &["eval", "--json", "--impure", "--expr", &expression],
        )
        .unwrap(),
    )
    .unwrap();
    let nodes = read_graph(&tool, &path).unwrap();
    let root = nodes[&path].as_ref();
    CargoOperation::Catalogue
        .require_execution(root, None)
        .unwrap();
    for (original, replacement) in [
        ("$out/listing-cargo.json", "$out/cargo.json"),
        ("--all-features --filter-platform \"$listing_target\"", ""),
        (".targets[0].platform", ".host.platform"),
        ("--workspace-remap \"$src\"", ""),
        ("--target-dir-remap \"$out/target\"", ""),
        ("--build-dir-remap \"$listing_build_directory\"", ""),
        ("--workspace --all-targets", "--workspace"),
    ] {
        let mut changed = root.clone();
        let phase = changed.env.get_mut("buildPhase").unwrap();
        assert!(phase.contains(original), "{original}");
        *phase = phase.replace(original, replacement);
        assert!(matches!(
            CargoOperation::Catalogue.require_execution(&changed, None),
            Err(Error::CargoExecution)
        ));
    }
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
    std::fs::write(
        root.path().join("project/runtime-marker"),
        "current fixtures",
    )
    .unwrap();
    let scripts = loom_test_support::manifest_dir(env!("CARGO_MANIFEST_DIR")).join("../../nix");
    let search =
        std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&search))).unwrap();
    for profile in ["dev", "release"] {
        let report = root.path().join(format!("report-{profile}"));
        let output = Command::new(&bash)
            .args(["-euc", CargoOperation::Test.command()])
            .env("PATH", &search)
            .env("CARGO_PROFILE", profile)
            .env("cargoArtifacts", root.path().join("catalogue"))
            .env("src", root.path().join("project"))
            .env("LOOM_TEST_SOURCE_ROOT", root.path().join("project"))
            .env("TMPDIR", root.path())
            .env("LOOM_TEST_SCHEDULING", scripts.join("test-scheduling.sh"))
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
                "--cargo-metadata",
                root.path().join("catalogue/cargo.json").to_str().unwrap(),
                "--binaries-metadata",
                root.path()
                    .join("catalogue/binaries.json")
                    .to_str()
                    .unwrap(),
                "--workspace-remap",
                arguments.lines().nth(7).unwrap(),
                "--target-dir-remap",
                root.path().join("catalogue/target").to_str().unwrap(),
                "--ignore-default-filter",
                "--no-fail-fast",
                "--message-format",
                "libtest-json-plus",
            ]
        );
        let runtime = PathBuf::from(arguments.lines().nth(7).unwrap());
        assert!(runtime.starts_with(root.path()));
        assert_ne!(runtime, root.path().join("project"));
        assert_eq!(
            std::fs::read_to_string(runtime.join("runtime-marker")).unwrap(),
            "current fixtures"
        );
        assert_eq!(
            std::fs::read_to_string(report.join("loom-report/status")).unwrap(),
            "0\n"
        );
    }
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
    assert!(removed.contains(std::ffi::OsStr::new("NIX_REMOTE")));
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
fn native_process_policy_is_local_only_and_never_a_hermetic_fallback() {
    let root = fixture();
    let tool = locate_tool().unwrap();
    let local = process::Context::LocalAgent {
        image: Digest::of(b"test-image"),
    };
    for context in [None, Some(&process::Context::Sandboxed), Some(&local)] {
        let mut arguments = vec!["config", "show", "--json"];
        process::policy(context, &mut arguments);
        let mut command = native_command(&tool, &arguments);
        isolated(&mut command, root.path());
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let settings: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        let is_local = context.is_some_and(process::Context::local);
        assert_eq!(settings["sandbox"]["value"], !is_local);
        assert_eq!(settings["sandbox-fallback"]["value"], false);
        assert_eq!(settings["builders"]["value"], "");
        assert_eq!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--store", "local"]),
            is_local
        );
    }
}

#[test]
fn packaged_fixture_registration_retains_compiler_source_and_running_catalogue() {
    if std::env::var_os("LOOM_TEST_NIX_REGISTRATION").is_none_or(|value| value.is_empty()) {
        return;
    }
    let compiler_source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap();
    let executable = StorePath::root(&std::env::current_exe().unwrap()).unwrap();
    let result = native_command(
        &locate_tool().unwrap(),
        &[
            "path-info",
            "--offline",
            "--json",
            "--store",
            "local",
            compiler_source.to_str().unwrap(),
            executable.as_str(),
        ],
    )
    .output()
    .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
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
