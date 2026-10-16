//! Explicit acceptance candidates for isolated workflow fixtures, never resolved authority.

use loom_events::identifier::SpecLabel;
use loom_protocol::acceptance::{Candidates, Reference, References};
use loom_protocol::criterion::CriterionId;
use std::fmt::Write as _;
use std::path::Path;

pub const REQUIREMENT: &str = "The fixture worker preserves its dispatched base, source state and recovery context while routing outcomes.";

/// Encode candidates derived from the fixture contract.
///
/// # Errors
/// Rejects malformed fixture labels or serialization failures.
pub fn metadata(label: &str) -> std::io::Result<serde_json::Value> {
    let label = SpecLabel::new(label).map_err(std::io::Error::other)?;
    let id = CriterionId::for_spec_text(&label, REQUIREMENT);
    Ok(serde_json::Value::String(
        serde_json::to_string(&Candidates::Implementation {
            criteria: References::new(vec![Reference::new(label, id)])
                .map_err(std::io::Error::other)?,
        })
        .map_err(std::io::Error::other)?,
    ))
}

/// Write the explicit fixture requirement and its package index entry.
///
/// # Errors
/// Returns fixture filesystem failures.
pub fn write(workspace: &Path, label: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(workspace.join("specs"))?;
    std::fs::create_dir_all(workspace.join("docs"))?;
    std::fs::write(
        workspace.join(format!("specs/{label}.md")),
        format!(
            "# {label}\n\n## Success Criteria\n\n- {REQUIREMENT} [test?](fixture_worker_contract)\n"
        ),
    )?;
    let index = workspace.join("docs/README.md");
    let mut text = match std::fs::read_to_string(&index) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let link = format!("../specs/{label}.md");
    if !text.contains(&link) {
        writeln!(text, "\n- [{label}]({link})").map_err(std::io::Error::other)?;
        std::fs::write(index, text)?;
    }
    Ok(())
}

/// Establish the fixture contract as the common baseline, before any worker edits.
/// All repositories here are disposable test fixtures, never the live integration workspace.
///
/// # Errors
/// Returns fixture filesystem or Git failures.
pub fn seed_repository(workspace: &Path) -> std::io::Result<()> {
    seed_repository_for(workspace, "harness")
}

/// Establish a common fixture baseline for the selected package.
///
/// # Errors
/// Returns fixture filesystem or Git failures.
pub fn seed_repository_for(workspace: &Path, label: &str) -> std::io::Result<()> {
    write(workspace, label)?;
    loom_driver::git::seed_test_baseline(workspace, &["specs", "docs/README.md"])
        .map_err(std::io::Error::other)?;
    Ok(())
}
