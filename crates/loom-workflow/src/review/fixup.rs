//! Verdict-gate fix-up bead chokepoint (`specs/harness.md` lines
//! 866-889 + 1963-1972).
//!
//! Every fix-up bead spawned by the verdict gate during recovery must be
//! bonded to its originating bead's molecule before becoming eligible for
//! `loom run` dispatch. The bond is atomic with creation — a fix-up bead
//! that leaves this chokepoint unbonded is a bug. The chokepoint also
//! refuses to spawn when the originating bead is itself unbonded, applying
//! `loom:blocked` with cause `unbonded-origin` to surface the upstream
//! inconsistency instead of propagating it.
//!
//! Two driver invariants live here:
//!
//! 1. **Atomic bond.** [`spawn_fixup_bead`] is the single chokepoint;
//!    callers cannot observe a created-but-unbonded fix-up bead because
//!    the function does not return the new id until the bond completes.
//! 2. **Unbonded-origin refusal.** When the originating bead has no
//!    molecule parent, the function applies `loom:blocked` +
//!    `unbonded-origin` to the origin (not a freshly-created fix-up) and
//!    returns [`FixupOutcome::RefusedUnbondedOrigin`] without creating
//!    anything.

use loom_driver::bd::Bead;
use loom_driver::identifier::{BeadId, MoleculeId};

use super::error::ReviewError;

/// Cause string written to `bd update --notes` when the verdict gate
/// refuses to spawn a fix-up bead because the originating bead is itself
/// unbonded (no molecule parent). Mirrored from
/// `specs/harness.md` §"Verdict gate · Fix-up beads bond to the
/// originating molecule".
pub const UNBONDED_ORIGIN_CAUSE: &str = "unbonded-origin";

/// Inputs required to create a fix-up bead. The chokepoint adds the bond
/// and the originating-bead `parent`; callers supply the human-readable
/// fields and any labels the bead should carry on dispatch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FixupRequest {
    pub title: String,
    pub description: String,
    pub labels: Vec<String>,
    pub priority: Option<u8>,
}

/// Outcome of one [`spawn_fixup_bead`] invocation. Mutually exclusive:
/// either a fix-up bead was created and bonded, or the origin's
/// unbondedness made the gate refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixupOutcome {
    /// Fix-up bead was created and bonded to `molecule` in one chokepoint
    /// turn — the new id is safe to dispatch to `loom run` because no
    /// observer can see it before the bond completes.
    Spawned {
        fixup_id: BeadId,
        molecule: MoleculeId,
    },
    /// Originating bead was unbonded. No fix-up bead was created; the
    /// originating bead carries `loom:blocked` + `unbonded-origin` so
    /// the inconsistency surfaces immediately. The origin id is repeated
    /// for callers that want to log / surface the affected bead.
    RefusedUnbondedOrigin { origin: BeadId },
}

/// Side-effect surface the [`spawn_fixup_bead`] chokepoint depends on.
///
/// The trait abstracts the `BdClient` calls so the chokepoint logic stays
/// testable without spawning a real `bd`. Production wires the methods
/// to:
///
/// - `show_origin` → `BdClient::show(origin)`
/// - `create_and_bond` → `BdClient::create(.. --parent=mol ..)` followed
///   immediately by `BdClient::mol_bond(mol, new_id)`. The combination
///   is the "atomic" guarantee — no caller sees the new id until both
///   steps have run.
/// - `apply_blocked` → `BdClient::update(origin, add_label=loom:blocked,
///   notes="unbonded-origin: …")`.
pub trait FixupContext: Send {
    fn show_origin(
        &mut self,
        origin: &BeadId,
    ) -> impl std::future::Future<Output = Result<Bead, ReviewError>> + Send;

    fn create_and_bond(
        &mut self,
        molecule: &MoleculeId,
        request: FixupRequest,
    ) -> impl std::future::Future<Output = Result<BeadId, ReviewError>> + Send;

