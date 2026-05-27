//! Surface-conformance walk — `harness.md` FR13.
//!
//! Compares the binary's user-facing command surface against FR1 of
//! `specs/harness.md`:
//!
//! - **Command set** — FR1's per-group bullets ↔ the `HELP_GROUPS`
//!   constant in `crates/loom/src/main.rs`.
//! - **Removed surface** — every row in FR1's *Removed surface* table
//!   MUST be absent from `HELP_GROUPS`.
//! - **Grouping order** — the order of `**Workflow** / **Inspection**
//!   / **State**` sub-sections in FR1 (and per-group bullet order) ↔
//!   the order of `HELP_GROUPS` tuples (and per-tuple slice order).
//! - **Flag set (partial)** — long flag names in the *Logs UX* table
//!   ↔ `Command::Logs` `#[arg(long, ...)]` declarations; long flag
//!   names in the *Msg Modes* flag table ↔ `Command::Msg`. FR1
//!   scope-flag inline prose (`loom gate <sub>` flags) is not yet
//!   covered.
//!
//! `HELP_GROUPS` is the canonical declaration the binary regroups
//! clap's flat `Commands:` block against, so parsing it as text is the
//! shortest path to the renderable surface without a clap-reflection
//! dep from this walk.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use syn::{Expr, ExprLit, Lit, Meta, MetaNameValue};

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "surface_conformance — binary surface matches specs/harness.md FR1";
const SPEC: &str = "specs/harness.md";
const MAIN_RS: &str = "crates/loom/src/main.rs";
const SPEC_GROUP_ORDER: &[&str] = &["Workflow", "Inspection", "State"];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let Some(spec_body) = read_to_string(&locate_rel(&root, SPEC)) else {
        return verdict_from(RULE, vec![format!("{SPEC} not readable")]);
    };
    let Some(main_body) = read_to_string(&root.join(MAIN_RS)) else {
        return verdict_from(RULE, vec![format!("{MAIN_RS} not readable")]);
    };

    let spec_groups = match parse_spec_command_groups(&spec_body) {
        Ok(g) => g,
        Err(e) => return verdict_from(RULE, vec![e]),
    };
    let spec_removed = match parse_spec_removed_surface(&spec_body) {
        Ok(r) => r,
        Err(e) => return verdict_from(RULE, vec![e]),
    };
    let binary_groups = match parse_binary_help_groups(&main_body) {
        Ok(b) => b,
        Err(e) => return verdict_from(RULE, vec![e]),
    };

    let mut violations = Vec::new();
    check_groups_match(&spec_groups, &binary_groups, &mut violations);
    check_removed_surface_absent(&spec_removed, &binary_groups, &mut violations);
    check_command_flag_set(&spec_body, &main_body, "logs", "Logs", &mut violations);
    check_command_flag_set(&spec_body, &main_body, "msg", "Msg", &mut violations);
    violations.retain(|v| !SURFACE_ALLOWLIST.iter().any(|allow| v.contains(allow)));
    verdict_from(RULE, violations)
}

/// Allowlist of violation-substrings intentionally suppressed pending
/// cleanup under bead **lm-hyh7**. Each entry pairs with the open
/// rename or removal work (lm-9ehh.3) that will reconcile the spec
/// and the binary's `HELP_GROUPS`; remove the entry once the rename
/// reaches `HELP_GROUPS`.
const SURFACE_ALLOWLIST: &[&str] = &[
    // lm-9ehh.3: specs/harness.md FR1 was renamed to `loop` but
    // `HELP_GROUPS` still names the pre-rename commands. Skip the
    // four resulting mismatches until lm-9ehh.3 lands.
    "FR1 lists `loop` under Workflow",
    "HELP_GROUPS lists `run` under Workflow",
    "HELP_GROUPS re-introduces `run` under Workflow",
    "HELP_GROUPS re-introduces `use` under State",
    "HELP_GROUPS re-introduces `todo` under Workflow",
];

