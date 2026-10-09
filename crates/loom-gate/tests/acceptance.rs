//! Current package snapshots resolve ordinary task acceptance through the public boundary.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use loom_driver::spec::package;
use loom_events::identifier::SpecLabel;
use loom_protocol::acceptance::{Reference, References, ResolveError, SnapshotError};
use loom_protocol::criterion::{AnnotationTier, CriterionId};

use loom_gate::acceptance::{Error, load};

#[cfg(test)]
mod fixture {
    use super::*;

    pub fn indexed(root: &Path, labels: &[&str], canonical: bool) {
        fs::create_dir_all(root.join("docs")).unwrap();
        let mut rows = String::new();
        for label in labels {
            let destination = if canonical {
                format!("../specs/{label}/spec.md")
            } else {
                format!("../specs/{label}.md")
            };
            writeln!(rows, "| [{label}]({destination}) | Contract |").unwrap();
        }
        fs::write(
            root.join("docs/README.md"),
            format!("## Specs\n\n| Spec | Purpose |\n| --- | --- |\n{rows}"),
        )
        .unwrap();
    }

    pub fn package(root: &Path, label: &str, criteria: &str) {
        let directory = root.join("specs").join(label);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("spec.md"),
            "# Contract\n[Acceptance](tests.md#success-criteria)\n",
        )
        .unwrap();
        fs::write(
            directory.join("tests.md"),
            format!("## Success Criteria\n{criteria}\n"),
        )
        .unwrap();
    }

    pub fn reference(label: &str, text: &str) -> Reference {
        let label = SpecLabel::new(label).unwrap();
        Reference::new(label.clone(), CriterionId::for_spec_text(&label, text))
    }

    pub fn assigned(label: &str, text: &str) -> References {
        References::new(vec![reference(label, text)]).unwrap()
    }
}

use fixture::{assigned, indexed, package, reference};

#[test]
fn task_acceptance_references_require_rebinding_after_requirement_edits() {
    let root = tempfile::tempdir().unwrap();
    package(
        root.path(),
        "alpha",
        "- Preserve exact bytes [test](original_verifier)",
    );
    indexed(root.path(), &["alpha"], true);
    let references = assigned("alpha", "Preserve exact bytes");
    let original = load(root.path()).unwrap().resolve(&references).unwrap();

    package(
        root.path(),
        "alpha",
        "- Preserve normalized bytes [test](original_verifier)",
    );
    let current = load(root.path()).unwrap();
    assert!(
        matches!(current.resolve(&references), Err(ResolveError::MissingCriterion { reference }) if reference == references.as_slice()[0])
    );
    let replacement = reference("alpha", "Preserve normalized bytes");
    let rebinding = current
        .rebind(&original.obligations()[0], &replacement)
        .unwrap();
    assert_eq!(rebinding.previous().reference(), &references.as_slice()[0]);
    assert_eq!(rebinding.current().reference(), &replacement);
    assert_eq!(
        rebinding.current().criterion_text(),
        "Preserve normalized bytes"
    );
    assert!(current.resolve(&references).is_err());
    assert!(
        current
            .resolve(&References::new(vec![replacement]).unwrap())
            .is_ok()
    );
}

#[test]
fn task_acceptance_resolution_produces_typed_obligations() {
    let root = tempfile::tempdir().unwrap();
    package(
        root.path(),
        "alpha",
        "- First requirement [judge?](../../rubrics/not-written.sh#judge_future)",
    );
    package(
        root.path(),
        "beta",
        "- Second requirement [check](command-not-installed --exact)",
    );
    indexed(root.path(), &["alpha", "beta"], true);
    fs::create_dir(root.path().join(".loom")).unwrap();
    fs::write(
        root.path().join(".loom/cache.db"),
        "not a database or passing evidence",
    )
    .unwrap();
    let first = reference("alpha", "First requirement");
    let second = reference("beta", "Second requirement");
    let references = References::parse([
        (second.spec_label().as_str(), second.criterion_id().as_str()),
        (first.spec_label().as_str(), first.criterion_id().as_str()),
    ])
    .unwrap();
    let resolved = load(root.path()).unwrap().resolve(&references).unwrap();
    let obligations = resolved.obligations();
    assert_eq!(obligations.len(), 2);
    assert_eq!(obligations[0].reference(), &second);
    assert_eq!(obligations[0].criterion_text(), "Second requirement");
    assert_eq!(obligations[0].annotation().tier, AnnotationTier::Check);
    assert_eq!(
        obligations[0].annotation().target.as_str(),
        "command-not-installed --exact"
    );
    assert!(!obligations[0].annotation().pending);
    assert_eq!(obligations[1].reference(), &first);
    assert_eq!(obligations[1].criterion_text(), "First requirement");
    assert_eq!(obligations[1].annotation().tier, AnnotationTier::Judge);
    assert_eq!(
        obligations[1].annotation().target.as_str(),
        "../../rubrics/not-written.sh#judge_future"
    );
    assert!(obligations[1].annotation().pending);
    assert_eq!(
        obligations[1].source(),
        root.path().join("specs/alpha/tests.md")
    );
    assert_eq!(obligations[1].line(), 2);
}

