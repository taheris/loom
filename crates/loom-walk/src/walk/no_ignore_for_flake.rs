use std::path::Path;

use syn::visit::Visit;

use super::util::{
    all_rs_files, line_of, narrow_to_loom_files, parse_rs, rel, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "TST-3 #[ignore] is limited to enumerated process entry points";
const ALLOWLIST: &[(&str, &str)] = &[
    // The issues-native-conformance Nix check supplies pinned Beads to these isolated fixtures.
    (
        "crates/loom-issues/tests/native.rs",
        "tracking_conformance_covers_lightweight_and_pinned_beads",
    ),
    (
        "crates/loom-issues/tests/native.rs",
        "native_multi_id_status_predicates_do_not_make_updates_atomic",
    ),
    (
        "crates/loom-issues/tests/native.rs",
        "beads_tracking_adapter_retains_native_interoperability_coverage",
    ),
    // Replay acceptance tests re-execute this worker with isolated subprocess environment.
    ("crates/loom/tests/tune_cli.rs", "tune_replay_clock_worker"),
    (
        "crates/loom-driver/tests/lock_manager.rs",
        "crash_helper_take_lock_then_exit",
    ),
    // This entry point requires the real bind mount installed by the system verifier.
    (
        "crates/loom-agent/tests/workspace_mount.rs",
        "direct_tools_read_against_container_workspace_mount",
    ),
];

fn rule(input: &WalkInput) -> Verdict {
    run_with_root(input, &workspace_root(input))
}

fn run_with_root(input: &WalkInput, root: &Path) -> Verdict {
    let files = narrow_to_loom_files(all_rs_files(input, root), input, root);
    let mut violations = Vec::new();
    for path in files {
        let Some(parsed) = parse_rs(input, &path) else {
            continue;
        };
        let rel_path = rel(root, &path);
        let mut visitor = IgnoreVisitor {
            rel_path: &rel_path,
            violations: &mut violations,
        };
        visitor.visit_file(&parsed);
    }
    verdict_from(RULE, violations)
}

struct IgnoreVisitor<'a> {
    rel_path: &'a str,
    violations: &'a mut Vec<String>,
}

impl<'ast> Visit<'ast> for IgnoreVisitor<'_> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if node.attrs.iter().any(|attr| attr.path().is_ident("ignore")) {
            let name = node.sig.ident.to_string();
            let allowed = ALLOWLIST
                .iter()
                .any(|(path, function)| *path == self.rel_path && *function == name);
            if !allowed {
                self.violations.push(format!(
                    "{}:{} unaudited #[ignore] on {name}",
                    self.rel_path,
                    line_of(&node.sig.ident),
                ));
            }
        }
        syn::visit::visit_item_fn(self, node);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;

    use super::*;

    fn fixture(source: &str) -> Result<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        let tests = dir.path().join("crates/example/tests");
        fs::create_dir_all(&tests)?;
        fs::write(tests.join("coverage.rs"), source)?;
        Ok(dir)
    }

    #[test]
    fn accepts_tests_without_ignore() -> Result<()> {
        let dir = fixture("#[test]\nfn runs_normally() {}\n")?;
        assert!(run_with_root(&WalkInput::default(), dir.path()).pass);
        Ok(())
    }

    #[test]
    fn allows_only_exact_process_entrypoints() -> Result<()> {
        for (path, function) in ALLOWLIST {
            let dir = tempfile::tempdir()?;
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(&path, format!("#[test]\n#[ignore]\nfn {function}() {{}}\n"))?;
            assert!(run_with_root(&WalkInput::default(), dir.path()).pass);
            fs::write(&path, "#[test]\n#[ignore]\nfn hidden_flake() {}\n")?;
            assert!(!run_with_root(&WalkInput::default(), dir.path()).pass);
        }
        Ok(())
    }

    #[test]
    fn rejects_unenumerated_ignore() -> Result<()> {
        let dir = fixture("#[test]\n#[ignore]\nfn hidden_flake() {}\n")?;
        let verdict = run_with_root(&WalkInput::default(), dir.path());
        assert!(!verdict.pass);
        assert!(verdict.evidence.contains("hidden_flake"));
        Ok(())
    }
}

/// Standalone and checked dispatch execute the identical rule body.
pub fn run(input: &WalkInput) -> Verdict {
    input.finish(rule(input))
}

super::source::registration!("no_ignore_for_flake");
