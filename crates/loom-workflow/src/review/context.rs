use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use loom_driver::bd::Bead;
use loom_driver::identifier::{MoleculeId, ProfileName, SpecLabel};
use loom_gate::annotation::{Annotation, Tier, parse_content};
use loom_templates::review::{ReviewContext, ReviewLane, ReviewSource, TreeScopeEpic};

use crate::spec::{SpecError, target_file_path};

/// Inputs for [`build_review_context`]. Constructed once per `loom review`
/// invocation; the reviewer only runs once per molecule per gate pass.
pub struct ReviewContextInputs {
    pub label: SpecLabel,
    pub spec_path: String,
    pub pinned_context: String,
    pub companion_paths: Vec<String>,
    pub molecule_id: Option<MoleculeId>,
    pub base_commit: Option<String>,
    pub beads_summary: Option<String>,
    pub test_sources: Vec<ReviewSource>,
    pub judge_rubrics: Vec<ReviewSource>,
    /// Absolute path to `.wrapix/loom/scratch/<spec-label>/scratch.md` for
    /// this reviewer session. Embedded in the rendered prompt so the agent
    /// can write to the correct file under compaction recovery.
    pub scratchpad_path: String,
    /// Workspace-relative path to the style-rules document the reviewer
    /// must walk rule-by-rule when judging the diff.
    pub style_rules: String,
    /// Which lane(s) of the review the reviewer agent is being asked to
    /// run; gates the template sections that only one lane needs.
    pub lane: ReviewLane,
    /// At `--tree` scope, the per-spec resolved (or freshly minted) epic
    /// IDs the orchestrator threads in as bonding targets. Empty at
    /// non-`--tree` scopes.
    pub tree_scope_epics: Vec<TreeScopeEpic>,
    /// Default profile label applied to fix-up and clarify beads the
    /// reviewer mints under this spec. Threaded into the rendered
    /// `bd create --labels="…,profile:<default>"` examples so the bead's
    /// dispatch picks up a toolchain that can actually run the spec's
    /// verifiers (e.g. `profile:rust` for cargo-bound specs).
    pub default_profile: ProfileName,
}

/// Render the typed [`ReviewContext`] used by the `review.md` Askama template.
pub fn build_review_context(inputs: ReviewContextInputs) -> ReviewContext {
    ReviewContext {
        pinned_context: inputs.pinned_context,
        label: inputs.label,
        spec_path: inputs.spec_path,
        companion_paths: inputs.companion_paths,
        beads_summary: inputs.beads_summary,
        base_commit: inputs.base_commit,
        molecule_id: inputs.molecule_id,
        test_sources: inputs.test_sources,
        judge_rubrics: inputs.judge_rubrics,
        scratchpad_path: inputs.scratchpad_path,
        style_rules: inputs.style_rules,
        lane: inputs.lane,
        tree_scope_epics: inputs.tree_scope_epics,
        default_profile: inputs.default_profile,
    }
}

/// Default `profile:<name>` label for fix-up beads minted under `spec`.
///
/// Cargo-bound specs (whose `[check]` / `[test]` verifiers run cargo) need
/// `profile:rust` so the bead's dispatch container has the Rust toolchain
/// and the `/home/wrapix/.cargo` writable-dirs setup. The Nix-only specs
/// (currently `pre-commit`) stay on `profile:base` so they aren't forced
/// to pull the rust image. Unknown specs fall through to `base` — a
/// container without cargo will fail loudly on a cargo-bound verifier
/// rather than silently regressing into the old `profile:base` recurrence.
pub fn default_profile_for_spec(spec: &SpecLabel) -> ProfileName {
    match spec.as_str() {
        "harness" | "templates" | "agent" | "gate" | "llm" | "tests" => ProfileName::new("rust"),
        _ => ProfileName::new("base"),
    }
}

