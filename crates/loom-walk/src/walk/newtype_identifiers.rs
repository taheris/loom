//! RS-7: identifiers in `loom-events` are single-field tuple-struct newtypes.
//! Error payloads are exempt; absent or unreadable source is not passing evidence.

use std::path::{Path, PathBuf};

use syn::Fields;

use super::util::{line_of, narrow_to_loom_files, rel, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "RS-7 identifiers are single-field tuple-struct newtypes";
const DIRECTORY: &str = "crates/loom-events/src/identifier";

pub fn run(input: &WalkInput) -> Verdict {
    let root = workspace_root();
    match inspect(&root, input) {
        Ok(violations) => verdict_from(RULE, violations),
        Err(error) => verdict_from(RULE, vec![format!("{DIRECTORY}: {error}")]),
    }
}

fn inspect(root: &Path, input: &WalkInput) -> Result<Vec<String>, std::io::Error> {
    let mut paths = std::fs::read_dir(root.join(DIRECTORY))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<PathBuf>, _>>()?;
    paths.retain(|path| path.extension().is_some_and(|extension| extension == "rs"));
    paths.sort();
    if paths.is_empty() {
        return Ok(vec![format!(
            "{DIRECTORY}: no identifier source files found"
        )]);
    }
    let mut violations = Vec::new();
    let mut identifiers = 0;
    for path in narrow_to_loom_files(paths, input, root) {
        let rel_path = rel(root, &path);
        let body = match std::fs::read_to_string(&path) {
            Ok(body) => body,
            Err(error) => {
                violations.push(format!("{rel_path}: unable to read Rust source: {error}"));
                continue;
            }
        };
        let parsed = match syn::parse_file(&body) {
            Ok(parsed) => parsed,
            Err(error) => {
                violations.push(format!("{rel_path}: unable to parse Rust source: {error}"));
                continue;
            }
        };
        for item in &parsed.items {
            let syn::Item::Struct(s) = item else { continue };
            if s.ident.to_string().ends_with("Error") {
                continue;
            }
            identifiers += 1;
            match &s.fields {
                Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {}
                _ => violations.push(format!(
                    "{rel_path}:{} identifier `{}` must be a single-field tuple-struct newtype",
                    line_of(&s.ident),
                    s.ident,
                )),
            }
        }
    }
    if input.files.is_none() && identifiers == 0 {
        violations.push(format!("{DIRECTORY}: no identifier definitions found"));
    }
    Ok(violations)
}
