//! Canonical-store software scenarios remain part of the complete native Cargo owner.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::Context;

const POLICY: &str = "experimental-features = nix-command flakes\nsandbox = true\nsandbox-fallback = false\nsandbox-paths =\nextra-sandbox-paths =\nrequire-sigs = true\nsubstitute = false\nbuilders =\nallow-import-from-derivation = false\nmax-jobs = 1\ncores = 4\n";

#[derive(serde::Deserialize)]
struct Program {
    program: PathBuf,
    derivation: PathBuf,
}

fn nix() -> Command {
    let mut command = Command::new("nix");
    command.env("NIX_CONFIG", POLICY);
    command
}

fn success(command: &mut Command) -> anyhow::Result<Output> {
    let output = command.output()?;
    assert!(
        output.status.success(),
        "{command:?}: stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    writeln!(
        std::io::stdout().lock(),
        "{command:?}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )?;
    Ok(output)
}

fn source_fixture(source: &Path) -> anyhow::Result<tempfile::TempDir> {
    let fixture = tempfile::tempdir()?;
    loom_driver::git::init_test_repo(fixture.path())?;
    let git = loom_driver::git::GitClient::open(source)?;
    for path in git.tracked_files_sync()? {
        let destination = fixture.path().join(&path);
        std::fs::create_dir_all(
            destination
                .parent()
                .context("tracked source must have a parent")?,
        )?;
        std::fs::copy(source.join(path), destination)?;
    }
    loom_driver::git::commit_all_in(fixture.path(), "Capture native system source")?;
    Ok(fixture)
}

#[test]
fn native_system_scenarios_execute_actual_programs_in_canonical_store() -> anyhow::Result<()> {
    let store = success(nix().args(["store", "info", "--json"]))?;
    let store: serde_json::Value = serde_json::from_slice(&store.stdout).unwrap();
    assert!(matches!(store["url"].as_str(), Some("local" | "daemon")));
    let manifest = loom_test_support::manifest_dir(env!("CARGO_MANIFEST_DIR"));
    let source = manifest.parent().unwrap().parent().unwrap();
    let fixture = source_fixture(source)?;
    let target = format!(
        ".#legacyPackages.{}-darwin.native-system-programs",
        std::env::consts::ARCH
    );
    let result =
        success(
            nix()
                .current_dir(fixture.path())
                .args(["eval", "--offline", "--raw", &target]),
        )?;
    let path = String::from_utf8(result.stdout).unwrap();
    let programs: BTreeMap<String, Program> =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        programs.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "ci-shell",
            "decision-admission",
            "profile-bootstrap",
            "smoke"
        ]
    );
    let mut preparation = nix();
    preparation.args(["build", "--offline", "--no-substitute", "--no-link"]);
    for program in programs.values() {
        assert!(program.program.starts_with("/nix/store"));
        assert!(program.derivation.starts_with("/nix/store"));
        preparation.arg(format!("{}^*", program.derivation.display()));
    }
    success(&mut preparation)?;
    for (name, program) in programs {
        let fixture = source_fixture(source)?;
        let output = success(
            Command::new(program.program)
                .current_dir(fixture.path())
                .env("NIX_CONFIG", POLICY),
        )?;
        if name == "ci-shell" {
            assert!(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .starts_with("loom ")
            );
        }
    }
    Ok(())
}
