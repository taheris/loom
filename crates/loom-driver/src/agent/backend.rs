use std::path::{Path, PathBuf};
use std::time::Duration;

use loom_skills::registry::RegisteredSkills;
use serde::{Deserialize, Serialize};

use super::error::ProtocolError;
use super::repin::RePinContent;
use super::session::{Active, AgentSession, Idle};

/// Configuration `loom` hands to `wrix spawn` describing how to launch
/// the per-bead container and what initial agent state to install.
///
/// Serialized to a JSON file (`/tmp/loom-<id>.json`) and read back by
/// `wrix spawn --spawn-config <file>` — this is the single stable
/// boundary between loom and the wrapper. `env` is an explicit allowlist;
/// the wrapper never inherits the host environment wholesale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnConfig {
    /// Podman image reference (e.g. `localhost/wrix-rust:<hash>`) — the
    /// argument passed to `podman run`. Populated by loom from the
    /// profile-image manifest at dispatch time.
    pub image_ref: String,
    /// Nix store path to an image source that materializes `image_ref`.
    /// The wrapper installs it before `podman run` when needed. Empty means
    /// no per-launch image-source override and is omitted from the JSON.
    #[serde(default, skip_serializing_if = "path_is_empty")]
    pub image_source: PathBuf,
    /// Source kind for [`SpawnConfig::image_source`]. Wrix requires this
    /// whenever an `image_source` override is non-empty so launchers never
    /// infer the install path from filenames or platform defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_source_kind: Option<ImageSourceKind>,
    /// Wrix ProfileConfig path selected from the same manifest entry as
    /// [`SpawnConfig::image_ref`]. Host-side backends pass it as a launcher
    /// flag rather than serializing it into the spawn-config JSON; the
    /// ProfileConfig carries the matching image digest for wrix install
    /// preflight.
    #[serde(skip)]
    pub profile_config: Option<PathBuf>,
    pub workspace: PathBuf,
    pub env: Vec<(String, String)>,
    /// Per-spawn bind mounts beyond [`SpawnConfig::workspace`]. Loom uses this
    /// to project the `wrix-beads` dolt socket into every bead container at
    /// `/workspace/.wrix/dolt.sock` (replacing the host-side hardlink shim
    /// in [`crate::git::GitClient`]) and, when configured, the shared sccache
    /// directory at the configured container path. Additive to the resolved
    /// profile's `mounts`; see `specs/agent.md` § SpawnConfig.
    ///
    /// Single-file mounts (sockets) and directory mounts both pass through
    /// virtiofs on Linux. On Darwin, the wrix sandbox classifier accepts
    /// directories (staged + copied at launch) and regular files
    /// (copy-from-parent-dir), but rejects Unix-socket `host_path` entries at
    /// launch — Apple's VirtioFS does not pass socket operations across the
    /// VM boundary. Callers that emit a socket mount on Darwin will see the
    /// launcher exit non-zero with a clear error naming the offending
    /// `host_path`; route the same resource over TCP for the Darwin path.
    ///
    /// Skipped during serialization when empty so existing wrapper fixtures
    /// round-trip identically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<MountSpec>,
    pub initial_prompt: String,
    pub agent_args: Vec<String>,
    pub repin: RePinContent,
    /// Host-side materialized skill registry plus selected disclosure mode.
    /// Prompt-disclosure backends rely on [`SpawnConfig::initial_prompt`]
    /// already carrying readable paths, while native-capable backends consult
    /// this field during setup. It is skipped from the wrix spawn-config JSON
    /// because the wrapper and in-container Direct runner do not need the
    /// host-side registry payload.
    #[serde(skip)]
    pub skills: Option<RegisteredSkills>,
    /// Pre-populated `.loom/scratch/<key>/` for this session. Owned
    /// by the workflow code through a [`ScratchSession`] guard whose
    /// lifetime spans the spawn; backends read `repin.sh` and
    /// `claude-settings.json` from here and write their own
    /// `spawn-config.json` alongside. Spec: `harness.md` § Compaction
    /// Recovery.
    ///
    /// [`ScratchSession`]: crate::scratch::ScratchSession
    #[serde(default)]
    pub scratch_dir: PathBuf,
    /// Optional post-spawn model override consumed by the host-side backend
    /// (currently only [`PiBackend`](crate::agent::AgentBackend) — claude
    /// receives its model via CLI flags). When present, the pi backend sends
    /// a `set_model` RPC after the startup probe; failure is hard-fail.
    /// Skipped during serialization when `None` so the wrapper's input JSON
    /// remains identical to existing fixtures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSelection>,
    /// Optional post-spawn reasoning-effort override consumed by the pi
    /// backend (claude has no analog). When present, the pi backend sends a
    /// best-effort `set_thinking_level` RPC after the startup probe (and
    /// after [`SpawnConfig::model`], if set); pi rejection logs a `warn!`
    /// and the handshake continues — providers that do not support
    /// thinking-effort levels degrade silently per [`AgentBackend`]'s
    /// graceful-degradation contract. Skipped during serialization when
    /// `None` so existing wrapper fixtures round-trip identically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
    /// Inline-output cap for the Direct backend, resolved from `[direct]` in
    /// `loom.toml` at dispatch time. Carries `max_inline_bytes`, the byte
    /// budget above which content-returning Direct tools offload to the
    /// scratch offload directory (see `specs/agent.md` § Direct Output
    /// Bounding). Direct-only: the Pi and Claude backends own their own
    /// transcripts and never consult this field, so it is `None` for them.
    /// Skipped during serialization when `None` so existing wrapper fixtures
    /// round-trip identically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_limits: Option<OutputLimits>,
    /// Grace window the workflow's `after_session_complete` hook waits for
    /// the agent to exit on its own before escalating signals. Currently
    /// consumed only by [`ClaudeBackend`](crate::agent::AgentBackend) — pi
    /// exits naturally on `agent_end` so the field is unused for that
    /// backend. `None` means the backend's own default applies. Skipped
    /// during serialization when `None` so wrappers built before this
    /// field round-trip identically.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "duration_secs_opt"
    )]
    pub shutdown_grace: Option<Duration>,
    /// Maximum time the pi handshake (probe + optional `set_model`) is
    /// allowed to take before returning [`ProtocolError::HandshakeTimeout`].
    /// Claude has no host-side handshake, so this field is unused there.
    /// `None` means the backend's [`DEFAULT_HANDSHAKE_TIMEOUT_SECS`] applies.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "duration_secs_opt"
    )]
    pub handshake_timeout: Option<Duration>,
    /// Cadence for the run-loop stall watchdog: when no agent event arrives
    /// within this window, [`run_agent`] emits a `warn!` line and keeps
    /// waiting. `Some(Duration::ZERO)` disables the watchdog explicitly;
    /// `None` means the workflow's [`DEFAULT_STALL_WARN_SECS`] applies.
    ///
    /// [`run_agent`]: ../../../loom_workflow/fn.run_agent.html
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "duration_secs_opt"
    )]
    pub stall_warn_interval: Option<Duration>,
    /// Host-side environment loom sets on the `wrix spawn` **launcher**
    /// process (not the in-container env — that is [`SpawnConfig::env`]).
    /// Carries `WRIX_DEPLOY_KEY` / `WRIX_SIGNING_KEY` pointing at the
    /// host key paths so `wrix spawn` can bind-mount the keys into the
    /// bead container; the host paths never cross the boundary (wrix
    /// `specs/security.md` § Credential Surfaces). `#[serde(skip)]`: this
    /// never reaches the wrapper's spawn-config JSON — it is applied to the
    /// child process environment by the backend before `wrix spawn` runs,
    /// and keeping it out of the on-disk JSON avoids leaking host key paths
    /// into a world-readable file.
    #[serde(skip)]
    pub launcher_env: Vec<(String, String)>,
}

