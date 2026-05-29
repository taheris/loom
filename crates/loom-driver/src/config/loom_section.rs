use serde::Deserialize;

/// `[loom]` block — workspace-level loom knobs that don't fit any of the
/// phase-, agent-, runner-, or component-specific sections. See
/// `specs/harness.md` § Configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LoomTopConfig {
    /// Name of the branch the loom workspace
    /// (`.wrapix/loom/integration/`) has checked out and into which bead
    /// branches rebase + fast-forward. Pushed to
    /// `origin/<integration_branch>` from the gate. Default `main`.
    #[serde(default = "default_integration_branch")]
    pub integration_branch: String,
}

impl Default for LoomTopConfig {
    fn default() -> Self {
        Self {
            integration_branch: default_integration_branch(),
        }
    }
}

pub fn default_integration_branch() -> String {
    "main".to_string()
}
