use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tracing::info;

use loom_driver::agent::AgentKind;
use loom_driver::config::{AgentSelection, LoomConfig, Phase};
use loom_driver::identifier::{ProfileName, SpecLabel};
use loom_driver::lock::LockManager;
use loom_driver::profile_manifest::{ImageEntry, ProfileImageManifest};
use loom_driver::scratch::{ScratchSession, resolve_plan_scratch_key};
use loom_driver::state::CacheDb;

use crate::skill::SkillPlan;
use crate::spawn::container_workspace_path;

use super::command::{WRIX_BIN, build_wrix_argv};
use super::error::PlanError;
use super::prompt::{PlanPromptInputs, render_prompt};

/// Env var read by `wrix run` to pick the podman ref of the per-profile
/// image. Mirrors `lib/sandbox/linux/default.nix`.
pub const WRIX_DEFAULT_IMAGE_REF: &str = "WRIX_DEFAULT_IMAGE_REF";

/// Env var read by `wrix run` to pick the Nix store path handed to
/// `podman load`. Mirrors `lib/sandbox/linux/default.nix`.
pub const WRIX_DEFAULT_IMAGE_SOURCE: &str = "WRIX_DEFAULT_IMAGE_SOURCE";

/// Default timeout used by [`run`] — mirrors the phase-lock command surface.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Options accepted by [`run`].
pub struct PlanOpts {
    pub anchor_labels: Vec<SpecLabel>,
    /// Explicit path to the `wrix` launcher. `None` falls back to
    /// [`WRIX_BIN`] on `PATH`. Tests pass a stub here.
    pub wrix_bin: Option<PathBuf>,
    /// CLI `--profile` override — wins over `[phase.plan]` /
    /// `[phase.default]` resolution per `specs/harness.md` § Profile-Image
    /// Manifest. `None` falls back to the per-phase config chain.
    pub cli_profile: Option<ProfileName>,
    /// CLI `--agent` override. When absent, `[phase.plan].agent.backend` /
    /// `[phase.default].agent.backend` select the interactive command passed
    /// to `wrix run`.
    pub agent_override: Option<AgentKind>,
    /// Parsed profile-image manifest. The runner looks the resolved profile
    /// up against this to populate the `WRIX_DEFAULT_IMAGE_REF` /
    /// `WRIX_DEFAULT_IMAGE_SOURCE` env vars the launcher reads when no
    /// `--spawn-config` is supplied (see `lib/sandbox/linux/default.nix`).
    pub manifest: ProfileImageManifest,
    /// Repository key paths for the `wrix run` launcher process.
    pub launcher_env: Vec<(String, String)>,
}

/// Summary returned after the interactive plan session exits.
#[derive(Debug, Clone)]
pub struct PlanReport {
    pub anchor_labels: Vec<SpecLabel>,
    pub companion_paths: Vec<String>,
}

/// Run `loom plan` against `workspace`.
///
/// 1. Acquire `plan.lock` for the duration of the call.
/// 2. Render the Askama template into a prompt body.
/// 3. Spawn `wrix run <workspace> <agent command> ... <prompt>` with stdio
///    inherited and wait for it to exit.
pub fn run(workspace: &Path, opts: PlanOpts) -> Result<PlanReport, PlanError> {
    run_with_timeout(workspace, opts, DEFAULT_LOCK_TIMEOUT)
}

