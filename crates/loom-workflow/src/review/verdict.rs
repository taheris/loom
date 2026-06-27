use loom_driver::identifier::BeadId;
use loom_gate::IntegrityFinding;

/// Which of the four push-gate inputs refused the push. Carried on
/// [`ReviewVerdict::PushBlocked`] so the `push_gate_refuse` driver event
/// can name the failing condition without the consumer re-deriving it
/// from the payload shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushGateRefuseCause {
    /// Some bead in the molecule still carries a terminal inbox or deferred
    /// label.
    BeadNotDone,
    /// `loom gate verify --diff <molecule.base_commit>..HEAD` exited
    /// non-zero (or a dispatch error counted as a fail).
    VerifierFailed,
    /// The reviewer agent's exit marker was `LOOM_CONCERN`.
    ReviewConcern,
    /// The integrity gate produced at least one `UnresolvedAnnotation`
    /// or `StubTestFunction` finding within the molecule's diff scope.
    IntegrityFinding,
}

impl PushGateRefuseCause {
    /// Stable wire string used in `push_gate_refuse` driver-event
    /// payloads and `bd update --notes` surfaces.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BeadNotDone => "bead-not-done",
            Self::VerifierFailed => "verifier-failed",
            Self::ReviewConcern => "review-concern",
            Self::IntegrityFinding => "integrity-finding",
        }
    }
}

/// Snapshot of bead state taken on either side of the reviewer agent. The
/// driver pre-counts beads with `spec:<label>`, runs the reviewer, then
/// re-counts and inspects the same query for terminal inbox/deferred label
/// membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeadSnapshot {
    /// Total number of beads carrying `spec:<label>`.
    pub spec_total: u32,
    /// IDs of beads currently labelled `loom:blocked` within the spec.
    pub blocked_ids: Vec<BeadId>,
    /// IDs of beads currently labelled `loom:clarify` within the spec.
    pub clarify_ids: Vec<BeadId>,
    /// IDs of beads currently labelled `loom:deferred` within the spec.
    pub deferred_ids: Vec<BeadId>,
    /// IDs of beads currently labelled `loom:infra` within the spec.
    pub infra_ids: Vec<BeadId>,
    /// IDs that appeared after the reviewer ran. Only populated for the
    /// post-snapshot — set is computed by [`super::diff_snapshots`].
    pub new_bead_ids: Vec<BeadId>,
}

/// The four post-review branches `loom review` can take. The driver computes
/// this enum, then runs the side effects: push, set loom:clarify, exec
/// `loom loop`, etc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewVerdict {
    /// No new beads + no terminal inbox/deferred labels → push code + beads.
    Clean,

    /// Push gate refused. `cause` names which of the four conditions
    /// failed; label-id lists populate only when `cause = BeadNotDone` and
    /// are empty for the other three causes; `integrity_findings` populates
    /// only when `cause = IntegrityFinding` and is empty otherwise. Per the four-condition
    /// AND in `specs/harness.md` FR9, push fires only when every
    /// condition passes.
    PushBlocked {
        cause: PushGateRefuseCause,
        blocked_ids: Vec<BeadId>,
        clarify_ids: Vec<BeadId>,
        deferred_ids: Vec<BeadId>,
        infra_ids: Vec<BeadId>,
        integrity_findings: Vec<IntegrityFinding>,
    },

    /// New fix-up beads, no terminal inbox/deferred labels, iteration cap not
    /// reached → exec
    /// `loom loop` for another forward pass. The driver increments the
    /// counter before returning this variant.
    AutoIterate {
        new_bead_ids: Vec<BeadId>,
        next_iteration: u32,
    },

    /// Integrity-gate findings present, iteration cap not yet exhausted →
    /// normalize each finding into a typed `Finding`, dispatch the batch
    /// through the standard mint pipeline, refuse the push for this
    /// iteration, increment the counter, and re-enter the loop so the
    /// worker can address the fix-up batch. Per `specs/gate.md` §
    /// *Integrity gate* (recovery branch). The cap-exhausted fallback
    /// routes to [`Self::PushBlocked`] with cause
    /// [`PushGateRefuseCause::IntegrityFinding`] instead.
    IntegrityRecover {
        findings: Vec<IntegrityFinding>,
        next_iteration: u32,
    },

    /// New fix-up beads, no terminal inbox/deferred labels, iteration cap
    /// exhausted →
    /// escalate the newest fix-up bead to `loom:clarify` and stop.
    IterationCap {
        new_bead_ids: Vec<BeadId>,
        escalate_id: BeadId,
        cap: u32,
    },
}

/// Compute the post-review snapshot diff: which bead IDs in `after` are not
/// present in `before`. Order is preserved from `after`.
pub fn diff_new_bead_ids(before: &[BeadId], after: &[BeadId]) -> Vec<BeadId> {
    use std::collections::HashSet;
    let known: HashSet<&BeadId> = before.iter().collect();
    after
        .iter()
        .filter(|id| !known.contains(id))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(id: &str) -> BeadId {
        BeadId::new(id).expect("valid bead id")
    }

    #[test]
    fn diff_returns_only_new_ids_in_post_order() {
        let before = vec![b("lm-a"), b("lm-b")];
        let after = vec![b("lm-a"), b("lm-b"), b("lm-c"), b("lm-d")];
        assert_eq!(
            diff_new_bead_ids(&before, &after),
            vec![b("lm-c"), b("lm-d")]
        );
    }

    #[test]
    fn diff_empty_when_no_new() {
        let before = vec![b("lm-a"), b("lm-b")];
        let after = vec![b("lm-a"), b("lm-b")];
        assert!(diff_new_bead_ids(&before, &after).is_empty());
    }

    #[test]
    fn diff_handles_empty_before() {
        let after = vec![b("lm-a")];
        assert_eq!(diff_new_bead_ids(&[], &after), vec![b("lm-a")]);
    }
}
