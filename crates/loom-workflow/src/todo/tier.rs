use std::path::{Path, PathBuf};

use loom_driver::identifier::{MoleculeId, SpecLabel};

use super::error::TodoError;

/// Pluggable git surface used by [`compute_spec_diff`]. Real callers wire
/// this to the `GitClient` in `loom-driver`; tests substitute a fake.
///
/// Operations are sync because the driver runs them under a single tokio
/// task — concurrency comes from the agent session, not from these probes.
pub trait GitDiffSource {
    /// `git rev-parse --verify <rev>^{commit}` — returns Ok(()) iff the rev
    /// resolves to a commit object.
    fn rev_exists(&self, rev: &str) -> bool;

    /// `git merge-base --is-ancestor <ancestor> HEAD` — returns true iff the
    /// rev is an ancestor of the current `HEAD`.
    fn is_ancestor_of_head(&self, rev: &str) -> bool;

    /// `git diff <base> HEAD --name-only -- specs/` — repo-relative spec
    /// file paths changed since `base`.
    fn changed_spec_files(&self, base: &str) -> Vec<PathBuf>;

    /// `git diff <base> HEAD -- <spec_path>` — unified diff of one spec
    /// file. Empty string when there is no diff.
    fn diff_spec(&self, base: &str, spec_path: &Path) -> String;
}

/// Snapshot of the anchor's molecule row from the state DB. `None` means no
/// molecule has been recorded yet (tiers 3/4 territory).
#[derive(Debug, Clone)]
pub struct MoleculeState {
    pub id: MoleculeId,
    pub base_commit: Option<String>,
}

/// Inputs to [`compute_spec_diff`] — every field the four-tier decision tree
/// reads. Built once in the driver from the state DB and CLI flags, then
/// handed in as a value type.
pub struct TierInputs<'a> {
    /// Anchor spec label (resolved via `--spec` flag or `current_spec`).
    pub label: &'a SpecLabel,

    /// Anchor spec path (typically `specs/<label>.md`). Hidden specs are not
    /// supported in loom (see specs/harness.md "Out of Scope").
    pub spec_path: &'a Path,

    /// Anchor's molecule row from the state DB, if any.
    pub molecule: Option<MoleculeState>,

    /// Override the anchor's `base_commit` for this run. Applies to the
    /// candidate-set diff and to the anchor's own per-spec diff inside the
    /// fan-out loop. Sibling specs retain their own `base_commit` values.
    /// Errors if `commit` is not a reachable commit.
    pub since: Option<&'a str>,

    /// Per-spec base-commit lookup: returns the recorded `base_commit` for
    /// each candidate sibling spec encountered during fan-out, or `None` if
    /// the sibling is not yet tracked.
    pub sibling_base: &'a dyn Fn(&SpecLabel) -> Option<String>,
}

/// Output of [`compute_spec_diff`] — the tier the agent should run under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TierDecision {
    /// Tier 1 — git diff against the molecule's `base_commit` with per-spec
    /// fan-out. `candidates` may be empty (no spec changes since
    /// `base_commit`), in which case the driver exits early with the "no
    /// spec changes" message.
    Diff {
        anchor_base: String,
        candidates: Vec<DiffCandidate>,
    },

    /// Tier 2 — molecule exists but has no usable `base_commit`. The agent
    /// must compare the spec against the existing task descriptions, which
    /// the caller fetches via `bd list --parent <molecule>`.
    Tasks { molecule: MoleculeId },

    /// Tier 4 — fresh decomposition.
    New,
}

/// One per-spec candidate in a tier-1 fan-out. Each carries the diff that
/// was actually computed against the *effective* base for that spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffCandidate {
    pub label: SpecLabel,
    pub spec_path: PathBuf,
    pub effective_base: String,
    pub diff: String,
}

