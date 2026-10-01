//! Per-session scratch directory at `.loom/scratch/<key>/`.
//!
//! The scratch dir is the agent's compaction-recovery surface. It holds:
//! - `prompt.txt` — the initial prompt sent at session start.
//! - `scratch.md` — empty scratchpad. The agent appends decisions, open
//!   questions, and TODOs as the session progresses.
//! - `repin.sh` — emits the `SessionStart[compact]` JSON envelope by
//!   `cat`-ing `prompt.txt` and `scratch.md` at run time, so live
//!   scratchpad edits flow into the re-pin payload.
//! - `claude-settings.json` — registers `repin.sh` under
//!   `SessionStart[matcher: "compact"]`.
//!
//! `<key>` is the joined plan anchors (or `plan`), the todo work epic id,
//! or the bead id for `loom loop` / `loom gate` / `loom inbox`. Two parallel
//! `loom loop` workers on different beads get independent dirs.
//!
//! Cleanup runs on every exit path: [`Drop`] removes the directory
//! unconditionally, so a panic in the workflow engine still leaves no
//! carry-over for the next session. [`ScratchSession::close`] is the
//! explicit teardown and is idempotent w.r.t. the [`Drop`] cleanup.

mod directory;
pub mod runtime;

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use directory::Directory;
use rustix::fs::Mode;
use serde_json::json;

use crate::config::Phase;
use crate::identifier::{BeadId, SpecLabel};

const SCRATCH_SUBDIR: &str = ".loom/scratch";

/// Resolve the per-session scratch-dir key for a phase.
///
/// Plan sessions use joined anchor labels or `plan`; todo passes the work
/// epic as `bead_id`; loop, gate review, and inbox pass the bead under discussion.
pub fn resolve_scratch_key(
    phase: Phase,
    anchor_labels: &[SpecLabel],
    bead_id: Option<&BeadId>,
) -> String {
    match phase {
        Phase::Plan => resolve_plan_scratch_key(anchor_labels),
        Phase::Todo | Phase::Loop | Phase::Review | Phase::Inbox => bead_id.map_or_else(
            || fallback_label_key(anchor_labels, phase),
            |b| b.as_str().to_string(),
        ),
    }
}

/// Resolve the plan-session scratch key from its initial anchors.
pub fn resolve_plan_scratch_key(anchor_labels: &[SpecLabel]) -> String {
    if anchor_labels.is_empty() {
        return "plan".to_string();
    }
    anchor_labels
        .iter()
        .map(SpecLabel::as_str)
        .collect::<Vec<_>>()
        .join("+")
}

fn fallback_label_key(anchor_labels: &[SpecLabel], phase: Phase) -> String {
    anchor_labels.first().map_or_else(
        || phase.as_str().replace('.', "+"),
        |label| label.as_str().to_string(),
    )
}

/// Owns a `.loom/scratch/<key>/` directory for the duration of an
/// agent session. Drops the directory when the guard goes out of scope.
#[derive(Debug)]
pub struct ScratchSession {
    path: PathBuf,
    parent: Directory,
    directory: Directory,
    name: PathBuf,
}

impl ScratchSession {
    /// Compute the absolute path to `scratch.md` for `key` without opening
    /// a session. The path is deterministic so phase render contexts that
    /// need to embed it in the rendered prompt can resolve it before
    /// [`ScratchSession::open`] writes the on-disk layout.
    pub fn scratchpad_path_for(workspace: &Path, key: &str) -> PathBuf {
        workspace.join(SCRATCH_SUBDIR).join(key).join("scratch.md")
    }

