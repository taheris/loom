//! Claude Code backend: spawn + shutdown watchdog.
//!
//! [`ClaudeBackend::spawn`] writes the re-pin files into the workspace
//! runtime dir, serializes the [`SpawnConfig`] to JSON, and execs `wrix
//! --profile-config <file> spawn --spawn-config <file> --stdio` with
//! stdin/stdout piped. The
//! watchdog ([`ClaudeBackend::shutdown_after_result`]) handles the
//! post-`result` cleanup: drop the writer, wait `grace`, escalate
//! SIGTERM → SIGKILL.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use loom_driver::agent::{
    Active, AgentBackend, AgentSession, Idle, JsonlReader, ProtocolError, SpawnConfig,
};
use loom_driver::clock::Clock;
use loom_driver::clock::SystemClock;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::io::BufWriter;
use tokio::process::{Child, Command};
use tracing::{debug, info, warn};

use super::parser::ClaudeParser;
use crate::apply_launcher_env;
use crate::skill::{NoNativeRegistrar, register_native_skills};

/// File name for the JSON-serialized [`SpawnConfig`] handed to
/// `wrix spawn --spawn-config`. Written into the per-session
/// [`SpawnConfig::scratch_dir`] alongside `repin.sh` and
/// `claude-settings.json` (which the workflow code wrote earlier via
/// [`ScratchSession`]).
///
/// [`ScratchSession`]: loom_driver::scratch::ScratchSession
const SPAWN_CONFIG_FILE: &str = "spawn-config.json";

/// Default seconds to wait for claude to exit naturally after observing
/// `result`. Per spec the value is configurable via
/// `[claude] post_result_grace_secs`; this constant is the fallback the
/// dispatcher uses when no override is wired up yet.
pub const DEFAULT_POST_RESULT_GRACE_SECS: u64 = 5;

/// Env var that overrides the launcher binary. Production resolves
/// `wrix` from `PATH`.
const ENV_WRIX_BIN: &str = "LOOM_WRIX_BIN";

/// Zero-sized marker for the Claude Code stream-json backend.
///
/// Per the spec's static-dispatch design, all runtime state lives in the
/// spawned [`AgentSession`] and the [`SpawnConfig`] passed to
/// [`AgentBackend::spawn`]. The body launches `claude --print
/// --input-format stream-json --output-format stream-json` (via
/// `wrix spawn --stdio`) with `--permission-prompt-tool stdio` so
/// tool permissions flow over the same pipe.
pub struct ClaudeBackend;

impl AgentBackend for ClaudeBackend {
    async fn spawn(config: &SpawnConfig) -> Result<AgentSession<Idle>, ProtocolError> {
        register_native_skills::<NoNativeRegistrar>(config)?;
        let spawn_config_path = prepare_runtime(config)?;

        let wrix_bin = std::env::var_os(ENV_WRIX_BIN).unwrap_or_else(|| OsString::from("wrix"));
        info!(
            wrix = %wrix_bin.to_string_lossy(),
            spawn_config = %spawn_config_path.display(),
            "claude backend spawn",
        );

        let mut cmd = build_wrix_command(
            &wrix_bin,
            config.profile_config.as_deref(),
            &spawn_config_path,
        );
        apply_launcher_env(&mut cmd, &config.launcher_env);

        spawn_session(cmd, Vec::new()).await
    }

    async fn after_session_complete(
        session: AgentSession<Active>,
        config: &SpawnConfig,
    ) -> Result<(), ProtocolError> {
        let grace = config
            .shutdown_grace
            .unwrap_or_else(|| Duration::from_secs(DEFAULT_POST_RESULT_GRACE_SECS));
        info!(
            grace_secs = grace.as_secs(),
            "claude session_complete observed; running shutdown watchdog",
        );
        Self::shutdown_after_result(session, &SystemClock::new(), grace).await?;
        Ok(())
    }
}