/// Read every file-shaped `[test]` and `[judge]` target referenced from
/// the spec into [`ReviewSource`] bundles for the reviewer prompt. Files
/// are de-duplicated by path so a script referenced from N criteria
/// appears once.
///
/// `[check]` / `[system]` targets are command strings, not files, so they
/// contribute nothing to the reviewer's source view. Rust-style `[test]`
/// targets (`crate::module::fn`) are also skipped — they resolve via the
/// language-native runner, not a file body.
///
/// Returns `(test_sources, judge_rubrics)` in the order the annotations
/// appear in the spec. Bubbles up [`SpecError::Io`] when a referenced file
/// is missing — the gate must fail loudly rather than review with a
/// truncated context.
pub fn load_review_sources(
    workspace: &Path,
    spec_path: &Path,
) -> Result<(Vec<ReviewSource>, Vec<ReviewSource>), SpecError> {
    let body = fs::read_to_string(spec_path).map_err(|source| SpecError::Io {
        path: spec_path.to_path_buf(),
        source,
    })?;
    let parsed = parse_content(spec_path, &body);
    let mut tests = Vec::new();
    let mut judges = Vec::new();
    let mut seen_test: BTreeSet<String> = BTreeSet::new();
    let mut seen_judge: BTreeSet<String> = BTreeSet::new();

    for annotation in &parsed.annotations {
        match annotation.tier {
            Tier::Test => {
                push_unique(workspace, annotation, &mut tests, &mut seen_test)?;
            }
            Tier::Judge => {
                push_unique(workspace, annotation, &mut judges, &mut seen_judge)?;
            }
            Tier::Check | Tier::System => {}
        }
    }
    Ok((tests, judges))
}

fn push_unique(
    workspace: &Path,
    annotation: &Annotation,
    out: &mut Vec<ReviewSource>,
    seen: &mut BTreeSet<String>,
) -> Result<(), SpecError> {
    let Some(rel) = target_file_path(&annotation.target) else {
        return Ok(());
    };
    let display = rel.display().to_string();
    if !seen.insert(display.clone()) {
        return Ok(());
    }
    let abs = if rel.is_absolute() {
        rel.clone()
    } else {
        workspace.join(&rel)
    };
    let body = fs::read_to_string(&abs).map_err(|source| SpecError::Io { path: abs, source })?;
    out.push(ReviewSource {
        path: display,
        body,
    });
    Ok(())
}

