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
//! `<key>` is the spec label for `loom plan` / `loom todo` and the bead
//! id for `loom loop` / `loom gate`. Two parallel `loom loop` workers on
//! different beads of the same molecule get independent dirs.
//!
//! Cleanup runs on every exit path: [`Drop`] removes the directory
//! unconditionally, so a panic in the workflow engine still leaves no
//! carry-over for the next session. [`ScratchSession::close`] is the
//! explicit teardown and is idempotent w.r.t. the [`Drop`] cleanup.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::config::Phase;
use crate::identifier::{BeadId, SpecLabel};

const SCRATCH_SUBDIR: &str = ".loom/scratch";

/// Resolve the per-session scratch-dir key for `phase`. Single source of
/// truth for the spec-label-vs-bead-id choice documented in
/// `specs/harness.md`: spec label for `loom plan` / `loom todo`,
/// bead id for `loom loop` / `loom gate` / `loom msg`. `loom gate verify`
/// currently runs once per molecule and has no bead at the call site —
/// when `bead_id` is `None` for a bead-scoped phase the helper falls
/// back to the spec label so the on-disk path stays deterministic.
pub fn resolve_scratch_key(phase: Phase, label: &SpecLabel, bead_id: Option<&BeadId>) -> String {
    match phase {
        Phase::Plan | Phase::Todo => label.as_str().to_string(),
        Phase::Run | Phase::Check | Phase::Review | Phase::Msg => {
            bead_id.map_or_else(|| label.as_str().to_string(), |b| b.as_str().to_string())
        }
    }
}

/// Owns a `.loom/scratch/<key>/` directory for the duration of an
/// agent session. Drops the directory when the guard goes out of scope.
#[derive(Debug)]
pub struct ScratchSession {
    path: PathBuf,
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
    pub fn open(workspace: &Path, key: &str, prompt: &str, banner: &str) -> io::Result<Self> {
        if key.is_empty() || key.contains('/') || key.contains("..") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid scratch key: {key:?}"),
            ));
        }
        let path = workspace.join(SCRATCH_SUBDIR).join(key);
        if path.exists() {
            fs::remove_dir_all(&path)?;
        }
        fs::create_dir_all(&path)?;
        fs::write(path.join("prompt.txt"), prompt)?;
        fs::write(path.join("scratch.md"), "")?;
        write_repin_script(&path, banner)?;
        write_claude_settings(&path)?;
        Ok(Self { path })
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
    pub fn close(self) -> io::Result<()> {
        let path = self.path.clone();
        // Suppress the Drop-time cleanup so we own the error path.
        std::mem::forget(self);
        match fs::remove_dir_all(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

impl Drop for ScratchSession {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.path)
            && e.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.path.display(),
                error = %e,
                "scratch dir cleanup failed",
            );
        }
    }
}

fn write_repin_script(dir: &Path, banner: &str) -> io::Result<()> {
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
    let path = dir.join("repin.sh");
    fs::write(&path, script)?;
    set_executable(&path)
}

fn write_claude_settings(dir: &Path) -> io::Result<()> {
    let script_path = dir.join("repin.sh");
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
    fs::write(dir.join("claude-settings.json"), body)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
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
        for bad in ["", "a/b", "..", "../escape"] {
            let err = ScratchSession::open(workspace.path(), bad, "p", "b").expect_err(bad);
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn repin_script_runs_jq_envelope_against_files() {
        // Skip when the test environment does not have jq — repin.sh
        // depends on it for JSON escaping.
        if !std::process::Command::new("jq")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            eprintln!("jq missing; skipping envelope smoke test");
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let session = ScratchSession::open(
            workspace.path(),
            "lm-3",
            "the initial prompt\nwith\nlines",
            "loom loop @ lm-3",
        )
        .unwrap();
        // Simulate the agent appending to the scratchpad mid-session.
        fs::write(
            session.path().join("scratch.md"),
            "## decisions\n- pick option A\n",
        )
        .unwrap();

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
        let context = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
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
        assert_eq!(
            parsed["hookSpecificOutput"]["hookEventName"],
            "SessionStart",
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
    fn resolve_scratch_key_picks_label_for_spec_scoped_phases() {
        let label = SpecLabel::new("harness");
        let bead = BeadId::new("lm-3hhwq.15").unwrap();
        assert_eq!(
            resolve_scratch_key(Phase::Plan, &label, Some(&bead)),
            "harness",
        );
        assert_eq!(
            resolve_scratch_key(Phase::Todo, &label, Some(&bead)),
            "harness",
        );
        assert_eq!(resolve_scratch_key(Phase::Plan, &label, None), "harness",);
    }

    #[test]
    fn resolve_scratch_key_picks_bead_id_for_bead_scoped_phases() {
        let label = SpecLabel::new("harness");
        let bead = BeadId::new("lm-3hhwq.15").unwrap();
        assert_eq!(
            resolve_scratch_key(Phase::Run, &label, Some(&bead)),
            "lm-3hhwq.15",
        );
        assert_eq!(
            resolve_scratch_key(Phase::Check, &label, Some(&bead)),
            "lm-3hhwq.15",
        );
        assert_eq!(
            resolve_scratch_key(Phase::Msg, &label, Some(&bead)),
            "lm-3hhwq.15",
        );
    }

    #[test]
    fn resolve_scratch_key_falls_back_to_label_when_bead_missing() {
        let label = SpecLabel::new("harness");
        assert_eq!(resolve_scratch_key(Phase::Check, &label, None), "harness",);
        assert_eq!(resolve_scratch_key(Phase::Run, &label, None), "harness",);
    }
}