    fn apply_blocked(
        &mut self,
        bead: &BeadId,
        cause: &str,
        detail: &str,
    ) -> impl std::future::Future<Output = Result<(), ReviewError>> + Send;
}

/// Spawn a fix-up bead under the verdict gate's atomic-bond invariant.
///
/// Looks up the originating bead, reads its `parent` (the molecule bond
/// per `bd show --json`), and either:
///
/// - bonds: dispatches `create_and_bond` so the new bead lands with its
///   molecule parent set in one chokepoint turn, returning
///   [`FixupOutcome::Spawned`]; or
/// - refuses: applies `loom:blocked` + [`UNBONDED_ORIGIN_CAUSE`] to the
///   originating bead and returns [`FixupOutcome::RefusedUnbondedOrigin`]
///   without creating anything downstream.
pub async fn spawn_fixup_bead<C: FixupContext>(
    ctx: &mut C,
    origin: &BeadId,
    request: FixupRequest,
) -> Result<FixupOutcome, ReviewError> {
    let bead = ctx.show_origin(origin).await?;
    let Some(molecule_parent) = bead.parent.clone() else {
        let detail = format!(
            "Originating bead {origin} has no molecule parent; refusing to spawn fix-up bead.",
        );
        ctx.apply_blocked(origin, UNBONDED_ORIGIN_CAUSE, &detail)
            .await?;
        return Ok(FixupOutcome::RefusedUnbondedOrigin {
            origin: origin.clone(),
        });
    };
    let molecule = MoleculeId::new(molecule_parent.as_str());
    let fixup_id = ctx.create_and_bond(&molecule, request).await?;
    Ok(FixupOutcome::Spawned { fixup_id, molecule })
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests use panicking helpers"
)]
mod tests {
    use super::*;
    use loom_driver::bd::{Bead, Label};
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeContext {
        /// Origin records keyed by id. `show_origin` returns the matching
        /// row, mimicking `bd show <id> --json`.
        origins: HashMap<String, Bead>,
        /// Sequence of `(molecule, fixup_id)` tuples produced for each
        /// `create_and_bond` invocation, in call order.
        create_calls: Vec<(MoleculeId, FixupRequest)>,
        /// Pre-seeded ids to return from `create_and_bond`. If the queue
        /// runs dry the fake panics (a real bug would be a logic error).
        next_ids: std::collections::VecDeque<BeadId>,
        /// `(bead, cause, detail)` tuples captured from `apply_blocked`.
        blocked_calls: Vec<(BeadId, String, String)>,
    }

    impl FixupContext for FakeContext {
        async fn show_origin(&mut self, origin: &BeadId) -> Result<Bead, ReviewError> {
            self.origins
                .get(origin.as_str())
                .cloned()
                .ok_or_else(|| ReviewError::ReviewIncomplete(format!("no origin {origin}")))
        }

        async fn create_and_bond(
            &mut self,
            molecule: &MoleculeId,
            request: FixupRequest,
        ) -> Result<BeadId, ReviewError> {
            self.create_calls.push((molecule.clone(), request));
            Ok(self
                .next_ids
                .pop_front()
                .expect("test seeded a next id for every create_and_bond"))
        }

        async fn apply_blocked(
            &mut self,
            bead: &BeadId,
            cause: &str,
            detail: &str,
        ) -> Result<(), ReviewError> {
            self.blocked_calls
                .push((bead.clone(), cause.to_string(), detail.to_string()));
            Ok(())
        }
    }

    fn bead(id: &str, parent: Option<&str>) -> Bead {
        Bead {
            id: BeadId::new(id).expect("valid bead id"),
            title: format!("title for {id}"),
            description: String::new(),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            labels: vec![Label::new("spec:harness")],
            parent: parent.map(|p| BeadId::new(p).expect("valid parent")),
            metadata: Default::default(),
            notes: None,
        }
    }