    /// Open a fresh scratch session for `key`. Removes any leftover
    /// directory from a crashed prior session, recreates the layout, and
    /// writes `prompt.txt`, an empty `scratch.md`, an executable
    /// `repin.sh`, and `claude-settings.json`.
    ///
    /// `banner` is the fixed preamble emitted by `repin.sh` ahead of the
    /// `prompt.txt` and `scratch.md` contents — typically a short
    /// orientation string like `loom loop @ <bead-id>`.
    ///
    /// # Errors
    ///
    /// Returns an error when scratch workspace state cannot be created or inspected.
    pub fn open(workspace: &Path, key: &str, prompt: &str, banner: &str) -> io::Result<Self> {
        if key.is_empty() || key == "." || key.contains('/') || key.contains("..") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid scratch key: {key:?}"),
            ));
        }
        let workspace_dir = Directory::open(workspace)?;
        let loom = workspace_dir.ensure_child(Path::new(".loom"))?;
        let parent = loom.ensure_child(Path::new("scratch"))?;
        parent.make_private()?;
        let name = Path::new(key);
        match parent.child(name) {
            Ok(stale) => {
                stale.make_private()?;
                parent.remove_child(name, &stale)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let directory = parent.create_child(name)?;
        let session = Self {
            path: workspace.join(SCRATCH_SUBDIR).join(key),
            parent,
            directory,
            name: PathBuf::from(key),
        };
        session
            .directory
            .create_file(Path::new("prompt.txt"), Mode::RUSR | Mode::WUSR)?
            .write_all(prompt.as_bytes())?;
        session
            .directory
            .create_file(Path::new("scratch.md"), Mode::RUSR | Mode::WUSR)?;
        write_repin_script(&session.directory, banner)?;
        write_claude_settings(&session.directory, key)?;
        Ok(session)
    }

    /// Path to the scratch dir.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Path to the `repin.sh` script.
    pub fn repin_script(&self) -> PathBuf {
        self.path.join("repin.sh")
    }

    /// Path to the `claude-settings.json` hook fragment.
    pub fn claude_settings(&self) -> PathBuf {
        self.path.join("claude-settings.json")
    }

    /// Explicit cleanup. Equivalent to letting the guard drop, but
    /// surfaces I/O errors instead of swallowing them. Idempotent — a
    /// second call (or a follow-on Drop) on a missing directory is a
    /// no-op.
    ///
    /// # Errors
    ///
    /// Returns an error when scratch workspace state cannot be created or inspected.
    pub fn close(self) -> io::Result<()> {
        self.parent.remove_child(&self.name, &self.directory)
    }
}

impl Drop for ScratchSession {
    fn drop(&mut self) {
        if let Err(e) = self.parent.remove_child(&self.name, &self.directory) {
            tracing::warn!(
                path = %self.path.display(),
                error = ?e,
                "scratch dir cleanup failed",
            );
        }
    }
}

fn write_repin_script(dir: &Directory, banner: &str) -> io::Result<()> {
    let banner_lit = bash_single_quote(banner);
    let script = format!(
        "#!/usr/bin/env bash\n\
         set -euo pipefail\n\
         here=\"$(dirname \"$0\")\"\n\
         {{\n  \
           printf '%s\\n\\n' {banner_lit}\n  \
           cat \"$here/prompt.txt\"\n  \
           printf '\\n\\n'\n  \
           cat \"$here/scratch.md\"\n\
         }} | jq -Rs '{{hookSpecificOutput:{{hookEventName:\"SessionStart\",additionalContext:.}}}}'\n",
    );
    dir.create_file(Path::new("repin.sh"), Mode::RWXU)?
        .write_all(script.as_bytes())
}

fn write_claude_settings(dir: &Directory, key: &str) -> io::Result<()> {
    let script_path = Path::new(SCRATCH_SUBDIR).join(key).join("repin.sh");
    let settings = json!({
        "hooks": {
            "SessionStart": [
                {
                    "matcher": "compact",
                    "hooks": [
                        {
                            "type": "command",
                            "command": script_path.to_string_lossy(),
                        }
                    ]
                }
            ]
        }
    });
    let body = serde_json::to_string_pretty(&settings).map_err(io::Error::other)?;
    dir.create_file(Path::new("claude-settings.json"), Mode::RUSR | Mode::WUSR)?
        .write_all(body.as_bytes())
}

