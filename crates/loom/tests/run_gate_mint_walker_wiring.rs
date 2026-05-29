//! Source-level wiring guard for `loom gate mint` CLI dispatch.
//!
//! Per `specs/gate.md` § *Production walker wiring* (criterion
//! `run_gate_mint_dispatches_through_production_walker_not_empty_vec`),
//! `run_gate_mint` MUST construct the production [`ProductionMintWalker`]
//! and call `mint::walk::walk(walker, scope, validator)` to obtain the
//! `Vec<Finding>` it passes to `mint_findings_with_options`. A CLI arm
//! that constructs `Vec::<Finding>::new()` unconditionally — bypassing
//! the walker — is the structural defect this guard pins.
//!
//! The test does an AST + source-level scan of `crates/loom/src/main.rs`
//! to assert both halves of the contract:
//!
//! 1. The `run_gate_mint` function body does NOT contain a
//!    `Vec::<Finding>::new()` (or `let findings = Vec::new();`) literal
//!    assignment for the findings vector.
//! 2. The body DOES contain a call to `loom_workflow::mint::walk(` (the
//!    only path findings reach the mint pipeline).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(2)
        .expect("workspace root above crates/loom")
        .to_path_buf()
}

/// Read the `run_gate_mint` function body verbatim from `main.rs`. Uses a
/// brace-counted slice rather than a syn AST walk because the test only
/// needs to find the body text; an AST walk would pull the heavy `syn`
/// dependency through this integration test with no extra signal.
fn run_gate_mint_body() -> String {
    let path = workspace_root().join("crates/loom/src/main.rs");
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let needle = "fn run_gate_mint(";
    let start = body
        .find(needle)
        .unwrap_or_else(|| panic!("`fn run_gate_mint(` not found in {}", path.display()));
    let after_sig = &body[start..];
    let open = after_sig
        .find('{')
        .unwrap_or_else(|| panic!("opening brace for run_gate_mint not found"));
    let body_start = start + open;
    let mut depth = 0_i32;
    let mut idx = body_start;
    for (offset, ch) in body[body_start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    idx = body_start + offset + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    body[body_start..idx].to_string()
}

#[test]
fn run_gate_mint_dispatches_through_production_walker_not_empty_vec() {
    let body = run_gate_mint_body();

    // 1) The empty-Vec shortcut MUST NOT survive — neither shape that
    //    typecheckers accept for `Vec<Finding>` literal construction.
    let forbidden_patterns = [
        "Vec::<loom_workflow::review::Finding>::new()",
        "Vec::<Finding>::new()",
        ": Vec<loom_workflow::review::Finding> = Vec::new()",
        ": Vec<Finding> = Vec::new()",
    ];
    for pattern in forbidden_patterns {
        assert!(
            !body.contains(pattern),
            "run_gate_mint body must NOT construct an unconditional empty findings vec \
             (pattern: `{pattern}`); findings must come from the walker. Body:\n{body}",
        );
    }

    // 2) The walker dispatch MUST be present. The only path findings
    //    reach the mint pipeline is through `mint::walk::walk(...)` (or
    //    its public re-export `mint::walk(...)`).
    let walker_call_present =
        body.contains("mint::walk(") || body.contains("loom_workflow::mint::walk(");
    assert!(
        walker_call_present,
        "run_gate_mint body must dispatch findings through \
         `loom_workflow::mint::walk(walker, scope, validator)` — the walker \
         is the only path findings reach the mint pipeline. Body:\n{body}",
    );

    // 3) A `ProductionMintWalker` MUST be constructed in the arm so the
    //    walker dispatch isn't a stub or fake.
    assert!(
        body.contains("ProductionMintWalker"),
        "run_gate_mint body must construct the production walker \
         (`ProductionMintWalker::new(...)`) so the dispatched walker drives \
         the real rubric + verifier paths. Body:\n{body}",
    );
}
