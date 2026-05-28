//! Scan annotated targets for tool references and emit nixpkgs names.
//!
//! Two flavours of `loom-gate` annotation feed the sweep:
//!
//! - `[test]` / `[judge]` carry a file-shaped target (`tests/foo.sh#fn`,
//!   `tests/judges/x.sh#fn`, `rubrics/y.md`). The fragment is stripped, the
//!   file body is read from disk, and the body is scanned for tool tokens.
//! - `[check]` / `[system]` carry a shell command string as the target
//!   (`cargo run -p w -- a`, `nix run .#x`). The command string is scanned
//!   directly; no disk read.
//!
//! Targets that look like a Rust path (`crate::module::fn`) are not
//! file-shaped and are silently skipped — the language-native runner
//! resolves them later in dispatch.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use loom_gate::annotation::{Annotation, Tier};

use super::error::SpecError;

const TOOLS: &[(&str, &str)] = &[
    ("curl", "curl"),
    ("jq", "jq"),
    ("tmux", "tmux"),
    ("python", "python3"),
    ("python3", "python3"),
    ("node", "nodejs"),
    ("nodejs", "nodejs"),
    ("git", "git"),
    ("rsync", "rsync"),
    ("wget", "wget"),
    ("ssh", "openssh"),
    ("scp", "openssh"),
    ("socat", "socat"),
    ("nc", "netcat"),
    ("ncat", "netcat"),
    ("netcat", "netcat"),
    ("dig", "dnsutils"),
    ("nslookup", "dnsutils"),
    ("sqlite3", "sqlite"),
    ("psql", "postgresql"),
    ("docker", "docker"),
    ("podman", "podman"),
    ("nix", "nix"),
    ("shellcheck", "shellcheck"),
    ("shfmt", "shfmt"),
    ("rg", "ripgrep"),
    ("ripgrep", "ripgrep"),
    ("fd", "fd"),
    ("fzf", "fzf"),
    ("bat", "bat"),
    ("diff", "diffutils"),
    ("patch", "patch"),
    ("make", "gnumake"),
    ("gcc", "gcc"),
    ("cc", "gcc"),
    ("go", "go"),
    ("cargo", "rustc"),
    ("rustc", "rustc"),
];

/// Walk `annotations` and return the set of nixpkgs names referenced by
/// each tier's target. File-shaped `[test]`/`[judge]` targets are read
/// from disk; `[check]`/`[system]` command strings are scanned in-place.
/// Files that do not exist on disk are silently skipped so missing tests
/// don't poison the sweep.
pub fn collect_deps(
    workspace: &Path,
    annotations: &[Annotation],
) -> Result<BTreeSet<String>, SpecError> {
    let mut files: BTreeSet<PathBuf> = BTreeSet::new();
    let mut packages = BTreeSet::new();
    for ann in annotations {
        match ann.tier {
            Tier::Check | Tier::System => {
                for pkg in scan_file_body(&ann.target) {
                    packages.insert(pkg);
                }
            }
            Tier::Test | Tier::Judge => {
                if let Some(file) = target_file_path(&ann.target) {
                    files.insert(file);
                }
            }
        }
    }
    for rel in files {
        let abs = if rel.is_absolute() {
            rel.clone()
        } else {
            workspace.join(&rel)
        };
        let body = match fs::read_to_string(&abs) {
            Ok(b) => b,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(SpecError::Io { path: abs, source }),
        };
        for pkg in scan_file_body(&body) {
            packages.insert(pkg);
        }
    }
    Ok(packages)
}

/// If `target` is file-shaped (contains a `/` or has a file extension on
/// the part before any `#`/`::` fragment), return the bare path. Returns
/// `None` for Rust-style paths (`crate::a::b`) and other non-file shapes.
pub fn target_file_path(target: &str) -> Option<PathBuf> {
    let head = target.split_once('#').map(|(h, _)| h).unwrap_or(target);
    let head = match head.split_once("::") {
        Some((h, _)) if path_shaped(h) => h,
        Some(_) => return None,
        None => head,
    };
    if !path_shaped(head) {
        return None;
    }
    Some(PathBuf::from(head))
}

fn path_shaped(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.contains(' ') {
        return false;
    }
    if s.contains('/') {
        return true;
    }
    if let Some(dot) = s.rfind('.') {
        let ext = &s[dot + 1..];
        return !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric());
    }
    false
}

/// Return the set of package names referenced by `body`. Public so the
/// shell-level tests can dispatch directly to the dep matcher without writing
/// to disk first.
pub fn scan_file_body(body: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (tool, pkg) in TOOLS {
        if has_command_use(body, tool) {
            out.insert((*pkg).to_string());
        }
    }
    out
}

