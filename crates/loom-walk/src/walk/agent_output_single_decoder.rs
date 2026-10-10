//! Audit executable agent-marker parsing, not unrelated transport message types.

use std::path::{Path, PathBuf};

use syn::visit::Visit;
use syn::{Expr, ExprCall, ExprMethodCall, Item, ItemEnum, ItemFn, Lit, Type};

use super::util::{line_of, narrow_to_loom_files, rel, src_files, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "agent_output_single_decoder — agent messages delegate to loom-protocol::output";
const CANONICAL: &str = "crates/loom-protocol/src/output/";

pub(super) fn inputs(root: &Path) -> Vec<PathBuf> {
    src_files(root)
        .into_iter()
        .filter(|path| {
            let path = rel(root, path);
            [
                "loom-protocol",
                "loom-workflow",
                "loom-gate",
                "loom-tune",
                "loom",
            ]
            .iter()
            .any(|name| path.starts_with(&format!("crates/{name}/src/")))
                && !path.starts_with("crates/loom/src/bin/mock-")
        })
        .collect()
}

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let mut violations = Vec::new();
    for path in narrow_to_loom_files(inputs(&root), input, &root) {
        let location = rel(&root, &path);
        if location.starts_with(CANONICAL) {
            continue;
        }
        let file = match std::fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|source| syn::parse_file(&source).map_err(|error| error.to_string()))
        {
            Ok(file) => file,
            Err(error) => {
                violations.push(format!("{location}:1 source cannot be audited: {error}"));
                continue;
            }
        };
        Visitor {
            location: &location,
            violations: &mut violations,
            payloads: payload_bindings(&file),
        }
        .visit_file(&file);
    }
    verdict_from(RULE, violations)
}

struct Visitor<'a> {
    location: &'a str,
    violations: &'a mut Vec<String>,
    payloads: Vec<String>,
}

fn payload_bindings(file: &syn::File) -> Vec<String> {
    fn collect(tree: &syn::UseTree, names: &mut Vec<String>) {
        match tree {
            syn::UseTree::Path(path) => collect(&path.tree, names),
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    collect(tree, names);
                }
            }
            syn::UseTree::Rename(rename) if names.iter().any(|name| rename.ident == name) => {
                names.push(rename.rename.to_string());
            }
            _ => {}
        }
    }
    let mut names = [
        "RawFinding",
        "TodoSuccess",
        "Decisions",
        "Proposals",
        "Summary",
        "Reason",
    ]
    .map(str::to_owned)
    .to_vec();
    for item in &file.items {
        if let Item::Use(import) = item {
            collect(&import.tree, &mut names);
        }
    }
    names
}

fn marker(value: &str) -> bool {
    value.starts_with("LOOM_")
        && value
            .trim_end()
            .trim_end_matches(':')
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && !value.starts_with("LOOM_FINDING_STATUS")
}

fn literal(expr: &Expr) -> Option<String> {
    if let Expr::Lit(node) = expr
        && let Lit::Str(value) = &node.lit
    {
        Some(value.value())
    } else {
        None
    }
}

impl Visitor<'_> {
    fn reject(&mut self, node: &impl syn::spanned::Spanned, reason: &str) {
        self.violations
            .push(format!("{}:{} {reason}", self.location, line_of(node)));
    }
}

