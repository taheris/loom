use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tracing::info;

use loom_driver::agent::AgentKind;
use loom_driver::config::{LoomConfig, Phase};
use loom_driver::identifier::{ProfileName, SpecLabel};
use loom_driver::lock::LockManager;
use loom_driver::profile_manifest::{ImageEntry, ProfileImageManifest};
use loom_driver::scratch::{ScratchSession, resolve_scratch_key};
use loom_driver::state::StateDb;

use super::args::PlanMode;
use super::command::{WRIX_BIN, build_wrix_argv};
use super::companions::reconcile_companions;
use super::error::PlanError;
use super::prompt::{PlanPromptInputs, render_prompt};

/// Env var read by `wrix run` to pick the podman ref of the per-profile
/// image. Mirrors `lib/sandbox/linux/default.nix`.
pub const WRIX_DEFAULT_IMAGE_REF: &str = "WRIX_DEFAULT_IMAGE_REF";

/// Env var read by `wrix run` to pick the Nix store path handed to
/// `podman load`. Mirrors `lib/sandbox/linux/default.nix`.
pub const WRIX_DEFAULT_IMAGE_SOURCE: &str = "WRIX_DEFAULT_IMAGE_SOURCE";

/// Default timeout used by [`run`] — mirrors the rest of the spec-scoped
/// command surface (see `LockManager::acquire_spec`).
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Options accepted by [`run`].
pub struct PlanOpts {
    pub mode: PlanMode,
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
}

/// Files touched by [`run`]. Surfaces the resolved spec path and the
/// reconciled companion paths so the binary can print a useful summary.
#[derive(Debug, Clone)]
pub struct PlanReport {
    pub label: SpecLabel,
    pub spec_path: PathBuf,
    pub companion_paths: Vec<String>,
    /// `true` when the spec markdown contained a `## Companions` heading.
    /// `false` lets the CLI distinguish "intentionally empty" from "interview
    /// did not declare any" in the human-readable summary.
    pub companions_section_present: bool,
}

