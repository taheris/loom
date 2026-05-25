//! RS-8: tuple-struct newtypes must not carry `derive(From)` or
//! `derive(Into)`. Either bypasses the newtype's `new()` constructor
//! and the validation it enforces at the boundary.

use syn::visit::Visit;
use syn::{Fields, ItemStruct};

use super::util::{
    derive_idents, line_of, narrow_to_loom_files, parse_rs, rel, src_files, verdict_from,
    workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "RS-8 no derive(From)/derive(Into) on tuple-struct newtypes";

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let scope = narrow_to_loom_files(src_files(&root), input, &root);
    let mut violations = Vec::new();
    for path in scope {
        let Some(file) = parse_rs(&path) else {
            continue;
        };
        let rel_path = rel(&root, &path);
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
        if !matches!(node.fields, Fields::Unnamed(_)) {
            return;
        }
        for attr in &node.attrs {
            if !attr.path().is_ident("derive") {
                continue;
            }
            let derived = derive_idents(attr);
            for forbidden in ["From", "Into"] {
                if derived.iter().any(|i| i == forbidden) {
                    let line = line_of(attr);
                    self.violations.push(format!(
                        "{}:{} derive({forbidden}) on tuple struct `{}`",
                        self.rel_path, line, node.ident,
                    ));
                }
            }
        }
    }
}
