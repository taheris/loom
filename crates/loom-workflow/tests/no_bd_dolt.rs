//! Source-level guard against `bd dolt`-shaped state-sync paths.
//!
//! Containers reach the authoritative Dolt store via the bind-mounted Dolt
//! socket (see `specs/harness.md` § "Container-to-host plumbing"); a
//! per-bead `bd dolt push` / `bd dolt pull` from inside the driver — or a
//! `BdClient::dolt_*` typed wrapper — would defeat that design by spawning
//! the bd CLI's dolt subcommand from the workflow layer rather than relying
//! on the socket. The tests in this file pin both halves of the contract:
//!
//! - `BdClient`'s public surface declares no `dolt_push` / `dolt_pull`
//!   method (a typed wrapper would be the natural foothold for accidental
//!   adoption);
//! - no `loom-workflow` source file (production OR test) bakes the literal
//!   shell argv prefix `bd dolt` into a subprocess invocation.
//!
//! Both directions are needed: removing a typed `dolt_push` does not stop a
//! workflow path from shelling out directly; conversely, a stray
//! `BdClient::dolt_push` could re-introduce the pattern even if no current
//! caller invokes it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(2)
        .expect("workspace root above crates/loom-workflow")
        .to_path_buf()
}

/// `BdClient`'s typed surface lives in
/// `crates/loom-driver/src/bd/client.rs`. The presence of a method matching
/// `fn dolt_push` or `fn dolt_pull` (sync OR async, with or without `pub`)
/// would imply the workflow has a structured affordance for spawning the
/// `bd dolt` subcommand — exactly the foothold the spec rules out.
#[test]
fn bd_client_exposes_no_dolt_push_or_dolt_pull_method() {
    let path = workspace_root().join("crates/loom-driver/src/bd/client.rs");
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    for needle in ["fn dolt_push", "fn dolt_pull"] {
        assert!(
            !body.contains(needle),
            "`{}` in {} would expose a typed wrapper for `bd dolt {}` — \
             containers must reach Dolt only via the bind-mounted socket",
            needle,
            path.display(),
            needle.trim_start_matches("fn dolt_"),
        );
    }
}

/// No file under `crates/loom-workflow/` may bake `"bd dolt"` into a
/// subprocess argv. The two patterns that would land such a call are:
///
/// - `Command::new("bd")` followed by `.arg("dolt")` or `.args([..."dolt"`
/// - A string literal `"bd dolt …"` passed to a shell-style runner
///
/// Mentions of the pattern as documentation — comment lines AND text
/// inside markdown-style backticks (e.g. assertion messages explaining
/// the rule) — are allowed: those are evidence the rule was considered,
/// not a subprocess call site. The test flags any non-documentation
/// occurrence so a real call site would surface here.
#[test]
fn loom_workflow_never_spawns_bd_dolt_subprocess() {
    let workflow_src = workspace_root().join("crates/loom-workflow");
    let mut offenders: Vec<String> = Vec::new();

    for entry in WalkDir::new(&workflow_src)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        if path.ends_with("tests/no_bd_dolt.rs") {
            continue;
        }
        let body = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for (lineno, line) in body.lines().enumerate() {
            if !line.contains("bd dolt") {
                continue;
            }
            if is_documentation_mention(line) {
                continue;
            }
            offenders.push(format!(
                "{}:{}: {}",
                path.display(),
                lineno + 1,
                line.trim(),
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "loom-workflow must not spawn `bd dolt …`; containers reach Dolt via \
         the bind-mounted socket. Offenders:\n{}",
        offenders.join("\n"),
    );
}

/// True if every `bd dolt` occurrence on `line` is documentation: either
/// the line starts with a Rust line/doc comment, or each occurrence sits
/// between markdown-style backticks (typical for assertion messages that
/// explain *why* the call would be wrong without making it).
fn is_documentation_mention(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("///") || trimmed.starts_with("//!") || trimmed.starts_with("//") {
        return true;
    }
    let mut idx = 0;
    while let Some(found) = line[idx..].find("bd dolt") {
        let abs = idx + found;
        let pre = &line[..abs];
        let post = &line[abs..];
        let backticks_before = pre.chars().filter(|c| *c == '`').count();
        let in_backticks = backticks_before % 2 == 1 && post.contains('`');
        if !in_backticks {
            return false;
        }
        idx = abs + "bd dolt".len();
    }
    true
}
