//! Actual compiled Test effects intersect their captured inputs, not runner-supplied lists.
#![allow(
    clippy::unwrap_used,
    reason = "isolated compiler fixture setup and assertions"
)]

use std::process::Command;

#[path = "common/cargo.rs"]
mod cargo;

#[test]
fn test_tier_filters_targets_by_files_scope_intersection() {
    let fixture = cargo::fixture();
    std::fs::write(
        fixture.path().join("docs/unrelated.md"),
        "# Unrelated document\n",
    )
    .unwrap();
    let run = |path: &str| {
        Command::new(env!("CARGO_BIN_EXE_loom"))
            .arg("--workspace")
            .arg(fixture.path())
            .args(["gate", "test", "--files", path])
            .output()
            .unwrap()
    };
    let output = run("payload/input.txt");
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{diagnostic}");
    assert!(
        diagnostic
            .lines()
            .any(|line| line.starts_with("loom gate [native_case]: ")
                && line.contains("\"verdict\":\"pass\"")),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("\"kind\":\"fresh-execution\""),
        "actual native output, not discovery or cached authority: {diagnostic}"
    );
    let unrelated = run("docs/unrelated.md");
    let diagnostic = String::from_utf8_lossy(&unrelated.stderr);
    assert!(unrelated.status.success(), "{diagnostic}");
    assert!(
        !diagnostic
            .lines()
            .any(|line| line.starts_with("loom gate [native_case]: ")),
        "{diagnostic}"
    );
    assert!(
        !diagnostic.contains("\"kind\":\"fresh-execution\""),
        "unrelated documentation must not execute the Cargo suite: {diagnostic}"
    );
}
