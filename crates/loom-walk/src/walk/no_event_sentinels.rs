//! RS-12/13: event identities cannot be created through sentinel constructors.
use syn::visit::Visit;
use syn::{ExprCall, ExprPath, ItemImpl, Type};

use super::util::{
    line_of, narrow_to_loom_files, parse_rs, rel, src_files, verdict_from, workspace_root,
};
use super::{Verdict, WalkInput};

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let mut violations = Vec::new();
    for path in narrow_to_loom_files(src_files(&root), input, &root) {
        let Some(file) = parse_rs(&path) else {
            violations.push(format!(
                "{}: unable to read or parse Rust source",
                rel(&root, &path)
            ));
            continue;
        };
        Visitor {
            path: rel(&root, &path),
            violations: &mut violations,
        }
        .visit_file(&file);
    }
    verdict_from("RS-12/13 no event sentinel constructors", violations)
}

fn forbidden(owner: &str, method: &str) -> bool {
    (method == "placeholder" && matches!(owner, "EventEnvelope" | "BeadId" | "AgentEvent"))
        || (owner == "EventEnvelope" && method == "default")
}

struct Visitor<'a> {
    path: String,
    violations: &'a mut Vec<String>,
}

impl Visitor<'_> {
    fn report(&mut self, owner: &str, method: &str, line: usize) {
        self.violations.push(format!(
            "{}:{line} {owner}::{method} is a sentinel constructor",
            self.path
        ));
    }
}

impl<'ast> Visit<'ast> for Visitor<'_> {
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let syn::Expr::Path(ExprPath { path, .. }) = &*node.func {
            let mut parts = path.segments.iter().rev();
            if let (Some(method), Some(owner)) = (parts.next(), parts.next())
                && forbidden(&owner.ident.to_string(), &method.ident.to_string())
            {
                self.report(
                    &owner.ident.to_string(),
                    &method.ident.to_string(),
                    line_of(node),
                );
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        if let Type::Path(owner) = &*node.self_ty
            && let Some(owner) = owner.path.segments.last()
        {
            for item in &node.items {
                if let syn::ImplItem::Fn(method) = item
                    && forbidden(&owner.ident.to_string(), &method.sig.ident.to_string())
                {
                    self.report(
                        &owner.ident.to_string(),
                        &method.sig.ident.to_string(),
                        line_of(method),
                    );
                }
            }
        }
        syn::visit::visit_item_impl(self, node);
    }
}
