use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::error::ProtocolError;
use super::repin::RePinContent;
use super::session::{Active, AgentSession, Idle};

/// Configuration `loom` hands to `wrapix spawn` describing how to launch
/// the per-bead container and what initial agent state to install.
///
/// Serialized to a JSON file (`/tmp/loom-<id>.json`) and read back by
/// `wrapix spawn --spawn-config <file>` — this is the single stable
/// boundary between loom and the wrapper. `env` is an explicit allowlist;
/// the wrapper never inherits the host environment wholesale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnConfig {
    /// Podman image reference (e.g. `localhost/wrapix-rust:<hash>`) — the
    /// argument passed to `podman run`. Populated by loom from the
    /// profile-image manifest at dispatch time.
    pub image_ref: String,
    /// Nix store path to a `podman load`-compatible archive that materializes
    /// `image_ref`. The wrapper runs `podman load < image_source` before
    /// `podman run`; the load is idempotent on the ref's hash tag.
    pub image_source: PathBuf,
    pub workspace: PathBuf,
    pub env: Vec<(String, String)>,
    /// Per-spawn bind mounts beyond [`SpawnConfig::workspace`]. Loom uses this
    /// to project the `wrapix-beads` dolt socket into every bead container at
    /// `/workspace/.wrapix/dolt.sock` (replacing the host-side hardlink shim
    /// in [`crate::git::GitClient`]) and, when configured, the shared sccache
    /// directory at the configured container path. Additive to the resolved
    /// profile's `mounts`; see `specs/agent.md` § SpawnConfig.
    ///
    /// Single-file mounts (sockets) and directory mounts both pass through
    /// virtiofs on Linux. On Darwin, the wrapix sandbox classifier accepts
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
    /// Host-side environment loom sets on the `wrapix spawn` **launcher**
    /// process (not the in-container env — that is [`SpawnConfig::env`]).
    /// Carries `WRAPIX_DEPLOY_KEY` / `WRAPIX_SIGNING_KEY` pointing at the
    /// host key paths so `wrapix spawn` can bind-mount the keys into the
    /// bead container; the host paths never cross the boundary (wrapix
    /// `specs/security.md` § Credential Surfaces). `#[serde(skip)]`: this
    /// never reaches the wrapper's spawn-config JSON — it is applied to the
    /// child process environment by the backend before `wrapix spawn` runs,
    /// and keeping it out of the on-disk JSON avoids leaking host key paths
    /// into a world-readable file.
    #[serde(skip)]
    pub launcher_env: Vec<(String, String)>,
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
    /// Pi overrides this to send `config.repin.to_prompt()` via `steer` —
    /// the spec requires the driver to re-pin context as soon as compaction
    /// begins so the next turn after `compaction_end` reaches the agent
    /// with orientation restored. Claude's default no-op stands: claude
    /// installs a `SessionStart` hook pre-spawn that re-pins inside the
    /// agent process, so the workflow driver has nothing to do here.
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
            image_ref: "localhost/wrapix-test:tag".into(),
            image_source: PathBuf::from("/nix/store/zzz-wrapix-test.tar"),
            workspace: PathBuf::from("/workspace"),
            env: vec![("WRAPIX_AGENT".into(), "pi".into())],
            mounts: Vec::new(),
            initial_prompt: "hello".into(),
            agent_args: vec!["--print".into()],
            repin: RePinContent {
                orientation: "ori".into(),
                pinned_context: "pc".into(),
                partial_bodies: vec![],
            },
            scratch_dir: PathBuf::from("/workspace/.loom/scratch/test"),
            model,
            thinking_level: None,
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
        // Seven top-level keys remain — any silent rename or drop fails here.
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        for required in [
            "image_ref",
            "image_source",
            "workspace",
            "env",
            "initial_prompt",
            "agent_args",
            "repin",
        ] {
            assert!(keys.contains(&required), "missing key {required}: {json}");
        }
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

    /// `set_loom_inside` appends `LOOM_INSIDE=1` when missing and is a no-op
    /// when already present. Spec: `harness.md` § Nested-Loom Guard.
    #[test]
    fn set_loom_inside_appends_when_missing() {
        let mut env = vec![("WRAPIX_AGENT".into(), "claude".into())];
        set_loom_inside(&mut env);
        assert_eq!(
            env,
            vec![
                ("WRAPIX_AGENT".into(), "claude".into()),
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
        assert_eq!(cfg.env, vec![("A".to_string(), "1".to_string())]);
    }

    /// The on-disk JSON shape is the contract with
    /// `wrapix spawn --spawn-config <file>`. Key order is fixed by serde's
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

    /// With every optional field skipped, the wire payload is the eight
    /// mandatory keys in declaration order — the steady-state shape every
    /// wrapper fixture pre-dating the optional knobs round-trips against.
    #[test]
    fn spawn_config_skips_all_optional_keys_when_unset() {
        let cfg = sample_config(None);
        let json = serde_json::to_string(&cfg).expect("serialize");
        for absent in [
            "\"mounts\":",
            "\"model\":",
            "\"thinking_level\":",
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
            "\"image_source\":",
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

    /// `MountSpec` is constructible from production code and serializes the
    /// three documented fields (`host_path`, `container_path`, `read_only`)
    /// at the expected wire-key names.
    #[test]
    fn mount_spec_is_constructible_and_serializes_documented_fields() {
        let spec = MountSpec {
            host_path: PathBuf::from("/run/wrapix-beads/dolt.sock"),
            container_path: PathBuf::from("/workspace/.wrapix/dolt.sock"),
            read_only: false,
        };
        let json = serde_json::to_string(&spec).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        let obj = v.as_object().expect("object");
        assert_eq!(obj["host_path"], "/run/wrapix-beads/dolt.sock");
        assert_eq!(obj["container_path"], "/workspace/.wrapix/dolt.sock");
        assert_eq!(obj["read_only"], false);
        let back: MountSpec = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, spec);
    }

    /// A populated `mounts` round-trips through JSON with both
    /// single-file (socket) and directory mounts, preserving the
    /// `read_only` discipline per entry. This is the wrapix-facing contract
    /// for projecting the dolt socket and the optional sccache directory.
    #[test]
    fn spawn_config_mounts_round_trip_preserves_per_entry_fields() {
        let mut cfg = sample_config(None);
        cfg.mounts = vec![
            MountSpec {
                host_path: PathBuf::from("/run/wrapix-beads/dolt.sock"),
                container_path: PathBuf::from("/workspace/.wrapix/dolt.sock"),
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
    /// the `wrapix spawn` launcher) and must NEVER reach the spawn-config
    /// JSON: it is the in-container `env` allowlist's host-side counterpart,
    /// and leaking host key paths into a world-readable file is a security
    /// regression. `#[serde(skip)]` enforces this — pin it so a future
    /// derive change fails here, not in production.
    #[test]
    fn launcher_env_is_never_serialized() {
        let mut cfg = sample_config(None);
        cfg.launcher_env = vec![
            (
                "WRAPIX_DEPLOY_KEY".into(),
                "/home/op/.ssh/deploy_keys/k".into(),
            ),
            (
                "WRAPIX_SIGNING_KEY".into(),
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
    /// the absence-equals-empty contract that lets older wrapix payloads
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
