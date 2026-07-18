use std::path::{Path, PathBuf};

use anyhow::Result;
use loom_driver::git::{commit_all_in, init_test_repo_with_integration, validate_hooks_config};
use loom_driver::identifier::{BeadId, SpecLabel};

fn install_hook_chain(root: &Path, pre_commit_body: &str) -> Result<PathBuf> {
    let hooks = root.join("canonical-prek-hooks");
    std::fs::create_dir_all(&hooks)?;
    loom_test_support::write_executable_bash_script(hooks.join("pre-commit"), pre_commit_body)?;
    loom_test_support::write_executable_bash_script(hooks.join("pre-push"), "set -euo pipefail\n")?;
    Ok(hooks)
}

fn bead_workspace_with_hooks(root: &Path, hooks: &Path, bead: &str) -> Result<PathBuf> {
    let workspace = root.join("operator");
    let mut git = init_test_repo_with_integration(&workspace)?;
    git.set_prek_hooks_path_override(hooks.to_path_buf());
    let created = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(git.create_worktree(&SpecLabel::new("pre-commit"), &BeadId::new(bead)?))?;
    validate_hooks_config(&created.path, hooks)?;
    Ok(created.path)
}

#[test]
fn agent_commit_runs_pre_commit_chain() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let marker = dir.path().join("pre-commit-chain.log");
    let body = format!(
        "set -euo pipefail\nprintf '%s\\n' 'treefmt' >> '{marker}'\nprintf '%s\\n' 'loom-gate-verify-files' >> '{marker}'\n",
        marker = marker.display(),
    );
    let hooks = install_hook_chain(dir.path(), &body)?;
    let bead = bead_workspace_with_hooks(dir.path(), &hooks, "lm-agentcommit")?;

    std::fs::write(bead.join("agent-change.txt"), "agent change\n")?;
    commit_all_in(&bead, "exercise agent commit")?;

    assert_eq!(
        std::fs::read_to_string(marker)?,
        "treefmt\nloom-gate-verify-files\n"
    );
    Ok(())
}

#[test]
fn bead_container_skips_nix_flake_check() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let fixture_bin = dir.path().join("bead-bin");
    std::fs::create_dir_all(&fixture_bin)?;
    let skip = fixture_bin.join("skip-if-missing");
    loom_test_support::write_executable_bash_script(
        &skip,
        "set -euo pipefail\n\
         tool=\"$1\"\n\
         shift\n\
         [[ \"$1\" == '--' ]]\n\
         shift\n\
         if ! command -v \"$tool\" >/dev/null; then\n\
             exit 0\n\
         fi\n\
         exec \"$@\"\n",
    )?;
    let nix_marker = dir.path().join("nix-flake-check-ran");
    let nix_check = dir.path().join("nix-flake-check.sh");
    loom_test_support::write_executable_bash_script(
        &nix_check,
        &format!(
            "set -euo pipefail\nprintf 'ran\\n' > '{marker}'\n",
            marker = nix_marker.display(),
        ),
    )?;
    let remaining_marker = dir.path().join("remaining-hook-ran");
    let remaining = dir.path().join("remaining-hook.sh");
    loom_test_support::write_executable_bash_script(
        &remaining,
        &format!(
            "set -euo pipefail\nprintf 'ran\\n' > '{marker}'\n",
            marker = remaining_marker.display(),
        ),
    )?;
    let body = format!(
        "set -euo pipefail\nexport PATH='{path}'\n'{skip}' nix -- '{nix_check}'\n'{remaining}'\n",
        path = fixture_bin.display(),
        skip = skip.display(),
        nix_check = nix_check.display(),
        remaining = remaining.display(),
    );
    let hooks = install_hook_chain(dir.path(), &body)?;
    let bead = bead_workspace_with_hooks(dir.path(), &hooks, "lm-nonix")?;

    std::fs::write(bead.join("agent-change.txt"), "agent change\n")?;
    commit_all_in(&bead, "exercise bead hook chain")?;

    assert!(
        !nix_marker.exists(),
        "nix-only hook must not execute without nix on PATH"
    );
    assert_eq!(std::fs::read_to_string(remaining_marker)?, "ran\n");
    Ok(())
}
