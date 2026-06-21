//! Surface-conformance walk — `harness.md` FR13.
//!
//! Compares the binary's user-facing command surface against FR1 of
//! `specs/harness.md`:
//!
//! - **Command set** — FR1's per-group bullets ↔ the `HELP_GROUPS`
//!   constant in `crates/loom/src/main.rs`.
//! - **Removed surface** — every row in FR1's *Removed surface* table
//!   MUST be absent from `HELP_GROUPS`, the `Command` enum, and nested
//!   inbox actions/flags.
//! - **Grouping order** — the order of `**Workflow** / **Inspection**
//!   / **State**` sub-sections in FR1 (and per-group bullet order) ↔
//!   the order of `HELP_GROUPS` tuples (and per-tuple slice order).
//! - **Flag set (partial)** — long flag names in the *Logs UX* and
//!   *Inbox Modes* tables ↔ the corresponding clap `#[arg(...)]`
//!   declarations. FR1 scope-flag inline prose (`loom gate <sub>`
//!   flags) is not yet covered.
//!
//! `HELP_GROUPS` is the canonical declaration the binary regroups
//! clap's flat `Commands:` block against, so parsing it as text is the
//! shortest path to the renderable surface without a clap-reflection
//! dep from this walk.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::cli_surface;
use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "surface_conformance — binary surface matches specs/harness.md FR1";
const SPEC: &str = "specs/harness.md";
const MAIN_RS: &str = "crates/loom/src/main.rs";
const SPEC_GROUP_ORDER: &[&str] = &["Workflow", "Inspection", "State"];

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemovedEntry {
    TopCommand(String),
    Subcommand { command: String, subcommand: String },
    Flag { command: String, flag: RemovedFlag },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemovedFlag {
    Long(String),
    Short(char),
}

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
    let main_file = match cli_surface::parse_file(&main_body, MAIN_RS) {
        Ok(file) => file,
        Err(e) => return verdict_from(RULE, vec![e]),
    };
    let binary_top_commands = match cli_surface::enum_variant_names(&main_file, "Command", MAIN_RS)
    {
        Ok(commands) => commands,
        Err(e) => return verdict_from(RULE, vec![e]),
    };

    let mut violations = Vec::new();
    check_groups_match(&spec_groups, &binary_groups, &mut violations);
    check_removed_surface_absent(
        &spec_removed,
        &binary_groups,
        &binary_top_commands,
        &main_file,
        &mut violations,
    );
    check_command_flag_set(&spec_body, &main_file, "logs", "Logs", &mut violations);
    check_inbox_surface(&spec_body, &main_file, &mut violations);
    violations.retain(|v| !SURFACE_ALLOWLIST.iter().any(|allow| v.contains(allow)));
    verdict_from(RULE, violations)
}

/// Allowlist of violation-substrings intentionally suppressed pending
/// cleanup under bead **lm-hyh7**. Each entry pairs with the open
/// removal work that will reconcile the spec and the binary's
/// `Command::*` definitions; remove the entry once the matching flag
/// is gone from the binary.
const SURFACE_ALLOWLIST: &[&str] = &[];

