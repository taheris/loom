//! Pinning-matrix consistency: the `### Pinning Policy` table in
//! `specs/templates.md` is the authoritative contract for which
//! partials each workflow template pins. Drift between matrix and code
//! is silent — a partial dropped from a template (or quietly added)
//! does not break compilation yet changes what the agent sees in that
//! phase. This walk parses both sides and surfaces every divergent
//! cell.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str =
    "template_pinning_matrix — pinning matrix in specs/templates.md matches the include graph";

const SPEC_REL: &str = "specs/templates.md";
const TEMPLATES_REL: &str = "crates/loom-templates/templates";
const PINNING_HEADING_PREFIX: &str = "### Pinning Policy";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let spec_path = locate_spec(&root);
    let templates_dir = root.join(TEMPLATES_REL);

    let matrix = match parse_pinning_matrix(&spec_path) {
        Ok(m) => m,
        Err(e) => {
            return Verdict {
                pass: false,
                evidence: format!("{e}\n{RULE}"),
            };
        }
    };
    let code = match template_partials(&templates_dir) {
        Ok(c) => c,
        Err(e) => {
            return Verdict {
                pass: false,
                evidence: format!("{e}\n{RULE}"),
            };
        }
    };

    let mut all_partials: BTreeSet<String> = matrix.partials.iter().cloned().collect();
    for set in code.values() {
        for p in set {
            all_partials.insert(p.clone());
        }
    }

    let mut violations = Vec::new();
    for partial in &all_partials {
        for template in &matrix.templates {
            let spec_cell = matrix
                .cells
                .get(&(partial.clone(), template.clone()))
                .copied()
                .unwrap_or(Cell::Absent);
            let code_includes = code.get(template).is_some_and(|s| s.contains(partial));
            match (spec_cell, code_includes) {
                (Cell::Present, false) => violations.push(format!(
                    "{SPEC_REL} spec marks `{partial}` ✓ for `{template}` \
                     but the template does not (transitively) include it"
                )),
                (Cell::Absent, true) => violations.push(format!(
                    "{TEMPLATES_REL}/{template}.md (transitively) includes `{partial}` \
                     but the spec matrix marks the cell blank"
                )),
                (Cell::PendingAddition, true) | (Cell::PendingRemoval, false) => {
                    violations.push(format!(
                        "{SPEC_REL} pending-marker-resolved for `{partial}` / `{template}` — update the matrix cell"
                    ));
                }
                _ => {}
            }
        }
    }
    violations.sort();
    verdict_from(RULE, violations)
}

