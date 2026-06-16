//! Architectural: `todo.md` receives a driver-created work epic and must not
//! instruct the decomposition agent to create the batch epic itself.

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "todo_template_uses_driver_created_work_epic — todo.md uses the injected work_epic and does not create an epic itself";
const TEMPLATE_REL: &str = "crates/loom-templates/templates/todo.md";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let Some(body) = read_to_string(&root.join(TEMPLATE_REL)) else {
        return Verdict {
            pass: false,
            evidence: format!("{TEMPLATE_REL}:1 template not found\n{RULE}"),
        };
    };

    let mut violations = Vec::new();
    let lower = body.to_lowercase();
    if !lower.contains("driver-created work epic") && !lower.contains("driver has already created")
    {
        violations.push(format!(
            "{TEMPLATE_REL}:1 missing driver-created work epic framing"
        ));
    }
    if !body.contains("{{ work_epic }}") {
        violations.push(format!(
            "{TEMPLATE_REL}:1 does not render `{{{{ work_epic }}}}`"
        ));
    }
    if !body.contains("--parent=\"{{ work_epic }}\"") {
        violations.push(format!(
            "{TEMPLATE_REL}:1 bead creation example does not parent tasks to `{{{{ work_epic }}}}`"
        ));
    }
    if lower.contains("bd create --type=epic") || lower.contains("create the epic") {
        violations.push(format!(
            "{TEMPLATE_REL}:1 instructs the agent to create an epic instead of using the injected work epic"
        ));
    }
    verdict_from(RULE, violations)
}
