//! Pi-mono RPC backend: spawn + startup probe + optional `set_model`.
//!
//! [`PiBackend::spawn`] serializes the [`SpawnConfig`] to a JSON file,
//! execs the raw wrix launcher as
//! `wrix --profile-config <file> spawn --spawn-config <file> --stdio`
//! when the manifest provides a ProfileConfig, and drives the pi RPC
//! handshake before handing back an [`AgentSession`] in the [`Idle`] state:
//!
//! 1. `get_state` probe — verifies the RPC process is responsive and
//!    returns the documented state object shape before any workflow begins.
//!    Pi's `get_commands` lists slash commands/templates/skills, not built-in
//!    RPC verbs, so it is not a startup capability probe.
//! 2. `set_model` (optional) — sent only when [`SpawnConfig::model`] is
//!    populated by per-phase config. Failure is hard-fail.
//!
//! Process IO during the handshake is direct (no [`AgentSession`] yet) —
//! the typestate session only starts taking events once `prompt` is
//! called by the workflow layer. Compaction re-pin delivery is triggered
//! from `AgentEvent::CompactionStart`; [`PiParser`] owns Pi-private
//! overflow auto-retry fail-fast policy.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use loom_driver::agent::{
    AgentBackend, AgentSession, DEFAULT_HANDSHAKE_TIMEOUT_SECS, Idle, JsonlReader, ModelSelection,
    ProtocolError, SpawnConfig, ThinkingLevel,
};
use loom_driver::clock::{Clock, SystemClock};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{ChildStderr, ChildStdin, Command};
use tracing::{debug, error, info, warn};

use super::messages::{PiEnvelope, PiResponse, SetThinkingLevelCommand};
use super::parser::{AgentEndMode, PiParser};
use crate::skill::{NoNativeRegistrar, register_native_skills};
use crate::{apply_launcher_env, resolve_wrix_spawn_bin};

/// Probe id used for the startup `get_state` request. The id appears in
/// pi's response so the backend can correlate request/response without
/// blocking on intervening events.
const PROBE_REQUEST_ID: &str = "loom-pi-probe";

/// Request id used for the optional post-probe `set_model` request.
const SET_MODEL_REQUEST_ID: &str = "loom-pi-set-model";

/// Request id used for the optional post-probe `set_thinking_level` request.
const SET_THINKING_LEVEL_REQUEST_ID: &str = "loom-pi-set-thinking-level";

/// State fields Loom requires in a successful `get_state` probe. Missing or
/// mistyped fields indicate a protocol-shape mismatch and fail the startup
/// handshake.
const REQUIRED_STATE_FIELDS: &[&str] = &[
    "isStreaming",
    "isCompacting",
    "messageCount",
    "pendingMessageCount",
];

/// Wrix stderr markers proving the sandbox reached the container-start
/// boundary. Older shell launchers emit `Starting container` on the host just
/// before `podman run`; newer Rust launchers may only expose the in-container
/// entrypoint's `Network mode:` line after `podman run` succeeds. Either line
/// keeps image materialization out of Pi's RPC probe timeout without waiting
/// for a marker the selected launcher never prints.
const WRIX_CONTAINER_START_MARKERS: &[&str] = &["Starting container", "Network mode:"];

/// Upper bound for wrix image materialization + launcher setup. Separate
/// from the Pi probe budget: a cold image load may be slow, but it is not an
/// unresponsive Pi RPC process.
const WRIX_CONTAINER_START_TIMEOUT_SECS: u64 = 600;

/// Counter that distinguishes simultaneous spawn-config files inside the
/// same loom process. The pid component handles cross-process uniqueness.
static SPAWN_CONFIG_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Zero-sized marker for the pi-mono RPC backend.
///
/// Per the spec's static-dispatch design, all runtime state lives in the
/// spawned [`AgentSession`] and the [`SpawnConfig`] passed to
/// [`AgentBackend::spawn`] — the backend itself carries no fields. The type
/// parameter alone is what dispatches `<B: AgentBackend>` call sites in
/// `loom-workflow` (`run_agent::<PiBackend>(..)` versus
/// `run_agent::<ClaudeBackend>(..)`).
pub struct PiBackend;

impl PiBackend {
    pub async fn spawn_with_wrix_bin(
        config: &SpawnConfig,
        wrix_bin: &OsStr,
    ) -> Result<AgentSession<Idle>, ProtocolError> {
        Self::spawn_with_wrix_bin_and_agent_end_mode(
            config,
            wrix_bin,
            AgentEndMode::SessionComplete,
        )
        .await
    }

    pub async fn spawn_bridge(config: &SpawnConfig) -> Result<AgentSession<Idle>, ProtocolError> {
        let wrix_bin = resolve_wrix_spawn_bin(config);
        Self::spawn_bridge_with_wrix_bin(config, &wrix_bin).await
    }

    pub async fn spawn_bridge_with_wrix_bin(
        config: &SpawnConfig,
        wrix_bin: &OsStr,
    ) -> Result<AgentSession<Idle>, ProtocolError> {
        Self::spawn_with_wrix_bin_and_agent_end_mode(config, wrix_bin, AgentEndMode::AgentEnd).await
    }

