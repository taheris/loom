//! Immutable fixture data provisioned automatically; never execution authority.
#![expect(
    clippy::unwrap_used,
    reason = "fixture provisioning failures abort instead of inventing compiler facts"
)]

use std::path::Path;
use std::process::Command;

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == ".git" {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &destination.join(entry.file_name()));
        } else {
            let path = destination.join(entry.file_name());
            std::fs::copy(entry.path(), &path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let permissions = std::fs::metadata(&path).unwrap().permissions().mode() | 0o200;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(permissions))
                    .unwrap();
            }
        }
    }
}

pub fn fixture(variable: &str, package: &str) -> tempfile::TempDir {
    let source = std::env::var_os(variable).unwrap_or_else(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let attribute = format!(
            ".#packages.{}.{package}",
            loom_gate::runner::result::current_platform()
        );
        let output = Command::new("nix")
            .current_dir(root)
            .args([
                "build",
                "--no-link",
                "--print-out-paths",
                "--offline",
                "--max-jobs",
                "1",
                "--cores",
                "1",
                &attribute,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "declared compiler fixture could not be provisioned: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().into()
    });
    let fixture = tempfile::tempdir().unwrap();
    copy_tree(Path::new(&source), fixture.path());
    let mut git = Command::new("git");
    loom_test_support::scrub_git_local_env(&mut git);
    assert!(
        git.current_dir(fixture.path())
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    fixture
}
