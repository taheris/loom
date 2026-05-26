//! Architectural: `todo_new.md` directs the agent to create the molecule
//! epic before any criterion-by-criterion gap-analysis step, so the
//! `LOOM_CLARIFY`-on-epic fallback always has a valid target if the
//! audit cannot complete mid-decomposition. See `specs/templates.md`
//! § Decomposition Discipline (epic-first-always ordering).

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "todo_new_creates_epic_before_decomposition — todo_new.md must create the molecule epic before any criterion-by-criterion gap-analysis step";

const TEMPLATE_REL: &str = "crates/loom-templates/templates/todo_new.md";
const INSTRUCTIONS_HEADING: &str = "## Instructions";
const EPIC_CREATE_NEEDLE: &str = "bd create --type=epic";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let path = root.join(TEMPLATE_REL);
    let Some(body) = read_to_string(&path) else {
        return Verdict {
            pass: false,
            evidence: format!("{TEMPLATE_REL}:1 template file not found\n{RULE}"),
        };
    };
    let lines: Vec<&str> = body.lines().collect();

    let Some(instructions_idx) = lines
        .iter()
        .position(|l| l.trim_start().starts_with(INSTRUCTIONS_HEADING))
    else {
        return Verdict {
            pass: false,
            evidence: format!("{TEMPLATE_REL}:1 no `## Instructions` heading found\n{RULE}"),
        };
    };

    let after = &lines[instructions_idx + 1..];

    let Some(epic_offset) = after.iter().position(|l| l.contains(EPIC_CREATE_NEEDLE)) else {
        return Verdict {
            pass: false,
            evidence: format!(
                "{TEMPLATE_REL}:{} no `bd create --type=epic` invocation found under `## Instructions`\n{RULE}",
                instructions_idx + 1,
            ),
        };
    };

    let Some(gap_offset) = after.iter().position(|l| is_gap_analysis_step(l)) else {
        return Verdict {
            pass: false,
            evidence: format!(
                "{TEMPLATE_REL}:{} no gap-analysis instruction (numbered step or heading mentioning criterion_status / criteria / gap analysis) found under `## Instructions`\n{RULE}",
                instructions_idx + 1,
            ),
        };
    };

    let epic_line = instructions_idx + epic_offset + 2;
    let gap_line = instructions_idx + gap_offset + 2;

    if epic_line >= gap_line {
        return Verdict {
            pass: false,
            evidence: format!(
                "{TEMPLATE_REL}:{epic_line} epic creation appears at or after the gap-analysis step at line {gap_line} — `todo_new.md` must create the molecule epic before any criterion-by-criterion gap analysis so the `LOOM_CLARIFY`-on-epic fallback has a valid target mid-decomposition\n{RULE}",
            ),
        };
    }

    verdict_from(RULE, Vec::new())
}

fn is_gap_analysis_step(line: &str) -> bool {
    let trimmed = line.trim_start();
    let is_numbered_step =
        trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) && trimmed.contains(". ");
    let is_heading = trimmed.starts_with('#');

    if !is_numbered_step && !is_heading {
        return false;
    }

    let lower = line.to_lowercase();
    lower.contains("criterion_status")
        || lower.contains("criterion status")
        || lower.contains("criteria")
        || lower.contains("gap analysis")
        || lower.contains("gap-analysis")
}