    async fn spawn_with_wrix_bin_and_agent_end_mode(
        config: &SpawnConfig,
        wrix_bin: &OsStr,
        agent_end_mode: AgentEndMode,
    ) -> Result<AgentSession<Idle>, ProtocolError> {
        register_native_skills::<NoNativeRegistrar>(config)?;
        let spawn_config_path = write_spawn_config(config)?;

        let handshake_budget = config
            .handshake_timeout
            .unwrap_or_else(|| Duration::from_secs(DEFAULT_HANDSHAKE_TIMEOUT_SECS));
        info!(
            wrix = %wrix_bin.to_string_lossy(),
            spawn_config = %spawn_config_path.display(),
            handshake_timeout_secs = handshake_budget.as_secs(),
            "pi backend spawn",
        );

        let mut cmd = build_wrix_command(
            wrix_bin,
            config.profile_config.as_deref(),
            &spawn_config_path,
        );
        apply_launcher_env(&mut cmd, &config.launcher_env);
        // Needed for the stderr readiness boundary below. The marker is
        // emitted by wrix after image load/staging and before `podman run`.
        cmd.env("WRIX_VERBOSE", "1");

        spawn_with_handshake_after_wrix_start(
            cmd,
            config.model.as_ref(),
            config.thinking_level,
            handshake_budget,
            &SystemClock::new(),
            agent_end_mode,
        )
        .await
    }
}

impl AgentBackend for PiBackend {
    async fn spawn(config: &SpawnConfig) -> Result<AgentSession<Idle>, ProtocolError> {
        let wrix_bin = resolve_wrix_spawn_bin(config);
        Self::spawn_with_wrix_bin(config, &wrix_bin).await
    }

    fn compaction_repin(config: &SpawnConfig) -> Result<Option<String>, ProtocolError> {
        debug!(
            scratch_dir = %config.scratch_dir.display(),
            "pi compaction_start observed; reading scratch dir for re-pin payload",
        );
        build_repin_payload(&config.scratch_dir).map(Some)
    }
}

/// Read `prompt.txt` and `scratch.md` from the per-session scratch dir and
/// concatenate them into the `steer` payload. Same source files Claude
/// reads through `repin.sh` (see [`ScratchSession`]'s `repin.sh` in
/// `loom-driver/src/scratch.rs`); pi's transport is `steer` rather than a
/// JSON envelope, but the text content matches.
///
/// [`ScratchSession`]: loom_driver::scratch::ScratchSession
fn build_repin_payload(scratch_dir: &Path) -> Result<String, ProtocolError> {
    let prompt = std::fs::read_to_string(scratch_dir.join("prompt.txt"))?;
    let scratch = std::fs::read_to_string(scratch_dir.join("scratch.md"))?;
    Ok(format!("{prompt}\n\n{scratch}"))
}

/// `get_state` request body. Sent on stdin during the startup handshake
/// before any [`AgentSession`] is constructed.
#[derive(Serialize)]
struct GetStateCommand<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    id: &'a str,
}

/// Minimal `get_state.data` shape required by Loom's startup probe. Pi
/// includes additional fields (`model`, `thinkingLevel`, session metadata),
/// but these four pin the liveness-relevant state object without depending on
/// provider-specific model configuration.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateProbeData {
    is_streaming: bool,
    is_compacting: bool,
    message_count: u64,
    pending_message_count: u64,
}

/// `set_model` request body. Sent only when [`SpawnConfig::model`] is
/// populated; the wrapper never sees this — it is consumed by pi inside
/// the container.
#[derive(Serialize)]
struct SetModelCommand<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    id: &'a str,
    provider: &'a str,
    #[serde(rename = "modelId")]
    model_id: &'a str,
}

pub(crate) fn build_wrix_command(
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

/// Spawn the launcher [`Command`], drive the startup handshake (probe +
/// optional `set_model`), and return a session in the [`Idle`] state.
///
/// Public so integration tests under `loom-agent/tests/` can substitute a
/// mock pi binary in place of the real `wrix spawn` exec without going
/// through the process-env launcher override (Rust 2024 makes
/// `env::set_var` unsafe, and the workspace forbids `unsafe_code`).
/// Production callers go through [`PiBackend::spawn`].
pub async fn spawn_with_handshake(
    cmd: Command,
    model: Option<&ModelSelection>,
    thinking_level: Option<ThinkingLevel>,
    handshake_timeout: Duration,
    clock: &dyn Clock,
) -> Result<AgentSession<Idle>, ProtocolError> {
    spawn_with_handshake_inner(
        cmd,
        model,
        thinking_level,
        handshake_timeout,
        clock,
        false,
        AgentEndMode::SessionComplete,
    )
    .await
}

async fn spawn_with_handshake_after_wrix_start(
    cmd: Command,
    model: Option<&ModelSelection>,
    thinking_level: Option<ThinkingLevel>,
    handshake_timeout: Duration,
    clock: &dyn Clock,
    agent_end_mode: AgentEndMode,
) -> Result<AgentSession<Idle>, ProtocolError> {
    spawn_with_handshake_inner(
        cmd,
        model,
        thinking_level,
        handshake_timeout,
        clock,
        true,
        agent_end_mode,
    )
    .await
}

async fn spawn_with_handshake_inner(
    mut cmd: Command,
    model: Option<&ModelSelection>,
    thinking_level: Option<ThinkingLevel>,
    handshake_timeout: Duration,
    clock: &dyn Clock,
    wait_for_wrix_start: bool,
    agent_end_mode: AgentEndMode,
) -> Result<AgentSession<Idle>, ProtocolError> {
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    if wait_for_wrix_start {
        cmd.stderr(Stdio::piped());
    } else {
        cmd.stderr(Stdio::inherit());
    }
    cmd.kill_on_drop(true);

    let mut child = cmd.spawn().map_err(ProtocolError::Io)?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| ProtocolError::Io(io::Error::other("pi child stdin not piped")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ProtocolError::Io(io::Error::other("pi child stdout not piped")))?;

    if wait_for_wrix_start {
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ProtocolError::Io(io::Error::other("pi child stderr not piped")))?;
        wait_for_wrix_container_start(stderr, clock).await?;
    }

    let mut writer = BufWriter::new(stdin);
    let mut reader = JsonlReader::new(stdout);

    run_probe(&mut writer, &mut reader, handshake_timeout, clock).await?;

    if let Some(model) = model {
        run_set_model(&mut writer, &mut reader, model, handshake_timeout, clock).await?;
    }

    if let Some(level) = thinking_level {
        run_set_thinking_level(&mut writer, &mut reader, level, handshake_timeout, clock).await?;
    }

    let parser = PiParser::with_agent_end_mode(agent_end_mode);
    Ok(AgentSession::new(child, writer, reader, Box::new(parser)))
}