impl<'ast> Visit<'ast> for Visitor<'_> {
    fn visit_item(&mut self, node: &'ast Item) {
        let attributes = match node {
            Item::Mod(node) => &node.attrs,
            Item::Fn(node) => &node.attrs,
            Item::Impl(node) => &node.attrs,
            Item::Enum(node) => &node.attrs,
            Item::Const(node) => &node.attrs,
            _ => return syn::visit::visit_item(self, node),
        };
        if attributes.iter().any(|attribute| {
            attribute.path().is_ident("test")
                || (attribute.path().is_ident("cfg")
                    && attribute
                        .parse_args::<syn::Path>()
                        .is_ok_and(|path| path.is_ident("test")))
        }) {
            return;
        }
        syn::visit::visit_item(self, node);
    }

    fn visit_item_enum(&mut self, node: &'ast ItemEnum) {
        if node.ident == "ExitSignal" || node.ident == "TerminalMarker" {
            self.reject(
                node,
                "independent terminal vocabulary; re-export canonical Message instead",
            );
        }
        syn::visit::visit_item_enum(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        if matches!(
            node.method.to_string().as_str(),
            "contains"
                | "find"
                | "rfind"
                | "starts_with"
                | "ends_with"
                | "strip_prefix"
                | "split"
                | "split_once"
                | "eq"
        ) && node
            .args
            .iter()
            .filter_map(literal)
            .any(|value| marker(&value))
        {
            self.reject(node, "independent live-marker string scanner");
        }
        if node.method == "replace"
            && node.args.len() == 2
            && literal(&node.args[0]).as_deref() == Some("\n")
            && literal(&node.args[1]).as_deref() == Some("\\n")
        {
            self.reject(node, "raw-newline JSON repair");
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
        for arm in &node.arms {
            if let syn::Pat::Lit(value) = &arm.pat
                && let Lit::Str(value) = &value.lit
                && marker(&value.value())
            {
                self.reject(&arm.pat, "independent literal marker dispatch");
            }
        }
        syn::visit::visit_expr_match(self, node);
    }

    fn visit_expr_binary(&mut self, node: &'ast syn::ExprBinary) {
        if matches!(node.op, syn::BinOp::Eq(_) | syn::BinOp::Ne(_))
            && [node.left.as_ref(), node.right.as_ref()]
                .into_iter()
                .filter_map(literal)
                .any(|value| marker(&value))
        {
            self.reject(node, "independent literal marker comparison");
        }
        syn::visit::visit_expr_binary(self, node);
    }

    fn visit_expr_macro(&mut self, node: &'ast syn::ExprMacro) {
        fn has_marker(tokens: proc_macro2::TokenStream) -> bool {
            tokens.into_iter().any(|token| match token {
                proc_macro2::TokenTree::Group(group) => has_marker(group.stream()),
                proc_macro2::TokenTree::Literal(value) => {
                    syn::parse_str::<syn::LitStr>(&value.to_string())
                        .is_ok_and(|value| marker(&value.value()))
                }
                _ => false,
            })
        }
        if (node.mac.path.is_ident("matches") || node.mac.path.is_ident("vec"))
            && has_marker(node.mac.tokens.clone())
        {
            self.reject(node, "independent literal marker macro dispatch/registry");
        }
        syn::visit::visit_expr_macro(self, node);
    }

    fn visit_expr_array(&mut self, node: &'ast syn::ExprArray) {
        if node
            .elems
            .iter()
            .filter_map(literal)
            .any(|value| marker(&value))
        {
            self.reject(node, "independent marker string registry");
        }
        syn::visit::visit_expr_array(self, node);
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        if matches!(
            node.sig.ident.to_string().as_str(),
            "reason_at"
                | "question_at"
                | "terminal_markers"
                | "finding_records"
                | "scan_json_object"
                | "escape_raw_newlines"
        ) {
            self.reject(
                node,
                "legacy marker/prose scanner or JSON repair entry point",
            );
        }
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Expr::Path(call) = node.func.as_ref()
            && let Some(segment) = call.path.segments.last()
            && matches!(
                segment.ident.to_string().as_str(),
                "from_str" | "from_slice" | "from_value"
            )
            && let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments
        {
            for argument in &arguments.args {
                if let syn::GenericArgument::Type(Type::Path(ty)) = argument
                    && let Some(name) = ty.path.segments.last()
                    && self.payloads.iter().any(|payload| name.ident == payload)
                {
                    self.reject(node, "independent typed agent payload decoder");
                }
            }
        }
        syn::visit::visit_expr_call(self, node);
    }
}
