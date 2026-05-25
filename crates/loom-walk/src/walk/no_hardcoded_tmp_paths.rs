//! NFR #7: tests use `tempfile::tempdir`, never hardcoded `/tmp/...`
//! paths. Nix's Darwin build sandbox doesn't grant access to the
//! host's `/tmp`, so any test that hardcodes one fails to even start
//! under `nix flake check`.

use std::path::Path;

use syn::visit::Visit;
use syn::{Attribute, Meta};

use super::util::{
    all_rs_files, narrow_to_loom_files, parse_rs, rel, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

const RULE: &str = "no_hardcoded_tmp_paths — tests use tempfile::tempdir, not `/tmp/...`";

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let scope = narrow_to_loom_files(all_rs_files(&root), input, &root);
    let mut violations = Vec::new();
    for path in scope {
        let Some(file) = parse_rs(&path) else {
            continue;
        };
        let rel_path = rel(&root, &path);
        let mut visitor = Visitor {
            violations: &mut violations,
            rel_path,
            inside_test_module: is_under_tests_dir(&path),
        };
        visitor.visit_file(&file);
    }
    verdict_from(RULE, violations)
}

fn is_under_tests_dir(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "tests")
}

struct Visitor<'a> {
    violations: &'a mut Vec<String>,
    rel_path: String,
    inside_test_module: bool,
}

impl<'ast> Visit<'ast> for Visitor<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let was_in_test = self.inside_test_module;
        if has_cfg_test(&node.attrs) {
            self.inside_test_module = true;
        }
        syn::visit::visit_item_mod(self, node);
        self.inside_test_module = was_in_test;
    }

    fn visit_lit_str(&mut self, node: &'ast syn::LitStr) {
        if !self.inside_test_module {
            return;
        }
        let value = node.value();
        if !is_tmp_path_literal(&value) {
            return;
        }
        let line = node.span().start().line;
        self.violations.push(format!(
            "{}:{} hardcoded `/tmp/` path literal `{}` — use `tempfile::tempdir()` instead",
            self.rel_path, line, value
        ));
    }
}

fn is_tmp_path_literal(value: &str) -> bool {
    if !value.starts_with("/tmp/") && value != "/tmp" {
        return false;
    }
    !value.contains('{') && !value.contains('\n')
}

fn has_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        let Meta::List(list) = &a.meta else {
            return false;
        };
        if !list.path.is_ident("cfg") {
            return false;
        }
        list.tokens.to_string().contains("test")
    })
}
