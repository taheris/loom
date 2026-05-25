use askama::Template;
use loom_events::identifier::SpecLabel;

/// Context for `loom plan -n <label>` (new-spec interview).
#[derive(Template)]
#[template(path = "plan_new.md", escape = "none")]
pub struct PlanNewContext {
    pub pinned_context: String,
    pub label: SpecLabel,
    pub spec_path: String,
    pub scratchpad_path: String,
    /// Workspace-relative path to the spec-authoring conventions document
    /// (`docs/spec-conventions.md` by default). Pinned in the planning
    /// prompt so the agent reads the conventions before authoring the spec.
    pub spec_conventions: String,
}
