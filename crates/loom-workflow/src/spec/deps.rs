//! Scan annotated test files for tool references and emit nixpkgs names.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::annotations::{Annotation, AnnotationKind};
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

/// Walk `annotations` for `verify`/`judge` rows and return the set of nixpkgs
/// names referenced by each linked file. Files that do not exist on disk are
/// silently skipped so missing tests don't poison the sweep.
pub fn collect_deps(
    workspace: &Path,
    annotations: &[Annotation],
) -> Result<BTreeSet<String>, SpecError> {
    let mut files: BTreeSet<PathBuf> = BTreeSet::new();
    for ann in annotations {
        if !matches!(ann.kind, AnnotationKind::Verify | AnnotationKind::Judge) {
            continue;
        }
        if let Some(file) = &ann.file {
            files.insert(file.clone());
        }
    }
    let mut packages = BTreeSet::new();
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
    use std::path::PathBuf;

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
        // "curling" must not match "curl".
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
        let anns = vec![Annotation {
            criterion: "x".into(),
            kind: AnnotationKind::Verify,
            file: Some(PathBuf::from("tests/missing.sh")),
            function: None,
        }];
        let pkgs = collect_deps(dir.path(), &anns)?;
        assert!(pkgs.is_empty());
        Ok(())
    }

    #[test]
    fn collect_deps_skips_non_test_annotations() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let tests = dir.path().join("tests");
        fs::create_dir_all(&tests)?;
        fs::write(tests.join("a.sh"), "curl x\n")?;
        // Verify annotation should be picked up; None kind ignored.
        let anns = vec![
            Annotation {
                criterion: "verify".into(),
                kind: AnnotationKind::Verify,
                file: Some(PathBuf::from("tests/a.sh")),
                function: None,
            },
            Annotation {
                criterion: "ignored".into(),
                kind: AnnotationKind::None,
                file: None,
                function: None,
            },
        ];
        let pkgs = collect_deps(dir.path(), &anns)?;
        assert!(pkgs.contains("curl"));
        Ok(())
    }
}
