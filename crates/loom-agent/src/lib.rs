//! Agent backend implementations for Loom.
//!
//! Houses three zero-sized backends — [`PiBackend`] for pi-mono RPC,
//! [`ClaudeBackend`] for Claude Code stream-json, and [`DirectBackend`]
//! for the in-container `loom-direct-runner` — that implement the
//! [`AgentBackend`](loom_driver::agent::AgentBackend) trait declared in
//! `loom-driver`. The trait's job is process lifecycle only; conversation
//! driving (prompt, steer, abort, event streaming) lives on
//! [`AgentSession`](loom_driver::agent::AgentSession).

pub mod claude;
pub mod direct;
pub mod pi;
mod skill;

pub use claude::ClaudeBackend;
pub use direct::DirectBackend;
pub use pi::PiBackend;

/// Apply [`SpawnConfig::launcher_env`] to the `wrix spawn` child process
/// before it is spawned. These pairs (`WRIX_DEPLOY_KEY` /
/// `WRIX_SIGNING_KEY` → host key paths) are read by the wrix launcher to
/// bind-mount the deploy + signing keys into the bead container; they are
/// deliberately **not** part of [`SpawnConfig::env`] (the in-container
/// allowlist) and never reach the spawn-config JSON. Without this, loop
/// agents boot with no git keys and cannot sign or push (`specs/harness.md`
/// § Commit signing).
///
/// [`SpawnConfig::launcher_env`]: loom_driver::agent::SpawnConfig::launcher_env
/// [`SpawnConfig::env`]: loom_driver::agent::SpawnConfig::env
pub(crate) fn apply_launcher_env(
    cmd: &mut tokio::process::Command,
    launcher_env: &[(String, String)],
) {
    cmd.envs(launcher_env.iter().map(|(k, v)| (k, v)));
}

#[cfg(test)]
mod tests {
    use super::apply_launcher_env;

    /// `apply_launcher_env` places each pair on the child process
    /// environment — the load-bearing half of delivering git keys to a
    /// spawned loop agent. Verified by spawning `printenv` and reading the
    /// value back rather than inspecting `Command` (which exposes no env
    /// getter), so a regression that drops the call surfaces here.
    #[tokio::test]
    async fn apply_launcher_env_sets_child_process_env() {
        let mut cmd = tokio::process::Command::new("printenv");
        cmd.arg("WRIX_SIGNING_KEY");
        apply_launcher_env(
            &mut cmd,
            &[(
                "WRIX_SIGNING_KEY".to_string(),
                "/home/op/.ssh/deploy_keys/k-signing".to_string(),
            )],
        );
        let output = cmd.output().await.expect("spawn printenv");
        assert!(output.status.success(), "printenv must find the var");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "/home/op/.ssh/deploy_keys/k-signing",
        );
    }
}
