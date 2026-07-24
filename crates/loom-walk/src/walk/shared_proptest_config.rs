use std::path::Path;

use syn::visit::Visit;

use super::util::{
    all_rs_files, narrow_to_loom_files, parse_rs, rel, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "TST-1 shared proptest blocks use loom_test_support::proptest_config";

pub fn run(input: &WalkInput) -> Verdict {
    run_with_root(input, &workspace_root())
}

fn run_with_root(input: &WalkInput, root: &Path) -> Verdict {
    let files = narrow_to_loom_files(all_rs_files(root), input, root);
    let mut violations = Vec::new();
    for path in files {
        let Some(parsed) = parse_rs(&path) else {
            continue;
        };
        let mut visitor = ProptestVisitor::default();
        visitor.visit_file(&parsed);
        if visitor.blocks == 0 {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        let configured = source
            .matches("#![proptest_config(proptest_config())]")
            .count();
        if configured != visitor.blocks {
            violations.push(format!(
                "{}:1 found {} proptest blocks but {} shared configurations",
                rel(root, &path),
                visitor.blocks,
                configured,
            ));
        }
        if source.contains("ProptestConfig::with_cases") {
            violations.push(format!(
                "{}:1 declares a local proptest case configuration",
                rel(root, &path),
            ));
        }
    }
    verdict_from(RULE, violations)
}

#[derive(Default)]
struct ProptestVisitor {
    blocks: usize,
}

impl<'ast> Visit<'ast> for ProptestVisitor {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if node.path.is_ident("proptest") {
            self.blocks += 1;
        }
        syn::visit::visit_macro(self, node);
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
        fs::write(tests.join("properties.rs"), source)?;
        Ok(dir)
    }

    #[test]
    fn accepts_shared_configuration_for_each_property_block() -> Result<()> {
        let dir = fixture(
            "use loom_test_support::proptest_config;\nproptest! {\n#![proptest_config(proptest_config())]\n#[test] fn p(x in 0..2) {}\n}\n",
        )?;
        assert!(run_with_root(&WalkInput::default(), dir.path()).pass);
        Ok(())
    }

    #[test]
    fn rejects_local_case_configuration() -> Result<()> {
        let dir = fixture(
            "proptest! {\n#![proptest_config(ProptestConfig::with_cases(32))]\n#[test] fn p(x in 0..2) {}\n}\n",
        )?;
        let verdict = run_with_root(&WalkInput::default(), dir.path());
        assert!(!verdict.pass);
        assert!(
            verdict
                .evidence
                .contains("local proptest case configuration")
        );
        Ok(())
    }
}