/// Render `BEADS_SUMMARY` for the reviewer prompt. One line per bead in the
/// molecule: `- <id>: <title> [<status>]`. Returns `None` when `beads` is
/// empty so the template can render the em-dash placeholder; the reviewer
/// is expected to read full descriptions on demand via `bd show`.
pub fn beads_summary(beads: &[Bead]) -> Option<String> {
    if beads.is_empty() {
        return None;
    }
    let mut s = String::new();
    for bead in beads {
        s.push_str(&format!(
            "- {}: {} [{}]\n",
            bead.id, bead.title, bead.status
        ));
    }
    while s.ends_with('\n') {
        s.pop();
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use askama::Template;
    use loom_driver::identifier::BeadId;

    fn b(id: &str, title: &str, status: &str) -> Bead {
        Bead {
            id: BeadId::new(id).expect("valid bead id"),
            title: title.into(),
            description: String::new(),
            status: status.into(),
            priority: 2,
            issue_type: "task".into(),
            labels: vec![],
            parent: None,
            metadata: Default::default(),
            notes: None,
        }
    }

    fn inputs() -> ReviewContextInputs {
        ReviewContextInputs {
            label: SpecLabel::new("harness"),
            spec_path: "specs/harness.md".into(),
            pinned_context: "PIN".into(),
            companion_paths: vec![],
            molecule_id: Some(MoleculeId::new("wx-3hhwq")),
            base_commit: Some("abc123".into()),
            beads_summary: Some("- wx-1: First [open]".into()),
            test_sources: vec![],
            judge_rubrics: vec![],
            scratchpad_path: "/workspace/.wrapix/loom/scratch/harness/scratch.md".into(),
            style_rules: "docs/style-rules.md".into(),
            lane: ReviewLane::Both,
            tree_scope_epics: Vec::new(),
            default_profile: default_profile_for_spec(&SpecLabel::new("harness")),
        }
    }

    #[test]
    fn beads_summary_returns_none_for_empty_input() {
        assert!(beads_summary(&[]).is_none());
    }

    #[test]
    fn beads_summary_lines_carry_id_title_status() {
        let beads = vec![b("wx-1", "Plan", "open"), b("wx-2", "Run", "in_progress")];
        let s = beads_summary(&beads).expect("beads present");
        assert!(s.contains("wx-1: Plan [open]"));
        assert!(s.contains("wx-2: Run [in_progress]"));
        assert!(!s.ends_with('\n'), "trailing newline trimmed");
    }

    #[test]
    fn rendered_template_includes_label_and_base_commit() {
        let ctx = build_review_context(inputs());
        let body = ctx.render().expect("render");
        assert!(body.contains("harness"), "{body}");
        assert!(body.contains("abc123"), "{body}");
        assert!(body.contains("wx-3hhwq"), "{body}");
    }

    #[test]
    fn rendered_template_renders_em_dash_for_missing_base_commit() {
        let mut i = inputs();
        i.base_commit = None;
        let ctx = build_review_context(i);
        let body = ctx.render().expect("render");
        // The review.md template uses an em-dash placeholder for the None
        // arm of base_commit / molecule_id.
        assert!(body.contains("Base commit**: —"), "{body}");
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, body).expect("write");
    }

    #[test]
    fn load_review_sources_reads_test_and_judge_files_from_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path();
        write(
            &ws.join("specs/alpha.md"),
            "## Success Criteria\n\n\
             - thing one [test](tests/alpha.sh#test_one)\n\
             - thing two [judge](tests/judges/alpha.sh#judge_two)\n",
        );
        write(&ws.join("tests/alpha.sh"), "TEST_BODY\n");
        write(&ws.join("tests/judges/alpha.sh"), "JUDGE_BODY\n");

        let (tests, judge) = load_review_sources(ws, &ws.join("specs/alpha.md")).expect("load ok");

        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].path, "tests/alpha.sh");
        assert_eq!(tests[0].body, "TEST_BODY\n");

        assert_eq!(judge.len(), 1);
        assert_eq!(judge[0].path, "tests/judges/alpha.sh");
        assert_eq!(judge[0].body, "JUDGE_BODY\n");
    }

    #[test]
    fn load_review_sources_deduplicates_files_referenced_from_many_criteria() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path();
        write(
            &ws.join("specs/alpha.md"),
            "## Success Criteria\n\n\
             - one [test](tests/alpha.sh#test_one)\n\
             - two [test](tests/alpha.sh#test_two)\n\
             - three [test](tests/alpha.sh#test_three)\n",
        );
        write(&ws.join("tests/alpha.sh"), "shared body\n");

        let (tests, judge) = load_review_sources(ws, &ws.join("specs/alpha.md")).expect("load ok");

        assert_eq!(tests.len(), 1, "shared file collapsed to one entry");
        assert!(judge.is_empty());
    }

    #[test]
    fn load_review_sources_errors_when_referenced_file_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path();
        write(
            &ws.join("specs/alpha.md"),
            "## Success Criteria\n\n\
             - one [test](tests/missing.sh#test_one)\n",
        );

        let err = load_review_sources(ws, &ws.join("specs/alpha.md"))
            .expect_err("missing file must surface as error");
        assert!(
            matches!(err, SpecError::Io { .. }),
            "expected SpecError::Io, got {err:?}",
        );
    }

    #[test]
    fn load_review_sources_skips_unannotated_criteria() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path();
        write(
            &ws.join("specs/alpha.md"),
            "## Success Criteria\n\n\
             - no annotation here\n\
             - but this one has [test](tests/a.sh#t)\n",
        );
        write(&ws.join("tests/a.sh"), "body\n");

        let (tests, _) = load_review_sources(ws, &ws.join("specs/alpha.md")).expect("load ok");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].path, "tests/a.sh");
    }

    #[test]
    fn load_review_sources_skips_check_and_system_command_strings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path();
        write(
            &ws.join("specs/alpha.md"),
            "## Success Criteria\n\n\
             - cmd [check](cargo run -p w -- a)\n\
             - sys [system](nix run .#smoke)\n\
             - file [test](tests/a.sh#t)\n",
        );
        write(&ws.join("tests/a.sh"), "body\n");

        let (tests, _) = load_review_sources(ws, &ws.join("specs/alpha.md")).expect("load ok");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].path, "tests/a.sh");
    }

    #[test]
    fn load_review_sources_skips_language_native_test_targets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path();
        write(
            &ws.join("specs/alpha.md"),
            "## Success Criteria\n\n\
             - rust [test](crate::module::test_fn)\n",
        );

        let (tests, _) = load_review_sources(ws, &ws.join("specs/alpha.md")).expect("load ok");
        assert!(tests.is_empty());
    }

    #[test]
    fn rendered_template_includes_test_and_judge_bodies() {
        let mut i = inputs();
        i.test_sources = vec![ReviewSource {
            path: "tests/alpha.sh".into(),
            body: "TEST_BODY_MARKER".into(),
        }];
        i.judge_rubrics = vec![ReviewSource {
            path: "tests/judges/alpha.sh".into(),
            body: "JUDGE_BODY_MARKER".into(),
        }];
        let ctx = build_review_context(i);
        let body = ctx.render().expect("render");
        assert!(body.contains("tests/alpha.sh"), "{body}");
        assert!(body.contains("TEST_BODY_MARKER"), "{body}");
        assert!(body.contains("tests/judges/alpha.sh"), "{body}");
        assert!(body.contains("JUDGE_BODY_MARKER"), "{body}");
    }

    #[test]
    fn default_profile_for_spec_returns_rust_for_cargo_bound_specs() {
        for label in ["harness", "templates", "agent", "gate", "llm", "tests"] {
            assert_eq!(
                default_profile_for_spec(&SpecLabel::new(label)).as_str(),
                "rust",
                "{label} should default to profile:rust",
            );
        }
    }

    #[test]
    fn default_profile_for_spec_returns_base_for_nix_only_specs() {
        assert_eq!(
            default_profile_for_spec(&SpecLabel::new("pre-commit")).as_str(),
            "base",
        );
        assert_eq!(
            default_profile_for_spec(&SpecLabel::new("unknown-spec")).as_str(),
            "base",
        );
    }

    #[test]
    fn rendered_template_inlines_default_profile_in_bd_create_examples() {
        let mut i = inputs();
        i.label = SpecLabel::new("harness");
        i.default_profile = default_profile_for_spec(&SpecLabel::new("harness"));
        let body = build_review_context(i).render().expect("render");
        assert!(
            body.contains("spec:harness,profile:rust"),
            "harness fix-up minting block must default to profile:rust: {body}",
        );
        assert!(
            body.contains("spec:harness,loom:clarify,profile:rust"),
            "harness clarify minting block must default to profile:rust: {body}",
        );
        assert!(
            !body.contains("spec:harness,profile:base")
                && !body.contains("spec:harness,loom:clarify,profile:base"),
            "harness review prompt must not mint fix-ups under profile:base by default: {body}",
        );

        let mut j = inputs();
        j.label = SpecLabel::new("pre-commit");
        j.default_profile = default_profile_for_spec(&SpecLabel::new("pre-commit"));
        let body = build_review_context(j).render().expect("render");
        assert!(
            body.contains("spec:pre-commit,profile:base"),
            "pre-commit fix-up minting block must default to profile:base: {body}",
        );
        assert!(
            !body.contains("spec:pre-commit,profile:rust"),
            "pre-commit review prompt must not mint fix-ups under profile:rust by default: {body}",
        );
    }

    /// Once `--parent <epic>` is supplied to `bd create`, the redundant
    /// `bd mol bond <new-id> <epic>` retraces the parent edge and trips
    /// bd's cycle detector. The review template must not direct the
    /// reviewer to emit that second call after `--parent`-bonded creates.
    #[test]
    fn rendered_template_omits_redundant_bd_mol_bond_after_bd_create_parent() {
        let body = build_review_context(inputs()).render().expect("render");
        for example_block in body.split("```bash").skip(1) {
            let block = example_block
                .split_once("```")
                .map(|(b, _)| b)
                .unwrap_or(example_block);
            if block.contains("bd create") && block.contains("--parent") {
                assert!(
                    !block.contains("bd mol bond"),
                    "block contains `bd create --parent` and stray `bd mol bond`: {block}",
                );
            }
        }
    }

    #[test]
    fn rendered_template_renders_em_dash_when_no_review_sources() {
        let ctx = build_review_context(inputs());
        let body = ctx.render().expect("render");
        assert!(
            body.contains("## Deterministic-Verifier Sources"),
            "test section heading present: {body}",
        );
        assert!(
            body.contains("## `[judge]` Rubrics"),
            "judge section heading present: {body}",
        );
        assert!(
            body.contains("re-reading them from disk.\n\n—"),
            "test em-dash placeholder when empty: {body}",
        );
        assert!(
            body.contains("read the per-criterion rubric.\n\n—"),
            "judge em-dash placeholder when empty: {body}",
        );
    }
}