struct PinningMatrix {
    cells: BTreeMap<(String, String), Cell>,
    templates: Vec<String>,
    partials: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cell {
    Absent,
    Present,
    PendingAddition,
    PendingRemoval,
}

impl Cell {
    fn parse(raw: &str, line: usize, partial: &str, template: &str) -> Result<Self, String> {
        match raw.trim() {
            "" => Ok(Self::Absent),
            "✓" => Ok(Self::Present),
            "?" => Ok(Self::PendingAddition),
            "~" => Ok(Self::PendingRemoval),
            other => Err(format!(
                "{SPEC_REL}:{line} invalid pinning-matrix cell `{other}` for `{partial}` / `{template}`; expected blank, ✓, ?, or ~"
            )),
        }
    }
}

fn locate_spec(workspace: &Path) -> PathBuf {
    let direct = workspace.join(SPEC_REL);
    if direct.is_file() {
        return direct;
    }
    for ancestor in workspace.ancestors().skip(1) {
        let candidate = ancestor.join(SPEC_REL);
        if candidate.is_file() {
            return candidate;
        }
    }
    direct
}

fn parse_pinning_matrix(spec_path: &Path) -> Result<PinningMatrix, String> {
    let body =
        read_to_string(spec_path).ok_or_else(|| format!("spec file not found at {SPEC_REL}"))?;
    let lines: Vec<&str> = body.lines().collect();

    let heading_idx = lines
        .iter()
        .position(|l| l.trim_start().starts_with(PINNING_HEADING_PREFIX))
        .ok_or_else(|| format!("no `### Pinning Policy` heading in {SPEC_REL}"))?;

    let header_idx = lines
        .iter()
        .enumerate()
        .skip(heading_idx + 1)
        .find(|(_, l)| l.trim_start().starts_with("| Partial "))
        .map(|(i, _)| i)
        .ok_or_else(|| format!("no `| Partial …` header row under heading in {SPEC_REL}"))?;

    let separator_idx = header_idx + 1;
    if separator_idx >= lines.len() || !is_table_separator(lines[separator_idx]) {
        return Err(format!(
            "{SPEC_REL}:{} expected `|---|...` separator under header row",
            separator_idx + 1
        ));
    }

    let header_cells = split_table_row(lines[header_idx]);
    if header_cells.len() < 2 {
        return Err(format!(
            "{SPEC_REL}:{} header row has {} cells, want at least 2",
            header_idx + 1,
            header_cells.len()
        ));
    }
    let templates: Vec<String> = header_cells[1..]
        .iter()
        .map(|c| strip_inline_code(c).to_string())
        .collect();

    let mut partials: Vec<String> = Vec::new();
    let mut cells: BTreeMap<(String, String), Cell> = BTreeMap::new();

    for (offset, line) in lines[separator_idx + 1..].iter().enumerate() {
        if !line.trim_start().starts_with('|') {
            break;
        }
        let row = split_table_row(line);
        if row.is_empty() {
            continue;
        }
        let partial = strip_inline_code(&row[0]).to_string();
        let row_lineno = separator_idx + 2 + offset;
        if partial.is_empty() {
            return Err(format!("{SPEC_REL}:{row_lineno} empty partial cell in row"));
        }
        if row.len() != templates.len() + 1 {
            return Err(format!(
                "{SPEC_REL}:{row_lineno} row `{partial}` has {got} cells, header declares {expected}",
                got = row.len() - 1,
                expected = templates.len(),
            ));
        }
        for (tpl, cell) in templates.iter().zip(row[1..].iter()) {
            cells.insert(
                (partial.clone(), tpl.clone()),
                Cell::parse(cell, row_lineno, &partial, tpl)?,
            );
        }
        partials.push(partial);
    }

    if partials.is_empty() {
        return Err(format!("{SPEC_REL} matrix has zero partial rows"));
    }
    Ok(PinningMatrix {
        cells,
        templates,
        partials,
    })
}

fn is_table_separator(line: &str) -> bool {
    let l = line.trim();
    if !l.starts_with('|') || !l.ends_with('|') {
        return false;
    }
    l.split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .all(|s| s.chars().all(|c| c == '-' || c == ':'))
}

fn split_table_row(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let stripped = trimmed
        .strip_prefix('|')
        .and_then(|s| s.strip_suffix('|'))
        .unwrap_or(trimmed);
    stripped.split('|').map(|s| s.trim().to_string()).collect()
}

fn strip_inline_code(s: &str) -> &str {
    s.trim()
        .strip_prefix('`')
        .and_then(|s| s.strip_suffix('`'))
        .unwrap_or(s.trim())
}

fn template_partials(templates_dir: &Path) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    if !templates_dir.is_dir() {
        return Err(format!("templates directory not found at {TEMPLATES_REL}"));
    }
    let partial_dir = templates_dir.join("partial");
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(templates_dir)
        .map_err(|e| format!("failed to read {TEMPLATES_REL}: {e}"))?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let direct = read_includes(&path)?;
        let mut closure: BTreeSet<String> = BTreeSet::new();
        let mut frontier: Vec<String> = direct.into_iter().collect();
        while let Some(name) = frontier.pop() {
            if !closure.insert(name.clone()) {
                continue;
            }
            let partial_path = partial_dir.join(&name);
            if partial_path.is_file() {
                for next in read_includes(&partial_path)? {
                    if !closure.contains(&next) {
                        frontier.push(next);
                    }
                }
            }
        }
        out.insert(stem, closure);
    }
    Ok(out)
}

fn read_includes(path: &Path) -> Result<BTreeSet<String>, String> {
    let body = read_to_string(path).ok_or_else(|| format!("failed to read {}", path.display()))?;
    Ok(parse_includes(&body))
}

fn parse_includes(body: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let needle = "{% include \"partial/";
    let mut rest = body;
    while let Some(idx) = rest.find(needle) {
        let after = &rest[idx + needle.len()..];
        match after.find(".md\"") {
            Some(end) => {
                let stem = &after[..end];
                if !stem.is_empty()
                    && stem
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    out.insert(format!("{stem}.md"));
                }
                rest = &after[end + 4..];
            }
            None => break,
        }
    }
    out
}
