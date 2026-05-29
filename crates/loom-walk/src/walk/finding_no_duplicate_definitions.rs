//! Anti-duplication walker for the canonical `Finding` /
//! `ConcernToken` contract.
//!
//! `loom-templates::finding` is THE Rust home for the typed wire
//! shape — the `Finding` struct and the closed `ConcernToken` enum are
//! declared exactly once, in `crates/loom-templates/src/finding.rs`.
//! Other crates that need the types re-export via `pub use`; the walker
//! flags any other declaration so a parallel definition cannot drift
//! the wire format from the canonical one. Re-exports and trait /
//! struct fields whose type IS `Finding` are unaffected — the walk
//! inspects only top-level `struct Finding` / `enum ConcernToken`
//! declarations.

use syn::visit::Visit;
use syn::{ItemEnum, ItemStruct};

use super::util::{
    all_rs_files, line_of, narrow_to_loom_files, parse_rs, rel, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "finding_no_duplicate_definitions — \
                    `Finding` / `ConcernToken` are declared only in \
                    `crates/loom-templates/src/finding.rs`";

const CANONICAL: &str = "crates/loom-templates/src/finding.rs";

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let scope = narrow_to_loom_files(all_rs_files(&root), input, &root);
    let mut violations = Vec::new();
    for path in scope {
        let rel_path = rel(&root, &path);
        if rel_path == CANONICAL {
            continue;
        }
        let Some(file) = parse_rs(&path) else {
            continue;
        };
        let mut visitor = Visitor {
            violations: &mut violations,
            rel_path,
        };
        visitor.visit_file(&file);
    }
    verdict_from(RULE, violations)
}

struct Visitor<'a> {
    violations: &'a mut Vec<String>,
    rel_path: String,
}

impl<'ast> Visit<'ast> for Visitor<'_> {
    fn visit_item_struct(&mut self, node: &'ast ItemStruct) {
        if node.ident == "Finding" {
            self.violations.push(format!(
                "{}:{} struct `Finding` — canonical declaration lives in `{}`",
                self.rel_path,
                line_of(node),
                CANONICAL,
            ));
        }
    }

    fn visit_item_enum(&mut self, node: &'ast ItemEnum) {
        if node.ident == "ConcernToken" {
            self.violations.push(format!(
                "{}:{} enum `ConcernToken` — canonical declaration lives in `{}`",
                self.rel_path,
                line_of(node),
                CANONICAL,
            ));
        }
    }
}
