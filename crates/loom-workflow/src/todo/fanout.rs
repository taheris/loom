//! Multi-spec touched-set classification for `loom todo`.
//!
//! After [`super::touched::touched_specs`] enumerates the specs whose
//! markdown differs from `HEAD`, this module walks each touched spec's
//! open epic via `bd find --type=epic --label=spec:<X> --status=open`
//! and classifies the touched-set outcome.
//!
//! The molecule that owns each epic is read from the epic's `parent`
//! field (set by `bd mol bond`). The classifier returns one of:
//!
//! - [`FanoutOutcome::MintAll`] — every touched spec has no open epic.
//! - [`FanoutOutcome::Bond`] — every touched spec's open epic is bonded
//!   to the same molecule.
//! - [`FanoutOutcome::Collision`] — touched specs span different
//!   molecules or mix has-open-epic with no-open-epic. Loom does not
//!   mint anything in this case; the production controller writes a
//!   `loom:clarify` bead carrying the [Options Format
//!   Contract](../../../specs/gate.md#options-format-contract) block
//!   and exits.

use loom_driver::bd::{BdClient, CommandRunner, ListOpts};
use loom_driver::identifier::{BeadId, MoleculeId, SpecLabel};

use super::error::TodoError;
use super::touched::TouchedSpec;

/// One touched spec's resolved molecule, or `None` if the spec has no
/// open epic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecResolution {
    pub label: SpecLabel,
    /// Open-epic id for the spec, if any.
    pub epic_id: Option<BeadId>,
    /// Molecule the open epic is bonded to (the epic's `parent`).
    /// `None` when the spec has no open epic, or when the open epic is
    /// unbonded (a structurally invalid shape that the collision-clarify
    /// path treats as "no molecule").
    pub molecule_id: Option<MoleculeId>,
}

/// Outcome of [`classify_touched_set`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FanoutOutcome {
    /// Every touched spec has no open epic — mint a fresh cross-cutting
    /// molecule.
    MintAll,
    /// Every touched spec's open epic is bonded to the same molecule —
    /// the agent adds new beads under the right per-spec epic of that
    /// molecule.
    Bond(MoleculeId),
    /// Touched specs span different molecules, or mix has/has-not open
    /// epics. Loom mints nothing and emits a `loom:clarify` bead.
    Collision { resolutions: Vec<SpecResolution> },
}

/// Classify the touched-set after walking each spec's open epic.
///
/// Calls `bd list --type=epic --label=spec:<X> --status=open` for every
/// touched spec, reads the resulting epic's `parent` (its molecule), and
/// projects the result onto [`FanoutOutcome`].
pub async fn classify_touched_set<R: CommandRunner>(
    bd: &BdClient<R>,
    touched: &[TouchedSpec],
) -> Result<FanoutOutcome, TodoError> {
    if touched.is_empty() {
        return Ok(FanoutOutcome::MintAll);
    }
    let mut resolutions = Vec::with_capacity(touched.len());
    for spec in touched {
        resolutions.push(resolve_one(bd, &spec.label).await?);
    }
    Ok(classify(resolutions))
}

