#![allow(clippy::unwrap_used)]
//! Integration coverage for [`loom_gate::annotation::parse`].
//!
//! Exercises the on-disk path the dispatcher and integrity gate hit:
//! reading `specs/*.md` from a directory, sorting deterministically,
//! and aggregating annotation + criterion records across files. The
//! in-memory shape (tier discrimination, code-fence isolation,
//! atomic-acceptance line grouping) is covered by the per-file unit
//! tests inside `src/annotation.rs`.

use std::fs;
use std::path::Path;

use loom_gate::annotation::{Tier, parse};
use tempfile::tempdir;

fn write(dir: &Path, name: &str, content: &str) {
    fs::write(dir.join(name), content).unwrap();
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
