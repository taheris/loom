//! RS-7: domain identifiers under `loom-driver/src/identifier/` are
//! single-field tuple-struct newtypes. `*Error` structs are exempt —
//! the identifier crate also defines `Parse*Error` payload structs.

use super::util::{line_of, parse_rs, rel, rs_files_in, verdict_from, workspace_root};
use super::{Verdict, WalkInput};
use syn::Fields;

const RULE: &str = "RS-7 identifiers are single-field tuple-struct newtypes";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let dir = root.join("crates/loom-driver/src/identifier");
    let mut violations = Vec::new();
    for path in rs_files_in(&dir) {
        let file_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if file_name == "mod.rs" {
            continue;
        }
        let Some(parsed) = parse_rs(&path) else {
            continue;
        };
        let rel_path = rel(&root, &path);
        for item in &parsed.items {
            let syn::Item::Struct(s) = item else { continue };
            if s.ident.to_string().ends_with("Error") {
                continue;
            }
            let line = line_of(&s.ident);
            match &s.fields {
                Fields::Unnamed(unnamed) if unnamed.unnamed.len() == 1 => {}
                _ => violations.push(format!(
                    "{}:{} identifier `{}` must be a single-field tuple-struct newtype",
                    rel_path, line, s.ident,
                )),
            }
        }
    }
    verdict_from(RULE, violations)
}
