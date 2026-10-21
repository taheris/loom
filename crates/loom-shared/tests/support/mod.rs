use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use loom_driver::clock::{Clock, SystemClock};
use loom_driver::git::GitClient;
use loom_driver::identifier::{BeadId, SpecLabel};
use loom_shared::artifact::{self, Outcome, Reuse};
use serde::Serialize;
use tokio::process::Command;

const IMAGE: &str = "/nix/store/fixture-worker-profile.json";

pub struct Fixture {
    _directory: tempfile::TempDir,
    git: GitClient,
    home: PathBuf,
    pub dependency: PathBuf,
    pub beads: PathBuf,
    pub donor: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct Build {
    pub elapsed_ms: u128,
    pub external_fresh: bool,
    pub application_fresh: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Storage {
    pub allocated: u64,
    pub unique_inode_bytes: u64,
    pub shared_inode_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct Trial {
    pub policy: Reuse,
    pub seed_elapsed_ms: u128,
    pub seeded: bool,
    pub cloned_library_bytes: u64,
    pub linked_library_bytes: u64,
    pub copied_metadata_bytes: u64,
    pub libraries_before_build: Storage,
    pub build: Build,
    pub libraries_after_build: Storage,
}

#[derive(Debug, Serialize)]
pub struct Measurement {
    pub os: &'static str,
    pub architecture: &'static str,
    pub rustc: String,
    pub cargo: String,
    pub donor_build: Build,
    pub trials: Vec<Trial>,
    pub storage_note: &'static str,
}

impl Fixture {
    /// # Errors
    /// Returns fixture filesystem, Git or bounded Cargo failures.
    pub async fn new(parent: &Path) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("loom-target-reuse-")
            .tempdir_in(parent)?;
        let root = directory.path();
        let home = root.join("cargo-home");
        let dependency = root.join("dependency");
        fs::create_dir_all(dependency.join("src"))?;
        fs::write(
            dependency.join("Cargo.toml"),
            "[workspace]\n[package]\nname='external'\nversion='0.1.0'\nedition='2024'\n",
        )?;
        write_dependency(&dependency, 1)?;
        loom_driver::git::init_test_repo(&dependency)?;
        loom_driver::git::commit_all_in(&dependency, "Dependency")?;
        let operator = root.join("operator");
        let git = loom_driver::git::init_test_repo_with_integration(&operator)?;
        let integration = operator.join(".loom/integration");
        fs::create_dir_all(integration.join("src"))?;
        fs::write(
            integration.join("Cargo.toml"),
            format!(
                "[workspace]\n[package]\nname='application'\nversion='0.1.0'\nedition='2024'\n[dependencies]\nexternal={{git='file://{}'}}\n",
                dependency.display()
            ),
        )?;
        fs::write(
            integration.join("src/main.rs"),
            "fn main() { println!(\"{}:{}\", external::value(), env!(\"CARGO_MANIFEST_DIR\")); }\n",
        )?;
        execute(&mut cargo(&integration, &home, &["generate-lockfile"])).await?;
        loom_driver::git::commit_all_in(&integration, "Application")?;
        let donor = git
            .create_worktree(&SpecLabel::new("workspaces")?, &BeadId::new("lm-aaaa")?)
            .await?
            .path;
        let beads = operator.join(".loom/beads");
        ensure!(
            artifact::prepare(&beads, &donor, Path::new(IMAGE), Reuse::AllowHardlinks)?
                == Outcome::Cold,
            "fresh donor should start cold"
        );
        Ok(Self {
            _directory: directory,
            git,
            home,
            dependency,
            beads,
            donor,
        })
    }

    /// # Errors
    /// Returns invalid identifiers or Git clone failures.
    pub async fn workspace(&self, id: &str) -> Result<PathBuf> {
        Ok(self
            .git
            .create_worktree(&SpecLabel::new("workspaces")?, &BeadId::new(id)?)
            .await?
            .path)
    }

    /// # Errors
    /// Returns target preparation failures.
    pub fn prepare(&self, workspace: &Path, reuse: Reuse) -> Result<Outcome> {
        Ok(artifact::prepare(
            &self.beads,
            workspace,
            Path::new(IMAGE),
            reuse,
        )?)
    }

    /// # Errors
    /// Returns bounded Cargo failures or malformed artifact messages.
    pub async fn build(&self, workspace: &Path) -> Result<Build> {
        let started = SystemClock.now();
        let output = execute(&mut cargo(
            workspace,
            &self.home,
            &["build", "--offline", "--message-format=json"],
        ))
        .await?;
        let elapsed_ms = SystemClock.now().duration_since(started).as_millis();
        let mut fresh = BTreeMap::new();
        for line in output.lines() {
            let artifact: serde_json::Value = serde_json::from_str(line)?;
            if artifact["reason"] == "compiler-artifact" {
                let name = artifact["target"]["name"]
                    .as_str()
                    .context("artifact target name")?;
                fresh.insert(
                    name.to_string(),
                    artifact["fresh"].as_bool().context("artifact freshness")?,
                );
            }
        }
        Ok(Build {
            elapsed_ms,
            external_fresh: *fresh
                .get("external")
                .context("external compiler artifact")?,
            application_fresh: *fresh
                .get("application")
                .context("application compiler artifact")?,
        })
    }

    /// # Errors
    /// Returns bounded native execution or output decoding failures.
    pub async fn run(&self, workspace: &Path) -> Result<String> {
        execute(&mut Command::new(
            workspace.join("target/debug/application"),
        ))
        .await
    }

    /// # Errors
    /// Returns filesystem, Git or bounded Cargo update failures.
    pub async fn change_dependency(&self, workspace: &Path) -> Result<()> {
        write_dependency(&self.dependency, 5)?;
        loom_driver::git::commit_all_in(&self.dependency, "Change dependency")?;
        execute(&mut cargo(
            workspace,
            &self.home,
            &["update", "-p", "external"],
        ))
        .await?;
        Ok(())
    }

    /// # Errors
    /// Returns fixture build, storage accounting or native output failures.
    pub async fn measure(&self) -> Result<Measurement> {
        let donor_build = self.build(&self.donor).await?;
        let mut trials = Vec::new();
        for (name, policy) in [
            ("lm-bbbb", Reuse::Off),
            ("lm-cccc", Reuse::CopyOnWrite),
            ("lm-dddd", Reuse::AllowHardlinks),
        ] {
            let workspace = self.workspace(name).await?;
            let started = SystemClock.now();
            let outcome = self.prepare(&workspace, policy)?;
            let seed_elapsed_ms = SystemClock.now().duration_since(started).as_millis();
            let (seeded, cloned_library_bytes, linked_library_bytes, copied_metadata_bytes) =
                match outcome {
                    Outcome::Seeded(report) => (
                        true,
                        report.cloned_bytes,
                        report.linked_bytes,
                        report.copied_bytes,
                    ),
                    Outcome::Disabled | Outcome::Cold => (false, 0, 0, 0),
                    other => anyhow::bail!("unexpected fresh-target outcome: {other:?}"),
                };
            let libraries_before_build = libraries(&[&self.donor, &workspace])?;
            let build = self.build(&workspace).await?;
            ensure!(
                self.run(&workspace).await?.trim() == format!("1:{}", workspace.display()),
                "wrong application output"
            );
            let libraries_after_build = libraries(&[&self.donor, &workspace])?;
            trials.push(Trial {
                policy,
                seed_elapsed_ms,
                seeded,
                cloned_library_bytes,
                linked_library_bytes,
                copied_metadata_bytes,
                libraries_before_build,
                build,
                libraries_after_build,
            });
        }
        Ok(Measurement {
            os: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            rustc: execute(Command::new("rustc").arg("--version"))
                .await?
                .trim()
                .to_owned(),
            cargo: execute(Command::new("cargo").arg("--version"))
                .await?
                .trim()
                .to_owned(),
            donor_build,
            trials,
            storage_note: "Allocated regular-file blocks, deduplicated by device/inode for external rlib/rmeta files only. Shared-inode bytes quantify hardlink sharing, not CoW extent sharing. CoW physical sharing needs substrate-native evidence. Timings are advisory tiny-fixture observations, not production speedup claims.",
        })
    }
}

fn write_dependency(path: &Path, value: u8) -> Result<()> {
    fs::write(
        path.join("src/lib.rs"),
        format!(
            "pub static DATA: [u8; 262144] = [{value}; 262144];\npub fn value() -> u8 {{ DATA[0] }}\n"
        ),
    )?;
    Ok(())
}

/// # Errors
/// Returns filesystem failures or nonregular library paths.
pub fn external_libraries(workspace: &Path) -> Result<Vec<PathBuf>> {
    let directory = workspace.join("target/debug/deps");
    if !directory.try_exists()? {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("libexternal-")
            && path
                .extension()
                .is_some_and(|extension| extension == "rlib" || extension == "rmeta")
        {
            ensure!(
                entry.file_type()?.is_file(),
                "library is not a regular file"
            );
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn libraries(workspaces: &[&Path]) -> Result<Storage> {
    let mut storage = Storage::default();
    let mut seen = BTreeSet::new();
    for workspace in workspaces {
        for path in external_libraries(workspace)? {
            let metadata = fs::metadata(path)?;
            let bytes = metadata.blocks() * 512;
            storage.allocated += bytes;
            if seen.insert((metadata.dev(), metadata.ino())) {
                storage.unique_inode_bytes += bytes;
            } else {
                storage.shared_inode_bytes += bytes;
            }
        }
    }
    Ok(storage)
}

fn cargo(workspace: &Path, home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("cargo");
    command
        .current_dir(workspace)
        .args(args)
        .env("CARGO_HOME", home)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_BUILD_JOBS", "1");
    for name in [
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_BUILD_BUILD_DIR",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_MAKEFLAGS",
    ] {
        command.env_remove(name);
    }
    command
}

/// Actual Cargo/rustc and native executables are the freshness boundary; each process group is bounded and reaped.
async fn execute(command: &mut Command) -> Result<String> {
    let output =
        loom_driver::process::output(command, &SystemClock, Duration::from_secs(60)).await?;
    ensure!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}
