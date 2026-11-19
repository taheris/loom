//! Platform placement and actual canonical-store reporter conformance.

use super::*;
use std::process::Command;

fn nix_output(root: &Path, arguments: &[&str]) -> String {
    let mut command = Command::new("nix");
    command.current_dir(root).args(arguments);
    loom_test_support::scrub_git_local_env(&mut command);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn copy(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &destination);
        } else {
            std::fs::write(destination, std::fs::read(entry.path()).unwrap()).unwrap();
        }
    }
}

#[test]
fn workspace_store_execution_preserves_platform_and_report_contracts() {
    let workspace = loom_test_support::manifest_dir(env!("CARGO_MANIFEST_DIR")).join("../..");
    let system = loom_gate::runner::result::current_platform();
    let runtime = nix_output(
        &workspace,
        &[
            "eval",
            "--offline",
            "--no-write-lock-file",
            "--raw",
            &format!(".#packages.{system}.loom-tests.loom.runtime"),
        ],
    );
    if !cfg!(target_os = "macos") {
        assert_eq!(runtime, "native-process");
        return;
    }
    assert_eq!(runtime, "native-store");
    let template = std::env::var_os("LOOM_NATIVE_RUST_FIXTURE").map_or_else(
        || {
            PathBuf::from(
                nix_output(
                    &workspace,
                    &[
                        "build",
                        ".#native-rust-fixture",
                        "--offline",
                        "--no-substitute",
                        "--no-link",
                        "--print-out-paths",
                        "--option",
                        "sandbox",
                        "true",
                        "--option",
                        "sandbox-fallback",
                        "false",
                        "--option",
                        "builders",
                        "",
                        "--max-jobs",
                        "1",
                        "--cores",
                        "4",
                    ],
                )
                .trim(),
            )
        },
        PathBuf::from,
    );
    let dir = tempfile::tempdir().unwrap();
    copy(&template, dir.path());
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(
        dir.path(),
        "flake.nix",
        &flake.replace(
            "runtime = \"native-process\";",
            "runtime = if operation == \"test\" then \"native-store\" else \"native-process\";",
        ),
    );
    let source = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
    let control = std::fs::read_to_string(dir.path().join("payload/native-store.rs")).unwrap();
    write(dir.path(), "src/lib.rs", &(source + &control));
    for arguments in [["init", "-q"], ["add", "--all"]] {
        let mut command = Command::new("git");
        command
            .current_dir(dir.path())
            .args(arguments)
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
        loom_test_support::scrub_git_local_env(&mut command);
        assert!(command.status().unwrap().success());
    }
    let (snapshot, inventory) = context(&dir);
    let suites = Definition::native_rust_suites(&snapshot, &inventory).unwrap();
    assert_eq!(suites.len(), 1);
    let suite = Definition::hermetic_baseline(
        &snapshot,
        &inventory,
        name("native_case"),
        suites[0].description().target(),
    )
    .unwrap();
    assert!(matches!(
        suite.description(),
        Candidate::NativeProcess { .. }
    ));
    assert!(
        suite
            .description()
            .capabilities()
            .contains(&Capability::CanonicalNixStore)
    );
    assert!(
        !suite
            .description()
            .capabilities()
            .contains(&Capability::NixSandbox)
    );
    let registry = Registry::new(&snapshot, vec![suite.clone()]).unwrap();
    let origin = path("specs/probe/tests.md");
    let unit = registry
        .resolve(&snapshot, &inventory, suite.description(), origin.clone())
        .unwrap();
    let mut forged = serde_json::to_value(suite.description()).unwrap();
    forged["kind"] = "hermetic-nix".into();
    assert!(
        registry
            .resolve(
                &snapshot,
                &inventory,
                &Candidate::parse(&forged.to_string()).unwrap(),
                origin
            )
            .is_err()
    );
    let first = unit.run().unwrap();
    let second = unit.run().unwrap();
    for observed in [&first, &second] {
        assert!(matches!(observed.outcome(), Outcome::Pass), "{observed:?}");
        // Leave room for nested fixture directories and Darwin Unix socket names.
        assert!(observed.observations().iter().any(|observation| {
            matches!(observation, Observation::NativeReport { directory }
                if directory.parent().is_some_and(|root|
                    root.as_os_str().as_encoded_bytes().len() <= 32))
        }));
        assert!(
            observed
                .observations()
                .iter()
                .any(|observation| matches!(observation, Observation::NativeExecution { .. }))
        );
    }
    assert_ne!(first.observations(), second.observations());
    write(dir.path(), "payload/input.txt", "invalid\n");
    assert!(unit.run().is_err());
    let (snapshot, inventory) = context(&dir);
    let suites = Definition::native_rust_suites(&snapshot, &inventory).unwrap();
    let suite = Definition::hermetic_baseline(
        &snapshot,
        &inventory,
        name("native_case"),
        suites[0].description().target(),
    )
    .unwrap();
    let registry = Registry::new(&snapshot, vec![suite.clone()]).unwrap();
    let unit = registry
        .resolve(
            &snapshot,
            &inventory,
            suite.description(),
            path("specs/probe/tests.md"),
        )
        .unwrap();
    assert!(matches!(
        unit.run().unwrap().outcome(),
        Outcome::Fail { .. }
    ));
}
