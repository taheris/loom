//! `insta` snapshot tests for `loom --help` and every subcommand `--help`.
//!
//! `--help` output IS the user contract: clap's auto-generated layout, flag
//! ordering, and default-value rendering are surfaces a human reads. Substring
//! assertions miss subtle drift (a renamed flag whose name still contains the
//! checked substring slips past). Snapshots fail loudly on any text change.
//!
//! Snapshot updates require a "snapshot updated because: ..." line in the PR
//! description so accidental drift is caught at review time.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

fn loom_help(args: &[&str]) -> String {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .args(args)
        .arg("--help")
        // Pin the rendered width so terminal-size variation cannot shift
        // wrapping between the developer's TTY and CI.
        .env("COLUMNS", "100")
        .env("CLAP_TERM_WIDTH", "100")
        .output()
        .expect("spawn loom");
    assert!(
        output.status.success(),
        "loom {args:?} --help exited non-zero: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("utf-8")
}

/// Bare `loom` (no args) must render the same grouped help as `loom --help`.
/// Spec: `harness.md` § Functional #1.
#[test]
fn loom_bare_matches_help() {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .env("COLUMNS", "100")
        .env("CLAP_TERM_WIDTH", "100")
        .output()
        .expect("spawn loom");
    assert!(
        output.status.success(),
        "bare `loom` exited non-zero: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    let bare = String::from_utf8(output.stdout).expect("utf-8");
    assert_eq!(bare, loom_help(&[]));
}

#[test]
fn loom_help_snapshot() {
    insta::assert_snapshot!(loom_help(&[]));
}

/// Independent of the snapshot: assert the spec-required section order and
/// per-section subcommand membership in `loom --help`. The snapshot test
/// catches any wording or layout drift; this test fails loudly if the
/// section ordering itself regresses, with a more specific diagnostic.
/// Spec: `harness.md` § Functional #1.
#[test]
fn loom_help_groups_workflow_inspection_state_in_order() {
    let out = loom_help(&[]);
    let workflow = out.find("Workflow:").expect("Workflow heading missing");
    let inspection = out.find("Inspection:").expect("Inspection heading missing");
    let state = out.find("State:").expect("State heading missing");
    assert!(
        workflow < inspection && inspection < state,
        "headings must appear in order Workflow → Inspection → State, got {workflow} / {inspection} / {state}",
    );

    let workflow_section = &out[workflow..inspection];
    for sub in ["plan", "todo", "loop", "gate", "msg"] {
        assert!(
            workflow_section.contains(&format!("  {sub} "))
                || workflow_section.contains(&format!("  {sub}\n")),
            "Workflow section is missing `{sub}` row:\n{workflow_section}",
        );
    }

    let inspection_section = &out[inspection..state];
    for sub in ["status", "logs", "spec"] {
        assert!(
            inspection_section.contains(&format!("  {sub} "))
                || inspection_section.contains(&format!("  {sub}\n")),
            "Inspection section is missing `{sub}` row:\n{inspection_section}",
        );
    }

    let state_section = &out[state..];
    for sub in ["init", "use", "note"] {
        assert!(
            state_section.contains(&format!("  {sub} "))
                || state_section.contains(&format!("  {sub}\n")),
            "State section is missing `{sub}` row:\n{state_section}",
        );
    }

    // `doctor` was folded into `loom gate <subcommand>` and must not
    // surface in the top-level help — its absence is part of the user
    // contract (spec § Functional #1).
    assert!(
        !out.contains("\n  doctor "),
        "`doctor` must not appear in `loom --help` output:\n{out}",
    );
    // `check` is the renamed-away subcommand: `loom gate check` is
    // the new home for the [check]-tier dispatcher per
    // specs/gate.md. The top-level `loom check` must not exist.
    assert!(
        !out.contains("\n  check "),
        "`check` must not appear at top level (renamed to `loom gate check`):\n{out}",
    );
}

#[test]
fn loom_init_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["init"]));
}

#[test]
fn loom_status_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["status"]));
}

#[test]
fn loom_use_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["use"]));
}

#[test]
fn loom_logs_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["logs"]));
}

#[test]
fn loom_spec_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["spec"]));
}

#[test]
fn loom_plan_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["plan"]));
}

#[test]
fn loom_loop_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["run"]));
}

#[test]
fn loom_gate_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["gate"]));
}

#[test]
fn loom_gate_verify_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["gate", "verify"]));
}

#[test]
fn loom_gate_check_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["gate", "check"]));
}

#[test]
fn loom_gate_test_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["gate", "test"]));
}

#[test]
fn loom_gate_system_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["gate", "system"]));
}

#[test]
fn loom_gate_review_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["gate", "review"]));
}

#[test]
fn loom_gate_audit_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["gate", "audit"]));
}

#[test]
fn loom_gate_judge_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["gate", "judge"]));
}

#[test]
fn loom_gate_rubric_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["gate", "rubric"]));
}