/// Wait for wrix to finish image materialization/staging and start the
/// container, then continue relaying stderr in the background. This keeps cold
/// `podman`/`skopeo` image work out of Pi's RPC probe timeout while preserving
/// the operator-visible startup log.
async fn wait_for_wrix_container_start(
    stderr: ChildStderr,
    clock: &dyn Clock,
) -> Result<(), ProtocolError> {
    let wait = await_wrix_container_start_marker(stderr);
    let budget = Duration::from_secs(WRIX_CONTAINER_START_TIMEOUT_SECS);
    let sleep = clock.sleep(budget);
    tokio::select! {
        result = wait => result,
        () = sleep => {
            warn!(
                stage = "container_start",
                budget_secs = budget.as_secs(),
                "wrix container startup timed out before Pi RPC probe",
            );
            Err(ProtocolError::HandshakeTimeout {
                stage: "container_start",
                after: budget,
            })
        }
    }
}

async fn await_wrix_container_start_marker(stderr: ChildStderr) -> Result<(), ProtocolError> {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(ProtocolError::UnexpectedEof);
        }
        relay_stderr_line(&line).await?;
        if is_wrix_container_start_marker(&line) {
            debug!("wrix container start marker observed; starting Pi RPC probe");
            tokio::spawn(relay_remaining_stderr(reader));
            return Ok(());
        }
    }
}

fn is_wrix_container_start_marker(line: &str) -> bool {
    WRIX_CONTAINER_START_MARKERS
        .iter()
        .any(|marker| line.contains(marker))
}

async fn relay_remaining_stderr(mut reader: BufReader<ChildStderr>) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                if let Err(err) = relay_stderr_line(&line).await {
                    warn!(error = ?err, "failed to relay wrix stderr");
                    break;
                }
            }
            Err(err) => {
                warn!(error = ?err, "failed to read wrix stderr");
                break;
            }
        }
    }
}

async fn relay_stderr_line(line: &str) -> Result<(), ProtocolError> {
    let mut stderr = tokio::io::stderr();
    stderr.write_all(line.as_bytes()).await?;
    stderr.flush().await?;
    Ok(())
}

/// Send `get_state` on stdin and wait for the matching response. Events
/// emitted before the response are observed but ignored — pi can interleave
/// telemetry around request handling, so the loop drains lines until the
/// correlated response arrives.
async fn run_probe(
    writer: &mut BufWriter<ChildStdin>,
    reader: &mut JsonlReader,
    budget: Duration,
    clock: &dyn Clock,
) -> Result<(), ProtocolError> {
    let cmd = GetStateCommand {
        kind: "get_state",
        id: PROBE_REQUEST_ID,
    };
    info!(id = PROBE_REQUEST_ID, "pi probe: sending get_state");
    write_command(writer, &cmd).await?;

    let resp = bounded_await_response(reader, PROBE_REQUEST_ID, budget, "probe", clock).await?;
    if !resp.success {
        error!(
            error = ?resp.error,
            "pi get_state probe failed",
        );
        return Err(ProtocolError::Unsupported);
    }

    validate_state_probe(&resp)?;

    info!("pi probe: get_state succeeded");
    Ok(())
}

/// Send `set_model` on stdin and wait for the matching response. A failure
/// response is a hard fail — Loom requires the requested model to take
/// effect before the workflow begins.
async fn run_set_model(
    writer: &mut BufWriter<ChildStdin>,
    reader: &mut JsonlReader,
    model: &ModelSelection,
    budget: Duration,
    clock: &dyn Clock,
) -> Result<(), ProtocolError> {
    let cmd = SetModelCommand {
        kind: "set_model",
        id: SET_MODEL_REQUEST_ID,
        provider: &model.provider,
        model_id: &model.model_id,
    };
    info!(
        id = SET_MODEL_REQUEST_ID,
        provider = %model.provider,
        model_id = %model.model_id,
        "pi handshake: sending set_model",
    );
    write_command(writer, &cmd).await?;

    let resp =
        bounded_await_response(reader, SET_MODEL_REQUEST_ID, budget, "set_model", clock).await?;
    if !resp.success {
        error!(
            error = ?resp.error,
            provider = %model.provider,
            model_id = %model.model_id,
            "pi set_model failed",
        );
        return Err(ProtocolError::Unsupported);
    }

    info!(
        provider = %model.provider,
        model_id = %model.model_id,
        "pi set_model succeeded",
    );
    Ok(())
}

