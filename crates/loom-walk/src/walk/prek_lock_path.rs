//! pre-commit FR2 / Lock implementation: `lib/prek/lock.sh` resolves
//! the lock path on the host filesystem under `$XDG_STATE_HOME` (or its
//! `$HOME/.local/state` default), never inside the workspace. A
//! workspace-rooted lock could be deleted or forged by a bead container
//! with the workspace bind-mounted, defeating the serialization guard.
//!
//! The walk parses `lib/prek/lock.sh`, isolates assignments to
//! lock-named variables, and rejects values that reference `.wrapix`,
//! a `./` repo-relative path, or any prefix outside the
//! `$XDG_STATE_HOME` / `$HOME` envelope (with chained references to
//! previously validated lock variables permitted, since
//! `lock_file="${lock_dir}/prek.lock"` inherits `lock_dir`'s rooting).

use std::path::{Path, PathBuf};

use super::util::{narrow_to_loom_files, read_to_string, rel, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "prek_lock_path_outside_workspace — lib/prek/lock.sh must root the lock under $XDG_STATE_HOME / $HOME, never inside the workspace";

const LOCK_SH_REL: &str = "lib/prek/lock.sh";

const ALLOWED_PREFIXES: &[&str] = &["${XDG_STATE_HOME", "$XDG_STATE_HOME", "${HOME", "$HOME"];

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let lock_path = root.join(LOCK_SH_REL);
    let scope: Vec<PathBuf> = narrow_to_loom_files(vec![lock_path.clone()], input, &root);
    if scope.is_empty() {
        return Verdict {
            pass: true,
            evidence: format!("{LOCK_SH_REL} outside LOOM_FILES scope"),
        };
    }
    scan(&root, &lock_path)
}

fn scan(root: &Path, path: &Path) -> Verdict {
    let Some(body) = read_to_string(path) else {
        return Verdict {
            pass: false,
            evidence: format!("{LOCK_SH_REL} not readable\n{RULE}"),
        };
    };
    let rel_path = rel(root, path);
    let mut violations: Vec<String> = Vec::new();
    let mut blessed: Vec<String> = Vec::new();

    for (idx, raw) in body.lines().enumerate() {
        let lineno = idx + 1;
        let Some(assignment) = parse_assignment(raw) else {
            continue;
        };
        if !is_lock_var(assignment.name) {
            continue;
        }
        match reject_reason(assignment.value, &blessed) {
            Some(reason) => violations.push(format!(
                "{rel_path}:{lineno} `{name}={value}` — {reason}",
                name = assignment.name,
                value = assignment.value,
            )),
            None => blessed.push(assignment.name.to_string()),
        }
    }
    verdict_from(RULE, violations)
}

struct Assignment<'a> {
    name: &'a str,
    value: &'a str,
}

fn parse_assignment(line: &str) -> Option<Assignment<'_>> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let body = trimmed
        .strip_prefix("local ")
        .or_else(|| trimmed.strip_prefix("export "))
        .or_else(|| trimmed.strip_prefix("declare "))
        .or_else(|| trimmed.strip_prefix("readonly "))
        .unwrap_or(trimmed);
    let eq = body.find('=')?;
    let name = body[..eq].trim();
    let value_raw = body[eq + 1..].trim();
    if !is_identifier(name) || value_raw.is_empty() {
        return None;
    }
    let value = strip_inline_comment(strip_quotes(value_raw));
    Some(Assignment { name, value })
}

fn strip_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn strip_inline_comment(s: &str) -> &str {
    match s.find(" #") {
        Some(i) => s[..i].trim_end(),
        None => s,
    }
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_lock_var(name: &str) -> bool {
    name.to_ascii_lowercase().contains("lock")
}

fn reject_reason(value: &str, blessed: &[String]) -> Option<String> {
    if value.contains(".wrapix") {
        return Some("references in-workspace `.wrapix/` path".to_string());
    }
    if value.contains("./") {
        return Some("references repo-relative `./` path".to_string());
    }
    if ALLOWED_PREFIXES.iter().any(|p| value.starts_with(p)) {
        return None;
    }
    for var in blessed {
        let braced = format!("${{{var}");
        let bare = format!("${var}");
        if value.starts_with(&braced) || value.starts_with(&bare) {
            return None;
        }
    }
    Some(
        "does not begin with $XDG_STATE_HOME / $HOME or a previously validated lock variable"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_assignment() {
        let a = parse_assignment("lock_dir=\"$HOME/x\"").unwrap();
        assert_eq!(a.name, "lock_dir");
        assert_eq!(a.value, "$HOME/x");
    }

    #[test]
    fn skips_non_assignments() {
        assert!(parse_assignment("if [[ $x == y ]]; then").is_none());
        assert!(parse_assignment("_prek_acquire_lock() {").is_none());
        assert!(parse_assignment("    # comment").is_none());
        assert!(parse_assignment("").is_none());
    }

    #[test]
    fn strips_local_prefix() {
        let a = parse_assignment("    local lock_dir=\"$HOME/x\"").unwrap();
        assert_eq!(a.name, "lock_dir");
        assert_eq!(a.value, "$HOME/x");
    }

    #[test]
    fn rejects_wrapix_path() {
        let reason = reject_reason(".wrapix/locks/prek.lock", &[]).unwrap();
        assert!(reason.contains(".wrapix"), "got `{reason}`");
    }

    #[test]
    fn rejects_relative_path() {
        let reason = reject_reason("./locks/prek.lock", &[]).unwrap();
        assert!(reason.contains("./"), "got `{reason}`");
    }

    #[test]
    fn accepts_xdg_state_home_prefix() {
        assert!(
            reject_reason("${XDG_STATE_HOME:-$HOME/.local/state}/loom/prek/foo", &[]).is_none()
        );
        assert!(reject_reason("$XDG_STATE_HOME/loom/prek/foo", &[]).is_none());
    }

    #[test]
    fn accepts_home_prefix() {
        assert!(reject_reason("$HOME/.local/state/loom/prek/foo", &[]).is_none());
        assert!(reject_reason("${HOME}/foo", &[]).is_none());
    }

    #[test]
    fn accepts_blessed_chain() {
        let blessed = vec!["lock_dir".to_string()];
        assert!(reject_reason("${lock_dir}/prek.lock", &blessed).is_none());
        assert!(reject_reason("$lock_dir/prek.lock", &blessed).is_none());
    }

    #[test]
    fn rejects_unrooted_path() {
        let reason = reject_reason("/var/lib/prek/lock", &[]).unwrap();
        assert!(reason.contains("$XDG_STATE_HOME"), "got `{reason}`");
    }

    #[test]
    fn is_lock_var_matches_case_insensitive() {
        assert!(is_lock_var("lock_file"));
        assert!(is_lock_var("LOCK_DIR"));
        assert!(is_lock_var("PREK_LOCK_PATH"));
        assert!(!is_lock_var("workspace_basename"));
        assert!(!is_lock_var("deadline"));
    }
}