    #[tokio::test]
    async fn spawned_outcome_bonds_to_origins_parent_molecule() {
        let mut ctx = FakeContext::default();
        ctx.origins
            .insert("wx-origin.1".into(), bead("wx-origin.1", Some("wx-mola")));
        ctx.next_ids.push_back(BeadId::new("wx-fix.1").expect("id"));

        let origin = BeadId::new("wx-origin.1").expect("valid");
        let request = FixupRequest {
            title: "fix the leak".into(),
            description: "verify-fail recovery follow-up".into(),
            labels: vec!["spec:harness".into()],
            priority: Some(2),
        };

        let outcome = spawn_fixup_bead(&mut ctx, &origin, request.clone())
            .await
            .expect("spawn ok");

        match outcome {
            FixupOutcome::Spawned { fixup_id, molecule } => {
                assert_eq!(fixup_id, BeadId::new("wx-fix.1").expect("id"));
                assert_eq!(molecule, MoleculeId::new("wx-mola"));
            }
            other => panic!("expected Spawned, got {other:?}"),
        }

        assert_eq!(ctx.create_calls.len(), 1, "create_and_bond called once");
        let (mol, req) = &ctx.create_calls[0];
        assert_eq!(*mol, MoleculeId::new("wx-mola"));
        assert_eq!(req.title, "fix the leak");
        assert_eq!(req.description, "verify-fail recovery follow-up");

        assert!(
            ctx.blocked_calls.is_empty(),
            "spawn path never applies loom:blocked",
        );
    }

    #[tokio::test]
    async fn refused_outcome_applies_unbonded_origin_blocked_to_origin() {
        let mut ctx = FakeContext::default();
        // Origin with parent=None — unbonded.
        ctx.origins
            .insert("wx-orphan.5".into(), bead("wx-orphan.5", None));

        let origin = BeadId::new("wx-orphan.5").expect("valid");
        let request = FixupRequest {
            title: "should not be created".into(),
            ..FixupRequest::default()
        };

        let outcome = spawn_fixup_bead(&mut ctx, &origin, request)
            .await
            .expect("refuse path returns Ok");

        match outcome {
            FixupOutcome::RefusedUnbondedOrigin { origin: refused } => {
                assert_eq!(refused, BeadId::new("wx-orphan.5").expect("valid"));
            }
            other => panic!("expected RefusedUnbondedOrigin, got {other:?}"),
        }

        assert!(
            ctx.create_calls.is_empty(),
            "no fix-up bead created when origin is unbonded",
        );
        assert_eq!(ctx.blocked_calls.len(), 1);
        let (bead, cause, detail) = &ctx.blocked_calls[0];
        assert_eq!(*bead, BeadId::new("wx-orphan.5").expect("valid"));
        assert_eq!(cause, UNBONDED_ORIGIN_CAUSE);
        assert_eq!(cause, "unbonded-origin");
        assert!(
            detail.contains("wx-orphan.5"),
            "blocked detail names the origin: {detail}",
        );
    }

    #[tokio::test]
    async fn chokepoint_returns_only_after_bond_completes() {
        // The "atomic with creation" invariant is: callers cannot see the
        // new id before the bond has been recorded. The fake's
        // `create_and_bond` is a single operation, so observing the id
        // in the outcome proves the bond ran first.
        let mut ctx = FakeContext::default();
        ctx.origins
            .insert("wx-origin.2".into(), bead("wx-origin.2", Some("wx-molb")));
        ctx.next_ids
            .push_back(BeadId::new("wx-fix.42").expect("id"));

        let origin = BeadId::new("wx-origin.2").expect("valid");
        let outcome = spawn_fixup_bead(&mut ctx, &origin, FixupRequest::default())
            .await
            .expect("spawn ok");

        // The chokepoint must produce a Spawned outcome whose `fixup_id`
        // came from `create_and_bond` — which performs both create and
        // bond in one call. Any other shape would mean the function
        // returned before bonding.
        assert!(matches!(outcome, FixupOutcome::Spawned { .. }));
        assert_eq!(ctx.create_calls.len(), 1, "exactly one create+bond turn");
    }
}