/// Send `set_thinking_level` on stdin and wait for the matching response.
/// Best-effort per `specs/agent.md` § Functional 3: a failure response
/// logs a `warn!` and returns `Ok(())` so the handshake continues — providers
/// without thinking support (or pi builds that omit the command) degrade
/// silently. Transport-level errors (`HandshakeTimeout`, `Io`) propagate
/// because they indicate a broken pipe, not a rejected feature.
async fn run_set_thinking_level(
    writer: &mut BufWriter<ChildStdin>,
    reader: &mut JsonlReader,
    level: ThinkingLevel,
    budget: Duration,
    clock: &dyn Clock,
) -> Result<(), ProtocolError> {
    let cmd = SetThinkingLevelCommand {
        kind: "set_thinking_level",
        id: SET_THINKING_LEVEL_REQUEST_ID,
        level: level.as_str(),
    };
    info!(
        id = SET_THINKING_LEVEL_REQUEST_ID,
        level = level.as_str(),
        "pi handshake: sending set_thinking_level (best-effort)",
    );
    write_command(writer, &cmd).await?;

    let resp = bounded_await_response(
        reader,
        SET_THINKING_LEVEL_REQUEST_ID,
        budget,
        "set_thinking_level",
        clock,
    )
    .await?;
    if resp.success {
        info!(level = level.as_str(), "pi set_thinking_level succeeded",);
    } else {
        warn!(
            level = level.as_str(),
            error = ?resp.error,
            "pi set_thinking_level rejected — continuing without thinking override",
        );
    }
    Ok(())
}

/// Encode `payload` as JSONL and flush it to pi's stdin.
async fn write_command<T: Serialize>(
    writer: &mut BufWriter<ChildStdin>,
    payload: &T,
) -> Result<(), ProtocolError> {
    let mut line = serde_json::to_string(payload)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Read JSONL lines until one classifies as the `response` matching
/// `expected_id`. Other lines (events, unrelated responses, extension UI
/// requests) are observed and dropped — request/response correlation is
/// the only contract this loop enforces.
async fn await_response(
    reader: &mut JsonlReader,
    expected_id: &str,
) -> Result<PiResponse, ProtocolError> {
    loop {
        let line_owned = match reader.next_line().await? {
            Some(line) => line.to_owned(),
            None => return Err(ProtocolError::UnexpectedEof),
        };
        let env: PiEnvelope = serde_json::from_str(&line_owned)
            .map_err(|err| ProtocolError::invalid_protocol_line(&line_owned, err))?;
        if env.msg_type.as_deref() == Some("response") {
            let resp: PiResponse = serde_json::from_str(&line_owned)
                .map_err(|err| ProtocolError::invalid_protocol_line(&line_owned, err))?;
            if resp
                .id
                .as_ref()
                .is_some_and(|id| id.as_str() == expected_id)
            {
                return Ok(resp);
            }
            debug!(
                got = ?resp.id,
                want = %expected_id,
                "pi response id missing or mismatched — discarding",
            );
        } else {
            debug!(
                msg_type = ?env.msg_type,
                "pi handshake observed non-response line — discarding",
            );
        }
    }
}

/// [`await_response`] with a [`Clock`]-driven budget. Surfaces
/// [`ProtocolError::HandshakeTimeout`] when the budget elapses so loom
/// breaks out of a non-responsive pi process instead of blocking forever.
/// The reader is *not* re-used after timeout — the connection is torn down
/// by the caller (`spawn_with_handshake` returns the error and the child
/// drops, which kills the process via `kill_on_drop`). Uses
/// `clock.sleep(...)` in a `tokio::select!` rather than `Clock::timeout`
/// because the trait object surface (`&dyn Clock`) is not `Sized`, but
/// `Clock::timeout` carries a `Self: Sized` bound.
async fn bounded_await_response(
    reader: &mut JsonlReader,
    expected_id: &str,
    budget: Duration,
    stage: &'static str,
    clock: &dyn Clock,
) -> Result<PiResponse, ProtocolError> {
    let response = await_response(reader, expected_id);
    let sleep = clock.sleep(budget);
    tokio::select! {
        result = response => result,
        () = sleep => {
            warn!(
                stage,
                budget_secs = budget.as_secs(),
                "pi handshake timed out — agent process did not reply",
            );
            Err(ProtocolError::HandshakeTimeout {
                stage,
                after: budget,
            })
        }
    }
}

/// Validate the successful `get_state` response used as the Pi startup
/// liveness/protocol-shape probe. Pi 0.73's `get_commands` reports slash
/// commands/templates/skills, so the backend uses `get_state` for a stable
/// built-in RPC round-trip instead.
fn validate_state_probe(resp: &PiResponse) -> Result<(), ProtocolError> {
    let data = resp.data.as_ref().ok_or_else(|| {
        error!("pi get_state response missing `data`");
        ProtocolError::Unsupported
    })?;
    let state: StateProbeData = serde_json::from_value(data.clone()).map_err(|err| {
        error!(
            error = %err,
            required = ?REQUIRED_STATE_FIELDS,
            data = %data,
            "pi get_state probe has unexpected shape — version mismatch",
        );
        ProtocolError::Unsupported
    })?;
    debug!(
        is_streaming = state.is_streaming,
        is_compacting = state.is_compacting,
        message_count = state.message_count,
        pending_message_count = state.pending_message_count,
        "pi get_state probe shape validated",
    );
    Ok(())
}

/// Serialize `config` as JSON and write it to a uniquely-named tempfile
/// under the system temp dir. The path is handed to `wrix spawn
/// --spawn-config`; the wrapper reads it back and ignores any unknown
/// fields (`model` is consumed by the host-side backend, not the wrapper).
fn write_spawn_config(config: &SpawnConfig) -> Result<PathBuf, ProtocolError> {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let counter = SPAWN_CONFIG_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("loom-{pid}-{counter}.json"));
    write_spawn_config_to(&path, config)?;
    Ok(path)
}