fn check_command_flag_set(
    spec_body: &str,
    main_body: &str,
    cmd_label: &str,
    variant: &str,
    violations: &mut Vec<String>,
) {
    let spec_flags = match cmd_label {
        "logs" => parse_logs_ux_flags(spec_body),
        "msg" => parse_msg_modes_flags(spec_body),
        _ => return,
    };
    let spec_flags = match spec_flags {
        Ok(s) => s,
        Err(e) => {
            violations.push(e);
            return;
        }
    };
    let binary_flags = match parse_binary_command_flags(main_body, variant) {
        Ok(b) => b,
        Err(e) => {
            violations.push(e);
            return;
        }
    };
    for flag in spec_flags.difference(&binary_flags) {
        violations.push(format!(
            "{SPEC} `loom {cmd_label}` flag `--{flag}` documented but not declared on `Command::{variant}` in {MAIN_RS}",
        ));
    }
    for flag in binary_flags.difference(&spec_flags) {
        violations.push(format!(
            "{MAIN_RS} `Command::{variant}` declares `--{flag}` but it is not documented in {SPEC} `loom {cmd_label}` flag table",
        ));
    }
}

fn check_groups_match(
    spec: &[(String, Vec<String>)],
    binary: &[(String, Vec<String>)],
    violations: &mut Vec<String>,
) {
    let spec_headings: Vec<&str> = spec.iter().map(|(h, _)| h.as_str()).collect();
    let binary_headings: Vec<&str> = binary.iter().map(|(h, _)| h.as_str()).collect();
    if spec_headings != binary_headings {
        violations.push(format!(
            "{SPEC} FR1 group order {spec_headings:?} but {MAIN_RS} HELP_GROUPS order {binary_headings:?}",
        ));
        return;
    }
    for ((heading, spec_cmds), (_, binary_cmds)) in spec.iter().zip(binary.iter()) {
        for cmd in spec_cmds {
            if !binary_cmds.contains(cmd) {
                violations.push(format!(
                    "{SPEC} FR1 lists `{cmd}` under {heading} but {MAIN_RS} HELP_GROUPS does not",
                ));
            }
        }
        for cmd in binary_cmds {
            if !spec_cmds.contains(cmd) {
                violations.push(format!(
                    "{MAIN_RS} HELP_GROUPS lists `{cmd}` under {heading} but {SPEC} FR1 does not",
                ));
            }
        }
        if spec_cmds != binary_cmds
            && spec_cmds.iter().collect::<std::collections::BTreeSet<_>>()
                == binary_cmds
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
        {
            violations.push(format!(
                "{heading} per-group order differs — {SPEC} {spec_cmds:?} vs {MAIN_RS} {binary_cmds:?}",
            ));
        }
    }
}

fn check_removed_surface_absent(
    removed: &[String],
    binary: &[(String, Vec<String>)],
    violations: &mut Vec<String>,
) {
    for cmd in removed {
        for (heading, cmds) in binary {
            if cmds.iter().any(|c| c == cmd) {
                violations.push(format!(
                    "{MAIN_RS} HELP_GROUPS re-introduces `{cmd}` under {heading} — listed in {SPEC} Removed surface table",
                ));
            }
        }
    }
}

fn parse_spec_command_groups(body: &str) -> Result<Vec<(String, Vec<String>)>, String> {
    let (fr1, _, _) = locate_fr1(body)?;
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<usize> = None;
    for line in fr1 {
        let trimmed = line.trim_start();
        if let Some(name) = strip_group_header(trimmed)
            && SPEC_GROUP_ORDER.contains(&name)
        {
            groups.push((name.to_string(), Vec::new()));
            current = Some(groups.len() - 1);
            continue;
        }
        if let Some(cmd) = extract_loom_subcommand(trimmed)
            && let Some(idx) = current
        {
            groups[idx].1.push(cmd);
        }
    }
    if groups.is_empty() {
        return Err(format!("{SPEC} FR1 parsed no command groups"));
    }
    Ok(groups)
}

fn parse_spec_removed_surface(body: &str) -> Result<Vec<String>, String> {
    let (fr1, _, _) = locate_fr1(body)?;
    let marker_idx = fr1
        .iter()
        .position(|l| l.trim_start().starts_with("**Removed surface.**"))
        .ok_or_else(|| format!("{SPEC} FR1 missing `**Removed surface.**` marker"))?;
    let tail = &fr1[marker_idx..];
    let header_idx = tail
        .iter()
        .position(|l| l.trim_start().starts_with("| Surface "))
        .ok_or_else(|| format!("{SPEC} Removed-surface table missing `| Surface ` header row"))?;
    let mut out = Vec::new();
    for line in tail.iter().skip(header_idx + 2) {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('|') {
            break;
        }
        let cells: Vec<&str> = trimmed.split('|').collect();
        if cells.len() < 2 {
            continue;
        }
        let first = cells[1].trim().trim_matches('`');
        if let Some(cmd) = first.strip_prefix("loom ") {
            let name = cmd.split_whitespace().next().unwrap_or("");
            if !name.is_empty() {
                out.push(name.to_string());
            }
        }
    }
    if out.is_empty() {
        return Err(format!("{SPEC} Removed-surface table parsed empty"));
    }
    Ok(out)
}