impl ClaudeBackend {
    /// Run the post-`result` shutdown watchdog: drop the stdin writer (so
    /// claude sees EOF), wait up to `grace` for the child to exit on its
    /// own, then escalate SIGTERM, then SIGKILL.
    ///
    /// Returns the child's exit code (0 if the process was killed via
    /// signal). Errors only when the final `wait` fails — signal-send
    /// failures are logged and treated as best-effort because the child
    /// may already have exited between the wait timeout and the kill.
    ///
    /// `clock` drives the grace timer so tests can substitute
    /// [`loom_driver::clock::MockClock`].
    pub async fn shutdown_after_result<S>(
        session: AgentSession<S>,
        clock: &dyn Clock,
        grace: Duration,
    ) -> Result<i32, ProtocolError> {
        let (mut child, stdin) = session.into_parts();
        drop(stdin);

        if let Some(code) = wait_with_timeout(&mut child, clock, grace).await? {
            return Ok(code);
        }

        warn!(
            grace_ms = grace.as_millis(),
            "claude did not exit after result; sending SIGTERM",
        );
        send_signal(&child, Signal::SIGTERM);

        if let Some(code) = wait_with_timeout(&mut child, clock, grace).await? {
            return Ok(code);
        }

        warn!("claude ignored SIGTERM; sending SIGKILL");
        send_signal(&child, Signal::SIGKILL);

        let status = child.wait().await.map_err(ProtocolError::Io)?;
        Ok(status.code().unwrap_or(0))
    }
}

/// Serialize the [`SpawnConfig`] into the per-session
/// [`SpawnConfig::scratch_dir`] alongside the `repin.sh` and
/// `claude-settings.json` files the workflow already wrote there via
/// [`ScratchSession`]. Returns the path of the written spawn-config.
///
/// Module-public so tests can verify the side effects independently of
/// the launcher exec (which would otherwise require the real `wrix`
/// wrapper on `PATH`).
///
/// [`ScratchSession`]: loom_driver::scratch::ScratchSession
pub(crate) fn prepare_runtime(config: &SpawnConfig) -> Result<PathBuf, ProtocolError> {
    let mut config = config.clone();
    if let Ok(token) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        upsert_env(&mut config.env, "CLAUDE_CODE_OAUTH_TOKEN", &token);
    }
    write_spawn_config(&config.scratch_dir, &config)
}

fn upsert_env(env: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some(slot) = env.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value.to_string();
    } else {
        env.push((key.to_string(), value.to_string()));
    }
}

/// Build an [`AgentSession`] from a launcher [`Command`].
///
/// Module-private — the public surface is [`ClaudeBackend::spawn`]. Tests
/// call this through the `pub(crate)` re-export to substitute a mock claude
/// binary in place of the real `wrix spawn` exec.
pub(crate) async fn spawn_session(
    mut cmd: Command,
    denied_tools: Vec<String>,
) -> Result<AgentSession<Idle>, ProtocolError> {
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(ProtocolError::Io)?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| ProtocolError::Io(io::Error::other("claude child stdin not piped")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProtocolError::Io(io::Error::other("claude child stdout not piped")))?;

    let parser = ClaudeParser::new(denied_tools);
    Ok(AgentSession::new(
        child,
        BufWriter::new(stdin),
        JsonlReader::new(stdout),
        Box::new(parser),
    ))
}

fn build_wrix_command(
    wrix_bin: &OsStr,
    profile_config: Option<&Path>,
    spawn_config_path: &Path,
) -> Command {
    let mut cmd = Command::new(wrix_bin);
    if let Some(profile_config) = profile_config {
        cmd.arg("--profile-config").arg(profile_config);
    }
    cmd.arg("spawn")
        .arg("--spawn-config")
        .arg(spawn_config_path)
        .arg("--stdio");
    cmd
}

fn write_spawn_config(runtime_dir: &Path, config: &SpawnConfig) -> Result<PathBuf, ProtocolError> {
    std::fs::create_dir_all(runtime_dir).map_err(ProtocolError::Io)?;
    let path = runtime_dir.join(SPAWN_CONFIG_FILE);
    let json = serde_json::to_vec(config)?;
    std::fs::write(&path, json).map_err(ProtocolError::Io)?;
    Ok(path)
}