fn write_spawn_config_to(path: &Path, config: &SpawnConfig) -> Result<(), ProtocolError> {
    let json = serde_json::to_vec(config)?;
    std::fs::write(path, json).map_err(ProtocolError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom_driver::agent::RePinContent;
    use loom_driver::config::{LoomConfig, Phase};
    use loom_events::ParsedAgentEvent;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    fn mock_pi_path() -> PathBuf {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for ancestor in manifest_dir.ancestors() {
            let candidate = ancestor.join("tests/mock-pi/pi.sh");
            if candidate.is_file() {
                return candidate;
            }
        }
        panic!(
            "could not locate tests/mock-pi/pi.sh above {} — neither \
             dev-tree nor nix-sandbox layout matched.",
            manifest_dir.display(),
        );
    }

    fn mock_command(mode: &str) -> Command {
        let mut cmd = Command::new("bash");
        cmd.arg(mock_pi_path()).arg(mode);
        cmd
    }

    fn bash_path() -> PathBuf {
        let path_var = std::env::var_os("PATH").expect("PATH set");
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("bash");
            if candidate.is_file() {
                return candidate;
            }
        }
        panic!("bash not found in PATH");
    }

    fn install_wrix_pi_shim(dir: &Path) -> PathBuf {
        install_wrix_pi_shim_with_startup_line(dir, "[wrix] Starting container (mock)...")
    }

    fn install_wrix_pi_shim_with_startup_line(dir: &Path, startup_line: &str) -> PathBuf {
        let shim = dir.join("wrix");
        let bash = bash_path();
        let startup_line = startup_line.replace('\'', r"'\''");
        let body = format!(
            "#!{bash}\n\
             set -euo pipefail\n\
             : \"${{MOCK_PI:?}}\"\n\
             : \"${{MOCK_PI_MODE:?}}\"\n\
             printf '%s\\n' '{startup_line}' >&2\n\
             exec '{bash}' \"$MOCK_PI\" \"$MOCK_PI_MODE\"\n",
            bash = bash.display(),
        );
        std::fs::write(&shim, body).expect("write wrix shim");
        let mut perm = std::fs::metadata(&shim).expect("stat shim").permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&shim, perm).expect("chmod shim");
        shim
    }

    async fn spawn_from_phase_config(config_toml: &str, mock_mode: &str) -> AgentSession<Idle> {
        let wrix_dir = tempfile::tempdir().expect("tempdir");
        let wrix = install_wrix_pi_shim(wrix_dir.path());
        let cfg = LoomConfig::from_toml_str(config_toml).expect("parse config");
        let selection = cfg.agent_for(Phase::Loop).expect("resolve loop agent");
        let mut spawn = sample_config(None);
        spawn.handshake_timeout = Some(TEST_HANDSHAKE_BUDGET);
        spawn.launcher_env = vec![
            (
                "MOCK_PI".to_string(),
                mock_pi_path().to_string_lossy().into_owned(),
            ),
            ("MOCK_PI_MODE".to_string(), mock_mode.to_string()),
        ];
        selection.apply_to_spawn_config(&mut spawn, cfg.direct_output_limits());

        let session = PiBackend::spawn_with_wrix_bin(&spawn, wrix.as_os_str())
            .await
            .expect("spawn through wrix shim");
        drop(wrix_dir);
        session
    }

    fn sample_repin() -> RePinContent {
        RePinContent {
            orientation: "loom loop @ lm-test".to_string(),
            pinned_context: "Spec: specs/agent.md".to_string(),
            partial_bodies: vec!["partial alpha".to_string()],
        }
    }

    const PLANNING_INTERVIEW_PROMPT: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/planning_prompt_interview_modes.md"
    ));
    const POLISH_MODE_DEFINITION: &str = "- polish / do-a-polish: report-only mode. Review the proposed wording and report suggested edits, but do not modify files or apply edits unless the human explicitly asks you to make the edits.";
    const ONE_BY_ONE_MODE_DEFINITION: &str = "- one-by-one: ask exactly one design question per turn, then wait for the human's answer before asking the next question or changing topics.";

    fn json_string_body(value: &str) -> String {
        let encoded = serde_json::to_string(value).expect("encode string");
        encoded
            .strip_prefix('"')
            .and_then(|body| body.strip_suffix('"'))
            .expect("serde_json string has quotes")
            .to_string()
    }

    /// Mock-pi scenarios all reply within ~50ms; 5 s is comfortably above
    /// any legitimate jitter without making a hung scenario stall the suite.
    const TEST_HANDSHAKE_BUDGET: Duration = Duration::from_secs(5);

    fn sample_config(model: Option<ModelSelection>) -> SpawnConfig {
        SpawnConfig {
            image_ref: "localhost/wrix-test:pi".to_string(),
            image_source: PathBuf::from("/nix/store/zzz-wrix-test-pi.tar"),
            image_source_kind: Some(loom_driver::agent::ImageSourceKind::NixDescriptor),
            wrix_launcher: None,
            profile_config: Some(PathBuf::from("/nix/store/wrix-test-pi-profile-config.json")),
            workspace: PathBuf::from("/workspace"),
            env: vec![("WRIX_AGENT".into(), "pi".into())],
            mounts: vec![],
            initial_prompt: "hello pi".to_string(),
            agent_args: vec![],
            repin: sample_repin(),
            skills: None,
            scratch_dir: PathBuf::from("/workspace/.loom/scratch/test"),
            model_id: None,
            model,
            thinking_level: None,
            output_limits: None,
            shutdown_grace: None,
            denied_tools: Vec::new(),
            handshake_timeout: None,
            stall_warn_interval: None,
            launcher_env: Vec::new(),
        }
    }

    #[test]
    fn wrix_container_start_marker_accepts_legacy_and_entrypoint_lines() {
        assert!(is_wrix_container_start_marker(
            "[wrix] Starting container (cpus=8, memory=8192m)..."
        ));
        assert!(is_wrix_container_start_marker(
            "Network mode: open (local-network baseline enforced; firewall=nft)"
        ));
        assert!(!is_wrix_container_start_marker("image already present"));
    }

    #[tokio::test]
    async fn spawn_through_wrix_accepts_entrypoint_network_marker() {
        let wrix_dir = tempfile::tempdir().expect("tempdir");
        let wrix = install_wrix_pi_shim_with_startup_line(
            wrix_dir.path(),
            "Network mode: open (local-network baseline enforced; firewall=nft)",
        );
        let mut spawn = sample_config(None);
        spawn.handshake_timeout = Some(TEST_HANDSHAKE_BUDGET);
        spawn.launcher_env = vec![
            (
                "MOCK_PI".to_string(),
                mock_pi_path().to_string_lossy().into_owned(),
            ),
            ("MOCK_PI_MODE".to_string(), "happy-path".to_string()),
        ];

        let session = PiBackend::spawn_with_wrix_bin(&spawn, wrix.as_os_str())
            .await
            .expect("spawn should treat entrypoint Network mode line as container start");
        let mut session = session.prompt("hi").await.expect("prompt ok");
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF"),
            }
        }
        drop(wrix_dir);
    }

    #[test]
    fn wrix_command_uses_profile_config_before_spawn() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spawn_config_path = dir.path().join("loom-spawn.json");
        let profile_config = Path::new("/nix/store/wrix-rust-pi-profile-config.json");
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

    // -- write_spawn_config -----------------------------------------------

    #[test]
    fn write_spawn_config_round_trips_through_json() {
        let cfg = sample_config(Some(ModelSelection {
            provider: "deepseek".into(),
            model_id: "deepseek-v3".into(),
        }));
        let path = write_spawn_config(&cfg).expect("write");
        let bytes = std::fs::read(&path).expect("read");
        let decoded: SpawnConfig = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(decoded.image_ref, cfg.image_ref);
        assert_eq!(decoded.image_source, cfg.image_source);
        assert_eq!(decoded.initial_prompt, cfg.initial_prompt);
        let model = decoded.model.expect("model present");
        assert_eq!(model.provider, "deepseek");
        assert_eq!(model.model_id, "deepseek-v3");
        let _ = std::fs::remove_file(&path);
    }

    // -- test_pi_startup_probe --------------------------------------------

    #[tokio::test]
    async fn startup_probe_succeeds_when_get_state_shape_is_valid() {
        let session = spawn_with_handshake(
            mock_command("happy-path"),
            None,
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await
        .expect("probe should succeed");
        // Drive a prompt to confirm the session is wired and the mock keeps
        // running past the probe.
        let mut session = session.prompt("ping").await.expect("prompt ok");
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF"),
            }
        }
    }

    #[tokio::test]
    async fn startup_probe_fails_fast_when_get_state_shape_is_invalid() {
        let result = spawn_with_handshake(
            mock_command("probe-bad-state"),
            None,
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await;
        match result {
            Err(ProtocolError::Unsupported) => {}
            Err(other) => panic!("expected Unsupported, got {other:?}"),
            Ok(_) => panic!("probe should have failed"),
        }
    }

    // -- test_pi_rpc_command_sending --------------------------------------

    #[tokio::test]
    async fn driver_sends_prompt_as_jsonl_line() {
        let session = spawn_with_handshake(
            mock_command("echo-prompt"),
            None,
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await
        .expect("spawn");
        let mut session = session.prompt("HELLO_PROMPT").await.expect("prompt ok");

        let mut saw_echo = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains("HELLO_PROMPT") {
                        saw_echo = true;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF"),
            }
        }
        assert!(saw_echo, "mock did not echo the prompt");
    }

    #[tokio::test]
    async fn malformed_json_line_is_skipped_and_stream_continues() {
        let session = spawn_with_handshake(
            mock_command("malformed-then-happy"),
            None,
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await
        .expect("spawn");
        let mut session = session.prompt("hi").await.expect("prompt ok");

        let mut saw_valid_event_after_bad_line = false;
        loop {
            match session
                .next_event()
                .await
                .expect("malformed pi stdout line should be skipped")
            {
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains("after malformed line") {
                        saw_valid_event_after_bad_line = true;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { exit_code, .. }) => {
                    assert_eq!(exit_code, 0);
                    break;
                }
                Some(_) => continue,
                None => panic!("unexpected EOF"),
            }
        }
        assert!(
            saw_valid_event_after_bad_line,
            "valid event after malformed line was not observed",
        );
    }

    // -- test_pi_supports_steering ----------------------------------------

    #[tokio::test]
    async fn driver_steers_mid_session_and_mock_observes_payload() {
        let session = spawn_with_handshake(
            mock_command("steering"),
            None,
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await
        .expect("spawn");
        let mut session = session.prompt("first prompt").await.expect("prompt ok");

        // Drain events from the first turn until turn_end so the session is
        // ready for a steer.
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::TurnEnd) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before first TurnEnd"),
            }
        }

        session
            .steer("STEERED_TEXT")
            .await
            .expect("steer should succeed");

        let mut saw_steer_echo = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains("STEERED_TEXT") {
                        saw_steer_echo = true;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before SessionComplete"),
            }
        }
        assert!(saw_steer_echo, "mock did not observe the steer payload");
    }

    // -- test_pi_compaction_repin -----------------------------------------

    #[tokio::test]
    async fn driver_repins_on_compaction_start_via_steer() {
        let session = spawn_with_handshake(
            mock_command("compaction"),
            None,
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await
        .expect("spawn");
        let repin_text = "REPIN_PAYLOAD_TEXT";
        let mut session = session.prompt("kickoff").await.expect("prompt ok");

        // Drive events until CompactionStart arrives — at that point the
        // workflow layer (here represented by the test) sends a steer
        // carrying the re-pin payload.
        let mut sent_repin = false;
        let mut saw_repin_echo = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::CompactionStart { .. }) if !sent_repin => {
                    session.steer(repin_text).await.expect("steer ok");
                    sent_repin = true;
                }
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains(repin_text) {
                        saw_repin_echo = true;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before SessionComplete"),
            }
        }
        assert!(sent_repin, "compaction_start was not observed");
        assert!(saw_repin_echo, "mock did not echo the re-pin payload");
    }

    #[tokio::test]
    async fn on_compaction_start_steers_concatenated_scratch_files() {
        let scratch = tempfile::tempdir().expect("tempdir");
        std::fs::write(scratch.path().join("prompt.txt"), "PROMPT_FILE_BODY")
            .expect("write prompt.txt");
        std::fs::write(scratch.path().join("scratch.md"), "SCRATCH_FILE_BODY")
            .expect("write scratch.md");

        let mut config = sample_config(None);
        config.scratch_dir = scratch.path().to_path_buf();

        let session = spawn_with_handshake(
            mock_command("compaction"),
            None,
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await
        .expect("spawn");
        let mut session = session.prompt("kickoff").await.expect("prompt ok");

        let mut handler_called = false;
        let mut saw_prompt_echo = false;
        let mut saw_scratch_echo = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::CompactionStart { .. }) if !handler_called => {
                    PiBackend::on_compaction_start(&mut session, &config)
                        .await
                        .expect("on_compaction_start ok");
                    handler_called = true;
                }
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains("PROMPT_FILE_BODY") {
                        saw_prompt_echo = true;
                    }
                    if text.contains("SCRATCH_FILE_BODY") {
                        saw_scratch_echo = true;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before SessionComplete"),
            }
        }
        assert!(handler_called, "compaction_start was not observed");
        assert!(
            saw_prompt_echo,
            "prompt.txt content missing from steer payload"
        );
        assert!(
            saw_scratch_echo,
            "scratch.md content missing from steer payload"
        );
    }

    #[tokio::test]
    async fn driver_repins_interview_modes_on_compaction_start_via_steer() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let scratch_body = "## Scratchpad\n- decisions recorded after compaction started\n";
        std::fs::write(scratch.path().join("prompt.txt"), PLANNING_INTERVIEW_PROMPT)
            .expect("write prompt.txt");
        std::fs::write(scratch.path().join("scratch.md"), scratch_body).expect("write scratch.md");

        let mut config = sample_config(None);
        config.scratch_dir = scratch.path().to_path_buf();
        let expected_payload = build_repin_payload(scratch.path()).expect("build payload");
        let expected_wire_payload = json_string_body(&expected_payload);
        let prompt_wire = json_string_body(PLANNING_INTERVIEW_PROMPT);
        let scratch_wire = json_string_body(scratch_body);

        let session = spawn_with_handshake(
            mock_command("compaction"),
            None,
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await
        .expect("spawn");
        let mut session = session.prompt("kickoff").await.expect("prompt ok");

        let mut handler_called = false;
        let mut echoed_repin = String::new();
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::CompactionStart { .. }) if !handler_called => {
                    PiBackend::on_compaction_start(&mut session, &config)
                        .await
                        .expect("on_compaction_start ok");
                    handler_called = true;
                }
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains(&expected_wire_payload) {
                        echoed_repin = text;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF before SessionComplete"),
            }
        }
        assert!(handler_called, "compaction_start was not observed");
        assert!(
            echoed_repin.contains(&prompt_wire),
            "full prompt.txt payload missing from steer echo: {echoed_repin}",
        );
        assert!(
            echoed_repin.contains(POLISH_MODE_DEFINITION),
            "polish mode definition missing from steer echo: {echoed_repin}",
        );
        assert!(
            echoed_repin.contains(ONE_BY_ONE_MODE_DEFINITION),
            "one-by-one mode definition missing from steer echo: {echoed_repin}",
        );
        let prompt_pos = echoed_repin.find(&prompt_wire).expect("prompt position");
        let scratch_pos = echoed_repin.find(&scratch_wire).expect("scratch position");
        assert!(
            prompt_pos < scratch_pos,
            "prompt.txt payload must precede scratch.md payload: {echoed_repin}",
        );
    }

    #[test]
    fn build_repin_payload_concatenates_prompt_then_scratch() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("prompt.txt"), "the prompt").expect("write prompt.txt");
        std::fs::write(dir.path().join("scratch.md"), "the scratch").expect("write scratch.md");
        let payload = build_repin_payload(dir.path()).expect("build payload");
        assert_eq!(payload, "the prompt\n\nthe scratch");
    }

    #[test]
    fn build_repin_payload_surfaces_io_error_when_files_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = build_repin_payload(dir.path()).expect_err("missing files must error");
        match err {
            ProtocolError::Io(_) => {}
            other => panic!("expected Io error, got {other:?}"),
        }
    }

    // -- test_pi_set_model_from_phase_config ------------------------------

    #[tokio::test]
    async fn set_model_from_phase_config_reaches_mock_pi() {
        let session = spawn_from_phase_config(
            "[phase.loop]\nagent.backend = \"pi\"\nagent.provider = \"deepseek\"\nagent.model_id = \"deepseek-v3\"\n",
            "set-model",
        )
        .await;

        // The mock echoes provider/modelId via a MessageDelta on the first
        // prompt so the test can assert the values reached pi.
        let mut session = session.prompt("hi").await.expect("prompt ok");
        let mut saw_provider = false;
        let mut saw_model_id = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains("deepseek") {
                        saw_provider = true;
                    }
                    if text.contains("deepseek-v3") {
                        saw_model_id = true;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF"),
            }
        }
        assert!(saw_provider, "mock did not observe provider");
        assert!(saw_model_id, "mock did not observe model_id");
    }

    #[tokio::test]
    async fn set_model_rejection_from_pi_hard_fails_handshake() {
        let model = ModelSelection {
            provider: "deepseek".into(),
            model_id: "deepseek-v3".into(),
        };
        let result = spawn_with_handshake(
            mock_command("set-model-reject"),
            Some(&model),
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await;
        match result {
            Err(ProtocolError::Unsupported) => {}
            Err(other) => panic!("expected Unsupported, got {other:?}"),
            Ok(_) => panic!("set_model rejection should hard-fail the handshake"),
        }
    }

    // -- test_pi_set_thinking_level_from_phase_config ---------------------

    /// Driver-sends-when-config-set: the resolved phase config installs
    /// `thinking_level: Some(_)` on the spawn config before `PiBackend` issues
    /// `set_thinking_level` after the probe. The mock acks the command and
    /// echoes the level back via a `message_delta`, so the test verifies the
    /// wire token (`high`) reached pi.
    #[tokio::test]
    async fn set_thinking_level_from_phase_config_reaches_mock_pi() {
        let session = spawn_from_phase_config(
            "[phase.loop]\nagent.backend = \"pi\"\nagent.thinking_level = \"high\"\n",
            "set-thinking-level",
        )
        .await;

        let mut session = session.prompt("hi").await.expect("prompt ok");
        let mut saw_level = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains("thinking:high") {
                        saw_level = true;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF"),
            }
        }
        assert!(saw_level, "mock did not observe thinking_level");
    }

    /// With `thinking_level: None`, the driver must skip
    /// `set_thinking_level` entirely. `happy-path` only consumes a
    /// `prompt` after the probe — any extra post-probe command would
    /// desynchronize it and fail this test before the `LOOM_COMPLETE`
    /// delta arrives.
    #[tokio::test]
    async fn set_thinking_level_skipped_when_config_none() {
        let session = spawn_with_handshake(
            mock_command("happy-path"),
            None,
            None,
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await
        .expect("spawn without thinking_level");

        let mut session = session.prompt("hi").await.expect("prompt ok");
        let mut saw_loom_complete = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains("LOOM_COMPLETE") {
                        saw_loom_complete = true;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF"),
            }
        }
        assert!(
            saw_loom_complete,
            "happy-path delta missing — driver may have injected a set_thinking_level"
        );
    }

    /// Driver-tolerates-pi-rejection: when pi answers
    /// `set_thinking_level` with `success: false`, the driver logs a
    /// warn and continues — the spawn must still return an `Idle`
    /// session ready for `prompt`. The mock's `set-thinking-level-reject`
    /// mode emits an error response, then services a follow-up prompt
    /// to confirm the handshake did not abort.
    #[tokio::test]
    async fn set_thinking_level_tolerates_pi_rejection() {
        let session = spawn_with_handshake(
            mock_command("set-thinking-level-reject"),
            None,
            Some(ThinkingLevel::Medium),
            TEST_HANDSHAKE_BUDGET,
            &SystemClock::new(),
        )
        .await
        .expect("spawn must succeed even when pi rejects set_thinking_level");

        let mut session = session.prompt("hi").await.expect("prompt ok");
        let mut saw_rejection_echo = false;
        loop {
            match session.next_event().await.expect("event ok") {
                Some(ParsedAgentEvent::TextDelta { text, .. }) => {
                    if text.contains("thinking-rejected:medium") {
                        saw_rejection_echo = true;
                    }
                }
                Some(ParsedAgentEvent::SessionComplete { .. }) => break,
                Some(_) => continue,
                None => panic!("unexpected EOF"),
            }
        }
        assert!(
            saw_rejection_echo,
            "driver aborted instead of treating set_thinking_level rejection as advisory"
        );
    }

    // -- command struct serialization -------------------------------------

    /// `get_state` body serializes with the two documented fields
    /// (`type` discriminator, request `id`). The startup probe sends this
    /// over stdin; a rename here would silently break pi's classification
    /// of the probe request as a command rather than a stray event.
    #[test]
    fn get_state_command_serializes_to_expected_shape() {
        let cmd = GetStateCommand {
            kind: "get_state",
            id: PROBE_REQUEST_ID,
        };
        let json = serde_json::to_string(&cmd).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(v["type"], "get_state");
        assert_eq!(v["id"], PROBE_REQUEST_ID);
    }

    /// `set_model` body serializes with all four documented fields,
    /// including the `modelId` camelCase rename. Pi rejects unknown
    /// fields silently for this command, so a missed rename would
    /// produce a no-op handshake that the driver mistakes for success.
    #[test]
    fn set_model_command_serializes_to_expected_shape() {
        let cmd = SetModelCommand {
            kind: "set_model",
            id: SET_MODEL_REQUEST_ID,
            provider: "deepseek",
            model_id: "deepseek-v3",
        };
        let json = serde_json::to_string(&cmd).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(v["type"], "set_model");
        assert_eq!(v["id"], SET_MODEL_REQUEST_ID);
        assert_eq!(v["provider"], "deepseek");
        assert_eq!(v["modelId"], "deepseek-v3");
        assert!(
            v.get("model_id").is_none(),
            "snake_case must not appear on the wire — pi expects camelCase modelId"
        );
    }
}