#[test]
fn within_label_package_move_preserves_task_identity_and_binding() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("specs")).unwrap();
    let flat = root.path().join("specs/alpha.md");
    fs::write(
        &flat,
        "## Success Criteria\n- Exact requirement [system?](bash -c 'printf \"(bytes)\\n\"')\n",
    )
    .unwrap();
    indexed(root.path(), &["alpha"], false);
    let references = assigned("alpha", "Exact requirement");
    let before = load(root.path()).unwrap().resolve(&references).unwrap();

    fs::remove_file(flat).unwrap();
    package(
        root.path(),
        "alpha",
        "<!-- prettier-ignore -->\n- Exact <!-- format -->\n  requirement\n  [system?](bash -c 'printf \"(bytes)\\n\"')",
    );
    indexed(root.path(), &["alpha"], true);
    let after = load(root.path()).unwrap().resolve(&references).unwrap();
    assert_eq!(
        before.obligations()[0].reference(),
        after.obligations()[0].reference()
    );
    assert_eq!(
        before.obligations()[0].annotation(),
        after.obligations()[0].annotation()
    );
    assert_eq!(
        after.obligations()[0].source(),
        root.path().join("specs/alpha/tests.md")
    );
}

#[test]
fn verifier_only_edits_use_current_binding_without_changing_identity() {
    let root = tempfile::tempdir().unwrap();
    package(
        root.path(),
        "alpha",
        "- Exact requirement [test?](old_verifier)",
    );
    indexed(root.path(), &["alpha"], true);
    let references = assigned("alpha", "Exact requirement");
    let before = load(root.path()).unwrap().resolve(&references).unwrap();

    package(
        root.path(),
        "alpha",
        "- Exact requirement [system](new-command --flag)",
    );
    let after = load(root.path()).unwrap().resolve(&references).unwrap();
    assert_eq!(
        before.obligations()[0].reference(),
        after.obligations()[0].reference()
    );
    assert_eq!(
        before.obligations()[0].annotation().target.as_str(),
        "old_verifier"
    );
    assert!(before.obligations()[0].annotation().pending);
    assert_eq!(
        after.obligations()[0].annotation().tier,
        AnnotationTier::System
    );
    assert_eq!(
        after.obligations()[0].annotation().target.as_str(),
        "new-command --flag"
    );
    assert!(!after.obligations()[0].annotation().pending);
}

#[test]
fn cross_label_claim_relocation_requires_explicit_rebinding() {
    let root = tempfile::tempdir().unwrap();
    package(
        root.path(),
        "alpha",
        "- Exact requirement [test?](retained_verifier)",
    );
    package(root.path(), "beta", "");
    indexed(root.path(), &["alpha", "beta"], true);
    let references = assigned("alpha", "Exact requirement");
    let before = load(root.path()).unwrap().resolve(&references).unwrap();

    package(root.path(), "alpha", "");
    package(
        root.path(),
        "beta",
        "- Exact requirement [test?](retained_verifier)",
    );
    let snapshot = load(root.path()).unwrap();
    assert!(matches!(
        snapshot.resolve(&references),
        Err(ResolveError::MissingCriterion { .. })
    ));
    let relocated = reference("beta", "Exact requirement");
    assert_ne!(
        relocated.criterion_id(),
        references.as_slice()[0].criterion_id()
    );
    let rebinding = snapshot
        .rebind(&before.obligations()[0], &relocated)
        .unwrap();
    assert_eq!(rebinding.previous().reference(), &references.as_slice()[0]);
    assert_eq!(rebinding.current().reference(), &relocated);
    assert_eq!(
        rebinding.previous().annotation(),
        rebinding.current().annotation()
    );
    assert_eq!(
        rebinding.previous().source(),
        root.path().join("specs/alpha/tests.md")
    );
    assert_eq!(
        rebinding.current().source(),
        root.path().join("specs/beta/tests.md")
    );
    assert!(snapshot.resolve(&references).is_err());
    assert!(
        snapshot
            .resolve(&References::new(vec![relocated]).unwrap())
            .is_ok()
    );
}

