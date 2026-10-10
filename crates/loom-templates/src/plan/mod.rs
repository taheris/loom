use askama::Template;
use loom_events::identifier::SpecLabel;

use crate::SkillIndexMarkdown;

/// Context for `loom plan [SPEC_LABEL ...]`.
#[derive(Template)]
#[template(path = "plan.md", escape = "none")]
pub struct PlanContext {
    pub pinned_context: String,
    pub anchor_labels: Vec<SpecLabel>,
    pub spec_index: String,
    pub companion_paths: Vec<String>,
    pub scratchpad_path: String,
    pub spec_conventions: String,
    pub skill_index: SkillIndexMarkdown,
}

impl PlanContext {
    pub fn output_contract(&self) -> String {
        crate::output::contract(loom_protocol::output::Phase::Plan)
    }
}
