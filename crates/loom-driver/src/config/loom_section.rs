use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

pub use loom_shared::artifact::Reuse as CargoTargetReuse;

/// `[loom]` block — workspace-level loom knobs that don't fit any of the
/// phase-, agent-, runner-, or component-specific sections. See
/// `specs/harness.md` § Configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LoomTopConfig {
    /// Name of the branch the loom workspace
    /// (`.loom/integration/`) has checked out and into which bead
    /// branches rebase + fast-forward. Pushed to
    /// `origin/<integration_branch>` from the gate. Default `main`.
    #[serde(default = "default_integration_branch")]
    pub integration_branch: String,
    /// Host-side directory used as the shared sccache cache. When set,
    /// container spawns get a bind mount at
    /// [`sccache_container_path`](Self::sccache_container_path) plus
    /// `SCCACHE_DIR` + `RUSTC_WRAPPER=sccache` in container env, and
    /// trusted host commands explicitly disable compiler caching instead of
    /// consuming worker-writable cache entries. `None` omits this projection.
    /// See `specs/harness.md` § Configuration.
    #[serde(default)]
    pub sccache_dir: Option<PathBuf>,
    /// Worker-only Cargo dependency reuse; defaults to independent copy-on-write clones.
    pub cargo_target_reuse: CargoTargetReuse,
    /// Admission bounds for generated worker targets; uncertainty always retains output.
    pub cargo_retention: crate::workspace::cargo::retention::Policy,
    /// Container-side path the [`sccache_dir`](Self::sccache_dir) host
    /// directory is bind-mounted to. Defaults to `/sccache`. Consulted
    /// only when `sccache_dir` is set; harmless when unset.
    #[serde(default = "default_sccache_container_path")]
    pub sccache_container_path: PathBuf,
    /// Timeout in seconds for git operations whose hooks can legitimately
    /// run for minutes — notably `git push`, which fires the workspace's
    /// pre-push CI stage (nextest + nix build). Surfaces true hangs
    /// (deadlocked subprocess, runaway network) without aborting
    /// legitimate CI. Default 600 (10 minutes). Threaded into
    /// [`GitClient`](crate::git::GitClient) via
    /// `GitClient::with_hook_timeout` at the push call sites.
    #[serde(default = "default_git_hook_timeout_secs")]
    pub git_hook_timeout_secs: u64,
}

impl Default for LoomTopConfig {
    fn default() -> Self {
        Self {
            integration_branch: default_integration_branch(),
            sccache_dir: None,
            cargo_target_reuse: CargoTargetReuse::default(),
            cargo_retention: crate::workspace::cargo::retention::Policy::default(),
            sccache_container_path: default_sccache_container_path(),
            git_hook_timeout_secs: default_git_hook_timeout_secs(),
        }
    }
}

pub fn default_integration_branch() -> String {
    "main".to_string()
}

pub fn default_sccache_container_path() -> PathBuf {
    PathBuf::from("/sccache")
}

pub const fn default_git_hook_timeout_secs() -> u64 {
    600
}

impl LoomTopConfig {
    /// [`git_hook_timeout_secs`](Self::git_hook_timeout_secs) as a typed
    /// [`Duration`], ready to hand to `GitClient::with_hook_timeout`.
    pub const fn git_hook_timeout(&self) -> Duration {
        Duration::from_secs(self.git_hook_timeout_secs)
    }

    /// Container-side sccache env entries: `SCCACHE_DIR` set to the
    /// configured container path plus both Cargo Rust wrapper settings. Empty when
    /// [`sccache_dir`](Self::sccache_dir) is `None` so callers can
    /// unconditionally extend their env allowlist.
    pub fn container_sccache_env(&self) -> Vec<(String, String)> {
        if self.sccache_dir.is_none() {
            return Vec::new();
        }
        vec![
            (
                "SCCACHE_DIR".to_string(),
                self.sccache_container_path.to_string_lossy().into_owned(),
            ),
            ("RUSTC_WRAPPER".to_string(), "sccache".to_string()),
            (
                "CARGO_BUILD_RUSTC_WRAPPER".to_string(),
                "sccache".to_string(),
            ),
        ]
    }
}