#[test]
fn loom_check_is_removed_from_top_level() {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let output = Command::new(loom_bin)
        .args(["check"])
        .env("COLUMNS", "100")
        .env("CLAP_TERM_WIDTH", "100")
        .output()
        .expect("spawn loom");
    assert!(
        !output.status.success(),
        "top-level `loom check` must exit non-zero (renamed to `loom gate <subcommand>`)",
    );
    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(
        stderr.contains("unrecognized subcommand")
            || stderr.contains("invalid")
            || stderr.contains("unrecognised"),
        "stderr must explain the removal, got: {stderr}",
    );
}

#[test]
fn loom_gate_scope_flags_are_mutually_exclusive() {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    for args in [
        vec!["gate", "test", "--diff", "HEAD~1..HEAD", "--tree"],
        vec![
            "gate",
            "verify",
            "--target",
            "cargo test",
            "--diff",
            "HEAD~1..HEAD",
        ],
        vec![
            "gate",
            "audit",
            "--files",
            "src/lib.rs",
            "--diff",
            "HEAD~1..HEAD",
        ],
        vec!["gate", "system", "--files", "src/lib.rs", "--tree"],
    ] {
        let output = Command::new(loom_bin)
            .args(&args)
            .env("COLUMNS", "100")
            .env("CLAP_TERM_WIDTH", "100")
            .output()
            .expect("spawn loom");
        assert!(
            !output.status.success(),
            "loom {args:?} must exit non-zero (scope flags are mutually exclusive)",
        );
        let stderr = String::from_utf8(output.stderr).expect("utf-8");
        assert!(
            stderr.contains("cannot be used with")
                || stderr.contains("the argument")
                || stderr.contains("not be used with"),
            "stderr must explain mutual exclusion, got: {stderr}",
        );
    }
}

/// `loom gate verify --files <PATH>...` must accept variadic paths in
/// the shape pre-commit's `pass_filenames: true` emits: each staged
/// file passed as a separate value after `--files`. Without
/// `num_args = 1..` on the field, clap stops after the first value and
/// treats the trailing paths as unexpected arguments.
#[test]
fn loom_gate_verify_files_accepts_variadic_paths() {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    let tmp = tempfile::tempdir().expect("tmpdir");
    let output = Command::new(loom_bin)
        .args([
            "--workspace",
            tmp.path().to_str().expect("utf-8 tmp path"),
            "gate",
            "verify",
            "--files",
            "a.rs",
            "b.rs",
            "c.rs",
        ])
        .env("COLUMNS", "100")
        .env("CLAP_TERM_WIDTH", "100")
        .output()
        .expect("spawn loom");
    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(
        !stderr.contains("unexpected argument") && !stderr.contains("which wasn't expected"),
        "variadic --files must not surface as 'unexpected argument', got: {stderr}",
    );
}

#[test]
fn loom_gate_removed_scope_surfaces_are_rejected() {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    for args in [
        vec!["gate", "verify", "--spec", "gate"],
        vec!["gate", "verify", "--bead", "lm-1", "--diff", "HEAD~1..HEAD"],
        vec!["gate", "verify", "cargo test --lib"],
        vec!["gate", "mint", "--tree", "--spec", "gate"],
    ] {
        let output = Command::new(loom_bin)
            .args(&args)
            .env("COLUMNS", "100")
            .env("CLAP_TERM_WIDTH", "100")
            .output()
            .expect("spawn loom");
        assert!(
            !output.status.success(),
            "loom {args:?} must reject removed gate scope surface",
        );
    }
}

#[test]
fn loom_gate_bare_inspection_subcommands_print_help() {
    let loom_bin = env!("CARGO_BIN_EXE_loom");
    for subcommand in [
        "verify", "status", "audit", "review", "judge", "rubric", "mint",
    ] {
        let output = Command::new(loom_bin)
            .args(["gate", subcommand])
            .env("COLUMNS", "100")
            .env("CLAP_TERM_WIDTH", "100")
            .env_remove("LOOM_INSIDE")
            .output()
            .expect("spawn loom");
        assert!(
            output.status.success(),
            "bare gate {subcommand} prints help and exits 0: stderr={}",
            String::from_utf8_lossy(&output.stderr),
        );
        let stdout = String::from_utf8(output.stdout).expect("utf-8");
        assert!(
            stdout.contains("Usage: loom gate"),
            "bare gate {subcommand} must print subcommand help: {stdout}",
        );
    }
}

#[test]
fn loom_msg_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["msg"]));
}

#[test]
fn loom_todo_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["todo"]));
}

#[test]
fn loom_note_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["note"]));
}

#[test]
fn loom_note_set_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["note", "set"]));
}

#[test]
fn loom_note_add_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["note", "add"]));
}

#[test]
fn loom_note_clear_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["note", "clear"]));
}

#[test]
fn loom_note_list_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["note", "list"]));
}

#[test]
fn loom_note_rm_help_snapshot() {
    insta::assert_snapshot!(loom_help(&["note", "rm"]));
}