/// Wrix image source install path selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageSourceKind {
    /// Linux archive-less image descriptor consumed by the wrix installer.
    NixDescriptor,
    /// Docker/OCI archive consumed by tar-loadable platform installers.
    DockerArchive,
}

impl ImageSourceKind {
    /// Stable wire token used in SpawnConfig and profile-image manifests.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NixDescriptor => "nix-descriptor",
            Self::DockerArchive => "docker-archive",
        }
    }
}

fn path_is_empty(path: &Path) -> bool {
    path.as_os_str().is_empty()
}

/// Env var name set by the driver in every bead container's
/// [`SpawnConfig::env`] allowlist; checked at CLI entry to refuse nested-loom
/// invocations (`specs/harness.md` § Nested-Loom Guard).
pub const LOOM_INSIDE_ENV: &str = "LOOM_INSIDE";

/// Append `LOOM_INSIDE=1` to a [`SpawnConfig::env`] allowlist if not already
/// present. Idempotent so dispatch helpers can apply it without first
/// checking, and downstream code can re-apply it without duplicating the
/// entry.
pub fn set_loom_inside(env: &mut Vec<(String, String)>) {
    if env.iter().any(|(k, _)| k == LOOM_INSIDE_ENV) {
        return;
    }
    env.push((LOOM_INSIDE_ENV.to_string(), "1".to_string()));
}