/// Run `loom plan` against `workspace`.
///
/// 1. Acquire `<label>.lock` for the duration of the call.
/// 2. Render the appropriate Askama template into a prompt body.
/// 3. Spawn `wrix run <workspace> <agent command> ... <prompt>` with stdio
///    inherited and wait for it to exit.
/// 4. After the interactive session exits, replace the companion rows for
///    `label` in the state DB by re-parsing the spec file.
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
    let label = opts.mode.label().clone();
    let is_new = matches!(opts.mode, PlanMode::New(_));

    let lock_mgr = LockManager::new(workspace)?;
    let _guard = lock_mgr.acquire_spec_with_timeout(&label, timeout)?;

    let cfg = LoomConfig::load(LoomConfig::resolve_path(workspace))
        .unwrap_or_else(|_| LoomConfig::default());

    let (profile, agent_kind) =
        resolve_plan_selection(opts.cli_profile.as_ref(), opts.agent_override, &cfg)?;
    let image: &ImageEntry = opts.manifest.lookup(&profile)?;

    let spec_rel = format!("specs/{}.md", label.as_str());
    let spec_path = workspace.join(&spec_rel);

    if !is_new && !spec_path.exists() {
        return Err(PlanError::SpecMissing {
            path: spec_path.clone(),
        });
    }

    let pinned_context = read_pinned_context(workspace, &cfg.pinned_context)?;

    let db = StateDb::open(workspace.join(".loom/state.db"))?;
    let companion_paths = if is_new {
        Vec::new()
    } else {
        db.companions(&label)?
    };
    // For -u, surface the current notes so the agent can perform the merge
    // (`specs/harness.md` § Implementation-notes lifecycle: "interview
    //  reads existing, writes back merged"). For -n the row does not exist
    // yet so there are no notes to read.
    let implementation_notes = if is_new {
        Vec::new()
    } else {
        db.notes_list(Some(&label), Some("implementation"))?
            .into_iter()
            .map(|row| row.text)
            .collect()
    };
    let key = resolve_scratch_key(Phase::Plan, &label, None);
    let scratchpad_path = ScratchSession::scratchpad_path_for(workspace, &key)
        .to_string_lossy()
        .into_owned();
    let prompt_body = render_prompt(PlanPromptInputs {
        mode: opts.mode,
        spec_path: spec_rel.clone(),
        pinned_context,
        companion_paths,
        implementation_notes,
        scratchpad_path,
        spec_conventions: cfg.spec_conventions.clone(),
    })?;

    // Set before wrix runs so a non-zero interactive exit (Ctrl-C, agent
    // crash) does not leave current_spec pointing at a stale prior spec.
    db.set_current_spec(&label)?;

    let banner = format!("loom plan @ {}", label);
    let scratch = ScratchSession::open(workspace, &key, &prompt_body, &banner)
        .map_err(|source| PlanError::Spawn { source })?;

    let argv = build_wrix_argv(workspace, &prompt_body, agent_kind);
    let bin: PathBuf = opts.wrix_bin.unwrap_or_else(|| PathBuf::from(WRIX_BIN));
    info!(
        label = %label,
        profile = %profile,
        agent = ?agent_kind,
        image_ref = %image.r#ref,
        image_source = %image.source.display(),
        wrix_bin = %bin.display(),
        scratch_dir = %scratch.path().display(),
        "loom plan: shelling out to interactive wrix run",
    );
    let status = Command::new(&bin)
        .args(&argv)
        .env(WRIX_DEFAULT_IMAGE_REF, &image.r#ref)
        .env(WRIX_DEFAULT_IMAGE_SOURCE, &image.source)
        .status()
        .map_err(|source| PlanError::Spawn { source })?;
    drop(scratch);
    if !status.success() {
        return Err(PlanError::WrixExit {
            status: status.to_string(),
        });
    }

    if is_new && !spec_path.exists() {
        return Err(PlanError::InterviewProducedNoSpec {
            path: spec_path.clone(),
        });
    }

    let outcome = reconcile_companions(&db, &label, &spec_path)?;

    Ok(PlanReport {
        label,
        spec_path,
        companion_paths: outcome.paths,
        companions_section_present: outcome.section_present,
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
) -> Result<(ProfileName, AgentKind), PlanError> {
    let mut selection = config.agent_for(Phase::Plan)?;
    if let Some(p) = cli_profile {
        selection.profile = p.clone();
    }
    if let Some(kind) = agent_override {
        selection.kind = kind;
    }
    Ok((selection.profile, selection.kind))
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
    use loom_driver::lock::LockError;
    use loom_driver::profile_manifest::ProfileError;
    use std::os::unix::fs::PermissionsExt;

    /// Three-profile manifest stub matching the `mkProfileImages` flake
    /// output shape so tests exercise the same lookup path the runner uses
    /// in production. Image-source paths are dummy files on disk so a
    /// runtime caller could `podman load` them — the stub never executes
    /// that branch but the path needs to deserialize as a valid `PathBuf`.
    fn three_profile_manifest(dir: &Path) -> Result<ProfileImageManifest> {
        let manifest_path = dir.join("profile-images.json");
        let body = format!(
            r#"{{
              "base":   {{ "ref": "localhost/wrix-base:abc",   "source": {base:?} }},
              "rust":   {{ "ref": "localhost/wrix-rust:def",   "source": {rust:?} }},
              "python": {{ "ref": "localhost/wrix-python:ghi", "source": {py:?} }}
            }}"#,
            base = dir.join("base.tar").display().to_string(),
            rust = dir.join("rust.tar").display().to_string(),
            py = dir.join("python.tar").display().to_string(),
        );
        std::fs::write(&manifest_path, body)?;
        Ok(ProfileImageManifest::from_path(&manifest_path)?)
    }

    /// Write a stub `wrix` shell launcher under `dir/bin/`, recording argv
    /// to `dir/argv.log` and the env vars the runner injected to
    /// `dir/env.log`, and return the absolute binary path. The script
    /// touches `<workspace>/specs/<label>.md` so post-session companion
    /// reconciliation finds a file to read — mirroring what claude would
    /// have written during the interview.
    fn install_wrix_stub(dir: &Path, post_session_spec: Option<(&Path, &str)>) -> Result<PathBuf> {
        let bin_dir = dir.join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        let bin = bin_dir.join("wrix-stub");
        let log = dir.join("argv.log");
        let env_log = dir.join("env.log");
        let mut script = format!(
            "#!/bin/sh\nset -e\n\
             # log argv one-per-line so tests can grep for individual flags\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\" >> {log:?}; done\n\
             printf -- '---\\n' >> {log:?}\n\
             # log the launcher-image env vars so tests can pin the contract\n\
             # the runner has with `wrix run` (see lib/sandbox/linux/default.nix)\n\
             printf 'WRIX_DEFAULT_IMAGE_REF=%s\\n' \"${{WRIX_DEFAULT_IMAGE_REF:-}}\" >> {env_log:?}\n\
             printf 'WRIX_DEFAULT_IMAGE_SOURCE=%s\\n' \"${{WRIX_DEFAULT_IMAGE_SOURCE:-}}\" >> {env_log:?}\n",
            log = log,
            env_log = env_log,
        );
        if let Some((spec, body)) = post_session_spec {
            let parent = spec.parent().unwrap().to_path_buf();
            script.push_str(&format!(
                "mkdir -p {parent:?}\ncat > {spec:?} <<'WRIX_EOF'\n{body}\nWRIX_EOF\n",
            ));
        }
        std::fs::write(&bin, script)?;
        let mut perm = std::fs::metadata(&bin)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bin, perm)?;
        Ok(bin)
    }

    fn workspace_with_specs() -> Result<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("specs"))?;
        std::fs::create_dir_all(dir.path().join(".loom"))?;
        // Seed the state DB so the runner can replace_companions afterwards.
        let _seed = StateDb::open(dir.path().join(".loom/state.db"))?;
        Ok(dir)
    }

    fn plan_opts_new(label: &str, bin: PathBuf, manifest: ProfileImageManifest) -> PlanOpts {
        PlanOpts {
            mode: PlanMode::New(SpecLabel::new(label)),
            wrix_bin: Some(bin),
            cli_profile: None,
            agent_override: None,
            manifest,
        }
    }

    fn plan_opts_update(label: &str, bin: PathBuf, manifest: ProfileImageManifest) -> PlanOpts {
        PlanOpts {
            mode: PlanMode::Update(SpecLabel::new(label)),
            wrix_bin: Some(bin),
            cli_profile: None,
            agent_override: None,
            manifest,
        }
    }

    #[test]
    fn plan_new_invokes_wrix_run_and_records_companions() -> Result<()> {
        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/harness.md");
        let bin = install_wrix_stub(
            dir.path(),
            Some((
                &spec_path,
                "# loom-harness\n\n## Companions\n\n- `lib/sandbox/`\n",
            )),
        )?;

        let manifest = three_profile_manifest(dir.path())?;
        let report = run_with_timeout(
            dir.path(),
            plan_opts_new("harness", bin, manifest),
            Duration::from_millis(100),
        )?;

        assert_eq!(report.label.as_str(), "harness");
        assert_eq!(report.companion_paths, vec!["lib/sandbox/"]);
        assert!(report.companions_section_present);

        let argv_log = std::fs::read_to_string(dir.path().join("argv.log"))?;
        let lines: Vec<&str> = argv_log.lines().collect();
        assert_eq!(lines[0], "run", "first argv must be `run`");
        assert!(!lines.contains(&"spawn"));
        assert!(!lines.contains(&"run-bead"));
        assert!(!lines.contains(&"--stdio"));
        assert!(!lines.contains(&"--spawn-config"));
        assert!(lines.contains(&"claude"));
        assert!(lines.contains(&"--dangerously-skip-permissions"));
        Ok(())
    }

    #[test]
    fn plan_phase_agent_pi_selects_pi_command() -> Result<()> {
        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/harness.md");
        let bin = install_wrix_stub(
            dir.path(),
            Some((&spec_path, "# loom-harness\n\n## Companions\n\n")),
        )?;
        std::fs::write(
            dir.path().join("loom.toml"),
            "[phase.default]\nagent.backend = \"pi\"\n",
        )?;
        let manifest = three_profile_manifest(dir.path())?;

        run_with_timeout(
            dir.path(),
            plan_opts_new("harness", bin, manifest),
            Duration::from_millis(100),
        )?;

        let argv_log = std::fs::read_to_string(dir.path().join("argv.log"))?;
        let lines: Vec<&str> = argv_log.lines().collect();
        assert!(lines.contains(&"pi"), "expected pi argv: {argv_log}");
        assert!(
            !lines.contains(&"claude"),
            "pi-backed plan must not call claude: {argv_log}",
        );
        assert!(
            !lines.contains(&"--dangerously-skip-permissions"),
            "pi-backed plan must not receive claude-only flags: {argv_log}",
        );
        Ok(())
    }

    /// Default profile resolution (no CLI override, empty config) lands on
    /// `base` per `LoomConfig::agent_for(Phase::Plan)`. The runner must
    /// inject `WRIX_DEFAULT_IMAGE_REF` + `WRIX_DEFAULT_IMAGE_SOURCE`
    /// into the spawned `wrix run` env so the launcher (which now refuses
    /// to start without them — see `lib/sandbox/linux/default.nix`) can
    /// resolve the image without a `--spawn-config`.
    #[test]
    fn plan_exports_default_image_env_for_wrix_run() -> Result<()> {
        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/harness.md");
        let bin = install_wrix_stub(
            dir.path(),
            Some((&spec_path, "# loom-harness\n\n## Companions\n\n")),
        )?;
        let manifest = three_profile_manifest(dir.path())?;

        run_with_timeout(
            dir.path(),
            plan_opts_new("harness", bin, manifest),
            Duration::from_millis(100),
        )?;

        let env_log = std::fs::read_to_string(dir.path().join("env.log"))?;
        assert!(
            env_log.contains("WRIX_DEFAULT_IMAGE_REF=localhost/wrix-base:abc"),
            "expected base profile ref. env.log:\n{env_log}",
        );
        let expected_source = dir.path().join("base.tar").display().to_string();
        assert!(
            env_log.contains(&format!("WRIX_DEFAULT_IMAGE_SOURCE={expected_source}")),
            "expected base profile source. env.log:\n{env_log}",
        );
        Ok(())
    }

    /// CLI `--profile rust` override beats the empty-config default and
    /// resolves to the `rust` manifest entry — exercising the precedence
    /// chain `cli_profile → [phase.plan] → [phase.default] → built-in base`.
    #[test]
    fn plan_cli_profile_override_picks_manifest_entry() -> Result<()> {
        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/harness.md");
        let bin = install_wrix_stub(
            dir.path(),
            Some((&spec_path, "# loom-harness\n\n## Companions\n\n")),
        )?;
        let manifest = three_profile_manifest(dir.path())?;

        let opts = PlanOpts {
            mode: PlanMode::New(SpecLabel::new("harness")),
            wrix_bin: Some(bin),
            cli_profile: Some(ProfileName::new("rust")),
            agent_override: None,
            manifest,
        };
        run_with_timeout(dir.path(), opts, Duration::from_millis(100))?;

        let env_log = std::fs::read_to_string(dir.path().join("env.log"))?;
        assert!(
            env_log.contains("WRIX_DEFAULT_IMAGE_REF=localhost/wrix-rust:def"),
            "CLI override must select rust ref. env.log:\n{env_log}",
        );
        let expected_source = dir.path().join("rust.tar").display().to_string();
        assert!(
            env_log.contains(&format!("WRIX_DEFAULT_IMAGE_SOURCE={expected_source}")),
            "CLI override must select rust source. env.log:\n{env_log}",
        );
        Ok(())
    }

    /// The resolved profile (from `LoomConfig::agent_for(Phase::Plan)` or
    /// the CLI override) flows to `wrix run` via the
    /// `WRIX_DEFAULT_IMAGE_REF` / `WRIX_DEFAULT_IMAGE_SOURCE` env vars
    /// — not via argv. `wrix run` has no `--profile` parser; any
    /// trailing tokens are forwarded into the container as the command
    /// vector, so passing `--profile <name>` made the entrypoint exec
    /// `--profile` and exit 127. The env-var contract documented in
    /// `specs/harness.md` § Profile-Image Manifest is the sole hand-off.
    #[test]
    fn plan_runner_passes_resolved_profile_to_wrix_run() -> Result<()> {
        for (cli_profile, expected_ref, expected_source_name) in [
            (None, "localhost/wrix-base:abc", "base.tar"),
            (
                Some(ProfileName::new("rust")),
                "localhost/wrix-rust:def",
                "rust.tar",
            ),
        ] {
            let dir = workspace_with_specs()?;
            let spec_path = dir.path().join("specs/harness.md");
            let bin = install_wrix_stub(
                dir.path(),
                Some((&spec_path, "# loom-harness\n\n## Companions\n\n")),
            )?;
            let manifest = three_profile_manifest(dir.path())?;

            let opts = PlanOpts {
                mode: PlanMode::New(SpecLabel::new("harness")),
                wrix_bin: Some(bin),
                cli_profile,
                agent_override: None,
                manifest,
            };
            run_with_timeout(dir.path(), opts, Duration::from_millis(100))?;

            let argv_log = std::fs::read_to_string(dir.path().join("argv.log"))?;
            assert!(
                !argv_log.lines().any(|l| l == "--profile"),
                "wrix run has no --profile parser; the flag must not appear in argv. \
                 argv.log:\n{argv_log}",
            );

            let env_log = std::fs::read_to_string(dir.path().join("env.log"))?;
            assert!(
                env_log.contains(&format!("WRIX_DEFAULT_IMAGE_REF={expected_ref}")),
                "resolved profile must select image ref via env var. env.log:\n{env_log}",
            );
            let expected_source = dir.path().join(expected_source_name).display().to_string();
            assert!(
                env_log.contains(&format!("WRIX_DEFAULT_IMAGE_SOURCE={expected_source}")),
                "resolved profile must select image source via env var. env.log:\n{env_log}",
            );
        }
        Ok(())
    }

    /// `[phase.plan].profile` from `<workspace>/loom.toml` wins when no
    /// CLI override is set — verifies the second tier of precedence.
    #[test]
    fn plan_phase_config_profile_picks_manifest_entry() -> Result<()> {
        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/harness.md");
        let bin = install_wrix_stub(
            dir.path(),
            Some((&spec_path, "# loom-harness\n\n## Companions\n\n")),
        )?;
        std::fs::write(
            dir.path().join("loom.toml"),
            "[phase.plan]\nprofile = \"python\"\n",
        )?;
        let manifest = three_profile_manifest(dir.path())?;

        run_with_timeout(
            dir.path(),
            plan_opts_new("harness", bin, manifest),
            Duration::from_millis(100),
        )?;

        let env_log = std::fs::read_to_string(dir.path().join("env.log"))?;
        assert!(
            env_log.contains("WRIX_DEFAULT_IMAGE_REF=localhost/wrix-python:ghi"),
            "phase config must select python. env.log:\n{env_log}",
        );
        Ok(())
    }

    /// Regression: `[phase.default].profile` alone (no `[phase.plan]`
    /// override) must propagate to the image-env vars `wrix run`
    /// reads. Was masked while the bogus argv `--profile <name>` made
    /// the entrypoint exit 127 before the env vars could pick the
    /// image — the user's bug report ("kept getting a base profile").
    #[test]
    fn plan_phase_default_profile_alone_picks_manifest_entry() -> Result<()> {
        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/harness.md");
        let bin = install_wrix_stub(
            dir.path(),
            Some((&spec_path, "# loom-harness\n\n## Companions\n\n")),
        )?;
        std::fs::write(
            dir.path().join("loom.toml"),
            "[phase.default]\nprofile = \"rust\"\n",
        )?;
        let manifest = three_profile_manifest(dir.path())?;

        run_with_timeout(
            dir.path(),
            plan_opts_new("harness", bin, manifest),
            Duration::from_millis(100),
        )?;

        let env_log = std::fs::read_to_string(dir.path().join("env.log"))?;
        assert!(
            env_log.contains("WRIX_DEFAULT_IMAGE_REF=localhost/wrix-rust:def"),
            "phase.default profile must reach Phase::Plan via the env-var \
             hand-off. env.log:\n{env_log}",
        );
        let expected_source = dir.path().join("rust.tar").display().to_string();
        assert!(
            env_log.contains(&format!("WRIX_DEFAULT_IMAGE_SOURCE={expected_source}")),
            "phase.default profile must reach Phase::Plan via the env-var \
             hand-off. env.log:\n{env_log}",
        );
        Ok(())
    }

    /// A profile name not declared in the manifest fails with
    /// `ProfileError::UnknownProfile` — no silent fallback to `base`,
    /// matching the per-bead dispatch contract.
    #[test]
    fn plan_unknown_profile_returns_typed_error() -> Result<()> {
        let dir = workspace_with_specs()?;
        let manifest = three_profile_manifest(dir.path())?;
        let opts = PlanOpts {
            mode: PlanMode::New(SpecLabel::new("harness")),
            wrix_bin: Some(PathBuf::from("/nonexistent/wrix")),
            cli_profile: Some(ProfileName::new("ruby")),
            agent_override: None,
            manifest,
        };

        match run_with_timeout(dir.path(), opts, Duration::from_millis(100)) {
            Err(PlanError::Profile(ProfileError::UnknownProfile { name, .. })) => {
                assert_eq!(name, ProfileName::new("ruby"));
                Ok(())
            }
            other => Err(anyhow::anyhow!("expected UnknownProfile, got {other:?}")),
        }
    }

    #[test]
    fn plan_update_threads_existing_companions_into_prompt() -> Result<()> {
        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/harness.md");
        std::fs::write(
            &spec_path,
            "# loom-harness\n\n## Companions\n\n- `lib/sandbox/`\n",
        )?;
        let db = StateDb::open(dir.path().join(".loom/state.db"))?;
        db.replace_companions(&SpecLabel::new("harness"), &["lib/sandbox/".to_string()])?;
        drop(db);

        let bin = install_wrix_stub(
            dir.path(),
            Some((
                &spec_path,
                "# loom-harness\n\n## Companions\n\n- `lib/sandbox/`\n- `crates/loom-templates/templates/`\n",
            )),
        )?;
        let manifest = three_profile_manifest(dir.path())?;

        let report = run_with_timeout(
            dir.path(),
            plan_opts_update("harness", bin, manifest),
            Duration::from_millis(100),
        )?;

        assert_eq!(
            report.companion_paths,
            vec!["lib/sandbox/", "crates/loom-templates/templates/"]
        );

        let argv_log = std::fs::read_to_string(dir.path().join("argv.log"))?;
        assert!(argv_log.contains("# Specification Update Interview"));
        assert!(argv_log.contains("- lib/sandbox/"));
        Ok(())
    }

    /// `plan -u` must read the spec's prior `kind = 'implementation'` notes
    /// from the state DB and render them into the rendered prompt body so the
    /// agent can perform the keep/drop/add merge described in the spec's
    /// *Implementation-notes lifecycle* section. The notes appear verbatim
    /// in argv (the prompt body is the final positional for `wrix run`).
    #[test]
    fn plan_update_threads_existing_implementation_notes_into_prompt() -> Result<()> {
        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/harness.md");
        std::fs::write(
            &spec_path,
            "# loom-harness\n\n## Companions\n\n- `lib/sandbox/`\n",
        )?;
        let db = StateDb::open(dir.path().join(".loom/state.db"))?;
        let label = SpecLabel::new("harness");
        db.notes_set(
            &label,
            "implementation",
            &[
                "alpha-note about parser invariants".to_string(),
                "beta-note about retry/backoff".to_string(),
            ],
            0,
        )?;
        drop(db);

        let bin = install_wrix_stub(
            dir.path(),
            Some((
                &spec_path,
                "# loom-harness\n\n## Companions\n\n- `lib/sandbox/`\n",
            )),
        )?;
        let manifest = three_profile_manifest(dir.path())?;

        run_with_timeout(
            dir.path(),
            plan_opts_update("harness", bin, manifest),
            Duration::from_millis(100),
        )?;

        let argv_log = std::fs::read_to_string(dir.path().join("argv.log"))?;
        assert!(
            argv_log.contains("alpha-note about parser invariants"),
            "rendered prompt must surface the prior alpha note. argv.log:\n{argv_log}",
        );
        assert!(
            argv_log.contains("beta-note about retry/backoff"),
            "rendered prompt must surface the prior beta note. argv.log:\n{argv_log}",
        );
        // The prompt must frame the rewrite as a keep/drop/add merge.
        let lower = argv_log.to_lowercase();
        assert!(
            lower.contains("keep") && lower.contains("drop") && lower.contains("add"),
            "rendered prompt must name keep/drop/add merge ops. argv.log:\n{argv_log}",
        );
        // The agent must be instructed to write back via `loom note set`.
        assert!(
            argv_log.contains("loom note set"),
            "rendered prompt must direct the agent at `loom note set`. argv.log:\n{argv_log}",
        );
        Ok(())
    }

    /// `plan -n` runs in a workspace where no `notes` row exists yet for
    /// `label`, so the runner must not attempt to read prior notes; the
    /// rendered prompt nevertheless instructs the agent to seed the table
    /// via `loom note set <label> --kind implementation` so the spec row is
    /// inserted (via `ensure_spec_row`) and `loom todo` has something to
    /// consume.
    #[test]
    fn plan_new_prompt_directs_agent_to_seed_implementation_notes() -> Result<()> {
        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/harness.md");
        let bin = install_wrix_stub(
            dir.path(),
            Some((&spec_path, "# loom-harness\n\n## Companions\n\n")),
        )?;
        let manifest = three_profile_manifest(dir.path())?;

        run_with_timeout(
            dir.path(),
            plan_opts_new("harness", bin, manifest),
            Duration::from_millis(100),
        )?;

        let argv_log = std::fs::read_to_string(dir.path().join("argv.log"))?;
        assert!(
            argv_log.contains("loom note set"),
            "plan_new prompt must direct the agent at `loom note set`. argv.log:\n{argv_log}",
        );
        assert!(
            argv_log.contains("harness"),
            "plan_new prompt must inject the label into the example invocation. argv.log:\n{argv_log}",
        );
        assert!(
            argv_log.contains("--kind implementation"),
            "plan_new prompt must name the implementation note kind. argv.log:\n{argv_log}",
        );
        Ok(())
    }

    #[test]
    fn plan_update_errors_when_spec_missing() -> Result<()> {
        let dir = workspace_with_specs()?;
        let manifest = three_profile_manifest(dir.path())?;
        let result = run_with_timeout(
            dir.path(),
            plan_opts_update("harness", PathBuf::from("/nonexistent/wrix"), manifest),
            Duration::from_millis(100),
        );
        match result {
            Err(PlanError::SpecMissing { path }) => {
                assert!(path.ends_with("specs/harness.md"));
                Ok(())
            }
            other => Err(anyhow::anyhow!("expected SpecMissing, got {other:?}")),
        }
    }

    #[test]
    fn plan_new_errors_when_interview_writes_no_spec() -> Result<()> {
        let dir = workspace_with_specs()?;
        // Stub exits 0 without writing the spec — mimics the agent quitting
        // the interview without saving the file.
        let bin = install_wrix_stub(dir.path(), None)?;
        let manifest = three_profile_manifest(dir.path())?;

        let result = run_with_timeout(
            dir.path(),
            plan_opts_new("harness", bin, manifest),
            Duration::from_millis(100),
        );
        match result {
            Err(PlanError::InterviewProducedNoSpec { path }) => {
                assert!(path.ends_with("specs/harness.md"));
                Ok(())
            }
            other => Err(anyhow::anyhow!(
                "expected InterviewProducedNoSpec, got {other:?}"
            )),
        }
    }

    #[test]
    fn plan_new_flags_missing_companions_section() -> Result<()> {
        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/harness.md");
        // Spec written, but the agent did not include `## Companions`.
        let bin = install_wrix_stub(
            dir.path(),
            Some((&spec_path, "# loom-harness\n\nNo companions yet.\n")),
        )?;
        let manifest = three_profile_manifest(dir.path())?;

        let report = run_with_timeout(
            dir.path(),
            plan_opts_new("harness", bin, manifest),
            Duration::from_millis(100),
        )?;

        assert!(report.companion_paths.is_empty());
        assert!(!report.companions_section_present);
        Ok(())
    }

    #[test]
    fn plan_sets_current_spec_so_subsequent_commands_resolve_label() -> Result<()> {
        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/harness.md");
        let bin = install_wrix_stub(
            dir.path(),
            Some((&spec_path, "# loom-harness\n\n## Companions\n\n")),
        )?;
        let manifest = three_profile_manifest(dir.path())?;

        run_with_timeout(
            dir.path(),
            plan_opts_new("harness", bin, manifest),
            Duration::from_millis(100),
        )?;

        let db = StateDb::open(dir.path().join(".loom/state.db"))?;
        let current = db
            .current_spec()?
            .ok_or_else(|| anyhow::anyhow!("plan must set current_spec"))?;
        assert_eq!(current.as_str(), "harness");
        Ok(())
    }

    /// Before the interactive `wrix run` exec, the runner
    /// must have installed the per-key scratch directory with `prompt.txt`
    /// (rendered plan template), `repin.sh` (executable), and the
    /// `claude-settings.json` fragment registering `repin.sh` under
    /// `SessionStart[matcher: compact]`. The scratch dir is removed by Drop
    /// after exec, so the wrix stub snapshots the three files mid-run for
    /// the assertions below.
    #[test]
    fn plan_installs_scratch_dir_before_wrix_exec() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/harness.md");
        let snap_dir = dir.path().join("scratch-snap");
        std::fs::create_dir_all(&snap_dir)?;
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        let bin = bin_dir.join("wrix-stub");
        let scratch_root = "$2/.loom/scratch/harness";
        let script = format!(
            "#!/bin/sh\nset -e\n\
             cp {scratch_root}/prompt.txt {snap_dir:?}/prompt.txt\n\
             cp {scratch_root}/claude-settings.json {snap_dir:?}/claude-settings.json\n\
             cp {scratch_root}/repin.sh {snap_dir:?}/repin.sh\n\
             test -x {scratch_root}/repin.sh\n\
             cat > {spec_path:?} <<'WRIX_EOF'\n# loom-harness\n\n## Companions\n\nWRIX_EOF\n",
            scratch_root = scratch_root,
            snap_dir = snap_dir,
            spec_path = spec_path,
        );
        std::fs::write(&bin, script)?;
        let mut perm = std::fs::metadata(&bin)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bin, perm)?;

        let manifest = three_profile_manifest(dir.path())?;
        run_with_timeout(
            dir.path(),
            plan_opts_new("harness", bin, manifest),
            Duration::from_millis(100),
        )?;

        let prompt = std::fs::read_to_string(snap_dir.join("prompt.txt"))?;
        assert!(
            prompt.contains("# Specification Interview"),
            "prompt.txt must hold the rendered plan_new template body: {prompt}",
        );
        let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
            snap_dir.join("claude-settings.json"),
        )?)?;
        let hook = &settings["hooks"]["SessionStart"][0];
        assert_eq!(hook["matcher"], "compact");
        let cmd = hook["hooks"][0]["command"].as_str().unwrap();
        assert!(
            cmd.ends_with("repin.sh"),
            "claude-settings hook must invoke repin.sh: {cmd}",
        );
        let repin = std::fs::read_to_string(snap_dir.join("repin.sh"))?;
        assert!(repin.contains("hookSpecificOutput"));
        assert!(repin.contains("SessionStart"));

        // Drop must have cleaned the scratch dir after exec returned.
        assert!(
            !dir.path().join(".loom/scratch/harness").exists(),
            "scratch dir must be cleaned up after wrix returns",
        );
        Ok(())
    }

    /// Mirror of `plan_installs_scratch_dir_before_wrix_exec` for the
    /// update branch — the prompt.txt body must be the `plan_update`
    /// template, not `plan_new`, when an existing spec is being revised.
    #[test]
    fn plan_update_installs_scratch_dir_with_update_template() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let dir = workspace_with_specs()?;
        let spec_path = dir.path().join("specs/harness.md");
        std::fs::write(
            &spec_path,
            "# loom-harness\n\n## Companions\n\n- `lib/sandbox/`\n",
        )?;
        let snap_dir = dir.path().join("scratch-snap");
        std::fs::create_dir_all(&snap_dir)?;
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        let bin = bin_dir.join("wrix-stub");
        let scratch_root = "$2/.loom/scratch/harness";
        let script = format!(
            "#!/bin/sh\nset -e\n\
             cp {scratch_root}/prompt.txt {snap_dir:?}/prompt.txt\n\
             cat > {spec_path:?} <<'WRIX_EOF'\n# loom-harness\n\n## Companions\n\n- `lib/sandbox/`\nWRIX_EOF\n",
            scratch_root = scratch_root,
            snap_dir = snap_dir,
            spec_path = spec_path,
        );
        std::fs::write(&bin, script)?;
        let mut perm = std::fs::metadata(&bin)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bin, perm)?;

        let manifest = three_profile_manifest(dir.path())?;
        run_with_timeout(
            dir.path(),
            plan_opts_update("harness", bin, manifest),
            Duration::from_millis(100),
        )?;

        let prompt = std::fs::read_to_string(snap_dir.join("prompt.txt"))?;
        assert!(
            prompt.contains("# Specification Update Interview"),
            "prompt.txt must hold the rendered plan_update template body: {prompt}",
        );
        Ok(())
    }

    /// Interactive sessions (plan_new, plan_update, msg) skip the
    /// verdict-gate reconciliation path per `specs/templates.md`
    /// Implementation Note 5. A mid-session crash surfaces as a typed
    /// `WrixExit` error on the first attempt; the runner does NOT
    /// retry, does NOT construct a `PreviousFailure`, and does NOT
    /// mutate bd state. The wrix stub is invoked exactly once.
    #[test]
    fn plan_new_crash_surfaces_without_retry_dispatch() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let dir = workspace_with_specs()?;
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir)?;
        let bin = bin_dir.join("wrix-stub");
        let call_log = dir.path().join("call_count.log");
        // Record one line per invocation, then exit non-zero to simulate
        // a mid-session crash.
        let script = format!(
            "#!/bin/sh\necho call >> {call_log:?}\nexit 137\n",
            call_log = call_log,
        );
        std::fs::write(&bin, script)?;
        let mut perm = std::fs::metadata(&bin)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&bin, perm)?;

        let manifest = three_profile_manifest(dir.path())?;
        let result = run_with_timeout(
            dir.path(),
            plan_opts_new("harness", bin, manifest),
            Duration::from_millis(100),
        );
        match result {
            Err(PlanError::WrixExit { status }) => {
                assert!(
                    status.contains("137") || status.contains("exit"),
                    "WrixExit must surface the non-zero status: {status}",
                );
            }
            other => return Err(anyhow::anyhow!("expected WrixExit, got {other:?}")),
        }

        let calls = std::fs::read_to_string(&call_log).unwrap_or_default();
        let call_count = calls.lines().count();
        assert_eq!(
            call_count, 1,
            "interactive sessions must not retry on crash; wrix stub was \
             invoked {call_count} time(s) in:\n{calls}",
        );
        Ok(())
    }

    #[test]
    fn plan_acquires_per_spec_lock() -> Result<()> {
        let dir = workspace_with_specs()?;
        let mgr = LockManager::new(dir.path())?;
        let _hold = mgr.acquire_spec(&SpecLabel::new("alpha"))?;
        let manifest = three_profile_manifest(dir.path())?;

        match run_with_timeout(
            dir.path(),
            plan_opts_new("alpha", PathBuf::from("/nonexistent/wrix"), manifest),
            Duration::from_millis(100),
        ) {
            Err(PlanError::Lock(LockError::SpecBusy { label })) => {
                assert_eq!(label, "alpha");
                Ok(())
            }
            other => Err(anyhow::anyhow!("expected SpecBusy, got {other:?}")),
        }
    }
}