#[test]
fn resolution_is_all_or_nothing_when_a_later_member_is_missing() {
    let root = tempfile::tempdir().unwrap();
    package(root.path(), "alpha", "- Valid requirement [test](real)");
    indexed(root.path(), &["alpha"], true);
    let absent = reference("alpha", "Missing requirement");
    let references = References::new(vec![
        reference("alpha", "Valid requirement"),
        absent.clone(),
    ])
    .unwrap();
    assert!(
        matches!(load(root.path()).unwrap().resolve(&references), Err(ResolveError::MissingCriterion { reference }) if reference == absent)
    );
}

#[test]
fn resolution_rejects_unknown_packages() {
    let root = tempfile::tempdir().unwrap();
    package(root.path(), "alpha", "- Valid requirement [test](real)");
    indexed(root.path(), &["alpha"], true);
    assert!(
        matches!(load(root.path()).unwrap().resolve(&assigned("unknown", "Valid requirement")), Err(ResolveError::UnknownPackage { spec_label }) if spec_label.as_str() == "unknown")
    );
}

#[test]
fn snapshot_rejects_duplicate_normalized_requirements() {
    let root = tempfile::tempdir().unwrap();
    package(
        root.path(),
        "alpha",
        "- Exact requirement [test](first)\n- Exact\n  requirement [judge](second)",
    );
    indexed(root.path(), &["alpha"], true);
    assert!(
        matches!(load(root.path()), Err(Error::Snapshot(SnapshotError::DuplicateCriterion { reference })) if reference == self::reference("alpha", "Exact requirement"))
    );
}

#[test]
fn snapshot_rejects_non_atomic_or_malformed_bindings() {
    let root = tempfile::tempdir().unwrap();
    indexed(root.path(), &["alpha"], true);
    for (criterion, count) in [
        ("- Requirement with no annotation", 0),
        ("- Malformed annotation [test](unbalanced", 0),
        ("- Two annotations [test](first) [test?](second)", 2),
    ] {
        package(root.path(), "alpha", criterion);
        assert!(
            matches!(load(root.path()), Err(Error::Snapshot(SnapshotError::BindingCount { count: actual, .. })) if actual == count)
        );
    }
}

#[test]
fn snapshot_rejects_empty_binding_targets() {
    let root = tempfile::tempdir().unwrap();
    package(root.path(), "alpha", "- Requirement [test?](  )");
    indexed(root.path(), &["alpha"], true);
    assert!(matches!(
        load(root.path()),
        Err(Error::Snapshot(SnapshotError::EmptyTarget { .. }))
    ));
}

#[test]
fn snapshot_rejects_unindexed_package_inventory() {
    let root = tempfile::tempdir().unwrap();
    package(root.path(), "alpha", "- Requirement [test](real)");
    indexed(root.path(), &["other"], true);
    assert!(matches!(
        load(root.path()),
        Err(Error::Package(package::Error::Index { .. }))
    ));
}

#[test]
fn snapshot_and_resolved_acceptance_are_isolated_from_later_file_edits() {
    let root = tempfile::tempdir().unwrap();
    package(root.path(), "alpha", "- Requirement [test?](before)");
    indexed(root.path(), &["alpha"], true);
    let references = assigned("alpha", "Requirement");
    let snapshot = load(root.path()).unwrap();
    let resolved = snapshot.resolve(&references).unwrap();
    package(
        root.path(),
        "alpha",
        "- Different requirement [test](after)",
    );
    assert_eq!(snapshot.resolve(&references).unwrap(), resolved);
    assert!(load(root.path()).unwrap().resolve(&references).is_err());
    assert_eq!(
        resolved.obligations()[0].annotation().target.as_str(),
        "before"
    );
    assert_eq!(resolved.obligations()[0].criterion_text(), "Requirement");
}