/// Same as [`run`] with an explicit lock-wait timeout. Tests use this to
/// keep the contention path fast.
pub fn run_with_timeout(
    workspace: &Path,
    opts: PlanOpts,
    timeout: Duration,
) -> Result<PlanReport, PlanError> {
    let anchor_labels = opts.anchor_labels;

    let lock_mgr = LockManager::new(workspace)?;
    let _guard =
        lock_mgr.acquire_phase_with_timeout(loom_driver::lock::PhaseLock::Planning, timeout)?;

    let cfg = LoomConfig::load(LoomConfig::resolve_path(workspace))
        .unwrap_or_else(|_| LoomConfig::default());

    let selection = resolve_plan_selection(opts.cli_profile.as_ref(), opts.agent_override, &cfg)?;
    let image: &ImageEntry = opts.manifest.lookup(&selection.profile, selection.kind)?;

    let pinned_context = read_pinned_context(workspace, &cfg.pinned_context)?;
    let spec_index = read_pinned_context(workspace, "docs/README.md")?;
    let db = CacheDb::open(workspace.join(".loom/cache.db"))?;
    let companion_paths = anchor_companions(&db, &anchor_labels)?;
    let key = resolve_plan_scratch_key(&anchor_labels);
    let scratchpad_path = ScratchSession::scratchpad_path_for(workspace, &key);
    let scratch_dir = scratchpad_path.parent().ok_or_else(|| PlanError::Spawn {
        source: io::Error::other("scratchpad path has no parent"),
    })?;
    let skill_plan = SkillPlan::resolve_from_workspace_sync(
        workspace,
        Phase::Plan.as_str(),
        &selection.profile,
        selection.kind,
        &cfg.skills,
    )?;
    let skill_session = skill_plan.materialize(scratch_dir, workspace)?;
    let prompt_scratchpad_path = container_workspace_path(workspace, &scratchpad_path);
    let prompt_body = render_prompt(PlanPromptInputs {
        anchor_labels: anchor_labels.clone(),
        pinned_context,
        spec_index,
        companion_paths: companion_paths.clone(),
        scratchpad_path: prompt_scratchpad_path.to_string_lossy().into_owned(),
        spec_conventions: cfg.spec_conventions.clone(),
        skill_index: skill_session.skill_index,
    })?;

    let banner = format!("loom plan @ {}", key);
    let scratch = ScratchSession::open(workspace, &key, &prompt_body, &banner)
        .map_err(|source| PlanError::Spawn { source })?;
    let _restored_skills = skill_plan.materialize(scratch.path(), workspace)?;

    let bin: PathBuf = opts.wrix_bin.unwrap_or_else(|| PathBuf::from(WRIX_BIN));
    let argv = match selection.kind {
        AgentKind::Claude => {
            let claude_settings_path =
                container_workspace_path(workspace, &scratch.claude_settings());
            info!(
                anchors = %key,
                profile = %selection.profile,
                agent = ?selection.kind,
                image_ref = %image.r#ref,
                image_source = %image.source.display(),
                wrix_bin = %bin.display(),
                scratch_dir = %scratch.path().display(),
                "loom plan: shelling out to interactive wrix run",
            );
            build_wrix_argv(
                workspace,
                &prompt_body,
                selection.kind,
                Some(&claude_settings_path),
            )
        }
        AgentKind::Pi => {
            let launch =
                crate::pi_tui::prepare_launch(workspace, &selection, &prompt_body, scratch.path())
                    .map_err(|source| PlanError::Spawn { source })?;
            info!(
                anchors = %key,
                profile = %selection.profile,
                agent = ?selection.kind,
                image_ref = %image.r#ref,
                image_source = %image.source.display(),
                wrix_bin = %bin.display(),
                scratch_dir = %scratch.path().display(),
                session_dir = %launch.session_dir.display(),
                "loom plan: shelling out to native pi TUI",
            );
            launch.argv
        }
        AgentKind::Direct => return Err(PlanError::DirectInteractive),
    };
    let status = Command::new(&bin)
        .args(&argv)
        .envs(opts.launcher_env)
        .env(WRIX_DEFAULT_IMAGE_REF, &image.r#ref)
        .env(WRIX_DEFAULT_IMAGE_SOURCE, &image.source)
        .env("WRIX_AGENT", selection.kind.as_str())
        .status()
        .map_err(|source| PlanError::Spawn { source })?;
    drop(scratch);
    if !status.success() {
        return Err(PlanError::WrixExit {
            status: status.to_string(),
        });
    }

    Ok(PlanReport {
        anchor_labels,
        companion_paths,
    })
}

/// Resolve the profile that `loom plan` should pass through to the launcher.
///
/// Order of precedence (highest first):
/// 1. CLI `--profile` override (`cli_profile`).
/// 2. `[phase.plan].profile` / `[phase.default].profile` resolved through
///    [`LoomConfig::agent_for`].
/// 3. Built-in `base`, supplied by `agent_for` when neither phase populates
///    a profile.
///
/// `agent_for` also validates the resolved backend name. We surface that
/// failure via `PlanError::AgentSelection` so a typo in `[phase.plan]
/// agent.backend` fails loudly here rather than silently falling back.
fn resolve_plan_selection(
    cli_profile: Option<&ProfileName>,
    agent_override: Option<AgentKind>,
    config: &LoomConfig,
) -> Result<AgentSelection, PlanError> {
    let mut selection = config.agent_for(Phase::Plan)?;
    if let Some(p) = cli_profile {
        selection.profile = p.clone();
    }
    if let Some(kind) = agent_override {
        selection.kind = kind;
    }
    if matches!(selection.kind, AgentKind::Direct) {
        return Err(PlanError::DirectInteractive);
    }
    Ok(selection)
}

fn anchor_companions(db: &CacheDb, anchor_labels: &[SpecLabel]) -> Result<Vec<String>, PlanError> {
    let mut paths = Vec::new();
    for label in anchor_labels {
        for path in db.companions(label)? {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    Ok(paths)
}

fn read_pinned_context(workspace: &Path, rel: &str) -> Result<String, PlanError> {
    let path = workspace.join(rel);
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => Err(PlanError::ReadPinnedContext { path, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use loom_driver::profile_manifest::ProfileError;
    use std::os::unix::fs::PermissionsExt;

    const CANARY_NONCE: &str = "LOOM_COMPACTION_CANARY_NONCE_4f0b3f0f";

    fn three_profile_manifest(dir: &Path) -> Result<ProfileImageManifest> {
        let manifest_path = dir.join("profile-images.json");
        let body = format!(
            r#"{{
              "base":   {{ "claude": {{ "ref": "localhost/wrix-base-claude:abc",   "source": {base:?}, "source_kind": "nix-descriptor" }}, "pi": {{ "ref": "localhost/wrix-base-pi:abc",   "source": {base:?}, "source_kind": "nix-descriptor" }} }},
              "rust":   {{ "claude": {{ "ref": "localhost/wrix-rust-claude:def",   "source": {rust:?}, "source_kind": "nix-descriptor" }}, "pi": {{ "ref": "localhost/wrix-rust-pi:def",   "source": {rust:?}, "source_kind": "nix-descriptor" }} }},
              "python": {{ "claude": {{ "ref": "localhost/wrix-python-claude:ghi", "source": {py:?}, "source_kind": "nix-descriptor" }}, "pi": {{ "ref": "localhost/wrix-python-pi:ghi", "source": {py:?}, "source_kind": "nix-descriptor" }} }}
            }}"#,
            base = dir.join("base.tar").display().to_string(),
            rust = dir.join("rust.tar").display().to_string(),
            py = dir.join("python.tar").display().to_string(),
        );
        std::fs::write(&manifest_path, body)?;
        Ok(ProfileImageManifest::from_path(&manifest_path)?)
    }

    fn stub_wrix(dir: &Path) -> Result<PathBuf> {
        let bin_dir = dir.join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        let bin = bin_dir.join("wrix");
        let script = loom_test_support::bash_script(&format!(
            "set -euo pipefail\nargv_log={:?}\nenv_log={:?}\nfor arg in \"$@\"; do\n    printf '%s\\n' \"$arg\" >> \"$argv_log\"\ndone\nprintf 'ref=%s\\nsource=%s\\nagent=%s\\n' \"${{{}:-}}\" \"${{{}:-}}\" \"${{WRIX_AGENT:-}}\" > \"$env_log\"\n",
            dir.join("argv.log").display().to_string(),
            dir.join("env.log").display().to_string(),
            WRIX_DEFAULT_IMAGE_REF,
            WRIX_DEFAULT_IMAGE_SOURCE,
        ));
        std::fs::write(&bin, script)?;
        let mut perms = std::fs::metadata(&bin)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms)?;
        Ok(bin)
    }

    fn mock_script_path(rel: &str) -> PathBuf {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        for ancestor in manifest_dir.ancestors() {
            let candidate = ancestor.join("tests").join(rel);
            if candidate.is_file() {
                return candidate;
            }
        }
        panic!("could not locate tests/{rel}");
    }

    fn stub_wrix_invokes_mock_claude(dir: &Path) -> Result<PathBuf> {
        let bin_dir = dir.join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        let bin = bin_dir.join("wrix");
        let mock_claude = mock_script_path("mock-claude/claude.sh");
        let script = loom_test_support::bash_script(&format!(
            r#"set -euo pipefail
argv_log={argv_log:?}
env_log={env_log:?}
mock_claude={mock_claude:?}
canary_log={canary_log:?}

for arg in "$@"; do
    printf '%s\n' "$arg" >> "$argv_log"
done
printf 'ref=%s\nsource=%s\nagent=%s\n' "${{{ref_env}:-}}" "${{{source_env}:-}}" "${{WRIX_AGENT:-}}" > "$env_log"

if [[ "${{1:-}}" != "run" ]]; then
    printf 'expected wrix run, got: %s\n' "${{1:-}}" >&2
    exit 2
fi
workspace="${{2:?}}"
agent="${{3:-}}"
if [[ "$agent" != "claude" ]]; then
    printf 'expected claude child, got: %s\n' "$agent" >&2
    exit 2
fi
shift 3
mapped=()
for arg in "$@"; do
    if [[ "$arg" == /workspace/* ]]; then
        mapped+=("$workspace/${{arg#/workspace/}}")
    else
        mapped+=("$arg")
    fi
done
exec bash "$mock_claude" interactive-compaction-canary "${{mapped[@]}}" > "$canary_log"
"#,
            argv_log = dir.join("argv.log").display().to_string(),
            env_log = dir.join("env.log").display().to_string(),
            mock_claude = mock_claude.display().to_string(),
            canary_log = dir.join("canary.log").display().to_string(),
            ref_env = WRIX_DEFAULT_IMAGE_REF,
            source_env = WRIX_DEFAULT_IMAGE_SOURCE,
        ));
        std::fs::write(&bin, script)?;
        let mut perms = std::fs::metadata(&bin)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms)?;
        Ok(bin)
    }

    fn plan_opts(
        anchor_labels: Vec<SpecLabel>,
        bin: PathBuf,
        manifest: ProfileImageManifest,
    ) -> PlanOpts {
        PlanOpts {
            anchor_labels,
            wrix_bin: Some(bin),
            cli_profile: None,
            agent_override: None,
            manifest,
            launcher_env: Vec::new(),
        }
    }

    fn seed_workspace(dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir.join(".loom"))?;
        std::fs::create_dir_all(dir.join("docs"))?;
        std::fs::create_dir_all(dir.join("specs"))?;
        std::fs::write(
            dir.join("docs/README.md"),
            "# Loom Docs\n| Spec | Purpose |\n",
        )?;
        let _db = CacheDb::open(dir.join(".loom/cache.db"))?;
        Ok(())
    }

    fn seed_workspace_with_canary(dir: &Path) -> Result<()> {
        seed_workspace(dir)?;
        std::fs::write(
            dir.join("docs/README.md"),
            format!("# Loom Docs\n\nCompaction canary nonce: {CANARY_NONCE}\n"),
        )?;
        Ok(())
    }

    fn run_plan_compaction_canary() -> Result<String> {
        let dir = tempfile::tempdir()?;
        seed_workspace_with_canary(dir.path())?;
        let manifest = three_profile_manifest(dir.path())?;
        let bin = stub_wrix_invokes_mock_claude(dir.path())?;

        run_with_timeout(
            dir.path(),
            plan_opts(Vec::new(), bin, manifest),
            Duration::from_secs(1),
        )?;

        Ok(std::fs::read_to_string(dir.path().join("argv.log"))?)
    }

    fn run_plan_pi_launch() -> Result<(String, String)> {
        let dir = tempfile::tempdir()?;
        seed_workspace_with_canary(dir.path())?;
        std::fs::write(
            dir.path().join("loom.toml"),
            "[phase.plan]\nagent.backend = \"pi\"\n",
        )?;
        let manifest = three_profile_manifest(dir.path())?;
        let bin = stub_wrix(dir.path())?;

        run_with_timeout(
            dir.path(),
            plan_opts(vec![SpecLabel::new("harness")], bin, manifest),
            Duration::from_secs(1),
        )?;

        Ok((
            std::fs::read_to_string(dir.path().join("argv.log"))?,
            std::fs::read_to_string(dir.path().join("env.log"))?,
        ))
    }

    #[test]
    fn plan_threads_repository_keys_to_wrix_run() -> Result<()> {
        let dir = tempfile::tempdir()?;
        seed_workspace(dir.path())?;
        let manifest = three_profile_manifest(dir.path())?;
        let env_log = dir.path().join("launcher-env.log");
        let bin = dir.path().join("wrix-env");
        std::fs::write(
            &bin,
            loom_test_support::bash_script(&format!(
                "set -euo pipefail\nprintf 'deploy=%s\\nsigning=%s\\n' \"${{WRIX_DEPLOY_KEY:-}}\" \"${{WRIX_SIGNING_KEY:-}}\" > {:?}\n",
                env_log.display().to_string(),
            )),
        )?;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))?;
        let mut opts = plan_opts(vec![SpecLabel::new("harness")], bin, manifest);
        opts.launcher_env = vec![
            ("WRIX_DEPLOY_KEY".to_string(), "/keys/repo".to_string()),
            (
                "WRIX_SIGNING_KEY".to_string(),
                "/keys/repo-signing".to_string(),
            ),
        ];

        run_with_timeout(dir.path(), opts, Duration::from_secs(1))?;

        assert_eq!(
            std::fs::read_to_string(env_log)?,
            "deploy=/keys/repo\nsigning=/keys/repo-signing\n",
        );
        Ok(())
    }

    #[test]
    fn plan_invokes_wrix_run_with_optional_anchors() -> Result<()> {
        let dir = tempfile::tempdir()?;
        seed_workspace(dir.path())?;
        let manifest = three_profile_manifest(dir.path())?;
        let bin = stub_wrix(dir.path())?;

        let report = run_with_timeout(
            dir.path(),
            plan_opts(vec![SpecLabel::new("harness")], bin, manifest),
            Duration::from_secs(1),
        )?;

        assert_eq!(report.anchor_labels, vec![SpecLabel::new("harness")]);
        let argv_log = std::fs::read_to_string(dir.path().join("argv.log"))?;
        assert!(argv_log.starts_with("run\n"));
        assert!(argv_log.contains("# Specification Interview"));
        assert!(argv_log.contains("`harness`"));
        assert!(argv_log.contains("# Loom Docs"));
        Ok(())
    }

    #[test]
    fn plan_accepts_zero_anchors_and_uses_plan_scratch_key() -> Result<()> {
        let dir = tempfile::tempdir()?;
        seed_workspace(dir.path())?;
        let manifest = three_profile_manifest(dir.path())?;
        let bin = stub_wrix(dir.path())?;

        run_with_timeout(
            dir.path(),
            plan_opts(Vec::new(), bin, manifest),
            Duration::from_secs(1),
        )?;

        let argv_log = std::fs::read_to_string(dir.path().join("argv.log"))?;
        assert!(argv_log.contains("No anchor labels were supplied"));
        assert!(argv_log.contains(".loom/scratch/plan/scratch.md"));
        Ok(())
    }

    #[test]
    fn plan_does_not_write_selection_pointer() -> Result<()> {
        let dir = tempfile::tempdir()?;
        seed_workspace(dir.path())?;
        let manifest = three_profile_manifest(dir.path())?;
        let bin = stub_wrix(dir.path())?;

        run_with_timeout(
            dir.path(),
            plan_opts(vec![SpecLabel::new("harness")], bin, manifest),
            Duration::from_secs(1),
        )?;

        let _db = CacheDb::open(dir.path().join(".loom/cache.db"))?;
        Ok(())
    }

    #[test]
    fn plan_threads_anchor_companions_into_prompt_without_reconcile_write() -> Result<()> {
        let dir = tempfile::tempdir()?;
        seed_workspace(dir.path())?;
        let db = CacheDb::open(dir.path().join(".loom/cache.db"))?;
        db.replace_companions(&SpecLabel::new("harness"), &["lib/sandbox/".to_string()])?;
        drop(db);
        let manifest = three_profile_manifest(dir.path())?;
        let bin = stub_wrix(dir.path())?;

        let report = run_with_timeout(
            dir.path(),
            plan_opts(vec![SpecLabel::new("harness")], bin, manifest),
            Duration::from_secs(1),
        )?;

        assert_eq!(report.companion_paths, vec!["lib/sandbox/".to_string()]);
        let argv_log = std::fs::read_to_string(dir.path().join("argv.log"))?;
        assert!(argv_log.contains("- lib/sandbox/"));
        Ok(())
    }

    #[test]
    fn plan_cli_profile_override_picks_manifest_entry() -> Result<()> {
        let dir = tempfile::tempdir()?;
        seed_workspace(dir.path())?;
        let manifest = three_profile_manifest(dir.path())?;
        let bin = stub_wrix(dir.path())?;
        let mut opts = plan_opts(vec![SpecLabel::new("harness")], bin, manifest);
        opts.cli_profile = Some(ProfileName::new("rust"));

        run_with_timeout(dir.path(), opts, Duration::from_secs(1))?;

        let env_log = std::fs::read_to_string(dir.path().join("env.log"))?;
        assert!(env_log.contains("localhost/wrix-rust-claude:def"));
        Ok(())
    }

    #[test]
    fn plan_runner_passes_resolved_profile_runtime_to_wrix_run() -> Result<()> {
        let dir = tempfile::tempdir()?;
        seed_workspace(dir.path())?;
        std::fs::write(
            dir.path().join("loom.toml"),
            "[phase.default]\nprofile = \"python\"\nagent.backend = \"claude\"\n",
        )?;
        let manifest = three_profile_manifest(dir.path())?;
        let bin = stub_wrix(dir.path())?;
        let mut opts = plan_opts(vec![SpecLabel::new("harness")], bin, manifest);
        opts.cli_profile = Some(ProfileName::new("rust"));
        opts.agent_override = Some(AgentKind::Claude);

        run_with_timeout(dir.path(), opts, Duration::from_secs(1))?;

        let argv_log = std::fs::read_to_string(dir.path().join("argv.log"))?;
        let argv: Vec<&str> = argv_log.lines().collect();
        assert!(argv.contains(&"claude"), "expected claude argv: {argv_log}");
        assert!(
            argv.contains(&"--settings"),
            "settings not loaded: {argv_log}"
        );
        assert!(
            argv.iter().any(|arg| arg.ends_with("claude-settings.json")),
            "settings path missing: {argv_log}"
        );
        assert!(
            !argv.contains(&"pi"),
            "claude run must not call pi: {argv_log}"
        );
        assert!(
            !argv.contains(&"--profile"),
            "wrix run must not receive --profile: {argv_log}"
        );
        assert!(
            argv.contains(&"--dangerously-skip-permissions"),
            "claude run must receive claude flags: {argv_log}",
        );

        let env_log = std::fs::read_to_string(dir.path().join("env.log"))?;
        assert!(env_log.contains("ref=localhost/wrix-rust-claude:def"));
        assert!(env_log.contains("source="));
        assert!(env_log.contains("rust.tar"));
        assert!(env_log.contains("agent=claude"));
        Ok(())
    }

    #[test]
    fn plan_phase_default_profile_alone_picks_manifest_entry() -> Result<()> {
        let dir = tempfile::tempdir()?;
        seed_workspace(dir.path())?;
        std::fs::write(
            dir.path().join("loom.toml"),
            "[phase.default]\nprofile = \"python\"\n",
        )?;
        let manifest = three_profile_manifest(dir.path())?;
        let bin = stub_wrix(dir.path())?;

        run_with_timeout(
            dir.path(),
            plan_opts(vec![SpecLabel::new("harness")], bin, manifest),
            Duration::from_secs(1),
        )?;

        let env_log = std::fs::read_to_string(dir.path().join("env.log"))?;
        assert!(env_log.contains("localhost/wrix-python-claude:ghi"));
        Ok(())
    }

    #[test]
    fn interactive_claude_shell_out_loads_compaction_hook() -> Result<()> {
        let argv_log = run_plan_compaction_canary()?;
        let argv: Vec<&str> = argv_log.lines().collect();
        assert!(
            argv.contains(&"--settings"),
            "settings arg missing: {argv_log}"
        );
        assert!(
            argv.iter().any(|arg| arg.ends_with("claude-settings.json")),
            "settings path missing: {argv_log}",
        );
        Ok(())
    }

    #[test]
    fn loom_plan_compaction_repin_polish_canary() -> Result<()> {
        let argv_log = run_plan_compaction_canary()?;
        assert!(
            argv_log.contains("--dangerously-skip-permissions"),
            "mock canary should run through the launched claude argv: {argv_log}",
        );
        Ok(())
    }

    #[test]
    fn interactive_shell_out_installs_compaction_repin_delivery() -> Result<()> {
        let claude_argv_log = run_plan_compaction_canary()?;
        let claude_args: Vec<&str> = claude_argv_log.lines().collect();
        let claude_delivery = claude_args
            .iter()
            .position(|arg| *arg == "--settings")
            .ok_or_else(|| anyhow::anyhow!("claude settings missing: {claude_argv_log}"))?;
        let claude_prompt = claude_args
            .iter()
            .position(|arg| *arg == "# Specification Interview")
            .ok_or_else(|| anyhow::anyhow!("claude prompt missing: {claude_argv_log}"))?;
        assert!(claude_delivery < claude_prompt);

        let (pi_argv_log, _) = run_plan_pi_launch()?;
        let pi_args: Vec<&str> = pi_argv_log.lines().collect();
        let pi_delivery = pi_args
            .iter()
            .position(|arg| *arg == "-e")
            .ok_or_else(|| anyhow::anyhow!("pi extension missing: {pi_argv_log}"))?;
        let pi_prompt = pi_args
            .iter()
            .position(|arg| *arg == "# Specification Interview")
            .ok_or_else(|| anyhow::anyhow!("pi prompt missing: {pi_argv_log}"))?;
        assert!(pi_delivery < pi_prompt);
        Ok(())
    }

    #[test]
    fn interactive_pi_shell_out_installs_repin_extension() -> Result<()> {
        let (argv_log, env_log) = run_plan_pi_launch()?;
        let argv: Vec<&str> = argv_log.lines().collect();
        assert_eq!(argv.first().copied(), Some("run"));
        assert!(argv.contains(&"pi"), "expected pi argv: {argv_log}");
        assert!(
            argv.contains(&"--session-dir"),
            "session dir missing: {argv_log}"
        );
        assert!(argv.contains(&"-e"), "extension flag missing: {argv_log}");
        assert!(
            argv.iter()
                .any(|arg| arg.ends_with("loom-pi-repin-extension.js")),
            "extension path missing: {argv_log}"
        );
        assert!(
            !argv.contains(&"--settings"),
            "pi run must not receive claude settings: {argv_log}"
        );
        assert!(
            !argv.contains(&"--dangerously-skip-permissions"),
            "pi run must not receive claude flags: {argv_log}"
        );
        assert!(env_log.contains("ref=localhost/wrix-base-pi:abc"));
        assert!(env_log.contains("agent=pi"));
        Ok(())
    }

    #[test]
    fn plan_unknown_profile_returns_typed_error() -> Result<()> {
        let dir = tempfile::tempdir()?;
        seed_workspace(dir.path())?;
        let manifest = three_profile_manifest(dir.path())?;
        let bin = stub_wrix(dir.path())?;
        let mut opts = plan_opts(vec![SpecLabel::new("harness")], bin, manifest);
        opts.cli_profile = Some(ProfileName::new("missing"));

        let err = run_with_timeout(dir.path(), opts, Duration::from_secs(1)).unwrap_err();
        assert!(matches!(
            err,
            PlanError::Profile(ProfileError::UnknownProfile { .. })
        ));
        Ok(())
    }

    #[test]
    fn interactive_shell_out_rejects_direct_backend() -> Result<()> {
        let dir = tempfile::tempdir()?;
        seed_workspace(dir.path())?;
        std::fs::write(
            dir.path().join("loom.toml"),
            "[phase.plan]\nagent.backend = \"direct\"\n",
        )?;
        let manifest = three_profile_manifest(dir.path())?;
        let bin = stub_wrix(dir.path())?;

        let err = run_with_timeout(
            dir.path(),
            plan_opts(vec![SpecLabel::new("harness")], bin, manifest),
            Duration::from_secs(1),
        )
        .unwrap_err();

        assert!(matches!(err, PlanError::DirectInteractive));
        assert!(!dir.path().join("argv.log").exists());
        Ok(())
    }
}