/// Single-quote a string for safe inclusion in a bash script. A literal
/// single quote inside the input is closed, escaped, and reopened:
/// `it's` → `'it'\''s'`.
fn bash_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
            .is_ok_and(|o| o.status.success())
    }

    fn repin_context(prompt: &str, scratch: &str) -> Option<String> {
        if !jq_is_available() {
            eprintln!("jq missing; skipping repin.sh envelope test");
            return None;
        }
        let workspace = tempfile::tempdir().unwrap();
        let session =
            ScratchSession::open(workspace.path(), "lm-3", prompt, "loom loop @ lm-3").unwrap();
        fs::write(session.path().join("scratch.md"), scratch).unwrap();

        let out = std::process::Command::new("bash")
            .arg(session.repin_script())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "repin.sh failed: stderr={}",
            String::from_utf8_lossy(&out.stderr),
        );
        let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["hookEventName"],
            "SessionStart",
        );
        Some(
            parsed["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap()
                .to_string(),
        )
    }

    #[test]
    fn open_creates_layout_and_drop_removes_it() {
        let workspace = tempfile::tempdir().unwrap();
        let key = "harness";
        let path = {
            let session = ScratchSession::open(
                workspace.path(),
                key,
                "initial prompt body",
                "loom plan @ loom-harness",
            )
            .unwrap();
            assert!(session.path().exists());
            assert_eq!(
                fs::read_to_string(session.path().join("prompt.txt")).unwrap(),
                "initial prompt body",
            );
            assert_eq!(
                fs::read_to_string(session.path().join("scratch.md")).unwrap(),
                "",
            );
            assert!(session.repin_script().exists());
            assert!(session.claude_settings().exists());
            session.path().to_path_buf()
        };
        assert!(!path.exists(), "drop should remove scratch dir");
    }

    #[test]
    fn scratch_directories_are_owner_only_from_creation() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let session =
            ScratchSession::open(workspace.path(), "private", "prompt", "banner").unwrap();
        for path in [session.path(), session.path().parent().unwrap()] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o7777,
                0o700
            );
        }
    }

    #[test]
    fn scratch_setup_rejects_symlink_components_without_touching_targets() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        for component in [".loom", ".loom/scratch", ".loom/scratch/key"] {
            let workspace = tempfile::tempdir().unwrap();
            let target = tempfile::tempdir().unwrap();
            fs::write(target.path().join("sentinel"), "untouched").unwrap();
            fs::set_permissions(target.path(), fs::Permissions::from_mode(0o755)).unwrap();
            let path = workspace.path().join(component);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            symlink(target.path(), &path).unwrap();
            assert!(ScratchSession::open(workspace.path(), "key", "prompt", "banner").is_err());
            assert_eq!(
                fs::read_to_string(target.path().join("sentinel")).unwrap(),
                "untouched"
            );
            assert_eq!(fs::read_dir(target.path()).unwrap().count(), 1);
            assert_eq!(
                fs::metadata(target.path()).unwrap().permissions().mode() & 0o777,
                0o755
            );
            assert_eq!(fs::read_link(path).unwrap(), target.path());
        }
    }

    #[test]
    fn scratch_setup_rejects_writable_ancestors() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let loom = workspace.path().join(".loom");
        fs::create_dir(&loom).unwrap();
        fs::set_permissions(&loom, fs::Permissions::from_mode(0o777)).unwrap();
        let error = ScratchSession::open(workspace.path(), "key", "prompt", "banner").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(!loom.join("scratch").exists());
    }

    #[test]
    fn scratch_setup_rejects_existing_file_as_session_directory() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join(SCRATCH_SUBDIR).join("key");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "untouched").unwrap();
        assert!(ScratchSession::open(workspace.path(), "key", "prompt", "banner").is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "untouched");
    }

    #[test]
    fn scratch_cleanup_rejects_replaced_session_directory() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::write(target.path().join("sentinel"), "untouched").unwrap();
        let session = ScratchSession::open(workspace.path(), "key", "prompt", "banner").unwrap();
        let path = session.path().to_path_buf();
        fs::rename(&path, path.with_extension("moved")).unwrap();
        symlink(target.path(), &path).unwrap();
        assert!(session.close().is_err());
        assert_eq!(
            fs::read_to_string(target.path().join("sentinel")).unwrap(),
            "untouched"
        );
        assert_eq!(fs::read_link(path).unwrap(), target.path());
    }

    #[test]
    fn scratch_cleanup_is_anchored_when_parent_path_is_replaced() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::create_dir(target.path().join("key")).unwrap();
        fs::write(target.path().join("key/sentinel"), "untouched").unwrap();
        let session = ScratchSession::open(workspace.path(), "key", "prompt", "banner").unwrap();
        let parent = session.path().parent().unwrap().to_path_buf();
        let moved = parent.with_extension("moved");
        fs::rename(&parent, &moved).unwrap();
        symlink(target.path(), &parent).unwrap();
        session.close().unwrap();
        assert!(!moved.join("key").exists());
        assert_eq!(
            fs::read_to_string(target.path().join("key/sentinel")).unwrap(),
            "untouched"
        );
    }

    #[test]
    fn stale_scratch_cleanup_does_not_follow_nested_symlinks() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::write(target.path().join("sentinel"), "untouched").unwrap();
        let path = workspace.path().join(SCRATCH_SUBDIR).join("key");
        fs::create_dir_all(path.join("offload")).unwrap();
        symlink(target.path(), path.join("offload/link")).unwrap();
        let session = ScratchSession::open(workspace.path(), "key", "prompt", "banner").unwrap();
        assert!(!session.path().join("offload").exists());
        assert_eq!(
            fs::read_to_string(target.path().join("sentinel")).unwrap(),
            "untouched"
        );
    }

    #[test]
    fn open_clears_leftover_dir_from_prior_session() {
        let workspace = tempfile::tempdir().unwrap();
        let dir = workspace.path().join(SCRATCH_SUBDIR).join("lm-1");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("scratch.md"), "stale content").unwrap();

        let session = ScratchSession::open(workspace.path(), "lm-1", "fresh", "banner").unwrap();
        assert_eq!(
            fs::read_to_string(session.path().join("scratch.md")).unwrap(),
            "",
            "open should overwrite stale scratchpad",
        );
    }

    #[test]
    fn close_removes_dir_and_is_idempotent_with_drop() {
        let workspace = tempfile::tempdir().unwrap();
        let session = ScratchSession::open(workspace.path(), "lm-2", "p", "b").unwrap();
        let path = session.path().to_path_buf();
        session.close().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn parallel_keys_get_independent_dirs() {
        let workspace = tempfile::tempdir().unwrap();
        let a = ScratchSession::open(workspace.path(), "lm-a", "prompt-a", "banner-a").unwrap();
        let b = ScratchSession::open(workspace.path(), "lm-b", "prompt-b", "banner-b").unwrap();
        assert_ne!(a.path(), b.path());
        assert!(a.path().exists());
        assert!(b.path().exists());
        assert_eq!(
            fs::read_to_string(a.path().join("prompt.txt")).unwrap(),
            "prompt-a",
        );
        assert_eq!(
            fs::read_to_string(b.path().join("prompt.txt")).unwrap(),
            "prompt-b",
        );
    }

    #[test]
    fn invalid_keys_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        for bad in ["", ".", "a/b", "..", "../escape"] {
            let err = ScratchSession::open(workspace.path(), bad, "p", "b").expect_err(bad);
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn repin_script_runs_jq_envelope_against_files() {
        let Some(context) = repin_context(
            "the initial prompt\nwith\nlines",
            "## decisions\n- pick option A\n",
        ) else {
            return;
        };
        assert!(
            context.contains("loom loop @ lm-3"),
            "missing banner in {context}",
        );
        assert!(
            context.contains("the initial prompt"),
            "missing prompt in {context}",
        );
        assert!(
            context.contains("pick option A"),
            "missing scratch.md content in {context}",
        );
    }

    #[test]
    fn repin_script_preserves_full_prompt_verbatim() {
        let scratch = "## Scratchpad\n- decisions recorded after compaction started\n";
        let Some(context) = repin_context(PLANNING_INTERVIEW_PROMPT, scratch) else {
            return;
        };
        let expected = format!("loom loop @ lm-3\n\n{PLANNING_INTERVIEW_PROMPT}\n\n{scratch}");
        assert_eq!(context, expected);
        assert!(
            context.find(PLANNING_INTERVIEW_PROMPT).unwrap() < context.find(scratch).unwrap(),
            "prompt.txt bytes must precede scratch.md bytes",
        );
    }

    #[test]
    fn compacted_resume_preserves_polish_mode_definition() {
        let Some(context) = repin_context(
            PLANNING_INTERVIEW_PROMPT,
            "## Scratchpad\n- compacted summary omitted the polish mode\n",
        ) else {
            return;
        };
        assert!(
            context.contains(POLISH_MODE_DEFINITION),
            "polish mode definition missing from compacted context: {context}",
        );
    }

    #[test]
    fn post_compaction_polish_canary_requires_full_mode_definition() {
        let summary_only = "loom loop @ lm-3\n\nSummary: polish means report-only.";
        assert!(
            !summary_only.contains(POLISH_MODE_DEFINITION),
            "the canary must reject vague summaries as substitutes",
        );

        let Some(context) = repin_context(
            PLANNING_INTERVIEW_PROMPT,
            "## Scratchpad\n- user asks: do a polish\n",
        ) else {
            return;
        };
        assert!(
            context.contains(POLISH_MODE_DEFINITION),
            "full polish mode definition missing from post-compaction context: {context}",
        );
    }

    #[test]
    fn compacted_resume_preserves_one_by_one_mode_definition() {
        let Some(context) = repin_context(
            PLANNING_INTERVIEW_PROMPT,
            "## Scratchpad\n- compacted summary omitted one-by-one sequencing\n",
        ) else {
            return;
        };
        assert!(
            context.contains(ONE_BY_ONE_MODE_DEFINITION),
            "one-by-one mode definition missing from compacted context: {context}",
        );
    }

    #[test]
    fn claude_settings_registers_repin_under_session_start_compact() {
        let workspace = tempfile::tempdir().unwrap();
        let session = ScratchSession::open(workspace.path(), "lm-4", "p", "b").unwrap();
        let body = fs::read_to_string(session.claude_settings()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let hook = &parsed["hooks"]["SessionStart"][0];
        assert_eq!(hook["matcher"], "compact");
        assert_eq!(hook["hooks"][0]["type"], "command");
        let cmd = hook["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.ends_with("repin.sh"));
    }

    #[test]
    fn bash_single_quote_escapes_apostrophes() {
        assert_eq!(bash_single_quote(""), "''");
        assert_eq!(bash_single_quote("plain"), "'plain'");
        assert_eq!(bash_single_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn resolve_scratch_key_uses_plan_anchors_work_epic_or_bead() {
        let harness = SpecLabel::new("harness").unwrap();
        let templates = SpecLabel::new("templates").unwrap();
        let anchors = vec![harness.clone(), templates];
        let bead = BeadId::new("lm-3hhwq.15").unwrap();
        assert_eq!(resolve_scratch_key(Phase::Plan, &[], Some(&bead)), "plan");
        assert_eq!(
            resolve_scratch_key(Phase::Plan, &anchors, Some(&bead)),
            "harness+templates"
        );
        assert_eq!(
            resolve_scratch_key(Phase::Todo, std::slice::from_ref(&harness), Some(&bead)),
            "lm-3hhwq.15",
        );
        assert_eq!(
            resolve_scratch_key(Phase::Loop, std::slice::from_ref(&harness), Some(&bead)),
            "lm-3hhwq.15"
        );
        assert_eq!(
            resolve_scratch_key(Phase::Review, std::slice::from_ref(&harness), Some(&bead)),
            "lm-3hhwq.15"
        );
        assert_eq!(
            resolve_scratch_key(Phase::Inbox, std::slice::from_ref(&harness), Some(&bead)),
            "lm-3hhwq.15"
        );
    }

    #[test]
    fn resolve_scratch_key_falls_back_to_label_when_bead_missing() {
        let label = SpecLabel::new("harness").unwrap();
        assert_eq!(
            resolve_scratch_key(Phase::Review, std::slice::from_ref(&label), None),
            "harness",
        );
        assert_eq!(
            resolve_scratch_key(Phase::Loop, std::slice::from_ref(&label), None),
            "harness",
        );
    }
}
