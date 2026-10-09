#![allow(clippy::unwrap_used)]
//! Integration coverage for [`loom_gate::annotation::parse`].
//!
//! Exercises the on-disk path the dispatcher and integrity gate hit:
//! reading `specs/*.md` from a directory, sorting deterministically,
//! and aggregating annotation + criterion records across files. The
//! in-memory shape (tier discrimination, code-fence isolation,
//! atomic-acceptance line grouping) is covered by the per-file unit
//! tests inside `src/annotation.rs`.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use loom_gate::annotation::{Tier, parse};
use tempfile::tempdir;

fn write(dir: &Path, name: &str, content: &str) {
    fs::write(dir.join(name), content).unwrap();
}

fn write_package(workspace: &Path, label: &str, acceptance: &str) {
    let package = workspace.join("specs").join(label);
    fs::create_dir_all(&package).unwrap();
    fs::write(
        package.join("spec.md"),
        "# Contract\n[Acceptance](tests.md#success-criteria)\n",
    )
    .unwrap();
    fs::write(package.join("tests.md"), acceptance).unwrap();
    fs::write(
        package.join("support.md"),
        "## Success Criteria\n- Not acceptance [test](ignored)\n",
    )
    .unwrap();
    fs::write(package.join("model.qnt"), "not markdown acceptance").unwrap();
}

fn write_index(workspace: &Path, labels: &[&str]) {
    fs::create_dir_all(workspace.join("docs")).unwrap();
    let mut rows = String::new();
    for label in labels {
        writeln!(rows, "| [{label}](../specs/{label}/spec.md) | owner |").unwrap();
    }
    fs::write(
        workspace.join("docs/README.md"),
        format!("## Specs\n\n| Spec | Purpose |\n| --- | --- |\n{rows}"),
    )
    .unwrap();
}

#[test]
fn parse_walks_canonical_package_acceptance_documents() {
    let dir = tempdir().unwrap();
    write_package(
        dir.path(),
        "bravo",
        "## Success Criteria\n- B [test](crate::b::ok)\n",
    );
    write_package(
        dir.path(),
        "alpha",
        "## Success Criteria\n- A [check](cargo run -p w -- a)\n",
    );
    write_index(dir.path(), &["bravo", "alpha"]);
    let out = parse(&dir.path().join("specs")).unwrap();
    assert_eq!(
        out.annotations
            .iter()
            .map(|annotation| annotation.target.as_str())
            .collect::<Vec<_>>(),
        vec!["cargo run -p w -- a", "crate::b::ok"]
    );
    assert_eq!(out.criteria.len(), 2);
    assert_eq!(
        out.annotations[0].source_spec,
        dir.path().join("specs/alpha/tests.md")
    );
    assert_eq!(
        out.annotations[1].source_spec,
        dir.path().join("specs/bravo/tests.md")
    );
}

#[test]
fn parse_aggregates_criteria_across_packages() {
    let dir = tempdir().unwrap();
    write_package(
        dir.path(),
        "alpha",
        "## Success Criteria\n- First [test](crate::a::ok)\n- Unbound\n",
    );
    write_package(
        dir.path(),
        "beta",
        "## Success Criteria\n- Second [judge](../../rubrics/api.sh#judge_api)\n",
    );
    write_index(dir.path(), &["alpha", "beta"]);
    let out = loom_gate::annotation::parse_workspace(dir.path()).unwrap();
    assert_eq!(out.criteria.len(), 3);
    assert_eq!(out.annotations.len(), 2);
    let row = loom_gate::cache::row_for(
        &out.annotations[1],
        loom_gate::cache::Verdict::Pass,
        "verified",
        1,
        "commit",
    );
    assert_eq!(row.spec_label, "beta");
    assert_eq!(
        out.criteria[2].source_spec,
        dir.path().join("specs/beta/tests.md")
    );
    let finding = loom_gate::integrity::IntegrityFinding::UnresolvedAnnotation {
        spec: out.annotations[1].source_spec.clone(),
        line: out.annotations[1].line,
        tier: Tier::Judge,
        target: out.annotations[1].target.clone(),
    };
    assert_eq!(finding.to_raw_finding().unwrap().bonds[0].as_str(), "beta");
}

#[test]
fn package_relocation_preserves_criterion_hashing_and_target_bytes() {
    let dir = tempdir().unwrap();
    let label = loom_driver::identifier::SpecLabel::new("alpha").unwrap();
    let content = "## Success Criteria\n- Exact requirement [check](bash -c 'printf \"(first)\\n\";\n  printf \"second\"') qualifier\n";
    let flat = loom_gate::annotation::parse_content(Path::new("specs/alpha.md"), content);
    write_package(dir.path(), "alpha", content);
    write_index(dir.path(), &["alpha"]);
    let packaged = parse(&dir.path().join("specs")).unwrap();
    assert_eq!(packaged.annotations[0].target, flat.annotations[0].target);
    assert_eq!(packaged.criteria[0].text, "Exact requirement qualifier");
    assert_eq!(
        loom_gate::annotation::criterion_id_for(&label, &packaged.criteria[0].text),
        loom_gate::annotation::criterion_id_for(&label, &flat.criteria[0].text)
    );
}