/// Wait `grace` for the child to exit. `Ok(Some(code))` means the child
/// is reaped; `Ok(None)` means the wait timed out and the caller should
/// escalate.
async fn wait_with_timeout(
    child: &mut Child,
    clock: &dyn Clock,
    grace: Duration,
) -> Result<Option<i32>, ProtocolError> {
    let wait = child.wait();
    let sleep = clock.sleep(grace);
    tokio::select! {
        result = wait => match result {
            Ok(status) => Ok(Some(status.code().unwrap_or(0))),
            Err(e) => Err(ProtocolError::Io(e)),
        },
        () = sleep => Ok(None),
    }
}

/// Best-effort signal send. The child may already be dead (race between
/// the timeout firing and the OS reaping the process); failures are
/// logged but do not propagate so the watchdog can continue its
/// escalation.
fn send_signal(child: &Child, sig: Signal) {
    let Some(pid) = child.id() else {
        debug!("claude child id unavailable; skipping signal {}", sig);
        return;
    };
    let pid = match i32::try_from(pid) {
        Ok(p) => Pid::from_raw(p),
        Err(_) => {
            warn!(pid, "claude child id does not fit in i32; skipping signal");
            return;
        }
    };
    if let Err(e) = kill(pid, sig) {
        debug!(error = %e, signal = %sig, "kill returned error; child may already have exited");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::agent::RePinContent;
    use loom_driver::clock::{MockClock, SystemClock};
    use loom_events::ParsedAgentEvent;
    use std::path::PathBuf;

    fn sample_repin() -> RePinContent {
        RePinContent {
            orientation: "test orientation".to_string(),
            pinned_context: "test context".to_string(),
            partial_bodies: vec!["partial one".to_string()],
        }
    }

    const PLANNING_INTERVIEW_PROMPT: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/planning_prompt_interview_modes.md"
    ));
    const POLISH_MODE_DEFINITION: &str = "- polish / do-a-polish: report-only mode. Review the proposed wording and report suggested edits, but do not modify files or apply edits unless the human explicitly asks you to make the edits.";
    const ONE_BY_ONE_MODE_DEFINITION: &str = "- one-by-one: ask exactly one design question per turn, then wait for the human's answer before asking the next question or changing topics.";

    fn jq_is_available() -> bool {
        std::process::Command::new("jq")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn compact_hook_context_from_settings(
        scratch: &loom_driver::scratch::ScratchSession,
    ) -> Option<String> {
        if !jq_is_available() {
            eprintln!("jq missing; skipping compact-hook repin test");
            return None;
        }
        let body = std::fs::read_to_string(scratch.claude_settings()).expect("read settings");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("parse settings");
        let cmd = parsed["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .expect("hook command");
        assert!(cmd.ends_with("repin.sh"), "hook command: {cmd}");

        let mut command = std::process::Command::new("bash");
        command.arg(cmd);
        if !Path::new(cmd).is_absolute() {
            let workspace = scratch.path().ancestors().nth(3).expect("workspace path");
            command.current_dir(workspace);
        }
        let out = command.output().expect("run repin.sh");
        assert!(
            out.status.success(),
            "repin.sh failed: stderr={}",
            String::from_utf8_lossy(&out.stderr),
        );
        let envelope: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("parse hook output");
        assert_eq!(
            envelope["hookSpecificOutput"]["hookEventName"],
            "SessionStart",
        );
        Some(
            envelope["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .expect("additional context")
                .to_string(),
        )
    }

    fn mock_claude_path() -> PathBuf {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for ancestor in manifest_dir.ancestors() {
            let candidate = ancestor.join("tests/mock-claude/claude.sh");
            if candidate.is_file() {
                return candidate;
            }
        }
        panic!(
            "could not locate tests/mock-claude/claude.sh above {} — \
             neither dev-tree nor nix-sandbox layout matched.",
            manifest_dir.display(),
        );
    }

    fn mock_command(mode: &str) -> Command {
        let mut cmd = Command::new("bash");
        cmd.arg(mock_claude_path()).arg(mode);
        cmd
    }

    // -- test_claude_repin_files -------------------------------------------

    #[test]
    fn prepare_runtime_writes_spawn_config_into_scratch_dir() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let scratch = loom_driver::scratch::ScratchSession::open(
            workspace.path(),
            "lm-test",
            "hello",
            "loom loop @ lm-test",
        )
        .expect("open scratch");
        let cfg = SpawnConfig {
            image_ref: "localhost/wrix-test:claude".to_string(),
            image_source: PathBuf::from("/nix/store/zzz-wrix-test-claude.tar"),
            image_source_kind: Some(loom_driver::agent::ImageSourceKind::NixDescriptor),
            profile_config: Some(PathBuf::from(
                "/nix/store/wrix-test-claude-profile-config.json",
            )),
            workspace: workspace.path().to_path_buf(),
            env: vec![("WRIX_AGENT".into(), "claude".into())],
            mounts: vec![],
            initial_prompt: "hello".to_string(),
            agent_args: vec!["--print".into()],
            repin: sample_repin(),
            skills: None,
            scratch_dir: scratch.path().to_path_buf(),
            model: None,
            thinking_level: None,
            output_limits: None,
            shutdown_grace: None,
            handshake_timeout: None,
            stall_warn_interval: None,
            launcher_env: Vec::new(),
        };

        let spawn_config_path = prepare_runtime(&cfg).expect("prepare_runtime");

        // Scratch session pre-populated repin.sh + claude-settings.json.
        assert!(
            scratch.repin_script().exists(),
            "repin.sh missing in scratch dir",
        );
        assert!(
            scratch.claude_settings().exists(),
            "claude-settings.json missing in scratch dir",
        );
        assert_eq!(spawn_config_path, scratch.path().join(SPAWN_CONFIG_FILE));
        assert!(spawn_config_path.exists());

        // Round-trip the spawn-config file to confirm it carries the input
        // unchanged — the wrapper consumes this exact JSON.
        let bytes = std::fs::read(&spawn_config_path).expect("read");
        let decoded: SpawnConfig = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(decoded.image_ref, cfg.image_ref);
        assert_eq!(decoded.image_source, cfg.image_source);
        assert_eq!(decoded.initial_prompt, cfg.initial_prompt);
        assert_eq!(decoded.agent_args, cfg.agent_args);
    }

    #[test]
    fn claude_compact_hook_rehydrates_interview_modes() {
        let workspace = tempfile::tempdir().expect("tempdir");
        let scratch = loom_driver::scratch::ScratchSession::open(
            workspace.path(),
            "lm-interview",
            PLANNING_INTERVIEW_PROMPT,
            "loom plan @ agent",
        )
        .expect("open scratch");
        let scratch_body = "## Scratchpad\n- compacted summary omitted interview modes\n";
        std::fs::write(scratch.path().join("scratch.md"), scratch_body).expect("write scratch.md");

        let Some(context) = compact_hook_context_from_settings(&scratch) else {
            return;
        };
        let expected =
            format!("loom plan @ agent\n\n{PLANNING_INTERVIEW_PROMPT}\n\n{scratch_body}");
        assert_eq!(context, expected);
        assert!(
            context.contains(POLISH_MODE_DEFINITION),
            "polish mode definition missing from compact-hook context: {context}",
        );
        assert!(
            context.contains(ONE_BY_ONE_MODE_DEFINITION),
            "one-by-one mode definition missing from compact-hook context: {context}",
        );
        let prompt_pos = context
            .find(PLANNING_INTERVIEW_PROMPT)
            .expect("prompt position");
        let scratch_pos = context.find(scratch_body).expect("scratch position");
        assert!(
            prompt_pos < scratch_pos,
            "prompt.txt payload must precede scratch.md payload: {context}",
        );
    }

    #[test]
    fn wrix_command_uses_profile_config_before_spawn() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spawn_config_path = dir.path().join("loom-spawn.json");
        let profile_config = Path::new("/nix/store/wrix-claude-profile-config.json");
        let cmd = build_wrix_command(OsStr::new("wrix"), Some(profile_config), &spawn_config_path);
        let std_cmd = cmd.as_std();

        assert_eq!(std_cmd.get_program(), OsStr::new("wrix"));
        let args: Vec<&OsStr> = std_cmd.get_args().collect();
        assert_eq!(
            args,
            vec![
                OsStr::new("--profile-config"),
                profile_config.as_os_str(),
                OsStr::new("spawn"),
                OsStr::new("--spawn-config"),
                spawn_config_path.as_os_str(),
                OsStr::new("--stdio"),
            ],
        );
    }

    // -- test_claude_supports_steering -------------------------------------

    #[tokio::test]
    async fn steering_message_reaches_mock_and_emits_followup_turn() {
        let session = spawn_session(mock_command("steering"), Vec::new())
            .await
            .expect("spawn session");
        let mut session = session.prompt("first prompt").await.expect("prompt ok");

        // First assistant turn — proves the mock saw the prompt.
        match session.next_event().await.expect("event ok") {
            Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                assert!(text.contains("first turn"), "unexpected text: {text}");
            }
            other => panic!("expected first MessageDelta, got {other:?}"),
        }

        session
            .steer("STEERED_TEXT")
            .await
            .expect("steer should succeed");

        // Second assistant turn — proves steering reached the mock.
        match session.next_event().await.expect("event ok") {
            Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                assert!(
                    text.contains("STEERED_TEXT"),
                    "second turn did not echo steer: {text}",
                );
            }
            other => panic!("expected second MessageDelta, got {other:?}"),
        }

        // Drain remaining events until SessionComplete so the watchdog has
        // a result to consume.
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before SessionComplete"),
            }
        }

        let exit = ClaudeBackend::shutdown_after_result(
            session,
            &SystemClock::new(),
            Duration::from_millis(500),
        )
        .await
        .expect("shutdown ok");
        assert_eq!(exit, 0, "mock exited cleanly after result");
    }

    // -- test_claude_shutdown_watchdog -------------------------------------

    #[tokio::test]
    async fn shutdown_watchdog_escalates_to_sigkill_when_child_ignores_stdin_close() {
        // Real subprocess (mock claude) ignores SIGTERM, so the watchdog walks
        // the SIGTERM → SIGKILL escalation. Drive that escalation with a real
        // clock — paused-time would never resolve `child.wait()` because the
        // OS scheduler is not a tokio task. We use a small grace + an upper
        // bound to keep the test deterministic without measuring elapsed time
        // against an Instant.
        let session = spawn_session(mock_command("ignore-stdin"), Vec::new())
            .await
            .expect("spawn session");
        let mut session = session.prompt("hello").await.expect("prompt ok");

        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before SessionComplete"),
            }
        }

        let clock = SystemClock::new();
        let grace = Duration::from_millis(150);
        // Bound the watchdog to 5s so a hung child surfaces as a test
        // failure instead of a stuck CI job. Routes through SystemClock so
        // the call site stays clear of the wall-clock timer ban.
        let exit = clock
            .timeout(
                Duration::from_secs(5),
                ClaudeBackend::shutdown_after_result(session, &clock, grace),
            )
            .await
            .expect("watchdog within budget")
            .expect("shutdown ok");
        let _ = exit;
    }

    // -- mock-clock smoke for wait_with_timeout ----------------------------

    /// Confirms `wait_with_timeout` returns `None` when the inner future does
    /// not complete before `clock.sleep` resolves. Uses `MockClock` under
    /// `start_paused = true` so paused-time advance drives the timeout
    /// without a real subprocess wait.
    #[tokio::test(start_paused = true)]
    async fn wait_with_timeout_returns_none_via_mock_clock() {
        // Spawn a long-running child that will not exit on its own within the
        // grace window. We never actually wait wall-clock — the MockClock
        // sleep wins the select.
        let mut child = Command::new("sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sleep");
        let clock = MockClock::new();
        let result = wait_with_timeout(&mut child, &clock, Duration::from_millis(10))
            .await
            .expect("no IO error");
        assert!(result.is_none(), "expected timeout, got {result:?}");
    }
}