/// Pure four-tier detection. Parameterised on a [`GitDiffSource`] so tests
/// can drive every branch without a real repo.
///
/// The function does not read the state DB or call `bd` — those calls
/// happen in the driver, which threads the resolved values in via
/// [`TierInputs`]. The result is a pure decision the driver then acts on.
pub fn compute_spec_diff(
    git: &dyn GitDiffSource,
    inputs: &TierInputs<'_>,
) -> Result<TierDecision, TodoError> {
    // --since override: validate up front before consulting state.
    let since = inputs.since;
    if let Some(commit) = since
        && !git.rev_exists(commit)
    {
        return Err(TodoError::InvalidSinceCommit {
            commit: commit.to_string(),
        });
    }

    let molecule_ref = inputs.molecule.as_ref();
    let stored_base = molecule_ref.and_then(|m| m.base_commit.clone());
    let anchor_base = since.map(str::to_owned).or(stored_base);

    if let Some(base) = anchor_base.clone()
        && git.rev_exists(&base)
        && git.is_ancestor_of_head(&base)
    {
        let candidates = build_fanout(git, inputs, &base);
        return Ok(TierDecision::Diff {
            anchor_base: base,
            candidates,
        });
    }

    // `--since` always demands tier 1; orphaned/missing override is a hard
    // error rather than a silent fallback.
    if since.is_some() {
        return Err(TodoError::InvalidSinceCommit {
            commit: since.unwrap_or_default().to_string(),
        });
    }

    // No usable base_commit. Tier 2 if a molecule exists; else tier 4.
    if let Some(state) = molecule_ref {
        return Ok(TierDecision::Tasks {
            molecule: state.id.clone(),
        });
    }

    Ok(TierDecision::New)
}

fn build_fanout(
    git: &dyn GitDiffSource,
    inputs: &TierInputs<'_>,
    anchor_base: &str,
) -> Vec<DiffCandidate> {
    let candidates = git.changed_spec_files(anchor_base);
    let mut out = Vec::with_capacity(candidates.len());
    for cand_path in candidates {
        let cand_label = match cand_path.file_stem().and_then(|s| s.to_str()) {
            Some(stem) => SpecLabel::new(stem.to_string()),
            None => continue,
        };

        let effective_base = effective_base_for(git, inputs, anchor_base, &cand_label, &cand_path);
        let diff = git.diff_spec(&effective_base, &cand_path);
        if diff.is_empty() {
            continue;
        }
        out.push(DiffCandidate {
            label: cand_label,
            spec_path: cand_path,
            effective_base,
            diff,
        });
    }
    out
}