fn has_command_use(body: &str, tool: &str) -> bool {
    let bytes = body.as_bytes();
    let needle = tool.as_bytes();
    let mut i = 0;
    while let Some(off) = find_subslice(&bytes[i..], needle) {
        let start = i + off;
        let end = start + needle.len();
        let prev = if start == 0 { b'\n' } else { bytes[start - 1] };
        let next = if end == bytes.len() {
            b'\n'
        } else {
            bytes[end]
        };
        if is_command_boundary_before(prev) && is_command_boundary_after(next) {
            return true;
        }
        i = start + 1;
    }
    false
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

fn is_command_boundary_before(b: u8) -> bool {
    matches!(b, b'\n' | b' ' | b'\t' | b'|' | b';' | b'&' | b'(')
}

fn is_command_boundary_after(b: u8) -> bool {
    matches!(b, b'\n' | b' ' | b'\t' | b'|' | b';' | b'&' | b')')
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use loom_gate::annotation::Tier;
    use std::path::PathBuf;

    fn ann(tier: Tier, target: &str) -> Annotation {
        Annotation {
            tier,
            target: target.into(),
            source_spec: PathBuf::from("specs/x.md"),
            line: 1,
            criterion_line: 1,
            pending: false,
        }
    }

    #[test]
    fn maps_known_tools_to_nix_packages() {
        let body = "curl https://example.com\njq .field\n";
        let pkgs = scan_file_body(body);
        assert!(pkgs.contains("curl"));
        assert!(pkgs.contains("jq"));
    }

    #[test]
    fn aliases_collapse_to_canonical_package() {
        let body = "rg pattern\nripgrep pattern\n";
        let pkgs = scan_file_body(body);
        assert!(pkgs.contains("ripgrep"));
        assert_eq!(pkgs.len(), 1, "rg + ripgrep should both map to ripgrep");
    }

    #[test]
    fn ignores_substring_matches() {
        let body = "echo curling\n";
        assert!(scan_file_body(body).is_empty());
    }

    #[test]
    fn matches_after_pipes_and_command_subst() {
        let body = "echo x | jq .\nresult=$(curl -s url)\n";
        let pkgs = scan_file_body(body);
        assert!(pkgs.contains("jq"));
        assert!(pkgs.contains("curl"));
    }

    #[test]
    fn ssh_and_scp_both_map_to_openssh() {
        let body = "ssh host date\nscp foo bar\n";
        let pkgs = scan_file_body(body);
        assert_eq!(pkgs.len(), 1);
        assert!(pkgs.contains("openssh"));
    }

    #[test]
    fn collect_deps_ignores_missing_files() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let anns = vec![ann(Tier::Test, "tests/missing.sh#x")];
        let pkgs = collect_deps(dir.path(), &anns)?;
        assert!(pkgs.is_empty());
        Ok(())
    }

    #[test]
    fn collect_deps_reads_test_and_judge_file_bodies() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let tests = dir.path().join("tests");
        fs::create_dir_all(&tests)?;
        fs::write(tests.join("a.sh"), "curl x\n")?;
        fs::write(tests.join("b.sh"), "jq .\n")?;
        let anns = vec![
            ann(Tier::Test, "tests/a.sh#test_a"),
            ann(Tier::Judge, "tests/b.sh#test_b"),
        ];
        let pkgs = collect_deps(dir.path(), &anns)?;
        assert!(pkgs.contains("curl"));
        assert!(pkgs.contains("jq"));
        Ok(())
    }

    #[test]
    fn collect_deps_scans_check_and_system_command_strings_directly() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let anns = vec![
            ann(Tier::Check, "rg pattern files"),
            ann(Tier::System, "nix run .#smoke"),
        ];
        let pkgs = collect_deps(dir.path(), &anns)?;
        assert!(pkgs.contains("ripgrep"));
        assert!(pkgs.contains("nix"));
        Ok(())
    }

    #[test]
    fn collect_deps_skips_language_native_test_targets() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let anns = vec![ann(Tier::Test, "crate::module::test_fn")];
        let pkgs = collect_deps(dir.path(), &anns)?;
        assert!(pkgs.is_empty());
        Ok(())
    }

    #[test]
    fn target_file_path_recognises_slashes_and_extensions() {
        assert_eq!(
            target_file_path("tests/foo.sh#test_x"),
            Some(PathBuf::from("tests/foo.sh")),
        );
        assert_eq!(
            target_file_path("rubrics/api.md"),
            Some(PathBuf::from("rubrics/api.md")),
        );
        assert_eq!(target_file_path("crate::a::b"), None);
        assert_eq!(target_file_path("bare_word"), None);
    }
}
