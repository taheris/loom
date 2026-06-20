//! Architectural: `TodoContext` carries `criterion_status:
//! Vec<CriterionStatus>`; the other phase context structs (`PlanContext`,
//! `LoopContext`, `ReviewContext`, `InboxContext`) do not. The
//! criterion-status decomposition-evidence surface is scoped to the unified
//! `todo` phase per `specs/templates.md` § Criterion-Status Surface.

use std::collections::HashMap;

use syn::spanned::Spanned;

use super::util::{parse_rs, rel, rs_files_recursive, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "todo_contexts_carry_criterion_status — TodoContext carries `criterion_status: Vec<CriterionStatus>`; no other phase context does";

const FIELD: &str = "criterion_status";
const SRC_DIR: &str = "crates/loom-templates/src";

const REQUIRED: &[&str] = &["TodoContext"];
const FORBIDDEN: &[&str] = &[
    "PlanContext",
    "LoopContext",
    "ReviewContext",
    "InboxContext",
];

struct Found {
    rel_path: String,
    struct_line: usize,
    field: Option<FieldInfo>,
}

struct FieldInfo {
    line: usize,
    is_vec_criterion_status: bool,
}

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let src_dir = root.join(SRC_DIR);
    let mut found: HashMap<String, Found> = HashMap::new();

    for path in rs_files_recursive(&src_dir) {
        let Some(file) = parse_rs(&path) else {
            continue;
        };
        let rel_path = rel(&root, &path);
        for item in &file.items {
            let syn::Item::Struct(s) = item else { continue };
            let name = s.ident.to_string();
            if !REQUIRED.contains(&name.as_str()) && !FORBIDDEN.contains(&name.as_str()) {
                continue;
            }
            let field = struct_criterion_field(s);
            found.insert(
                name,
                Found {
                    rel_path: rel_path.clone(),
                    struct_line: s.span().start().line,
                    field,
                },
            );
        }
    }

    let mut violations = Vec::new();

    for name in REQUIRED {
        match found.get(*name) {
            None => violations.push(format!(
                "{SRC_DIR}/<{name}>:1 `{name}` struct not found — expected to carry `{FIELD}: Vec<CriterionStatus>`",
            )),
            Some(f) => match &f.field {
                None => violations.push(format!(
                    "{}:{} `{name}` is missing field `{FIELD}: Vec<CriterionStatus>`",
                    f.rel_path, f.struct_line,
                )),
                Some(info) if !info.is_vec_criterion_status => violations.push(format!(
                    "{}:{} `{name}.{FIELD}` has wrong type — expected `Vec<CriterionStatus>`",
                    f.rel_path, info.line,
                )),
                Some(_) => {}
            },
        }
    }

    for name in FORBIDDEN {
        if let Some(f) = found.get(*name)
            && let Some(info) = &f.field
        {
            violations.push(format!(
                "{}:{} `{name}` carries field `{FIELD}` — only `TodoContext` may",
                f.rel_path, info.line,
            ));
        }
    }

    verdict_from(RULE, violations)
}

fn struct_criterion_field(s: &syn::ItemStruct) -> Option<FieldInfo> {
    let syn::Fields::Named(named) = &s.fields else {
        return None;
    };
    for f in &named.named {
        let Some(ident) = &f.ident else { continue };
        if ident != FIELD {
            continue;
        }
        return Some(FieldInfo {
            line: f.span().start().line,
            is_vec_criterion_status: is_vec_criterion_status(&f.ty),
        });
    }
    None
}

fn is_vec_criterion_status(ty: &syn::Type) -> bool {
    let syn::Type::Path(p) = ty else { return false };
    let Some(segment) = p.path.segments.last() else {
        return false;
    };
    if segment.ident != "Vec" {
        return false;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return false;
    };
    let Some(arg) = args.args.first() else {
        return false;
    };
    let syn::GenericArgument::Type(inner_ty) = arg else {
        return false;
    };
    let syn::Type::Path(inner_p) = inner_ty else {
        return false;
    };
    let Some(inner_segment) = inner_p.path.segments.last() else {
        return false;
    };
    inner_segment.ident == "CriterionStatus"
}