/// Default budget for the pi handshake (probe + optional `set_model`). Used
/// when [`SpawnConfig::handshake_timeout`] is `None`.
pub const DEFAULT_HANDSHAKE_TIMEOUT_SECS: u64 = 30;

/// Default cadence for the [`run_agent`] stall watchdog. Used when
/// [`SpawnConfig::stall_warn_interval`] is `None`.
///
/// [`run_agent`]: ../../../loom_workflow/fn.run_agent.html
pub const DEFAULT_STALL_WARN_SECS: u64 = 60;

mod duration_secs_opt {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<Duration>, ser: S) -> Result<S::Ok, S::Error> {
        value.map(|d| d.as_secs()).serialize(ser)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Duration>, D::Error> {
        Option::<u64>::deserialize(de).map(|opt| opt.map(Duration::from_secs))
    }
}

/// Single bind mount entry in [`SpawnConfig::mounts`].
///
/// `host_path` is the absolute host-side path; `container_path` is the
/// absolute path the container sees. `read_only = true` requests a `ro`
/// bind. The wrapper resolves both paths verbatim — single-file mounts
/// (e.g. the dolt unix socket) and directory mounts use the same shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountSpec {
    pub host_path: PathBuf,
    pub container_path: PathBuf,
    pub read_only: bool,
}

/// Inline-output cap carried on [`SpawnConfig::output_limits`] for the Direct
/// backend (`specs/agent.md` § Direct Output Bounding).
///
/// `max_inline_bytes` is the raw-UTF-8 byte budget a content-returning Direct
/// tool (`Read`, `Bash`, `Grep`, `Glob`) may place inline before offloading
/// the full payload to the scratch offload directory and returning a
/// reference. Resolved from `[direct] max_inline_bytes` in `loom.toml`. The
/// wrapper does not consult this field; only the in-container Direct runner
/// does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputLimits {
    pub max_inline_bytes: usize,
}

/// Per-session model override: pi RPC's `set_model { provider, modelId }`.
///
/// Lives on [`SpawnConfig`] rather than a backend-specific config object so
/// the [`AgentBackend::spawn`] trait surface stays a single-argument call.
/// The wrapper ignores this field; it is consumed only by host-side backends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSelection {
    pub provider: String,
    pub model_id: String,
}

/// Per-session reasoning-effort knob sent by pi RPC's
/// `set_thinking_level { level }`. The level set matches the pi-mono protocol
/// (`specs/agent.md` Pi command table). The driver sends this
/// best-effort after the startup probe — pi rejections downgrade to a `warn!`
/// rather than aborting the handshake, so providers without thinking support
/// degrade silently per [`AgentBackend`]'s graceful-degradation contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

impl ThinkingLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            ThinkingLevel::Off => "off",
            ThinkingLevel::Minimal => "minimal",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Xhigh => "xhigh",
        }
    }
}

/// Outcome of a completed agent session — what the workflow engine receives
/// after the session reaches `SessionComplete`.
#[derive(Debug, Clone)]
pub struct SessionOutcome {
    pub exit_code: i32,
    pub cost_usd: Option<f64>,
}

