use askama::Template;
use loom_events::identifier::{MoleculeId, SpecLabel};

/// Context for `loom todo` adding tasks to an existing molecule (anchor + siblings).
#[derive(Template)]
#[template(path = "todo_update.md", escape = "none")]
pub struct TodoUpdateContext {
    pub pinned_context: String,
    pub label: SpecLabel,
    pub spec_path: String,
    pub companion_paths: Vec<String>,
    pub spec_diff: Option<String>,
    pub existing_tasks: Option<String>,
    pub molecule_id: Option<MoleculeId>,
    pub implementation_notes: Vec<String>,
    pub scratchpad_path: String,
}