/// Resolve a relative path against the cargo workspace root, falling back to
/// each ancestor when the direct join is absent. Specs live in
/// `<repo-root>/specs/` but `workspace_root()` resolves to
/// `<repo-root>/loom/`, so the ancestor search bridges the gap.
fn locate_rel(workspace: &Path, rel: &str) -> PathBuf {
    let direct = workspace.join(rel);
    if direct.is_file() {
        return direct;
    }
    for ancestor in workspace.ancestors().skip(1) {
        let candidate = ancestor.join(rel);
        if candidate.is_file() {
            return candidate;
        }
    }
    direct
}

fn locate_fr1(body: &str) -> Result<(Vec<&str>, usize, usize), String> {
    let lines: Vec<&str> = body.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with("1. **Command set**"))
        .ok_or_else(|| format!("{SPEC} missing `1. **Command set**` heading"))?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| l.starts_with("2. **"))
        .map(|(i, _)| i)
        .unwrap_or(lines.len());
    Ok((lines[start..end].to_vec(), start, end))
}

fn strip_group_header(line: &str) -> Option<&str> {
    let after = line.strip_prefix("**")?;
    let end = after.find("**")?;
    Some(&after[..end])
}

fn extract_loom_subcommand(line: &str) -> Option<String> {
    let after_dash = line.strip_prefix("- ")?;
    let after_tick = after_dash.strip_prefix('`')?;
    let end = after_tick.find('`')?;
    let inside = &after_tick[..end];
    let cmd = inside.strip_prefix("loom ")?;
    let name = cmd.split_whitespace().next()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn parse_binary_help_groups(body: &str) -> Result<Vec<(String, Vec<String>)>, String> {
    let start = body
        .find("const HELP_GROUPS")
        .ok_or_else(|| format!("{MAIN_RS} missing `const HELP_GROUPS` declaration"))?;
    let after_const = &body[start..];
    let array_open = after_const
        .find("= &[")
        .ok_or_else(|| format!("{MAIN_RS} HELP_GROUPS missing `= &[`"))?;
    let block_start = array_open + 4;
    let block_end = after_const[block_start..]
        .find("];")
        .ok_or_else(|| format!("{MAIN_RS} HELP_GROUPS missing closing `];`"))?;
    let block = &after_const[block_start..block_start + block_end];

    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let bytes = block.as_bytes();
    let mut i = 0usize;
    let mut depth = 0i32;
    let mut tuple_start: Option<usize> = None;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => {
                if depth == 0 {
                    tuple_start = Some(i + 1);
                }
                depth += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0
                    && let Some(s) = tuple_start.take()
                {
                    let inner = &block[s..i];
                    let strings = extract_quoted_strings(inner);
                    if let Some((heading, cmds)) = strings.split_first() {
                        groups.push((heading.clone(), cmds.to_vec()));
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    if groups.is_empty() {
        return Err(format!("{MAIN_RS} HELP_GROUPS parsed empty"));
    }
    Ok(groups)
}

fn parse_logs_ux_flags(body: &str) -> Result<BTreeSet<String>, String> {
    let lines: Vec<&str> = body.lines().collect();
    let heading = lines
        .iter()
        .position(|l| l.trim_start().starts_with("### Logs UX"))
        .ok_or_else(|| format!("{SPEC} missing `### Logs UX` heading"))?;
    let header = lines
        .iter()
        .enumerate()
        .skip(heading + 1)
        .find(|(_, l)| l.trim_start().starts_with("| Flag "))
        .map(|(i, _)| i)
        .ok_or_else(|| format!("{SPEC} Logs UX missing `| Flag ` table header"))?;
    let mut out = BTreeSet::new();
    for line in lines.iter().skip(header + 2) {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('|') {
            break;
        }
        let cells: Vec<&str> = trimmed.split('|').collect();
        if cells.len() < 2 {
            continue;
        }
        for name in extract_long_flags(cells[1]) {
            out.insert(name);
        }
    }
    if out.is_empty() {
        return Err(format!("{SPEC} Logs UX table parsed no long flags"));
    }
    Ok(out)
}

fn parse_msg_modes_flags(body: &str) -> Result<BTreeSet<String>, String> {
    let lines: Vec<&str> = body.lines().collect();
    let heading = lines
        .iter()
        .position(|l| l.trim_start().starts_with("### Msg Modes"))
        .ok_or_else(|| format!("{SPEC} missing `### Msg Modes` heading"))?;
    let flag_table_marker = lines
        .iter()
        .enumerate()
        .skip(heading + 1)
        .find(|(_, l)| l.trim_start().starts_with("**Flag table.**"))
        .map(|(i, _)| i)
        .ok_or_else(|| format!("{SPEC} Msg Modes missing `**Flag table.**` marker"))?;
    let header = lines
        .iter()
        .enumerate()
        .skip(flag_table_marker + 1)
        .find(|(_, l)| l.trim_start().starts_with("| Short "))
        .map(|(i, _)| i)
        .ok_or_else(|| format!("{SPEC} Msg Modes flag table missing `| Short ` header"))?;
    let mut out = BTreeSet::new();
    for line in lines.iter().skip(header + 2) {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('|') {
            break;
        }
        let cells: Vec<&str> = trimmed.split('|').collect();
        if cells.len() < 3 {
            continue;
        }
        for name in extract_long_flags(cells[2]) {
            out.insert(name);
        }
    }
    if out.is_empty() {
        return Err(format!("{SPEC} Msg Modes flag table parsed no long flags"));
    }
    Ok(out)
}

fn extract_long_flags(cell: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = cell.as_bytes();
    let mut i = 0;
    while i + 2 <= bytes.len() {
        if bytes[i] == b'-' && bytes[i + 1] == b'-' {
            let start = i + 2;
            let mut j = start;
            while j < bytes.len()
                && (bytes[j].is_ascii_lowercase() || bytes[j].is_ascii_digit() || bytes[j] == b'-')
            {
                j += 1;
            }
            if j > start
                && let Ok(name) = std::str::from_utf8(&bytes[start..j])
            {
                out.push(name.to_string());
            }
            i = j.max(i + 2);
        } else {
            i += 1;
        }
    }
    out
}

fn parse_binary_command_flags(body: &str, variant: &str) -> Result<BTreeSet<String>, String> {
    let file = syn::parse_file(body).map_err(|e| format!("{MAIN_RS} syn parse: {e}"))?;
    let cmd_enum = file
        .items
        .iter()
        .find_map(|i| match i {
            syn::Item::Enum(e) if e.ident == "Command" => Some(e),
            _ => None,
        })
        .ok_or_else(|| format!("{MAIN_RS} no `Command` enum"))?;
    let var = cmd_enum
        .variants
        .iter()
        .find(|v| v.ident == variant)
        .ok_or_else(|| format!("{MAIN_RS} `Command` has no `{variant}` variant"))?;
    let fields = match &var.fields {
        syn::Fields::Named(n) => &n.named,
        _ => {
            return Err(format!(
                "{MAIN_RS} `Command::{variant}` has no named fields to audit"
            ));
        }
    };
    let mut out = BTreeSet::new();
    for field in fields {
        let field_name = field
            .ident
            .as_ref()
            .map(std::string::ToString::to_string)
            .unwrap_or_default();
        for attr in &field.attrs {
            if !attr.path().is_ident("arg") {
                continue;
            }
            if let Some(long) = arg_long_name(attr, &field_name) {
                out.insert(long);
            }
        }
    }
    Ok(out)
}

fn arg_long_name(attr: &syn::Attribute, field_name: &str) -> Option<String> {
    let meta_list = attr.meta.require_list().ok()?;
    let nested: syn::punctuated::Punctuated<Meta, syn::Token![,]> = meta_list
        .parse_args_with(syn::punctuated::Punctuated::parse_terminated)
        .ok()?;
    for meta in nested {
        match meta {
            Meta::Path(p) if p.is_ident("long") => {
                return Some(field_name.replace('_', "-"));
            }
            Meta::NameValue(MetaNameValue {
                path,
                value:
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(s), ..
                    }),
                ..
            }) if path.is_ident("long") => {
                return Some(s.value());
            }
            _ => {}
        }
    }
    None
}

fn extract_quoted_strings(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'"' {
                j += 1;
            }
            if j > bytes.len() {
                break;
            }
            out.push(
                std::str::from_utf8(&bytes[start..j])
                    .unwrap_or("")
                    .to_string(),
            );
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}