async fn resolve_one<R: CommandRunner>(
    bd: &BdClient<R>,
    label: &SpecLabel,
) -> Result<SpecResolution, TodoError> {
    let beads = bd
        .list(ListOpts {
            issue_type: Some("epic".to_string()),
            label: Some(format!("spec:{}", label.as_str())),
            status: Some("open".to_string()),
            ..Default::default()
        })
        .await?;
    match beads.len() {
        0 => Ok(SpecResolution {
            label: label.clone(),
            epic_id: None,
            molecule_id: None,
        }),
        1 => {
            let epic = &beads[0];
            let molecule_id = epic.parent.as_ref().map(|p| MoleculeId::new(p.as_str()));
            Ok(SpecResolution {
                label: label.clone(),
                epic_id: Some(epic.id.clone()),
                molecule_id,
            })
        }
        _ => {
            let ids = beads
                .iter()
                .map(|b| b.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(TodoError::InvariantViolation {
                label: label.to_string(),
                ids,
            })
        }
    }
}

fn classify(resolutions: Vec<SpecResolution>) -> FanoutOutcome {
    let any_some = resolutions.iter().any(|r| r.epic_id.is_some());
    let all_none = !any_some;
    if all_none {
        return FanoutOutcome::MintAll;
    }
    // Single-spec touched set is never a multi-spec collision — the
    // "spans different molecules / mix has-has-not" predicate is empty
    // for one entry. The spec's collision criterion is multi-spec only;
    // single-spec resolution stays a Bond when the epic is present.
    if let [only] = resolutions.as_slice() {
        if let Some(mol) = only.molecule_id.clone() {
            return FanoutOutcome::Bond(mol);
        }
        // Unbonded epic on the sole touched spec — treat the epic id as
        // the molecule key so existing single-spec bonded flows keep
        // working without a `bd mol bond` parent. `epic_id` is `Some` by
        // construction (`any_some` was true and `len == 1`).
        if let Some(epic) = only.epic_id.clone() {
            return FanoutOutcome::Bond(MoleculeId::new(epic.as_str()));
        }
    }
    let any_none = resolutions.iter().any(|r| r.epic_id.is_none());
    if any_none {
        return FanoutOutcome::Collision { resolutions };
    }
    let first_molecule = resolutions[0].molecule_id.clone();
    let homogeneous =
        first_molecule.is_some() && resolutions.iter().all(|r| r.molecule_id == first_molecule);
    if let Some(mol) = first_molecule
        && homogeneous
    {
        return FanoutOutcome::Bond(mol);
    }
    FanoutOutcome::Collision { resolutions }
}

/// Render the canonical `## Options — …` body for a collision clarify.
///
/// Format obeys the *Options Format Contract* in `specs/gate.md`:
/// `## Options — <summary>` header, then `### Option <N> — <title>`
/// subsections enumerating (a) bond into each pre-existing molecule,
/// then (b) close pre-existing epics and mint a fresh cross-cutting
/// molecule covering every touched spec.
pub fn render_collision_options(resolutions: &[SpecResolution]) -> String {
    let labels: Vec<String> = resolutions.iter().map(|r| r.label.to_string()).collect();
    let mut existing: Vec<(MoleculeId, Vec<SpecLabel>)> = Vec::new();
    for res in resolutions {
        let Some(mol) = res.molecule_id.clone() else {
            continue;
        };
        if let Some(slot) = existing.iter_mut().find(|(m, _)| m == &mol) {
            slot.1.push(res.label.clone());
        } else {
            existing.push((mol, vec![res.label.clone()]));
        }
    }
    let summary = format!("multi-spec fan-out collision across {}", labels.join(", "));
    let mut body = format!("## Options — {summary}\n");
    let mut idx: u32 = 1;
    for (mol, specs) in &existing {
        let spec_list = specs
            .iter()
            .map(SpecLabel::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        body.push_str(&format!(
            "\n### Option {idx} — Bond into molecule `{mol}`\n"
        ));
        body.push_str(&format!(
            "Adopt the existing molecule `{mol}` (currently anchoring {spec_list}). \
             Re-run `loom todo` so fan-out beads bond under each touched spec's \
             epic; close any conflicting open epic in the other touched spec(s) first.\n",
            mol = mol,
            spec_list = spec_list,
        ));
        idx += 1;
    }
    body.push_str(&format!(
        "\n### Option {idx} — Close existing epics and mint a fresh cross-cutting molecule\n"
    ));
    let existing_mols: Vec<String> = existing
        .iter()
        .map(|(m, _)| format!("`{}`", m.as_str()))
        .collect();
    if existing_mols.is_empty() {
        body.push_str(
            "Mint one new molecule covering every touched spec. \
             Re-run `loom todo` to fan out fresh epics and bond them under the new molecule.\n",
        );
    } else {
        body.push_str(&format!(
            "Close the pre-existing epic(s) ({mols}) via `bd update --status=closed`, \
             then re-run `loom todo` to mint one fresh cross-cutting molecule covering \
             every touched spec ({labels}).\n",
            mols = existing_mols.join(", "),
            labels = labels.join(", "),
        ));
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(label: &str, epic: Option<&str>, molecule: Option<&str>) -> SpecResolution {
        SpecResolution {
            label: SpecLabel::new(label),
            epic_id: epic.map(|s| BeadId::new(s).unwrap()),
            molecule_id: molecule.map(MoleculeId::new),
        }
    }

    #[test]
    fn all_none_resolutions_classify_as_mint() {
        let outcome = classify(vec![res("alpha", None, None), res("beta", None, None)]);
        assert_eq!(outcome, FanoutOutcome::MintAll);
    }

    #[test]
    fn all_same_molecule_classifies_as_bond() {
        let outcome = classify(vec![
            res("alpha", Some("lm-a"), Some("lm-mol")),
            res("beta", Some("lm-b"), Some("lm-mol")),
        ]);
        assert_eq!(outcome, FanoutOutcome::Bond(MoleculeId::new("lm-mol")));
    }

    #[test]
    fn mix_has_and_no_epic_classifies_as_collision() {
        let outcome = classify(vec![
            res("alpha", Some("lm-a"), Some("lm-mol")),
            res("beta", None, None),
        ]);
        match outcome {
            FanoutOutcome::Collision { resolutions } => assert_eq!(resolutions.len(), 2),
            other => panic!("expected Collision, got {other:?}"),
        }
    }

    #[test]
    fn different_molecules_classifies_as_collision() {
        let outcome = classify(vec![
            res("alpha", Some("lm-a"), Some("lm-mol1")),
            res("beta", Some("lm-b"), Some("lm-mol2")),
        ]);
        match outcome {
            FanoutOutcome::Collision { .. } => {}
            other => panic!("expected Collision, got {other:?}"),
        }
    }

    #[test]
    fn unbonded_epic_classifies_as_collision_when_mixed() {
        let outcome = classify(vec![
            res("alpha", Some("lm-a"), Some("lm-mol")),
            res("beta", Some("lm-b"), None),
        ]);
        match outcome {
            FanoutOutcome::Collision { .. } => {}
            other => panic!("expected Collision, got {other:?}"),
        }
    }

    #[test]
    fn render_options_enumerates_existing_molecule_and_fresh_mint() {
        let resolutions = vec![
            res("alpha", Some("lm-a"), Some("lm-mol1")),
            res("beta", Some("lm-b"), Some("lm-mol2")),
        ];
        let body = render_collision_options(&resolutions);
        assert!(body.contains("## Options — "), "summary header present");
        assert!(body.contains("alpha"), "labels appear in summary");
        assert!(body.contains("beta"));
        assert!(body.contains("### Option 1 — Bond into molecule `lm-mol1`"));
        assert!(body.contains("### Option 2 — Bond into molecule `lm-mol2`"));
        assert!(body.contains(
            "### Option 3 — Close existing epics and mint a fresh cross-cutting molecule"
        ),);
    }
}
