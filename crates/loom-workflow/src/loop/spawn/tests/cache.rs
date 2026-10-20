use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use loom_driver::clock::SystemClock;
use loom_driver::git::{commit_all_in, init_test_repo};
use loom_driver::identifier::{BeadId, SpecLabel};
use tokio::process::Command;

use super::*;

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    spawns: Vec<SpawnConfig>,
}

impl Fixture {
    async fn new() -> Result<Self> {
        let directory = tempfile::Builder::new().prefix("loom-cache-").tempdir()?;
        let root = directory.path().to_path_buf();
        let dependency = root.join("dependency");
        init_test_repo(&dependency)?;
        fs::create_dir_all(dependency.join("src"))?;
        fs::write(
            dependency.join("Cargo.toml"),
            "[workspace]\n[package]\nname='cache-dependency'\nversion='0.1.0'\nedition='2024'\n",
        )?;
        fs::write(
            dependency.join("src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )?;
        commit_all_in(&dependency, "Immutable dependency")?;
        let operator = root.join("operator");
        let git = init_test_repo(&operator)?;
        let integration = operator.join(".loom/integration");
        init_test_repo(&integration)?;
        fs::create_dir_all(integration.join("src"))?;
        fs::write(
            integration.join("Cargo.toml"),
            format!(
                "[workspace]\n[package]\nname='cache-fixture'\nversion='0.1.0'\nedition='2024'\n[features]\nvariant=[]\n[dependencies]\ncache-dependency={{git='file://{}'}}\n",
                dependency.display()
            ),
        )?;
        fs::write(
            integration.join("src/lib.rs"),
            "#[cfg(not(feature=\"variant\"))]\npub fn value() -> u8 { cache_dependency::value() }\n#[cfg(feature=\"variant\")]\npub fn value() -> u8 { cache_dependency::value() + 1 }\n",
        )?;
        fs::write(
            integration.join("src/main.rs"),
            "fn main() { println!(\"{}\", cache_fixture::value()); }\n",
        )?;
        execute(
            Command::new("cargo")
                .current_dir(&integration)
                .env("CARGO_HOME", root.join("cargo-home"))
                .args(["generate-lockfile"]),
        )
        .await?;
        commit_all_in(&integration, "Cache fixture")?;
        let manifest = three_profile_manifest(&root);
        let config = LoomTopConfig {
            sccache_dir: Some(root.join("shared-worker-cache")),
            ..LoomTopConfig::default()
        };
        let mut spawns = Vec::new();
        for id in ["lm-first", "lm-second"] {
            let workspace = git
                .create_worktree(&SpecLabel::new("workspaces")?, &BeadId::new(id)?)
                .await?
                .path;
            let spawn = build_spawn_config_from_manifest(
                &manifest,
                &bead_with_labels(id, &["profile:rust"]),
                None,
                &base(),
                AgentRuntime::Pi,
                workspace.clone(),
                "Fixture".into(),
                root.join(id),
                config.container_sccache_env(),
                vec![],
                sccache_mount(&config)?.into_iter().collect(),
                vec![],
            )?;
            spawns.push(spawn);
        }
        fs::write(root.join("sccache.toml"), "")?;
        Ok(Self {
            _directory: directory,
            root,
            spawns,
        })
    }

    fn command(&self, executable: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(executable);
        command.current_dir(&self.root);
        for (key, _) in std::env::vars_os() {
            if key.as_encoded_bytes().starts_with(b"SCCACHE_") {
                command.env_remove(key);
            }
        }
        for name in [
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_BUILD_RUSTC_WRAPPER",
            "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_BUILD_BUILD_DIR",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_MAKEFLAGS",
        ] {
            command.env_remove(name);
        }
        command
            .env("CARGO_HOME", self.root.join("cargo-home"))
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_BUILD_JOBS", "1")
            .env("RUSTFLAGS", "-C debuginfo=0")
            .env("SCCACHE_CONF", self.root.join("sccache.toml"))
            .env("SCCACHE_DIR", &self.spawns[0].mounts[0].host_path)
            .env("SCCACHE_CACHE_SIZE", "16M")
            .env("SCCACHE_IDLE_TIMEOUT", "10")
            .env("SCCACHE_SERVER_UDS", self.root.join("server.sock"));
        command
    }

    async fn stats(&self) -> Result<serde_json::Value> {
        let mut command = self.command("sccache");
        command.args(["--show-stats", "--stats-format=json"]);
        Ok(serde_json::from_str(&execute(&mut command).await?)?)
    }

    async fn build(&self, index: usize, variant: bool) -> Result<String> {
        let spawn = &self.spawns[index];
        let mut command = self.command("cargo");
        command
            .current_dir(&spawn.workspace)
            .args(["build", "--offline", "--target-dir"])
            .arg(spawn.workspace.join("target"));
        for (key, value) in &spawn.env {
            if key == "SCCACHE_DIR" {
                let mount = spawn
                    .mounts
                    .iter()
                    .find(|mount| mount.container_path == Path::new(value))
                    .context("cache environment has no matching mount")?;
                command.env(key, &mount.host_path);
            } else {
                command.env(key, value);
            }
        }
        if variant {
            command.args(["--features", "variant"]);
        }
        execute(&mut command).await?;
        execute(&mut self.command(spawn.workspace.join("target/debug/cache-fixture"))).await
    }

    async fn exercise(&self) -> Result<()> {
        for spawn in &self.spawns {
            ensure!(
                spawn.mounts == self.spawns[0].mounts,
                "bead cache mounts differ"
            );
            ensure!(spawn.mounts.len() == 1, "expected one cache mount");
            ensure!(!spawn.mounts[0].read_only, "cache must be writable");
            for key in ["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER"] {
                ensure!(
                    spawn
                        .env
                        .iter()
                        .any(|(name, value)| name == key && value == "sccache"),
                    "missing cache wrapper setting {key}"
                );
            }
            ensure!(
                !spawn.workspace.join("target").exists(),
                "target must start cold"
            );
            ensure!(
                !spawn.workspace.join(".loom/sccache").exists(),
                "no per-bead cache"
            );
        }
        let stats = self.stats().await?;
        ensure!(
            stats["cache_location"]
                == format!(
                    "Local disk: \"{}\"",
                    self.spawns[0].mounts[0].host_path.display()
                ),
            "server selected an unexpected cache: {stats}"
        );
        ensure!(
            stats["max_cache_size"] == 16 * 1024 * 1024,
            "unexpected cache bound"
        );

        ensure!(
            self.build(0, false).await?.trim() == "1",
            "first clone output"
        );
        let first = self.stats().await?;
        ensure!(
            self.build(1, false).await?.trim() == "1",
            "second clone output"
        );
        let second = self.stats().await?;
        ensure!(
            hits(&second) > hits(&first),
            "second clone did not reuse the shared Rust cache: {first} -> {second}"
        );
        ensure!(
            self.build(1, true).await?.trim() == "2",
            "changed-feature output"
        );
        let changed = self.stats().await?;
        ensure!(
            misses(&changed) > misses(&second),
            "feature change must compile a distinct library"
        );
        ensure!(
            self.build(0, false).await?.trim() == "1",
            "first clone changed"
        );
        Ok(())
    }
}

fn hits(stats: &serde_json::Value) -> u64 {
    stats["stats"]["cache_hits"]["counts"]["Rust"]
        .as_u64()
        .unwrap_or(0)
}

fn misses(stats: &serde_json::Value) -> u64 {
    stats["stats"]["cache_misses"]["counts"]["Rust"]
        .as_u64()
        .unwrap_or(0)
}

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

/// Real Cargo and sccache verify reuse; only container mount projection is translated for native execution.
#[tokio::test]
async fn sccache_shared_across_bead_clones() -> Result<()> {
    let fixture = Fixture::new().await?;
    execute(fixture.command("sccache").arg("--start-server")).await?;
    let result = fixture.exercise().await;
    let stopped = execute(fixture.command("sccache").arg("--stop-server")).await;
    result?;
    stopped?;
    Ok(())
}
