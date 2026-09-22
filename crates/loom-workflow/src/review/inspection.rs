//! Explicit inspection selection is context, not permission to publish.
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use loom_driver::identifier::BeadId;
use loom_gate::annotation::{Tier, parse};
use loom_gate::scope::Resolved;
use loom_templates::review::{ReviewLane, ReviewSource};

use super::context::load_review_sources_for_annotations;
use super::error::ReviewError;

pub(super) struct Materials {
    pub test_sources: Vec<ReviewSource>,
    pub judge_rubrics: Vec<ReviewSource>,
    pub companion_paths: Vec<String>,
    pub pinned_context: String,
}

pub(super) fn load(
    workspace: &Path,
    scope: &Resolved,
    lane: ReviewLane,
    bead: Option<&BeadId>,
) -> Result<Materials, ReviewError> {
    let parsed = parse(&workspace.join("specs"))?;
    let mut annotations = parsed.annotations;
    if let Some(target) = scope.target() {
        annotations
            .retain(|annotation| annotation.tier == Tier::Judge && annotation.target == target);
        if annotations.is_empty() {
            return Err(ReviewError::UnknownJudgeTarget(target.to_owned()));
        }
    }
    let spec_paths: BTreeSet<_> = annotations
        .iter()
        .map(|annotation| &annotation.source_spec)
        .chain(
            parsed
                .criteria
                .iter()
                .filter(|_| scope.target().is_none())
                .map(|criterion| &criterion.source_spec),
        )
        .collect();
    let companion_paths = spec_paths
        .into_iter()
        .map(|path| display_path(workspace, path))
        .collect();
    let (test_sources, judge_rubrics) =
        load_review_sources_for_annotations(workspace, &annotations, lane)?;
    let mut pinned_context = scope_pin(workspace, scope);
    if lane.includes_judge() {
        pinned_context.push_str("\nSelected judge annotations (exact targets; evaluate only these selectors, not sibling functions in the same source file):\n");
        let selected: BTreeSet<_> = annotations
            .iter()
            .filter(|annotation| annotation.tier == Tier::Judge)
            .map(|annotation| {
                format!(
                    "- {:?} in {}\n",
                    annotation.target,
                    display_path(workspace, &annotation.source_spec)
                )
            })
            .collect();
        for target in selected {
            pinned_context.push_str(&target);
        }
    }
    if let Some(bead) = bead {
        // Writing to a String is infallible.
        let _ = writeln!(
            pinned_context,
            "\nIntent/context bead: `{bead}`. Metadata only: it does not narrow the inspection scope or authorize state transitions."
        );
    }
    Ok(Materials {
        test_sources,
        judge_rubrics,
        companion_paths,
        pinned_context,
    })
}

fn scope_pin(workspace: &Path, scope: &Resolved) -> String {
    if let Some(target) = scope.target() {
        return format!(
            "Current dispatch scope: `--target {target}`. Evaluate only this exact annotation target. Do not infer a diff range or run the rubric walk.\n"
        );
    }
    if let Some(range) = scope.diff() {
        return format!(
            "Current dispatch scope: `--diff {range}`. Use that exact range for diff/log evidence. Context labels do not filter sibling-spec obligations.\n"
        );
    }
    if let Some(files) = scope.files() {
        let files: Vec<_> = files
            .iter()
            .map(|path| display_path(workspace, path))
            .collect();
        return format!(
            "Current dispatch scope: `--files` {files:?}. Judge annotations are not filtered by test-input intersection. Do not infer a diff range.\n"
        );
    }
    "Current dispatch scope: `--tree`. The input set is every file in the workspace; do not narrow review to a base-to-HEAD diff or context label.\n".to_owned()
}

fn display_path(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .display()
        .to_string()
}