#[test]
fn live_package_parser_rejects_unindexed_inventory() {
    let dir = tempdir().unwrap();
    write_package(
        dir.path(),
        "alpha",
        "## Success Criteria\n- A [test](real)\n",
    );
    write_index(dir.path(), &["beta"]);
    assert!(matches!(
        parse(&dir.path().join("specs")),
        Err(loom_gate::annotation::ParseError::Package(_))
    ));
}

#[test]
fn live_package_parser_rejects_broken_contract_mappings() {
    let dir = tempdir().unwrap();
    write_package(
        dir.path(),
        "alpha",
        "## Success Criteria\n- A [test](real)\n",
    );
    write_index(dir.path(), &["alpha"]);
    fs::write(
        dir.path().join("specs/alpha/spec.md"),
        "[Acceptance](tests.md#absent)",
    )
    .unwrap();
    assert!(matches!(
        parse(&dir.path().join("specs")),
        Err(loom_gate::annotation::ParseError::Link(_))
    ));
}

#[test]
fn parse_walks_all_md_files_in_lex_order() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "bravo.md",
        "## Success Criteria\n\n- B [test](crate::b::ok)\n",
    );
    write(
        dir.path(),
        "alpha.md",
        "## Success Criteria\n\n- A [check](cargo run -p w -- a)\n",
    );

    let out = parse(dir.path()).unwrap();
    let targets: Vec<&str> = out.annotations.iter().map(|a| a.target.as_str()).collect();
    assert_eq!(
        targets,
        vec!["cargo run -p w -- a", "crate::b::ok"],
        "alpha.md sorts before bravo.md"
    );

    let tiers: Vec<Tier> = out.annotations.iter().map(|a| a.tier).collect();
    assert_eq!(tiers, vec![Tier::Check, Tier::Test]);

    let sources: Vec<&Path> = out
        .annotations
        .iter()
        .map(|a| a.source_spec.as_path())
        .collect();
    assert_eq!(sources[0], dir.path().join("alpha.md"));
    assert_eq!(sources[1], dir.path().join("bravo.md"));
}

#[test]
fn parse_skips_non_markdown_files_in_specs_dir() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "real.md",
        "## Success Criteria\n\n- X [test](crate::x::ok)\n",
    );
    write(dir.path(), "README", "ignored");
    write(dir.path(), "notes.txt", "ignored too");

    let out = parse(dir.path()).unwrap();
    assert_eq!(out.annotations.len(), 1);
    assert_eq!(out.annotations[0].target, "crate::x::ok");
}

#[test]
fn parse_aggregates_criteria_across_files() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "a.md",
        "## Success Criteria\n\n- one [test](crate::a::t)\n- two\n",
    );
    write(
        dir.path(),
        "b.md",
        "## Success Criteria\n\n- one [test](crate::b::t)\n",
    );

    let out = parse(dir.path()).unwrap();
    assert_eq!(out.criteria.len(), 3, "two from a.md plus one from b.md");
    assert_eq!(out.annotations.len(), 2);
}

#[test]
fn parse_returns_read_dir_error_for_missing_directory() {
    let missing = tempdir().unwrap().path().join("does-not-exist");
    let err = parse(&missing).unwrap_err();
    assert!(matches!(
        err,
        loom_gate::annotation::ParseError::ReadDir { .. }
    ));
}

#[test]
fn parse_recognises_pending_modifier_for_all_four_tiers() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "pending.md",
        "## Success Criteria\n\
         \n\
         - check tier [check?](cargo run -p loom-walk -- pending)\n\
         - test tier [test?](crate::pending::it)\n\
         - system tier [system?](nix run .#test-loom)\n\
         - judge tier [judge?](rubrics/pending.md)\n",
    );

    let out = parse(dir.path()).unwrap();
    assert_eq!(out.annotations.len(), 4);

    let by_tier: std::collections::HashMap<Tier, &loom_gate::annotation::Annotation> =
        out.annotations.iter().map(|a| (a.tier, a)).collect();

    for tier in [Tier::Check, Tier::Test, Tier::System, Tier::Judge] {
        let a = by_tier
            .get(&tier)
            .unwrap_or_else(|| panic!("missing annotation for tier {tier}"));
        assert!(
            a.pending,
            "tier {tier} carrying `?` modifier must parse as pending"
        );
    }
}

#[test]
fn parse_treats_unmarked_annotations_as_not_pending() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "mixed.md",
        "## Success Criteria\n\
         \n\
         - pending [test?](crate::p::pending)\n\
         - resolved [test](crate::p::resolved)\n",
    );

    let out = parse(dir.path()).unwrap();
    let by_target: std::collections::HashMap<&str, &loom_gate::annotation::Annotation> = out
        .annotations
        .iter()
        .map(|a| (a.target.as_str(), a))
        .collect();
    assert!(by_target["crate::p::pending"].pending);
    assert!(!by_target["crate::p::resolved"].pending);
}