fn effective_base_for(
    git: &dyn GitDiffSource,
    inputs: &TierInputs<'_>,
    anchor_base: &str,
    cand_label: &SpecLabel,
    cand_path: &Path,
) -> String {
    // Anchor + --since: the override applies to the anchor's per-spec diff
    // too, not just the candidate-set computation.
    if let Some(since) = inputs.since
        && cand_path == inputs.spec_path
    {
        return since.to_string();
    }

    let candidate_base = if cand_label == inputs.label {
        // Anchor without --since: use the anchor's own stored base.
        inputs.molecule.as_ref().and_then(|m| m.base_commit.clone())
    } else {
        (inputs.sibling_base)(cand_label)
    };

    match candidate_base {
        None => anchor_base.to_string(),
        Some(b) if !git.rev_exists(&b) => anchor_base.to_string(),
        Some(b) if !git.is_ancestor_of_head(&b) => anchor_base.to_string(),
        Some(b) => b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// In-memory fake covering every `GitDiffSource` operation used by
    /// `compute_spec_diff`. Records nothing — tests assert on the returned
    /// `TierDecision` only.
    struct FakeGit {
        commits: Vec<String>,
        ancestors_of_head: Vec<String>,
        candidate_sets: HashMap<String, Vec<PathBuf>>,
        // (base, path) -> diff contents
        diffs: HashMap<(String, PathBuf), String>,
        diff_calls: RefCell<Vec<(String, PathBuf)>>,
    }

    impl FakeGit {
        fn new() -> Self {
            Self {
                commits: Vec::new(),
                ancestors_of_head: Vec::new(),
                candidate_sets: HashMap::new(),
                diffs: HashMap::new(),
                diff_calls: RefCell::new(Vec::new()),
            }
        }

        fn with_commit(mut self, sha: &str) -> Self {
            self.commits.push(sha.to_string());
            self
        }

        fn with_ancestor(mut self, sha: &str) -> Self {
            self.ancestors_of_head.push(sha.to_string());
            self
        }

        fn with_candidates(mut self, base: &str, paths: &[&str]) -> Self {
            self.candidate_sets
                .insert(base.to_string(), paths.iter().map(PathBuf::from).collect());
            self
        }

        fn with_diff(mut self, base: &str, path: &str, diff: &str) -> Self {
            self.diffs
                .insert((base.to_string(), PathBuf::from(path)), diff.to_string());
            self
        }
    }

    impl GitDiffSource for FakeGit {
        fn rev_exists(&self, rev: &str) -> bool {
            self.commits.iter().any(|c| c == rev)
        }

        fn is_ancestor_of_head(&self, rev: &str) -> bool {
            self.ancestors_of_head.iter().any(|c| c == rev)
        }

        fn changed_spec_files(&self, base: &str) -> Vec<PathBuf> {
            self.candidate_sets.get(base).cloned().unwrap_or_default()
        }

        fn diff_spec(&self, base: &str, spec_path: &Path) -> String {
            self.diff_calls
                .borrow_mut()
                .push((base.to_string(), spec_path.to_path_buf()));
            self.diffs
                .get(&(base.to_string(), spec_path.to_path_buf()))
                .cloned()
                .unwrap_or_default()
        }
    }

    fn empty_sibling_base(_: &SpecLabel) -> Option<String> {
        None
    }

    fn anchor_inputs<'a>(
        label: &'a SpecLabel,
        spec_path: &'a Path,
        molecule: Option<MoleculeState>,
        since: Option<&'a str>,
        sibling_base: &'a dyn Fn(&SpecLabel) -> Option<String>,
    ) -> TierInputs<'a> {
        TierInputs {
            label,
            spec_path,
            molecule,
            since,
            sibling_base,
        }
    }

    #[test]
    fn tier_4_when_no_molecule_no_since() {
        let git = FakeGit::new();
        let label = SpecLabel::new("alpha");
        let path = PathBuf::from("specs/alpha.md");
        let inputs = anchor_inputs(&label, &path, None, None, &empty_sibling_base);
        let result = compute_spec_diff(&git, &inputs).expect("tier detection");
        assert_eq!(result, TierDecision::New);
    }

    #[test]
    fn tier_2_when_molecule_present_without_base_commit() {
        let git = FakeGit::new();
        let label = SpecLabel::new("alpha");
        let path = PathBuf::from("specs/alpha.md");
        let mol = MoleculeState {
            id: MoleculeId::new("wx-mol"),
            base_commit: None,
        };
        let inputs = anchor_inputs(&label, &path, Some(mol), None, &empty_sibling_base);
        let result = compute_spec_diff(&git, &inputs).expect("tier detection");
        assert_eq!(
            result,
            TierDecision::Tasks {
                molecule: MoleculeId::new("wx-mol"),
            }
        );
    }

    #[test]
    fn tier_2_when_base_commit_orphaned() {
        let git = FakeGit::new().with_commit("orphaned"); // exists but not an ancestor
        let label = SpecLabel::new("alpha");
        let path = PathBuf::from("specs/alpha.md");
        let mol = MoleculeState {
            id: MoleculeId::new("wx-mol"),
            base_commit: Some("orphaned".into()),
        };
        let inputs = anchor_inputs(&label, &path, Some(mol), None, &empty_sibling_base);
        let result = compute_spec_diff(&git, &inputs).expect("tier detection");
        assert_eq!(
            result,
            TierDecision::Tasks {
                molecule: MoleculeId::new("wx-mol"),
            }
        );
    }

    #[test]
    fn tier_2_when_base_commit_no_longer_exists() {
        let git = FakeGit::new(); // base does not even exist
        let label = SpecLabel::new("alpha");
        let path = PathBuf::from("specs/alpha.md");
        let mol = MoleculeState {
            id: MoleculeId::new("wx-mol"),
            base_commit: Some("vanished".into()),
        };
        let inputs = anchor_inputs(&label, &path, Some(mol), None, &empty_sibling_base);
        let result = compute_spec_diff(&git, &inputs).expect("tier detection");
        assert_eq!(
            result,
            TierDecision::Tasks {
                molecule: MoleculeId::new("wx-mol"),
            }
        );
    }

    #[test]
    fn tier_1_diff_with_empty_candidate_set() {
        let git = FakeGit::new()
            .with_commit("base1")
            .with_ancestor("base1")
            .with_candidates("base1", &[]);
        let label = SpecLabel::new("alpha");
        let path = PathBuf::from("specs/alpha.md");
        let mol = MoleculeState {
            id: MoleculeId::new("wx-mol"),
            base_commit: Some("base1".into()),
        };
        let inputs = anchor_inputs(&label, &path, Some(mol), None, &empty_sibling_base);
        let result = compute_spec_diff(&git, &inputs).expect("tier detection");
        match result {
            TierDecision::Diff {
                anchor_base,
                candidates,
            } => {
                assert_eq!(anchor_base, "base1");
                assert!(candidates.is_empty());
            }
            other => panic!("expected Diff, got {other:?}"),
        }
    }

    #[test]
    fn tier_1_anchor_only_diff() {
        let git = FakeGit::new()
            .with_commit("base1")
            .with_ancestor("base1")
            .with_candidates("base1", &["specs/alpha.md"])
            .with_diff("base1", "specs/alpha.md", "anchor diff body");
        let label = SpecLabel::new("alpha");
        let path = PathBuf::from("specs/alpha.md");
        let mol = MoleculeState {
            id: MoleculeId::new("wx-mol"),
            base_commit: Some("base1".into()),
        };
        let inputs = anchor_inputs(&label, &path, Some(mol), None, &empty_sibling_base);
        let result = compute_spec_diff(&git, &inputs).expect("tier detection");
        match result {
            TierDecision::Diff {
                anchor_base,
                candidates,
            } => {
                assert_eq!(anchor_base, "base1");
                assert_eq!(candidates.len(), 1);
                assert_eq!(candidates[0].label, SpecLabel::new("alpha"));
                assert_eq!(candidates[0].effective_base, "base1");
                assert_eq!(candidates[0].diff, "anchor diff body");
            }
            other => panic!("expected Diff, got {other:?}"),
        }
    }

    #[test]
    fn tier_1_fanout_uses_sibling_base_when_set() {
        let git = FakeGit::new()
            .with_commit("anchor_base")
            .with_commit("sibling_base")
            .with_ancestor("anchor_base")
            .with_ancestor("sibling_base")
            .with_candidates("anchor_base", &["specs/alpha.md", "specs/beta.md"])
            .with_diff("anchor_base", "specs/alpha.md", "alpha diff")
            .with_diff("sibling_base", "specs/beta.md", "beta diff from sibling");
        let label = SpecLabel::new("alpha");
        let path = PathBuf::from("specs/alpha.md");
        let mol = MoleculeState {
            id: MoleculeId::new("wx-mol"),
            base_commit: Some("anchor_base".into()),
        };
        let sibling = |l: &SpecLabel| -> Option<String> {
            if l.as_str() == "beta" {
                Some("sibling_base".to_string())
            } else {
                None
            }
        };
        let inputs = anchor_inputs(&label, &path, Some(mol), None, &sibling);
        let result = compute_spec_diff(&git, &inputs).expect("tier detection");
        match result {
            TierDecision::Diff {
                anchor_base,
                candidates,
            } => {
                assert_eq!(anchor_base, "anchor_base");
                assert_eq!(candidates.len(), 2);
                let beta = candidates
                    .iter()
                    .find(|c| c.label.as_str() == "beta")
                    .expect("beta candidate");
                assert_eq!(beta.effective_base, "sibling_base");
                assert_eq!(beta.diff, "beta diff from sibling");
            }
            other => panic!("expected Diff, got {other:?}"),
        }
    }

    #[test]
    fn tier_1_fanout_seeds_orphaned_sibling_from_anchor() {
        let git = FakeGit::new()
            .with_commit("anchor_base")
            .with_commit("orphaned_sibling")
            .with_ancestor("anchor_base") // sibling base is NOT an ancestor
            .with_candidates("anchor_base", &["specs/beta.md"])
            .with_diff("anchor_base", "specs/beta.md", "beta diff from anchor");
        let label = SpecLabel::new("alpha");
        let path = PathBuf::from("specs/alpha.md");
        let mol = MoleculeState {
            id: MoleculeId::new("wx-mol"),
            base_commit: Some("anchor_base".into()),
        };
        let sibling = |_: &SpecLabel| Some("orphaned_sibling".to_string());
        let inputs = anchor_inputs(&label, &path, Some(mol), None, &sibling);
        let result = compute_spec_diff(&git, &inputs).expect("tier detection");
        let TierDecision::Diff { candidates, .. } = result else {
            panic!("expected Diff");
        };
        let beta = candidates
            .iter()
            .find(|c| c.label.as_str() == "beta")
            .expect("beta");
        assert_eq!(beta.effective_base, "anchor_base");
        assert_eq!(beta.diff, "beta diff from anchor");
    }

    #[test]
    fn tier_1_skips_candidates_with_empty_diff() {
        let git = FakeGit::new()
            .with_commit("base1")
            .with_ancestor("base1")
            .with_candidates("base1", &["specs/alpha.md", "specs/beta.md"])
            .with_diff("base1", "specs/alpha.md", "alpha diff body");
        // beta has no recorded diff — fake returns empty string.
        let label = SpecLabel::new("alpha");
        let path = PathBuf::from("specs/alpha.md");
        let mol = MoleculeState {
            id: MoleculeId::new("wx-mol"),
            base_commit: Some("base1".into()),
        };
        let inputs = anchor_inputs(&label, &path, Some(mol), None, &empty_sibling_base);
        let result = compute_spec_diff(&git, &inputs).expect("tier detection");
        let TierDecision::Diff { candidates, .. } = result else {
            panic!("expected Diff");
        };
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].label, SpecLabel::new("alpha"));
    }

    #[test]
    fn since_override_replaces_anchor_base_for_anchor_only() {
        let git = FakeGit::new()
            .with_commit("override")
            .with_commit("stored_anchor")
            .with_commit("sibling_base")
            .with_ancestor("override")
            .with_ancestor("stored_anchor")
            .with_ancestor("sibling_base")
            .with_candidates("override", &["specs/alpha.md", "specs/beta.md"])
            .with_diff("override", "specs/alpha.md", "alpha override diff")
            .with_diff("sibling_base", "specs/beta.md", "beta diff");
        let label = SpecLabel::new("alpha");
        let path = PathBuf::from("specs/alpha.md");
        let mol = MoleculeState {
            id: MoleculeId::new("wx-mol"),
            base_commit: Some("stored_anchor".into()),
        };
        let sibling = |l: &SpecLabel| -> Option<String> {
            if l.as_str() == "beta" {
                Some("sibling_base".to_string())
            } else {
                None
            }
        };
        let inputs = anchor_inputs(&label, &path, Some(mol), Some("override"), &sibling);
        let result = compute_spec_diff(&git, &inputs).expect("tier detection");
        let TierDecision::Diff {
            anchor_base,
            candidates,
        } = result
        else {
            panic!("expected Diff");
        };
        assert_eq!(anchor_base, "override");
        let alpha = candidates
            .iter()
            .find(|c| c.label.as_str() == "alpha")
            .expect("alpha");
        assert_eq!(alpha.effective_base, "override");
        let beta = candidates
            .iter()
            .find(|c| c.label.as_str() == "beta")
            .expect("beta");
        assert_eq!(
            beta.effective_base, "sibling_base",
            "sibling base must NOT be overridden by --since"
        );
    }

    #[test]
    fn since_override_errors_when_commit_missing() {
        let git = FakeGit::new();
        let label = SpecLabel::new("alpha");
        let path = PathBuf::from("specs/alpha.md");
        let inputs = anchor_inputs(&label, &path, None, Some("nope"), &empty_sibling_base);
        let err = compute_spec_diff(&git, &inputs).expect_err("invalid commit must error");
        assert!(matches!(
            err,
            TodoError::InvalidSinceCommit { ref commit } if commit == "nope"
        ));
    }

    #[test]
    fn since_override_errors_when_commit_orphaned() {
        let git = FakeGit::new().with_commit("orphan"); // exists but not ancestor
        let label = SpecLabel::new("alpha");
        let path = PathBuf::from("specs/alpha.md");
        let inputs = anchor_inputs(&label, &path, None, Some("orphan"), &empty_sibling_base);
        let err = compute_spec_diff(&git, &inputs).expect_err("orphan must error under --since");
        assert!(matches!(
            err,
            TodoError::InvalidSinceCommit { ref commit } if commit == "orphan"
        ));
    }
}