fn check_command_flag_set(
    spec_body: &str,
    main_file: &syn::File,
    cmd_label: &str,
    variant: &str,
    violations: &mut Vec<String>,
) {
    let spec_flags = match cmd_label {
        "logs" => parse_logs_ux_flags(spec_body),
        _ => return,
    };
    let spec_flags = match spec_flags {
        Ok(s) => s,
        Err(e) => {
            violations.push(e);
            return;
        }
    };
    let binary_flags = match parse_binary_command_flags(main_file, variant) {
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
    removed: &[RemovedEntry],
    binary_groups: &[(String, Vec<String>)],
    binary_top_commands: &BTreeSet<String>,
    main_file: &syn::File,
    violations: &mut Vec<String>,
) {
    let needs_inbox = removed.iter().any(|entry| match entry {
        RemovedEntry::Subcommand { command, .. } | RemovedEntry::Flag { command, .. } => {
            command == "inbox"
        }
        RemovedEntry::TopCommand(_) => false,
    });
    let inbox_subcommands = if needs_inbox {
        match cli_surface::enum_variant_names(main_file, "InboxAction", MAIN_RS) {
            Ok(commands) => commands,
            Err(e) => {
                violations.push(e);
                BTreeSet::new()
            }
        }
    } else {
        BTreeSet::new()
    };
    let inbox_flags = if needs_inbox {
        match inbox_flags(main_file) {
            Ok(flags) => flags,
            Err(e) => {
                violations.push(e);
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    for entry in removed {
        match entry {
            RemovedEntry::TopCommand(cmd) => {
                for (heading, cmds) in binary_groups {
                    if cmds.iter().any(|c| c == cmd) {
                        violations.push(format!(
                            "{MAIN_RS} HELP_GROUPS re-introduces `{cmd}` under {heading} — listed in {SPEC} Removed surface table",
                        ));
                    }
                }
                if binary_top_commands.contains(cmd) {
                    violations.push(format!(
                        "{MAIN_RS} `Command` enum re-introduces `{cmd}` — listed in {SPEC} Removed surface table",
                    ));
                }
            }
            RemovedEntry::Subcommand {
                command,
                subcommand,
            } if command == "inbox" => {
                if inbox_subcommands.contains(subcommand) {
                    violations.push(format!(
                        "{MAIN_RS} `InboxAction` re-introduces `loom inbox {subcommand}` — listed in {SPEC} Removed surface table",
                    ));
                }
            }
            RemovedEntry::Subcommand { .. } => {}
            RemovedEntry::Flag { command, flag } if command == "inbox" => {
                if inbox_flags
                    .iter()
                    .any(|candidate| removed_flag_matches(flag, candidate))
                {
                    violations.push(format!(
                        "{MAIN_RS} inbox args re-declare `{}` — listed in {SPEC} Removed surface table",
                        format_removed_flag(flag),
                    ));
                }
            }
            RemovedEntry::Flag { .. } => {}
        }
    }
}

fn removed_flag_matches(removed: &RemovedFlag, candidate: &cli_surface::Flag) -> bool {
    match removed {
        RemovedFlag::Long(name) => candidate.long.as_deref() == Some(name.as_str()),
        RemovedFlag::Short(name) => candidate.short == Some(*name),
    }
}

fn format_removed_flag(flag: &RemovedFlag) -> String {
    match flag {
        RemovedFlag::Long(name) => format!("--{name}"),
        RemovedFlag::Short(name) => format!("-{name}"),
    }
}

fn inbox_flags(main_file: &syn::File) -> Result<Vec<cli_surface::Flag>, String> {
    let mut out = Vec::new();
    for struct_name in [
        "InboxArgs",
        "InboxFilterArgs",
        "InboxListArgs",
        "InboxViewArgs",
        "InboxChatArgs",
    ] {
        out.extend(cli_surface::struct_flags(main_file, struct_name, MAIN_RS)?);
    }
    Ok(out)
}

fn check_inbox_surface(spec_body: &str, main_file: &syn::File, violations: &mut Vec<String>) {
    let Some(expected_subcommands) = parse_spec_inbox_subcommands(spec_body, violations) else {
        return;
    };
    let expected_flags = match parse_spec_inbox_long_flags(spec_body) {
        Ok(Some(flags)) => flags,
        Ok(None) => return,
        Err(e) => {
            violations.push(e);
            return;
        }
    };
    let actual_subcommands =
        match cli_surface::enum_variant_names(main_file, "InboxAction", MAIN_RS) {
            Ok(commands) => commands,
            Err(e) => {
                violations.push(e);
                return;
            }
        };
    compare_named_set(
        "Inbox Modes subcommands",
        &expected_subcommands,
        &actual_subcommands,
        violations,
    );

    let filter_flags = match cli_surface::struct_long_flags(main_file, "InboxFilterArgs", MAIN_RS) {
        Ok(flags) => flags,
        Err(e) => {
            violations.push(e);
            return;
        }
    };
    let view_direct = match cli_surface::struct_long_flags(main_file, "InboxViewArgs", MAIN_RS) {
        Ok(flags) => flags,
        Err(e) => {
            violations.push(e);
            return;
        }
    };
    let chat_direct = match cli_surface::struct_long_flags(main_file, "InboxChatArgs", MAIN_RS) {
        Ok(flags) => flags,
        Err(e) => {
            violations.push(e);
            return;
        }
    };
    let expected_filter = expected_flags
        .iter()
        .filter(|flag| flag.as_str() == "spec" || flag.as_str() == "kind")
        .cloned()
        .collect::<BTreeSet<_>>();
    let expected_address = expected_flags
        .difference(&expected_filter)
        .cloned()
        .collect::<BTreeSet<_>>();
    compare_named_set(
        "Inbox filter flags",
        &expected_filter,
        &filter_flags,
        violations,
    );
    compare_named_set(
        "Inbox view address flags",
        &expected_address,
        &view_direct,
        violations,
    );
    compare_named_set(
        "Inbox chat address flags",
        &expected_address,
        &chat_direct,
        violations,
    );
}

fn compare_named_set(
    label: &str,
    expected: &BTreeSet<String>,
    actual: &BTreeSet<String>,
    violations: &mut Vec<String>,
) {
    for missing in expected.difference(actual) {
        violations.push(format!(
            "{SPEC} documents {label} `{missing}` but {MAIN_RS} does not declare it",
        ));
    }
    for extra in actual.difference(expected) {
        violations.push(format!(
            "{MAIN_RS} declares {label} `{extra}` but {SPEC} does not document it",
        ));
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

fn parse_spec_inbox_subcommands(
    body: &str,
    violations: &mut Vec<String>,
) -> Option<BTreeSet<String>> {
    let section = section_lines(body, "### Inbox Modes")?;
    let header_idx = match section
        .iter()
        .position(|line| line.trim_start().starts_with("| Mode "))
    {
        Some(idx) => idx,
        None => {
            violations.push(format!("{SPEC} Inbox Modes table missing `| Mode ` header"));
            return None;
        }
    };
    let mut out = BTreeSet::new();
    for line in section.iter().skip(header_idx + 2) {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('|') {
            break;
        }
        for span in extract_code_spans(trimmed) {
            if let Some(subcommand) = inbox_subcommand_from_invocation(&span) {
                out.insert(subcommand);
            }
        }
    }
    if out.is_empty() {
        violations.push(format!("{SPEC} Inbox Modes table parsed no subcommands"));
        None
    } else {
        Some(out)
    }
}

fn parse_spec_inbox_long_flags(body: &str) -> Result<Option<BTreeSet<String>>, String> {
    let Some(section) = section_lines(body, "### Inbox Modes") else {
        return Ok(None);
    };
    let header_idx = section
        .iter()
        .position(|line| line.trim_start().starts_with("| Flag "))
        .ok_or_else(|| format!("{SPEC} Inbox Modes missing `| Flag ` table header"))?;
    let mut out = BTreeSet::new();
    for line in section.iter().skip(header_idx + 2) {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('|') {
            break;
        }
        let cells: Vec<&str> = trimmed.split('|').collect();
        if cells.len() < 2 {
            continue;
        }
        for flag in extract_long_flags(cells[1]) {
            out.insert(flag);
        }
    }
    if out.is_empty() {
        Err(format!(
            "{SPEC} Inbox Modes flag table parsed no long flags"
        ))
    } else {
        Ok(Some(out))
    }
}

fn inbox_subcommand_from_invocation(invocation: &str) -> Option<String> {
    let rest = invocation.strip_prefix("loom inbox")?.trim();
    let mut tokens = rest.split_whitespace();
    let first = tokens.next()?;
    if first.starts_with('-') || first.starts_with('<') {
        None
    } else {
        Some(first.to_string())
    }
}

fn parse_spec_removed_surface(body: &str) -> Result<Vec<RemovedEntry>, String> {
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
        for span in extract_code_spans(cells[1]) {
            if let Some(entry) = removed_entry_from_invocation(&span) {
                out.push(entry);
            }
        }
    }
    if out.is_empty() {
        return Err(format!("{SPEC} Removed-surface table parsed empty"));
    }
    Ok(out)
}

fn removed_entry_from_invocation(invocation: &str) -> Option<RemovedEntry> {
    let rest = invocation.strip_prefix("loom ")?;
    let mut tokens = rest.split_whitespace();
    let command = tokens.next()?;
    let Some(next) = tokens.next() else {
        return Some(RemovedEntry::TopCommand(command.to_string()));
    };
    if let Some(flag) = removed_flag_from_token(next) {
        Some(RemovedEntry::Flag {
            command: command.to_string(),
            flag,
        })
    } else {
        Some(RemovedEntry::Subcommand {
            command: command.to_string(),
            subcommand: next.to_string(),
        })
    }
}

fn removed_flag_from_token(token: &str) -> Option<RemovedFlag> {
    if let Some(long) = token.strip_prefix("--")
        && !long.is_empty()
    {
        return Some(RemovedFlag::Long(long.to_string()));
    }
    if let Some(short) = token.strip_prefix('-') {
        let mut chars = short.chars();
        if let (Some(name), None) = (chars.next(), chars.next()) {
            return Some(RemovedFlag::Short(name));
        }
    }
    None
}

fn extract_code_spans(cell: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = cell;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else {
            break;
        };
        out.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    out
}

fn section_lines<'a>(body: &'a str, heading: &str) -> Option<Vec<&'a str>> {
    let lines: Vec<&str> = body.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.trim_start().starts_with(heading))?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, line)| line.starts_with("### "))
        .map(|(idx, _)| idx)
        .unwrap_or(lines.len());
    Some(lines[start..end].to_vec())
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

fn parse_binary_command_flags(
    main_file: &syn::File,
    variant: &str,
) -> Result<BTreeSet<String>, String> {
    let cmd_enum = main_file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(item) if item.ident == "Command" => Some(item),
            _ => None,
        })
        .ok_or_else(|| format!("{MAIN_RS} no `Command` enum"))?;
    let var = cmd_enum
        .variants
        .iter()
        .find(|v| v.ident == variant)
        .ok_or_else(|| format!("{MAIN_RS} `Command` has no `{variant}` variant"))?;
    let syn::Fields::Named(fields) = &var.fields else {
        return Err(format!(
            "{MAIN_RS} `Command::{variant}` has no named fields to audit"
        ));
    };
    let mut out = BTreeSet::new();
    for field in &fields.named {
        let field_name = field
            .ident
            .as_ref()
            .map(std::string::ToString::to_string)
            .unwrap_or_default();
        for attr in &field.attrs {
            if !attr.path().is_ident("arg") {
                continue;
            }
            if let Some(flag) =
                cli_surface::flag_from_arg_attr(attr, &field_name, MAIN_RS, variant)?
                && let Some(long) = flag.long
            {
                out.insert(long);
            }
        }
    }
    Ok(out)
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