/// Backend abstraction: spawn a session and return it in the `Idle` state.
///
/// The trait surface is deliberately minimal — process lifecycle only.
/// Conversation driving (prompt, steer, abort, event streaming) lives on
/// [`AgentSession`] so both backends share one concrete session type.
///
/// `async fn` in traits is used directly (no `async-trait`) — backends are
/// zero-sized types dispatched via a type parameter (`<B: AgentBackend>`),
/// so the compiler monomorphizes per concrete backend at each call site.
/// The desugared `impl Future + Send` form pins the auto-trait bound so the
/// returned future can cross task boundaries in `loom-workflow`.
pub trait AgentBackend: Send + Sync {
    fn spawn(
        config: &SpawnConfig,
    ) -> impl std::future::Future<Output = Result<AgentSession<Idle>, ProtocolError>> + Send;

    /// Per-backend handler for `AgentEvent::CompactionStart`.
    ///
    /// Pi overrides this to read `prompt.txt` + `scratch.md` and send the
    /// bytes via `steer` — the spec requires the driver to re-pin context as
    /// soon as compaction begins so the next turn after `compaction_end`
    /// reaches the agent with orientation restored. Claude's default no-op
    /// stands: claude installs a `SessionStart` hook pre-spawn that re-pins
    /// inside the agent process, so the workflow driver has nothing to do here.
    fn on_compaction_start<'a>(
        _session: &'a mut AgentSession<Active>,
        _config: &'a SpawnConfig,
    ) -> impl std::future::Future<Output = Result<(), ProtocolError>> + Send + 'a {
        async { Ok(()) }
    }

    /// Per-backend hook invoked once after the workflow observes
    /// `AgentEvent::SessionComplete`, before [`run_agent`] returns.
    ///
    /// Claude overrides this to drive the post-`result` shutdown watchdog
    /// (close stdin, wait `config.shutdown_grace`, escalate SIGTERM →
    /// SIGKILL) — without it the dispatcher leaves an unreaped child that
    /// only `kill_on_drop` cleans up at session drop. Pi exits naturally
    /// on `agent_end` so the default no-op stands.
    ///
    /// Takes the session by value because the watchdog must close stdin
    /// (drop the writer) before signaling the child, which requires
    /// owning the `AgentSession`.
    ///
    /// [`run_agent`]: ../../../loom_workflow/fn.run_agent.html
    fn after_session_complete(
        _session: AgentSession<Active>,
        _config: &SpawnConfig,
    ) -> impl std::future::Future<Output = Result<(), ProtocolError>> + Send {
        async { Ok(()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::repin::RePinContent;

    fn sample_config(model: Option<ModelSelection>) -> SpawnConfig {
        SpawnConfig {
            image_ref: "localhost/wrix-test:tag".into(),
            image_source: PathBuf::from("/nix/store/zzz-wrix-test.tar"),
            image_source_kind: Some(ImageSourceKind::NixDescriptor),
            profile_config: None,
            workspace: PathBuf::from("/workspace"),
            env: vec![("WRIX_AGENT".into(), "pi".into())],
            mounts: Vec::new(),
            initial_prompt: "hello".into(),
            agent_args: vec!["--print".into()],
            repin: RePinContent {
                orientation: "ori".into(),
                pinned_context: "pc".into(),
                partial_bodies: vec![],
            },
            skills: None,
            scratch_dir: PathBuf::from("/workspace/.loom/scratch/test"),
            model,
            thinking_level: None,
            output_limits: None,
            shutdown_grace: None,
            handshake_timeout: None,
            stall_warn_interval: None,
            launcher_env: Vec::new(),
        }
    }

    /// `model: None` is omitted from the on-disk JSON via
    /// `#[serde(skip_serializing_if = "Option::is_none")]`. Wrappers that
    /// pre-date the field added in lm-pkht8.* must continue to round-trip
    /// the serialized fixture identically — the absence of `model` proves
    /// the no-drift contract.
    #[test]
    fn spawn_config_with_model_none_omits_model_key() {
        let cfg = sample_config(None);
        let json = serde_json::to_string(&cfg).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let obj = v.as_object().expect("object");
        assert!(
            !obj.contains_key("model"),
            "model: None must be omitted, got JSON: {json}"
        );
        assert!(
            !obj.contains_key("image_digest_path"),
            "image_digest_path is not a SpawnConfig wire field: {json}"
        );
        // Required top-level keys remain — any silent rename or drop fails here.
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        for required in [
            "image_ref",
            "image_source",
            "image_source_kind",
            "workspace",
            "env",
            "initial_prompt",
            "agent_args",
            "repin",
        ] {
            assert!(keys.contains(&required), "missing key {required}: {json}");
        }
    }

    #[test]
    fn spawn_config_ignores_undocumented_image_digest_path() {
        let legacy = r#"{
            "image_ref": "localhost/img:tag",
            "image_source": "/nix/store/zzz-img.tar",
            "image_digest_path": "/nix/store/ddd-wrix-digest",
            "workspace": "/workspace",
            "env": [["A","1"]],
            "initial_prompt": "go",
            "agent_args": [],
            "repin": {"orientation":"o","pinned_context":"p","partial_bodies":[]}
        }"#;
        let cfg: SpawnConfig = serde_json::from_str(legacy).expect("legacy fixture parses");
        let json = serde_json::to_string(&cfg).expect("serialize");
        assert!(
            !json.contains("image_digest_path"),
            "image_digest_path is a wrix ProfileConfig field, not a SpawnConfig override: {json}",
        );
    }

    /// `model: Some(_)` round-trips with both `provider` and `model_id`
    /// reaching the deserialized struct. Pin both field names so the
    /// pi `set_model` RPC stays correct end-to-end.
    #[test]
    fn spawn_config_with_model_some_round_trips_both_fields() {
        let cfg = sample_config(Some(ModelSelection {
            provider: "deepseek".into(),
            model_id: "deepseek-v3".into(),
        }));
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: SpawnConfig = serde_json::from_str(&json).expect("deserialize");
        let model = back.model.expect("model present");
        assert_eq!(model.provider, "deepseek");
        assert_eq!(model.model_id, "deepseek-v3");
    }

    /// `image_source_kind` uses wrix's kebab-case wire tokens and round-trips
    /// as the typed source selector used by launchers.
    #[test]
    fn image_source_kind_serializes_wrix_wire_tokens() {
        let cfg = sample_config(None);
        let json = serde_json::to_string(&cfg).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(
            v["image_source_kind"],
            ImageSourceKind::NixDescriptor.as_str()
        );
        let back: SpawnConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.image_source_kind, Some(ImageSourceKind::NixDescriptor));

        let docker_json =
            serde_json::to_string(&ImageSourceKind::DockerArchive).expect("serialize");
        assert_eq!(docker_json, "\"docker-archive\"");
    }

    /// `set_loom_inside` appends `LOOM_INSIDE=1` when missing and is a no-op
    /// when already present. Spec: `harness.md` § Nested-Loom Guard.
    #[test]
    fn set_loom_inside_appends_when_missing() {
        let mut env = vec![("WRIX_AGENT".into(), "claude".into())];
        set_loom_inside(&mut env);
        assert_eq!(
            env,
            vec![
                ("WRIX_AGENT".into(), "claude".into()),
                ("LOOM_INSIDE".into(), "1".into()),
            ],
        );
    }

    #[test]
    fn set_loom_inside_is_idempotent() {
        let mut env = vec![("LOOM_INSIDE".into(), "1".into())];
        set_loom_inside(&mut env);
        set_loom_inside(&mut env);
        assert_eq!(
            env.iter().filter(|(k, _)| k == "LOOM_INSIDE").count(),
            1,
            "duplicate LOOM_INSIDE entries: {env:?}",
        );
    }

    /// `thinking_level: None` is omitted from on-disk JSON via
    /// `#[serde(skip_serializing_if = "Option::is_none")]` so existing
    /// wrapper fixtures (which predate the field) round-trip identically.
    #[test]
    fn spawn_config_with_thinking_level_none_omits_field() {
        let cfg = sample_config(None);
        let json = serde_json::to_string(&cfg).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let obj = v.as_object().expect("object");
        assert!(
            !obj.contains_key("thinking_level"),
            "thinking_level: None must be omitted, got JSON: {json}"
        );
    }

    /// `thinking_level: Some(_)` serializes the variant as a lowercase string
    /// and round-trips back to the same enum.
    #[test]
    fn spawn_config_with_thinking_level_some_round_trips_lowercase() {
        let mut cfg = sample_config(None);
        cfg.thinking_level = Some(ThinkingLevel::High);
        let json = serde_json::to_string(&cfg).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(v["thinking_level"], "high");
        let back: SpawnConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.thinking_level, Some(ThinkingLevel::High));
    }

    /// Every documented level (`specs/agent.md` Pi command table)
    /// serializes lowercase and round-trips. Pins the wire vocabulary so a
    /// silent rename surfaces here, not as a pi rejection in the field.
    #[test]
    fn thinking_level_serializes_each_variant_as_lowercase_wire_token() {
        for (variant, expected) in [
            (ThinkingLevel::Off, "off"),
            (ThinkingLevel::Minimal, "minimal"),
            (ThinkingLevel::Low, "low"),
            (ThinkingLevel::Medium, "medium"),
            (ThinkingLevel::High, "high"),
            (ThinkingLevel::Xhigh, "xhigh"),
        ] {
            let json = serde_json::to_string(&variant).expect("serialize");
            assert_eq!(json, format!("\"{expected}\""));
            let back: ThinkingLevel =
                serde_json::from_str(&format!("\"{expected}\"")).expect("deserialize");
            assert_eq!(back, variant);
            assert_eq!(variant.as_str(), expected);
        }
    }

    /// JSON without a `model` key still parses (treated as `None`) — this is
    /// the contract with wrappers built before lm-pkht8.* landed.
    #[test]
    fn spawn_config_legacy_fixture_without_model_key_parses() {
        let legacy = r#"{
            "image_ref": "localhost/img:tag",
            "image_source": "/nix/store/zzz-img.tar",
            "workspace": "/workspace",
            "env": [["A","1"]],
            "initial_prompt": "go",
            "agent_args": [],
            "repin": {"orientation":"o","pinned_context":"p","partial_bodies":[]}
        }"#;
        let cfg: SpawnConfig = serde_json::from_str(legacy).expect("legacy fixture parses");
        assert!(cfg.model.is_none());
        assert_eq!(cfg.image_ref, "localhost/img:tag");
        assert_eq!(cfg.image_source, PathBuf::from("/nix/store/zzz-img.tar"));
        assert_eq!(cfg.image_source_kind, None);
        assert_eq!(cfg.env, vec![("A".to_string(), "1".to_string())]);
    }

    #[test]
    fn spawn_config_without_image_source_override_omits_source_kind() {
        let mut cfg = sample_config(None);
        cfg.image_source = PathBuf::new();
        cfg.image_source_kind = None;
        let json = serde_json::to_string(&cfg).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let obj = v.as_object().expect("object");
        assert!(
            !obj.contains_key("image_source"),
            "no image override: {json}"
        );
        assert!(
            !obj.contains_key("image_source_kind"),
            "no source kind without an image override: {json}",
        );
        let back: SpawnConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.image_source, PathBuf::new());
        assert_eq!(back.image_source_kind, None);
    }

    /// The on-disk JSON shape is the contract with
    /// `wrix spawn --spawn-config <file>`. Key order is fixed by serde's
    /// field-declaration order; reordering fields in [`SpawnConfig`] would
    /// silently shift the wire payload. Pinning the expected key sequence
    /// makes such drift fail loud here instead of in the wrapper.
    #[test]
    fn spawn_config_serializes_top_level_keys_in_declaration_order() {
        let cfg = sample_config(Some(ModelSelection {
            provider: "deepseek".into(),
            model_id: "deepseek-v3".into(),
        }));
        let json = serde_json::to_string(&cfg).expect("serialize");
        let expected = [
            "\"image_ref\":",
            "\"image_source\":",
            "\"image_source_kind\":",
            "\"workspace\":",
            "\"env\":",
            "\"initial_prompt\":",
            "\"agent_args\":",
            "\"repin\":",
            "\"scratch_dir\":",
            "\"model\":",
        ];
        let mut cursor = 0usize;
        for key in expected {
            let rel = json[cursor..].find(key).unwrap_or_else(|| {
                panic!("key {key} missing or out of order after byte {cursor} in {json}");
            });
            cursor += rel + key.len();
        }
    }

    /// With every optional field skipped, the wire payload omits the
    /// per-launch image-source override and optional knobs while preserving
    /// the remaining mandatory keys in declaration order.
    #[test]
    fn spawn_config_skips_all_optional_keys_when_unset() {
        let mut cfg = sample_config(None);
        cfg.image_source = PathBuf::new();
        cfg.image_source_kind = None;
        let json = serde_json::to_string(&cfg).expect("serialize");
        for absent in [
            "\"image_source\":",
            "\"image_source_kind\":",
            "\"mounts\":",
            "\"model\":",
            "\"thinking_level\":",
            "\"output_limits\":",
            "\"shutdown_grace\":",
            "\"handshake_timeout\":",
            "\"stall_warn_interval\":",
        ] {
            assert!(
                !json.contains(absent),
                "{absent} must be omitted when None: {json}",
            );
        }
        let expected = [
            "\"image_ref\":",
            "\"workspace\":",
            "\"env\":",
            "\"initial_prompt\":",
            "\"agent_args\":",
            "\"repin\":",
            "\"scratch_dir\":",
        ];
        let mut cursor = 0usize;
        for key in expected {
            let rel = json[cursor..].find(key).unwrap_or_else(|| {
                panic!("key {key} missing or out of order after byte {cursor} in {json}");
            });
            cursor += rel + key.len();
        }
    }

    /// `output_limits: None` (the Pi/Claude steady state) is omitted from the
    /// on-disk JSON via `#[serde(skip_serializing_if = "Option::is_none")]`,
    /// so wrapper fixtures predating the Direct cap round-trip identically.
    #[test]
    fn spawn_config_with_output_limits_none_omits_field() {
        let cfg = sample_config(None);
        let json = serde_json::to_string(&cfg).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let obj = v.as_object().expect("object");
        assert!(
            !obj.contains_key("output_limits"),
            "output_limits: None must be omitted, got JSON: {json}"
        );
    }

    /// `output_limits: Some(_)` round-trips with `max_inline_bytes` reaching
    /// the deserialized struct — the wire field the in-container Direct runner
    /// reads to bound its tool output.
    #[test]
    fn spawn_config_with_output_limits_some_round_trips() {
        let mut cfg = sample_config(None);
        cfg.output_limits = Some(OutputLimits {
            max_inline_bytes: 16384,
        });
        let json = serde_json::to_string(&cfg).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(v["output_limits"]["max_inline_bytes"], 16384);
        let back: SpawnConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.output_limits
                .expect("output_limits present")
                .max_inline_bytes,
            16384,
        );
    }

    /// `MountSpec` is constructible from production code and serializes the
    /// three documented fields (`host_path`, `container_path`, `read_only`)
    /// at the expected wire-key names.
    #[test]
    fn mount_spec_is_constructible_and_serializes_documented_fields() {
        let spec = MountSpec {
            host_path: PathBuf::from("/run/wrix-beads/dolt.sock"),
            container_path: PathBuf::from("/workspace/.wrix/dolt.sock"),
            read_only: false,
        };
        let json = serde_json::to_string(&spec).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let obj = v.as_object().expect("object");
        assert_eq!(obj["host_path"], "/run/wrix-beads/dolt.sock");
        assert_eq!(obj["container_path"], "/workspace/.wrix/dolt.sock");
        assert_eq!(obj["read_only"], false);
        let back: MountSpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, spec);
    }

    /// A populated `mounts` round-trips through JSON with both
    /// single-file (socket) and directory mounts, preserving the
    /// `read_only` discipline per entry. This is the wrix-facing contract
    /// for projecting the dolt socket and the optional sccache directory.
    #[test]
    fn spawn_config_mounts_round_trip_preserves_per_entry_fields() {
        let mut cfg = sample_config(None);
        cfg.mounts = vec![
            MountSpec {
                host_path: PathBuf::from("/run/wrix-beads/dolt.sock"),
                container_path: PathBuf::from("/workspace/.wrix/dolt.sock"),
                read_only: false,
            },
            MountSpec {
                host_path: PathBuf::from("/home/op/.cache/loom-sccache"),
                container_path: PathBuf::from("/sccache"),
                read_only: true,
            },
        ];
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: SpawnConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.mounts, cfg.mounts);
    }

    /// Empty `mounts` is omitted from the wire payload via
    /// `#[serde(skip_serializing_if = "Vec::is_empty")]` so wrappers and
    /// fixtures pre-dating this field round-trip identically.
    #[test]
    fn spawn_config_with_empty_mounts_omits_key() {
        let cfg = sample_config(None);
        let json = serde_json::to_string(&cfg).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let obj = v.as_object().expect("object");
        assert!(
            !obj.contains_key("mounts"),
            "mounts: empty must be omitted, got JSON: {json}",
        );
    }

    /// `launcher_env` is host-only state (deploy/signing key paths handed to
    /// the `wrix spawn` launcher) and must NEVER reach the spawn-config
    /// JSON: it is the in-container `env` allowlist's host-side counterpart,
    /// and leaking host key paths into a world-readable file is a security
    /// regression. `#[serde(skip)]` enforces this — pin it so a future
    /// derive change fails here, not in production.
    #[test]
    fn launcher_env_is_never_serialized() {
        let mut cfg = sample_config(None);
        cfg.launcher_env = vec![
            (
                "WRIX_DEPLOY_KEY".into(),
                "/home/op/.ssh/deploy_keys/k".into(),
            ),
            (
                "WRIX_SIGNING_KEY".into(),
                "/home/op/.ssh/deploy_keys/k-signing".into(),
            ),
        ];
        let json = serde_json::to_string(&cfg).expect("serialize");
        assert!(
            !json.contains("launcher_env"),
            "launcher_env key must be absent from JSON: {json}",
        );
        assert!(
            !json.contains("deploy_keys"),
            "host key paths must not leak into the spawn-config JSON: {json}",
        );
    }

    /// `skills` is host-only setup state. It must never reach the wrix JSON
    /// payload because prompt-disclosure backends already receive readable
    /// paths through `initial_prompt`, and the wrapper does not consume the
    /// materialized registry.
    #[test]
    fn spawn_config_skills_are_host_only_and_never_serialized() {
        let mut cfg = sample_config(None);
        cfg.skills = Some(RegisteredSkills::new(
            loom_skills::registry::MaterializedRegistry::new(vec![]),
            loom_skills::disclosure::DisclosureMode::Prompt,
        ));
        let json = serde_json::to_string(&cfg).expect("serialize");
        assert!(
            !json.contains("skills"),
            "host-side skill registry must not reach wrix spawn JSON: {json}",
        );
        let back: SpawnConfig = serde_json::from_str(&json).expect("deserialize");
        assert!(back.skills.is_none());
    }

    /// `launcher_env` deserializes to an empty vector via `#[serde(skip)]`'s
    /// `Default` even when present in the wire bytes — the field is a
    /// host-process detail the wrapper neither emits nor consumes.
    #[test]
    fn launcher_env_defaults_to_empty_on_deserialize() {
        let cfg = sample_config(None);
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: SpawnConfig = serde_json::from_str(&json).expect("deserialize");
        assert!(back.launcher_env.is_empty());
    }

    /// Legacy fixtures without a `mounts` key parse as an empty vector —
    /// the absence-equals-empty contract that lets older wrix payloads
    /// round-trip into the new struct.
    #[test]
    fn spawn_config_legacy_fixture_without_mounts_defaults_to_empty() {
        let legacy = r#"{
            "image_ref": "localhost/img:tag",
            "image_source": "/nix/store/zzz-img.tar",
            "workspace": "/workspace",
            "env": [["A","1"]],
            "initial_prompt": "go",
            "agent_args": [],
            "repin": {"orientation":"o","pinned_context":"p","partial_bodies":[]}
        }"#;
        let cfg: SpawnConfig = serde_json::from_str(legacy).expect("legacy fixture parses");
        assert!(cfg.mounts.is_empty());
    }
}
