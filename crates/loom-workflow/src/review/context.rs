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

    // Per `specs/gate.md`: `[test]` / `[judge]` targets resolve relative
    // to the spec file's own directory, not the workspace root, so
    // `../tests/judges/x.sh` from `specs/foo.md` lands at
    // `<workspace>/tests/judges/x.sh`.
    let spec_dir = spec_path.parent().unwrap_or(workspace);
    for annotation in &parsed.annotations {
        match annotation.tier {
            Tier::Test => {
                push_unique(workspace, spec_dir, annotation, &mut tests, &mut seen_test)?;
            }
            Tier::Judge => {
                push_unique(
                    workspace,
                    spec_dir,
                    annotation,
                    &mut judges,
                    &mut seen_judge,
                )?;
            }
            Tier::Check | Tier::System => {}
        }
    }
    Ok((tests, judges))
}

fn push_unique(
    workspace: &Path,
    spec_dir: &Path,
    annotation: &Annotation,
    out: &mut Vec<ReviewSource>,
    seen: &mut BTreeSet<String>,
) -> Result<(), SpecError> {
    let Some(rel) = target_file_path(&annotation.target) else {
        return Ok(());
    };
    let abs = if rel.is_absolute() {
        rel.clone()
    } else {
        normalize(&spec_dir.join(&rel))
    };
    // Display path is workspace-relative when the resolved file lives under
    // the workspace; otherwise (file outside workspace — `[judge]` shared
    // across repos via symlink, etc.) fall back to the absolute path.
    let display = abs
        .strip_prefix(workspace)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| abs.display().to_string());
    if !seen.insert(display.clone()) {
        return Ok(());
    }
    let body = fs::read_to_string(&abs).map_err(|source| SpecError::Io { path: abs, source })?;
    out.push(ReviewSource {
        path: display,
        body,
    });
    Ok(())
}

/// Fold `..` and `.` components into a canonical form *without* touching
/// the filesystem. Used in place of `Path::canonicalize` so the resolution
/// works against constructed paths (tests build paths via `tempdir.join(...)`
/// that haven't been created yet) and so missing files surface through the
/// caller's own `fs::read_to_string` error rather than a separate canonicalize
/// failure.
fn normalize(path: &Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
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
            molecule_id: Some(MoleculeId::new("lm-3hhwq")),
            base_commit: Some("abc123".into()),
            beads_summary: Some("- lm-1: First [open]".into()),
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
        let beads = vec![b("lm-1", "Plan", "open"), b("lm-2", "Run", "in_progress")];
        let s = beads_summary(&beads).expect("beads present");
        assert!(s.contains("lm-1: Plan [open]"));
        assert!(s.contains("lm-2: Run [in_progress]"));
        assert!(!s.ends_with('\n'), "trailing newline trimmed");
    }

    #[test]
    fn rendered_template_includes_label_and_base_commit() {
        let ctx = build_review_context(inputs());
        let body = ctx.render().expect("render");
        assert!(body.contains("harness"), "{body}");
        assert!(body.contains("abc123"), "{body}");
        assert!(body.contains("lm-3hhwq"), "{body}");
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
             - thing one [test](../tests/alpha.sh#test_one)\n\
             - thing two [judge](../tests/judges/alpha.sh#judge_two)\n",
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
             - one [test](../tests/alpha.sh#test_one)\n\
             - two [test](../tests/alpha.sh#test_two)\n\
             - three [test](../tests/alpha.sh#test_three)\n",
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
             - one [test](../tests/missing.sh#test_one)\n",
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
             - but this one has [test](../tests/a.sh#t)\n",
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
             - file [test](../tests/a.sh#t)\n",
        );
        write(&ws.join("tests/a.sh"), "body\n");

        let (tests, _) = load_review_sources(ws, &ws.join("specs/alpha.md")).expect("load ok");
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].path, "tests/a.sh");
    }

    /// Regression: per `specs/gate.md`, `[test]` / `[judge]` targets
    /// resolve relative to the **spec file's own directory**, not the
    /// workspace root. A `[judge](../tests/judges/x.sh)` annotation in
    /// `specs/foo.md` must read `<ws>/tests/judges/x.sh`. The earlier
    /// implementation joined the relative target with `workspace`
    /// directly, producing `<ws>/../tests/judges/x.sh` — outside the
    /// workspace entirely. Observed 2026-05-28 in production
    /// (`loom gate review` exit 1, `No such file or directory` for
    /// `<workspace>/../tests/judges/loom.sh`).
    #[test]
    fn load_review_sources_resolves_dotdot_targets_against_spec_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path();
        write(
            &ws.join("specs/alpha.md"),
            "## Success Criteria\n\n\
             - thing [judge](../tests/judges/alpha.sh#judge_one)\n",
        );
        write(&ws.join("tests/judges/alpha.sh"), "JUDGE_BODY\n");

        let (_, judges) = load_review_sources(ws, &ws.join("specs/alpha.md")).expect("load ok");

        assert_eq!(judges.len(), 1);
        // Display path is workspace-relative — the `..` is folded away.
        assert_eq!(judges[0].path, "tests/judges/alpha.sh");
        assert_eq!(judges[0].body, "JUDGE_BODY\n");
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

    /// The review phase is inspection-only: the rendered prompt must
    /// not direct the reviewer to invoke bd mutations — the driver-side
    /// mint consumes the streamed `LOOM_FINDING:` records and performs
    /// the bd writes itself. The `default_profile` field stays on
    /// `ReviewContext` (consumed by the driver-side mint) but the
    /// rendered prompt no longer references `profile:<name>` strings.
    #[test]
    fn rendered_template_omits_bd_mutation_instructions_and_default_profile() {
        for label in ["harness", "pre-commit"] {
            let mut i = inputs();
            i.label = SpecLabel::new(label);
            i.default_profile = default_profile_for_spec(&SpecLabel::new(label));
            let body = build_review_context(i).render().expect("render");
            // Negative prose references like "do NOT invoke `bd create`"
            // are fine — what must be absent is *instruction* shapes
            // (bash code blocks, profile labels rendered from
            // `default_profile`).
            assert!(
                !body.contains("```bash"),
                "{label}: review prompt must contain no bash code blocks — every legacy bd-write example must be gone: {body}",
            );
            for forbidden in ["profile:rust", "profile:base"] {
                assert!(
                    !body.contains(forbidden),
                    "{label}: review prompt must not render `{forbidden}` — default_profile is consumed by the driver, not the template body: {body}",
                );
            }
            for forbidden_heading in [
                "Authorization — Bead Mutations",
                "Recovery Epic Resolution",
                "Handling Each Clash",
                "## Creating Fix-Up Beads",
                "## Flag Emission Schema",
            ] {
                assert!(
                    !body.contains(forbidden_heading),
                    "{label}: review prompt must not contain `{forbidden_heading}` heading: {body}",
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
