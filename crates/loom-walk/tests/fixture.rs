//! Integration tests for the `loom-walk` binary: dispatcher contract +
//! per-walk pass/fail fixtures.
//!
//! Each registered walk gets a pair of subprocess tests: synthesise
//! source under `tempfile::tempdir`, set CWD + `LOOM_FILES` on the
//! invocation, and assert verdict + exit code per the verifier-runner
//! contract in `specs/gate.md`.

#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// Invoke the built `loom-walk` binary with argv, CWD, and `LOOM_FILES`.
/// The dispatcher's contract is process-shaped (env in, JSON on
/// stdout, exit code) so subprocess invocation is the test surface.
fn invoke(args: &[&str], cwd: Option<&Path>, loom_files: Option<&str>) -> Output {
    let bin = env!("CARGO_BIN_EXE_loom-walk");
    let mut cmd = Command::new(bin);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    match loom_files {
        Some(value) => {
            cmd.env("LOOM_FILES", value);
        }
        None => {
            cmd.env_remove("LOOM_FILES");
        }
    }
    cmd.output().unwrap()
}

/// Build a minimal workspace tree (`Cargo.toml` with the marker, plus
/// the crates the caller seeds) under a tempdir so the walks'
/// `workspace_root()` detection points at the tempdir.
fn make_workspace() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let cargo = "[workspace]\n\
                 resolver = \"3\"\n\
                 members = [\"crates/loom-driver\"]\n\
                 \n\
                 [workspace.package]\n\
                 edition = \"2024\"\n";
    std::fs::write(dir.path().join("Cargo.toml"), cargo).unwrap();
    dir
}

fn seed(root: &Path, rel: &str, body: &str) -> PathBuf {
    let full = root.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full, body).unwrap();
    full
}

fn parse_verdict(out: &Output) -> (Value, i32) {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "stdout was not JSON: {e}\nstdout={stdout}\nstderr={}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (v, out.status.code().unwrap())
}

fn assert_pass(out: &Output) {
    let (v, code) = parse_verdict(out);
    assert!(
        v["pass"].as_bool().unwrap(),
        "expected pass, got {v:?}; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 0, "expected exit 0, got {code}");
}

fn assert_fail(out: &Output, evidence_contains: &str) {
    let (v, code) = parse_verdict(out);
    assert!(
        !v["pass"].as_bool().unwrap(),
        "expected fail, got {v:?}; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(code, 1, "expected exit 1, got {code}");
    let evidence = v["evidence"].as_str().unwrap();
    assert!(
        evidence.contains(evidence_contains),
        "evidence missing fragment `{evidence_contains}`:\n{evidence}"
    );
}

// ---------------------------------------------------------------------------
// Dispatcher contract
// ---------------------------------------------------------------------------

#[test]
fn missing_walk_name_exits_two_and_names_available_walks() {
    let out = invoke(&[], None, None);
    let code = out.status.code().unwrap();
    assert_eq!(code, 2, "stderr={}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage: loom-walk"), "stderr={stderr}");
    assert!(
        stderr.contains("available walks"),
        "must enumerate available walks; stderr={stderr}"
    );
}

#[test]
fn unknown_walk_name_exits_two_and_names_the_walk_and_available_set() {
    let out = invoke(&["definitely_not_a_walk"], None, None);
    let code = out.status.code().unwrap();
    assert_eq!(code, 2, "stderr={}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown walk"),
        "must say 'unknown walk'; stderr={stderr}"
    );
    assert!(
        stderr.contains("definitely_not_a_walk"),
        "must echo the offending walk name; stderr={stderr}"
    );
    assert!(
        stderr.contains("available walks"),
        "must enumerate available walks; stderr={stderr}"
    );
}

/// Multi-arg invocation: every positional arg gets a verdict line in
/// argv order, with the `target` field set so the gate's `json-lines`
/// parser can map each row back to its annotation.
#[test]
fn multi_arg_invocation_emits_one_target_verdict_line_per_name_in_argv_order() {
    let ws = make_workspace();
    // Seed the inputs each walk needs to pass. Both walks scan crate
    // manifests, so the harness is independent of which two walks we
    // pick — they just need to coexist and pass on the same tree.
    seed(
        ws.path(),
        "crates/loom-events/Cargo.toml",
        "[package]\nname=\"loom-events\"\n\n[dependencies]\nfutures-core = \"0.3\"\nserde = \"1\"\nserde_json = \"1\"\nthiserror = \"2\"\n",
    );
    seed(
        ws.path(),
        "crates/loom-render/Cargo.toml",
        "[package]\nname=\"loom-render\"\n\n[dependencies]\nloom-events = { workspace = true }\nserde_json = \"1\"\n",
    );
    let out = invoke(
        &["loom_events_minimal_deps", "loom_render_deps"],
        Some(ws.path()),
        None,
    );
    let code = out.status.code().unwrap();
    assert_eq!(
        code,
        0,
        "both pass → exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "one verdict line per arg; stdout={stdout}");

    let first: Value = serde_json::from_str(lines[0]).unwrap();
    let second: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(first["target"].as_str(), Some("loom_events_minimal_deps"));
    assert!(first["pass"].as_bool().unwrap());
    assert_eq!(second["target"].as_str(), Some("loom_render_deps"));
    assert!(second["pass"].as_bool().unwrap());
}

/// Multi-arg invocation with one failing walk: exit code mirrors
/// "any failed" semantics (exit 1), but every verdict line is still
/// emitted so the gate's parser can attribute each verdict back to
/// its annotation.
#[test]
fn multi_arg_invocation_exits_one_when_any_walk_fails_but_still_emits_all_lines() {
    let ws = make_workspace();
    // Pass for loom_events_minimal_deps.
    seed(
        ws.path(),
        "crates/loom-events/Cargo.toml",
        "[package]\nname=\"loom-events\"\n\n[dependencies]\nfutures-core = \"0.3\"\nserde = \"1\"\nserde_json = \"1\"\nthiserror = \"2\"\n",
    );
    // Fail for loom_render_deps — missing loom-events dep.
    seed(
        ws.path(),
        "crates/loom-render/Cargo.toml",
        "[package]\nname=\"loom-render\"\n\n[dependencies]\nserde_json = \"1\"\n",
    );
    let out = invoke(
        &["loom_events_minimal_deps", "loom_render_deps"],
        Some(ws.path()),
        None,
    );
    assert_eq!(
        out.status.code().unwrap(),
        1,
        "any-fail → exit 1; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        2,
        "both verdict lines emitted; stdout={stdout}"
    );
    let pass_line: Value = serde_json::from_str(lines[0]).unwrap();
    let fail_line: Value = serde_json::from_str(lines[1]).unwrap();
    assert!(pass_line["pass"].as_bool().unwrap());
    assert!(!fail_line["pass"].as_bool().unwrap());
    assert!(
        fail_line["evidence"]
            .as_str()
            .unwrap()
            .contains("loom-events"),
        "failing verdict carries the missing-dep evidence: {fail_line:?}"
    );
}

// ---------------------------------------------------------------------------
// no_derive_from_on_newtypes (RS-8)
// ---------------------------------------------------------------------------

#[test]
fn no_derive_from_on_newtypes_pass() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-driver/src/lib.rs",
        "pub struct Id(pub u32);\n#[derive(Clone)]\npub struct Token(pub String);\n",
    );
    let out = invoke(
        &["no_derive_from_on_newtypes"],
        Some(ws.path()),
        Some(
            &ws.path()
                .join("crates/loom-driver/src/lib.rs")
                .to_string_lossy(),
        ),
    );
    assert_pass(&out);
}

#[test]
fn no_derive_from_on_newtypes_fail() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-driver/src/lib.rs",
        "#[derive(From)]\npub struct Id(pub u32);\n",
    );
    let out = invoke(
        &["no_derive_from_on_newtypes"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "derive(From)");
}

// ---------------------------------------------------------------------------
// no_types_or_error_files (RS-5)
// ---------------------------------------------------------------------------

#[test]
fn no_types_or_error_files_pass() {
    let ws = make_workspace();
    seed(ws.path(), "crates/loom-driver/src/lib.rs", "pub mod foo;\n");
    seed(
        ws.path(),
        "crates/loom-driver/src/foo/mod.rs",
        "pub mod types;\n",
    );
    let out = invoke(&["no_types_or_error_files"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn no_types_or_error_files_fail() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-driver/src/lib.rs",
        "pub mod types;\n",
    );
    seed(
        ws.path(),
        "crates/loom-driver/src/types.rs",
        "pub struct X;\n",
    );
    let out = invoke(&["no_types_or_error_files"], Some(ws.path()), None);
    assert_fail(&out, "types.rs");
}

// ---------------------------------------------------------------------------
// git_client_encapsulation
// ---------------------------------------------------------------------------

#[test]
fn git_client_encapsulation_pass() {
    let ws = make_workspace();
    let allowed = seed(
        ws.path(),
        "crates/loom-driver/src/git/mod.rs",
        "use gix::Repository;\npub fn check() -> Repository { todo!() }\n",
    );
    let outside = seed(
        ws.path(),
        "crates/loom-workflow/src/lib.rs",
        "pub fn nothing() {}\n",
    );
    let scope = format!(
        "{}:{}",
        allowed.to_string_lossy(),
        outside.to_string_lossy()
    );
    let out = invoke(&["git_client_encapsulation"], Some(ws.path()), Some(&scope));
    assert_pass(&out);
}

#[test]
fn git_client_encapsulation_fail() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-workflow/src/lib.rs",
        "use gix::Repository;\n",
    );
    let out = invoke(
        &["git_client_encapsulation"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "gix");
}

// ---------------------------------------------------------------------------
// single_event_channel
// ---------------------------------------------------------------------------

#[test]
fn single_event_channel_pass() {
    let ws = make_workspace();
    let sink = "pub struct LogSink { file: std::fs::File, renderer: Renderer }\n\
                impl LogSink {\n\
                    pub fn emit(&mut self, ev: Event) {\n\
                        self.file.write_all(b\"\").unwrap();\n\
                        self.renderer.render(ev);\n\
                    }\n\
                }\n";
    seed(ws.path(), "crates/loom-render/src/sink/mod.rs", sink);
    let out = invoke(&["single_event_channel"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn single_event_channel_fail_missing_emit() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-render/src/sink/mod.rs",
        "pub struct LogSink;\nimpl LogSink { pub fn new() -> Self { Self } }\n",
    );
    let out = invoke(&["single_event_channel"], Some(ws.path()), None);
    assert_fail(&out, "LogSink::emit method not found");
}

#[test]
fn single_event_channel_fail_emit_misses_file() {
    let ws = make_workspace();
    let sink = "pub struct LogSink { renderer: Renderer }\n\
                impl LogSink {\n\
                    pub fn emit(&mut self, ev: Event) { self.renderer.render(ev); }\n\
                }\n";
    seed(ws.path(), "crates/loom-render/src/sink/mod.rs", sink);
    let out = invoke(&["single_event_channel"], Some(ws.path()), None);
    assert_fail(&out, "self.file");
}

// ---------------------------------------------------------------------------
// newtype_identifiers (RS-7)
// ---------------------------------------------------------------------------

#[test]
fn newtype_identifiers_pass() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-driver/src/identifier/bead.rs",
        "pub struct BeadId(String);\npub struct ParseBeadIdError { pub raw: String }\n",
    );
    let out = invoke(&["newtype_identifiers"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn newtype_identifiers_fail() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-driver/src/identifier/bead.rs",
        "pub struct BeadId { inner: String }\n",
    );
    let out = invoke(&["newtype_identifiers"], Some(ws.path()), None);
    assert_fail(&out, "BeadId");
}

// ---------------------------------------------------------------------------
// template_context_structs
// ---------------------------------------------------------------------------

#[test]
fn template_context_structs_pass() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/loop.md",
        "body\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "use askama::Template;\n\
         #[derive(Template)]\n\
         #[template(path = \"loop.md\")]\n\
         pub struct LoopContext;\n",
    );
    let out = invoke(&["template_context_structs"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn template_context_structs_fail() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/loop.md",
        "body\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "pub struct Nothing;\n",
    );
    let out = invoke(&["template_context_structs"], Some(ws.path()), None);
    assert_fail(&out, "loop.md");
}

// ---------------------------------------------------------------------------
// no_hardcoded_tmp_paths (NFR #7)
// ---------------------------------------------------------------------------

// The forbidden prefix string is built at runtime via `concat!` so the
// fixture source itself doesn't carry the verbatim literal — the walk
// (and the legacy `loom/tests/style.rs`) self-scan would otherwise
// flag the fixture file.
const BANNED_PREFIX: &str = concat!("/", "tmp/");

#[test]
fn no_hardcoded_tmp_paths_pass() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-driver/tests/foo.rs",
        "#[test]\nfn ok() {\n    let d = tempfile::tempdir().unwrap();\n    let _ = d.path();\n}\n",
    );
    let out = invoke(
        &["no_hardcoded_tmp_paths"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn no_hardcoded_tmp_paths_fail() {
    let ws = make_workspace();
    let body = format!(
        "#[test]\nfn bad() {{\n    let p = \"{BANNED_PREFIX}sneaky\";\n    let _ = p;\n}}\n"
    );
    let target = seed(ws.path(), "crates/loom-driver/tests/foo.rs", &body);
    let out = invoke(
        &["no_hardcoded_tmp_paths"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    let needle = format!("{BANNED_PREFIX}sneaky");
    assert_fail(&out, &needle);
}

// ---------------------------------------------------------------------------
// no_thread_sleep
// ---------------------------------------------------------------------------

#[test]
fn no_thread_sleep_pass() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-driver/src/lib.rs",
        "pub fn ok() { let _ = std::time::Duration::from_secs(1); }\n",
    );
    let out = invoke(
        &["no_thread_sleep"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn no_thread_sleep_fail() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-driver/src/lib.rs",
        "pub fn bad() { std::thread::sleep(std::time::Duration::from_secs(1)); }\n",
    );
    let out = invoke(
        &["no_thread_sleep"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "thread::sleep");
}

// ---------------------------------------------------------------------------
// no_tokio_sleep_outside_clock
// ---------------------------------------------------------------------------

#[test]
fn no_tokio_sleep_outside_clock_pass_allowed_site() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-driver/src/clock/system.rs",
        "pub async fn sleep() { tokio::time::sleep(std::time::Duration::ZERO).await; }\n",
    );
    let out = invoke(
        &["no_tokio_sleep_outside_clock"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn no_tokio_sleep_outside_clock_fail() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-workflow/src/lib.rs",
        "pub async fn bad() { tokio::time::sleep(std::time::Duration::ZERO).await; }\n",
    );
    let out = invoke(
        &["no_tokio_sleep_outside_clock"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "tokio::time::sleep");
}

// ---------------------------------------------------------------------------
// no_tokio_timeout_outside_clock
// ---------------------------------------------------------------------------

#[test]
fn no_tokio_timeout_outside_clock_pass_allowed_site() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-driver/src/clock/system.rs",
        "pub async fn timeout<F: std::future::Future>(f: F) { let _ = tokio::time::timeout(std::time::Duration::ZERO, f).await; }\n",
    );
    let out = invoke(
        &["no_tokio_timeout_outside_clock"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn no_tokio_timeout_outside_clock_fail() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-workflow/src/lib.rs",
        "pub async fn bad() { let _ = tokio::time::timeout(std::time::Duration::ZERO, async {}).await; }\n",
    );
    let out = invoke(
        &["no_tokio_timeout_outside_clock"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "tokio::time::timeout");
}

// ---------------------------------------------------------------------------
// renderer_no_insta_dependency
// ---------------------------------------------------------------------------

const RENDERER_CARGO_OK: &str = "[package]\nname = \"loom-render\"\n\n[dependencies]\nserde = \"1\"\n\n[dev-dependencies]\ntempfile = \"3\"\n";

#[test]
fn renderer_no_insta_dependency_pass_no_cargo_dep_no_rs_use() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-render/Cargo.toml",
        RENDERER_CARGO_OK,
    );
    seed(
        ws.path(),
        "crates/loom-render/src/lib.rs",
        "pub fn render() -> String { String::new() }\n\
         #[cfg(test)]\n\
         mod tests {\n\
             #[test]\n\
             fn smoke() {\n\
                 assert!(super::render().is_empty());\n\
             }\n\
         }\n",
    );
    let out = invoke(&["renderer_no_insta_dependency"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn renderer_no_insta_dependency_fail_dev_dep_declared() {
    let ws = make_workspace();
    let cargo = "[package]\nname = \"loom-render\"\n\n[dev-dependencies]\ninsta = \"1\"\n";
    seed(ws.path(), "crates/loom-render/Cargo.toml", cargo);
    let out = invoke(&["renderer_no_insta_dependency"], Some(ws.path()), None);
    assert_fail(&out, "crates/loom-render/Cargo.toml");
}

#[test]
fn renderer_no_insta_dependency_fail_use_in_test() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-render/Cargo.toml",
        RENDERER_CARGO_OK,
    );
    seed(
        ws.path(),
        "crates/loom-render/src/renderer.rs",
        "#[cfg(test)]\n\
         mod tests {\n\
             use insta::assert_snapshot;\n\
             #[test]\n\
             fn snap() { assert_snapshot!(\"x\"); }\n\
         }\n",
    );
    let out = invoke(&["renderer_no_insta_dependency"], Some(ws.path()), None);
    assert_fail(&out, "crates/loom-render/src/renderer.rs");
}

#[test]
fn renderer_no_insta_dependency_ignores_other_crates() {
    let ws = make_workspace();
    // Renderer is clean.
    seed(
        ws.path(),
        "crates/loom-render/Cargo.toml",
        RENDERER_CARGO_OK,
    );
    // A different crate is allowed to use insta — only loom-render is in scope.
    seed(
        ws.path(),
        "crates/loom-templates/Cargo.toml",
        "[package]\nname = \"loom-templates\"\n\n[dev-dependencies]\ninsta = \"1\"\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/tests/snap.rs",
        "use insta::assert_snapshot;\n#[test] fn t() { assert_snapshot!(\"x\"); }\n",
    );
    let out = invoke(&["renderer_no_insta_dependency"], Some(ws.path()), None);
    assert_pass(&out);
}

// ---------------------------------------------------------------------------
// no_real_clock_outside_system_clock
// ---------------------------------------------------------------------------

#[test]
fn no_real_clock_outside_system_clock_pass_allowed_site() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-driver/src/clock/system.rs",
        "pub fn now() -> std::time::Instant { std::time::Instant::now() }\n",
    );
    let out = invoke(
        &["no_real_clock_outside_system_clock"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn no_real_clock_outside_system_clock_fail() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-workflow/src/lib.rs",
        "pub fn bad() -> std::time::Instant { std::time::Instant::now() }\n",
    );
    let out = invoke(
        &["no_real_clock_outside_system_clock"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "Instant::now");
}

// ---------------------------------------------------------------------------
// no_panics_in_production (RS-9)
// ---------------------------------------------------------------------------

#[test]
fn no_panics_in_production_pass_skips_cfg_test_blocks() {
    let ws = make_workspace();
    let body = format!(
        "pub fn ok() -> Result<u32, String> {{ Ok(0) }}\n\
         {cfg_test}\n\
         mod tests {{\n\
             #[test] fn t() {{ let _ = \"x\".{unwrap}(); }}\n\
         }}\n",
        cfg_test = concat!("#[", "cfg(test)]"),
        unwrap = "unwrap()",
    );
    let target = seed(ws.path(), "crates/loom-driver/src/lib.rs", &body);
    let out = invoke(
        &["no_panics_in_production"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn no_panics_in_production_fail_unwrap_in_production() {
    let ws = make_workspace();
    let body = "pub fn bad() -> u32 { std::env::var(\"X\").unwrap().len() as u32 }\n";
    let target = seed(ws.path(), "crates/loom-driver/src/lib.rs", body);
    let out = invoke(
        &["no_panics_in_production"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "unwrap");
}

#[test]
fn no_panics_in_production_fail_unreachable_in_production() {
    let ws = make_workspace();
    let body =
        "pub fn bad(x: Option<u32>) -> u32 { match x { Some(v) => v, None => unreachable!() } }\n";
    let target = seed(ws.path(), "crates/loom-driver/src/lib.rs", body);
    let out = invoke(
        &["no_panics_in_production"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "unreachable!");
}

#[test]
fn no_panics_in_production_pass_with_intermediate_attrs_on_cfg_test() {
    let ws = make_workspace();
    let body = format!(
        "{cfg_test}\n\
         {expect_attr}\n\
         mod tests {{\n\
             #[test] fn t() {{ {p}; }}\n\
         }}\n",
        cfg_test = concat!("#[", "cfg(test)]"),
        expect_attr = concat!("#[", "expect(clippy::expect_used, reason = \"tests\")]"),
        p = "panic!(\"x\")",
    );
    let target = seed(ws.path(), "crates/loom-driver/src/lib.rs", &body);
    let out = invoke(
        &["no_panics_in_production"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

// ---------------------------------------------------------------------------
// no_allow_dead_code (RS-10)
// ---------------------------------------------------------------------------

#[test]
fn no_allow_dead_code_pass_uses_expect() {
    let ws = make_workspace();
    let body = format!(
        "{expect_attr}\nstruct Unused;\n",
        expect_attr = concat!("#[", "expect(dead_code, reason = \"future use\")]"),
    );
    let target = seed(ws.path(), "crates/loom-driver/src/lib.rs", &body);
    let out = invoke(
        &["no_allow_dead_code"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn no_allow_dead_code_fail_uses_allow() {
    let ws = make_workspace();
    let body = format!(
        "{allow_attr}\nstruct Unused;\n",
        allow_attr = concat!("#[", "allow(dead_code)]"),
    );
    let target = seed(ws.path(), "crates/loom-driver/src/lib.rs", &body);
    let out = invoke(
        &["no_allow_dead_code"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "dead_code");
}

// ---------------------------------------------------------------------------
// loom_does_not_invoke_podman
// ---------------------------------------------------------------------------

#[test]
fn loom_does_not_invoke_podman_pass_doc_mention_ok() {
    let ws = make_workspace();
    let target = seed(
        ws.path(),
        "crates/loom-agent/src/pi/backend.rs",
        "//! drives the wrix wrapper which talks to podman under the hood\npub fn ok() {}\n",
    );
    let out = invoke(
        &["loom_does_not_invoke_podman"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn loom_does_not_invoke_podman_fail_direct_command_new() {
    let ws = make_workspace();
    let body = format!(
        "pub fn bad() {{ let _ = {cmd_new}\"run\"); }}\n",
        cmd_new = concat!("Command::new(\"", "podman\").arg("),
    );
    let target = seed(ws.path(), "crates/loom-agent/src/pi/backend.rs", &body);
    let out = invoke(
        &["loom_does_not_invoke_podman"],
        Some(ws.path()),
        Some(&target.to_string_lossy()),
    );
    assert_fail(&out, "podman");
}

// ---------------------------------------------------------------------------
// crate_structure_includes_loom_tune
// ---------------------------------------------------------------------------

const STRUCTURE_CRATE_NAMES: &[&str] = &[
    "loom",
    "loom-driver",
    "loom-events",
    "loom-llm",
    "loom-skills",
    "loom-tune",
    "loom-render",
    "loom-agent",
    "loom-direct-runner",
    "loom-gate",
    "loom-protocol",
    "loom-workflow",
    "loom-templates",
    "loom-test-support",
    "loom-walk",
];

const STRUCTURE_LIB_NAMES: &[&str] = &[
    "loom-driver",
    "loom-events",
    "loom-llm",
    "loom-skills",
    "loom-tune",
    "loom-render",
    "loom-agent",
    "loom-direct-runner",
    "loom-gate",
    "loom-protocol",
    "loom-workflow",
    "loom-templates",
    "loom-test-support",
    "loom-walk",
];

fn seed_full_crate_set(ws: &TempDir) {
    let members = STRUCTURE_CRATE_NAMES
        .iter()
        .map(|name| format!("\"crates/{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        ws.path().join("Cargo.toml"),
        format!("[workspace]\nresolver = \"3\"\nmembers = [{members}]\n"),
    )
    .unwrap();
    for name in STRUCTURE_CRATE_NAMES {
        seed(
            ws.path(),
            &format!("crates/{name}/Cargo.toml"),
            &format!("[package]\nname=\"{name}\"\n"),
        );
        for entry in structure_entries(name) {
            seed(
                ws.path(),
                &format!("crates/{name}/{entry}"),
                "pub fn ok() {}\n",
            );
        }
    }
}

fn structure_entries(name: &str) -> &'static [&'static str] {
    match name {
        "loom" | "loom-walk" => &["src/main.rs"],
        "loom-direct-runner" => &["src/lib.rs", "src/main.rs"],
        _ => &["src/lib.rs"],
    }
}

#[test]
fn crate_structure_includes_loom_tune_pass_all_target_crates_present() {
    let ws = make_workspace();
    seed_full_crate_set(&ws);
    let out = invoke(
        &["crate_structure_includes_loom_tune"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn crate_structure_includes_loom_tune_fail_missing_crate() {
    let ws = make_workspace();
    seed_full_crate_set(&ws);
    let _ = std::fs::remove_dir_all(ws.path().join("crates/loom-events"));
    let out = invoke(
        &["crate_structure_includes_loom_tune"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "loom-events");
}

#[test]
fn crate_structure_includes_loom_tune_fail_missing_loom_tune() {
    let ws = make_workspace();
    seed_full_crate_set(&ws);
    let _ = std::fs::remove_dir_all(ws.path().join("crates/loom-tune"));
    let out = invoke(
        &["crate_structure_includes_loom_tune"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "loom-tune");
}

#[test]
fn crate_structure_includes_loom_tune_fail_extra_workspace_member() {
    let ws = make_workspace();
    seed_full_crate_set(&ws);
    let mut cargo = std::fs::read_to_string(ws.path().join("Cargo.toml")).unwrap();
    cargo = cargo.replace("\"]\n", "\", \"crates/extra\"]\n");
    std::fs::write(ws.path().join("Cargo.toml"), cargo).unwrap();
    seed(
        ws.path(),
        "crates/extra/Cargo.toml",
        "[package]\nname=\"extra\"\n",
    );
    seed(ws.path(), "crates/extra/src/lib.rs", "pub fn ok() {}\n");
    let out = invoke(
        &["crate_structure_includes_loom_tune"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "crates/extra");
}

#[test]
fn crate_structure_includes_loom_tune_fail_missing_workspace_member() {
    let ws = make_workspace();
    seed_full_crate_set(&ws);
    let mut cargo = std::fs::read_to_string(ws.path().join("Cargo.toml")).unwrap();
    cargo = cargo.replace("\"crates/loom-walk\"", "");
    std::fs::write(ws.path().join("Cargo.toml"), cargo).unwrap();
    let out = invoke(
        &["crate_structure_includes_loom_tune"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "crates/loom-walk");
}

// ---------------------------------------------------------------------------
// workspace_edition
// ---------------------------------------------------------------------------

fn full_workspace_cargo() -> String {
    let mut s = String::from(
        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/loom-driver\"]\n\n\
         [workspace.package]\nedition = \"2024\"\n",
    );
    s.push('\n');
    s
}

fn seed_full_workspace(ws: &TempDir) {
    std::fs::write(ws.path().join("Cargo.toml"), full_workspace_cargo()).unwrap();
    seed(
        ws.path(),
        "crates/loom/Cargo.toml",
        "[package]\nedition.workspace = true\n[lints]\nworkspace = true\n",
    );
    seed(ws.path(), "crates/loom/src/main.rs", "fn main() {}\n");
    for name in STRUCTURE_LIB_NAMES {
        seed(
            ws.path(),
            &format!("crates/{name}/Cargo.toml"),
            "[package]\nedition.workspace = true\n[lints]\nworkspace = true\n",
        );
        seed(
            ws.path(),
            &format!("crates/{name}/src/lib.rs"),
            "pub fn ok() {}\n",
        );
    }
}

#[test]
fn workspace_edition_pass_root_and_members() {
    let ws = make_workspace();
    seed_full_workspace(&ws);
    let out = invoke(&["workspace_edition"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn workspace_edition_fail_member_missing_inherit() {
    let ws = make_workspace();
    seed_full_workspace(&ws);
    // Replace one member's manifest with one that does NOT inherit edition.
    std::fs::write(
        ws.path().join("crates/loom-driver/Cargo.toml"),
        "[package]\nedition = \"2024\"\n[lints]\nworkspace = true\n",
    )
    .unwrap();
    let out = invoke(&["workspace_edition"], Some(ws.path()), None);
    assert_fail(&out, "edition.workspace");
}

// ---------------------------------------------------------------------------
// workspace_deps_pinned
// ---------------------------------------------------------------------------

#[test]
fn workspace_deps_pinned_pass_required_deps_present() {
    let ws = make_workspace();
    let mut cargo = String::from(
        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/loom-driver\"]\n\n\
         [workspace.package]\nedition = \"2024\"\n\n[workspace.dependencies]\n",
    );
    for dep in [
        "tokio",
        "serde",
        "serde_json",
        "thiserror",
        "displaydoc",
        "anyhow",
        "tracing",
        "tracing-subscriber",
        "rusqlite",
        "toml",
        "askama",
        "clap",
        "gix",
        "fd-lock",
    ] {
        cargo.push_str(&format!("{dep} = \"1\"\n"));
    }
    std::fs::write(ws.path().join("Cargo.toml"), &cargo).unwrap();
    let out = invoke(&["workspace_deps_pinned"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn workspace_deps_pinned_fail_missing_required_dep() {
    let ws = make_workspace();
    // Default workspace Cargo has no [workspace.dependencies] section.
    let out = invoke(&["workspace_deps_pinned"], Some(ws.path()), None);
    assert_fail(&out, "[workspace.dependencies]");
}

// ---------------------------------------------------------------------------
// workspace_lints
// ---------------------------------------------------------------------------

#[test]
fn workspace_lints_pass_inheritance_present() {
    let ws = make_workspace();
    let cargo = "[workspace]\nresolver = \"3\"\nmembers = [\"crates/loom\"]\n\n\
                 [workspace.package]\nedition = \"2024\"\n\n\
                 [workspace.lints.rust]\nunused = \"warn\"\n\n\
                 [workspace.lints.clippy]\npanic = \"deny\"\n";
    std::fs::write(ws.path().join("Cargo.toml"), cargo).unwrap();
    seed(
        ws.path(),
        "crates/loom/Cargo.toml",
        "[package]\nedition.workspace = true\n[lints]\nworkspace = true\n",
    );
    seed(ws.path(), "crates/loom/src/main.rs", "fn main() {}\n");
    for name in STRUCTURE_LIB_NAMES {
        seed(
            ws.path(),
            &format!("crates/{name}/Cargo.toml"),
            "[package]\nedition.workspace = true\n[lints]\nworkspace = true\n",
        );
        seed(
            ws.path(),
            &format!("crates/{name}/src/lib.rs"),
            "pub fn ok() {}\n",
        );
    }
    let out = invoke(&["workspace_lints"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn workspace_lints_fail_member_missing_workspace_true() {
    let ws = make_workspace();
    let cargo = "[workspace]\nresolver = \"3\"\nmembers = [\"crates/loom\"]\n\n\
                 [workspace.package]\nedition = \"2024\"\n\n\
                 [workspace.lints.rust]\nunused = \"warn\"\n\n\
                 [workspace.lints.clippy]\npanic = \"deny\"\n";
    std::fs::write(ws.path().join("Cargo.toml"), cargo).unwrap();
    seed(
        ws.path(),
        "crates/loom/Cargo.toml",
        "[package]\nedition.workspace = true\n",
    );
    seed(ws.path(), "crates/loom/src/main.rs", "fn main() {}\n");
    for name in STRUCTURE_LIB_NAMES {
        seed(
            ws.path(),
            &format!("crates/{name}/Cargo.toml"),
            "[package]\nedition.workspace = true\n",
        );
        seed(
            ws.path(),
            &format!("crates/{name}/src/lib.rs"),
            "pub fn ok() {}\n",
        );
    }
    let out = invoke(&["workspace_lints"], Some(ws.path()), None);
    assert_fail(&out, "workspace = true");
}

// ---------------------------------------------------------------------------
// loom_events_minimal_deps
// ---------------------------------------------------------------------------

#[test]
fn loom_events_minimal_deps_pass_exactly_four_runtime_deps() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/Cargo.toml",
        "[package]\nname=\"loom-events\"\n\n[dependencies]\nfutures-core = \"0.3\"\nserde = \"1\"\nserde_json = \"1\"\nthiserror = \"2\"\n",
    );
    let out = invoke(&["loom_events_minimal_deps"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_events_minimal_deps_fail_extra_runtime_dep() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/Cargo.toml",
        "[package]\nname=\"loom-events\"\n\n[dependencies]\nfutures-core = \"0.3\"\nserde = \"1\"\nserde_json = \"1\"\nthiserror = \"2\"\nchrono = \"0.4\"\n",
    );
    let out = invoke(&["loom_events_minimal_deps"], Some(ws.path()), None);
    assert_fail(&out, "chrono");
}

// ---------------------------------------------------------------------------
// loom_events_is_leaf
// ---------------------------------------------------------------------------

#[test]
fn loom_events_is_leaf_pass_no_internal_deps() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/Cargo.toml",
        "[package]\nname=\"loom-events\"\n\n[dependencies]\nserde = \"1\"\n",
    );
    let out = invoke(&["loom_events_is_leaf"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_events_is_leaf_fail_depends_on_loom_driver() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/Cargo.toml",
        "[package]\nname=\"loom-events\"\n\n[dependencies]\nloom-driver = { workspace = true }\n",
    );
    let out = invoke(&["loom_events_is_leaf"], Some(ws.path()), None);
    assert_fail(&out, "loom-driver");
}

// ---------------------------------------------------------------------------
// loom_render_deps
// ---------------------------------------------------------------------------

#[test]
fn loom_render_deps_pass_depends_on_loom_events() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-render/Cargo.toml",
        "[package]\nname=\"loom-render\"\n\n[dependencies]\nloom-events = { workspace = true }\nserde_json = \"1\"\n",
    );
    let out = invoke(&["loom_render_deps"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_render_deps_fail_missing_loom_events() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-render/Cargo.toml",
        "[package]\nname=\"loom-render\"\n\n[dependencies]\nserde_json = \"1\"\n",
    );
    let out = invoke(&["loom_render_deps"], Some(ws.path()), None);
    assert_fail(&out, "loom-events");
}

#[test]
fn loom_render_deps_fail_depends_on_loom_workflow() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-render/Cargo.toml",
        "[package]\nname=\"loom-render\"\n\n[dependencies]\nloom-events = { workspace = true }\nloom-workflow = { workspace = true }\n",
    );
    let out = invoke(&["loom_render_deps"], Some(ws.path()), None);
    assert_fail(&out, "loom-workflow");
}

// ---------------------------------------------------------------------------
// phase_verdict_decide_called_from_production
// ---------------------------------------------------------------------------

#[test]
fn phase_verdict_decide_called_from_production_pass() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-workflow/src/loop/production.rs",
        "use crate::review::{decide};\npub fn run() { let _ = decide(&None, ()); }\n",
    );
    seed(
        ws.path(),
        "crates/loom-workflow/src/review/production.rs",
        "use super::phase_verdict::{decide};\npub fn review() { let _ = decide(&None, ()); }\n",
    );
    let out = invoke(
        &["phase_verdict_decide_called_from_production"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

/// rustfmt rewrites long `use foo::{...}` lists onto multiple lines.
/// The walker must follow the brace list across line breaks so the
/// production import shape survives `treefmt` without spuriously flagging
/// a missing `decide` import.
#[test]
fn phase_verdict_decide_called_from_production_pass_with_multiline_import() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-workflow/src/loop/production.rs",
        "use crate::review::{decide};\npub fn run() { let _ = decide(&None, ()); }\n",
    );
    seed(
        ws.path(),
        "crates/loom-workflow/src/review/production.rs",
        "use super::phase_verdict::{\n    GateInputs, PhaseVerdict, RecoveryCause, ReviewConcern, ReviewFlag, decide,\n};\npub fn review() { let _ = decide(&None, ()); }\n",
    );
    let out = invoke(
        &["phase_verdict_decide_called_from_production"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn phase_verdict_decide_called_from_production_fail_run_missing_call() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-workflow/src/loop/production.rs",
        "pub fn run() { /* inlined classifier here, no decide call */ }\n",
    );
    seed(
        ws.path(),
        "crates/loom-workflow/src/review/production.rs",
        "use super::phase_verdict::{decide};\npub fn review() { let _ = decide(&None, ()); }\n",
    );
    let out = invoke(
        &["phase_verdict_decide_called_from_production"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "loop/production.rs");
}

// ---------------------------------------------------------------------------
// tune_surface_conformance
// ---------------------------------------------------------------------------

const TUNE_SPEC: &str = "# Harness\n\n### Tune Modes\n\ncontract\n";

const TUNE_MAIN: &str = concat!(
    "enum Command {\n",
    "    Tune(TuneArgs),\n",
    "}\n",
    "struct TuneArgs { action: Option<TuneAction> }\n",
    "enum TuneAction {\n",
    "    Skill(TuneSurfaceArgs),\n",
    "    Phase(TuneSurfaceArgs),\n",
    "    Partial(TuneSurfaceArgs),\n",
    "    Checker,\n",
    "    All(TuneAllArgs),\n",
    "}\n",
    "struct TuneSurfaceArgs {\n",
    "    level: Option<TuneLevelArg>,\n",
    "    targets: Vec<String>,\n",
    "    #[arg(long, requires = \"level\")]\n",
    "    dry_run: bool,\n",
    "    #[arg(long, requires = \"level\")]\n",
    "    seed: Option<u64>,\n",
    "}\n",
    "struct TuneAllArgs {\n",
    "    level: Option<TuneLevelArg>,\n",
    "    #[arg(long, requires = \"level\")]\n",
    "    dry_run: bool,\n",
    "    #[arg(long, requires = \"level\")]\n",
    "    seed: Option<u64>,\n",
    "}\n",
    "enum TuneLevelArg { Fast, Run, Full }\n",
);

fn seed_tune_surface(ws: &TempDir, main_body: &str) {
    seed(ws.path(), "specs/harness.md", TUNE_SPEC);
    seed(ws.path(), "crates/loom/src/main.rs", main_body);
}

#[test]
fn tune_surface_conformance_pass_when_sync_absent_and_tune_shape_matches() {
    let ws = make_workspace();
    seed_tune_surface(&ws, TUNE_MAIN);
    let out = invoke(&["tune_surface_conformance"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn tune_surface_conformance_fail_when_sync_variant_present() {
    let ws = make_workspace();
    seed_tune_surface(
        &ws,
        &TUNE_MAIN.replace("Tune(TuneArgs),", "Sync,\n    Tune(TuneArgs),"),
    );
    let out = invoke(&["tune_surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "sync");
}

#[test]
fn tune_surface_conformance_fail_when_tune_missing() {
    let ws = make_workspace();
    seed_tune_surface(&ws, &TUNE_MAIN.replace("    Tune(TuneArgs),\n", ""));
    let out = invoke(&["tune_surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "loom tune");
}

#[test]
fn tune_surface_conformance_fail_when_plural_subcommand_present() {
    let ws = make_workspace();
    seed_tune_surface(
        &ws,
        &TUNE_MAIN.replace("Skill(TuneSurfaceArgs)", "Skills(TuneSurfaceArgs)"),
    );
    let out = invoke(&["tune_surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "skills");
}

#[test]
fn tune_surface_conformance_fail_when_all_accepts_targets() {
    let ws = make_workspace();
    seed_tune_surface(
        &ws,
        &TUNE_MAIN.replace(
            "struct TuneAllArgs {\n    level: Option<TuneLevelArg>,",
            "struct TuneAllArgs {\n    level: Option<TuneLevelArg>,\n    targets: Vec<String>,",
        ),
    );
    let out = invoke(&["tune_surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "must not accept target names");
}

#[test]
fn tune_surface_conformance_fail_when_dry_run_allowed_on_list_command() {
    let ws = make_workspace();
    seed_tune_surface(
        &ws,
        &TUNE_MAIN.replace(
            "#[arg(long, requires = \"level\")]\n    dry_run: bool,",
            "#[arg(long)]\n    dry_run: bool,",
        ),
    );
    let out = invoke(&["tune_surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "must require `level`");
}

// ---------------------------------------------------------------------------
// template_pinning_matrix
// ---------------------------------------------------------------------------

fn seed_pinning_matrix(ws: &TempDir, matrix_body: &str) {
    seed(
        ws.path(),
        "specs/templates.md",
        &format!(
            "# Loom Templates\n\n## Architecture\n\n### Pinning Policy\n\n{matrix_body}\n\n## Other\n"
        ),
    );
}

#[test]
fn template_pinning_matrix_pass_clean_matrix() {
    let ws = make_workspace();
    seed_pinning_matrix(
        &ws,
        "| Partial | `loop` |\n\
         |---|:-:|\n\
         | `context_pinning.md` | ✓ |\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/loop.md",
        "{% include \"partial/context_pinning.md\" %}\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/context_pinning.md",
        "ctx\n",
    );
    let out = invoke(&["template_pinning_matrix"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn template_pinning_matrix_fail_spec_marks_but_template_missing_include() {
    let ws = make_workspace();
    seed_pinning_matrix(
        &ws,
        "| Partial | `loop` |\n\
         |---|:-:|\n\
         | `style_rules.md` | ✓ |\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/loop.md",
        "no style_rules include here\n",
    );
    let out = invoke(&["template_pinning_matrix"], Some(ws.path()), None);
    assert_fail(&out, "style_rules.md");
}

#[test]
fn template_pinning_matrix_fail_template_includes_but_spec_blank() {
    let ws = make_workspace();
    seed_pinning_matrix(
        &ws,
        "| Partial | `loop` |\n\
         |---|:-:|\n\
         | `style_rules.md` |  |\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/loop.md",
        "{% include \"partial/style_rules.md\" %}\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/style_rules.md",
        "rules\n",
    );
    let out = invoke(&["template_pinning_matrix"], Some(ws.path()), None);
    assert_fail(&out, "marks the cell blank");
}

#[test]
fn template_pinning_matrix_accepts_pending_cells() {
    let ws = make_workspace();
    seed_pinning_matrix(
        &ws,
        "| Partial | `plan` | `todo` |\n\
         |---|:-:|:-:|\n\
         | `context_pinning.md` | ? | ~ |\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/plan.md",
        "pending addition not included yet\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/todo.md",
        "{% include \"partial/context_pinning.md\" %}\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/context_pinning.md",
        "ctx\n",
    );
    let out = invoke(&["template_pinning_matrix"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn pending_addition_marker_fires_when_template_now_includes() {
    let ws = make_workspace();
    seed_pinning_matrix(
        &ws,
        "| Partial | `plan` |\n\
         |---|:-:|\n\
         | `context_pinning.md` | ? |\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/plan.md",
        "{% include \"partial/context_pinning.md\" %}\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/context_pinning.md",
        "ctx\n",
    );
    let out = invoke(&["template_pinning_matrix"], Some(ws.path()), None);
    assert_fail(&out, "pending-marker-resolved");
}

#[test]
fn pending_removal_marker_fires_when_template_no_longer_includes() {
    let ws = make_workspace();
    seed_pinning_matrix(
        &ws,
        "| Partial | `plan` |\n\
         |---|:-:|\n\
         | `context_pinning.md` | ~ |\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/plan.md",
        "pending removal complete\n",
    );
    let out = invoke(&["template_pinning_matrix"], Some(ws.path()), None);
    assert_fail(&out, "pending-marker-resolved");
}

#[test]
fn template_pinning_matrix_resolves_transitive_includes() {
    let ws = make_workspace();
    // Spec marks `invariant_clash.md` ✓ for `plan`, and the template
    // pulls it in transitively via `plan_stage_rubric.md`.
    seed_pinning_matrix(
        &ws,
        "| Partial | `plan` |\n\
         |---|:-:|\n\
         | `plan_stage_rubric.md` | ✓ |\n\
         | `invariant_clash.md` | ✓ |\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/plan.md",
        "{% include \"partial/plan_stage_rubric.md\" %}\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/plan_stage_rubric.md",
        "rubric\n{% include \"partial/invariant_clash.md\" %}\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/invariant_clash.md",
        "clash\n",
    );
    let out = invoke(&["template_pinning_matrix"], Some(ws.path()), None);
    assert_pass(&out);
}

// ---------------------------------------------------------------------------
// surface_conformance
// ---------------------------------------------------------------------------

const LOGS_UX_TABLE: &str = concat!(
    "### Logs UX\n",
    "\n",
    "| Flag | Behavior |\n",
    "|------|----------|\n",
    "| `-f` / `--follow` | tail |\n",
    "| `--raw` | raw bytes |\n",
);

const COMMAND_ENUM_DEFAULT: &str = concat!(
    "enum Command {\n",
    "    Logs {\n",
    "        #[arg(long, short = 'f')]\n",
    "        follow: bool,\n",
    "        #[arg(long)]\n",
    "        raw: bool,\n",
    "    },\n",
    "    Inbox(InboxArgs),\n",
    "    Todo {\n",
    "        #[arg(long)]\n",
    "        since: Option<String>,\n",
    "    },\n",
    "}\n",
    "struct InboxArgs { action: Option<InboxAction> }\n",
    "struct InboxFilterArgs {\n",
    "    #[arg(long, short = 's')]\n",
    "    spec: Option<String>,\n",
    "    #[arg(long, short = 'k')]\n",
    "    kind: Option<String>,\n",
    "}\n",
    "enum InboxAction { List(InboxListArgs), View(InboxViewArgs), Chat(InboxChatArgs) }\n",
    "struct InboxListArgs { filters: InboxFilterArgs }\n",
    "struct InboxViewArgs {\n",
    "    number: Option<u32>,\n",
    "    #[arg(long, short = 'b')]\n",
    "    bead: Option<String>,\n",
    "    #[arg(long, short = 'p')]\n",
    "    proposal: Option<String>,\n",
    "}\n",
    "struct InboxChatArgs {\n",
    "    number: Option<u32>,\n",
    "    #[arg(long, short = 'b')]\n",
    "    bead: Option<String>,\n",
    "    #[arg(long, short = 'p')]\n",
    "    proposal: Option<String>,\n",
    "}\n",
);

fn seed_surface_spec(ws: &TempDir, fr1_body: &str) {
    seed_surface_spec_with(ws, fr1_body, LOGS_UX_TABLE);
}

fn seed_surface_spec_with(ws: &TempDir, fr1_body: &str, logs_section: &str) {
    let body = format!(
        "# Loom Harness\n\n{logs_section}\n## Requirements\n\n### Functional\n\n1. **Command set** — header\n\n{fr1_body}\n2. **Compiled templates** — sentinel\n",
    );
    seed(ws.path(), "specs/harness.md", &body);
}

fn seed_surface_main(ws: &TempDir, tuples_body: &str) {
    seed_surface_main_with(ws, tuples_body, COMMAND_ENUM_DEFAULT);
}

fn seed_surface_main_with(ws: &TempDir, tuples_body: &str, command_enum: &str) {
    let body = format!(
        "fn main() {{}}\n\n{command_enum}\nconst HELP_GROUPS: &[(&str, &[&str])] = &[\n{tuples_body}];\n",
    );
    seed(ws.path(), "crates/loom/src/main.rs", &body);
}

const SPEC_FR1_MINIMAL: &str = concat!(
    "   **Workflow** — group\n",
    "   - `loom plan` — text\n",
    "\n",
    "   **Inspection** — group\n",
    "   - `loom status` — text\n",
    "\n",
    "   **State** — group\n",
    "   - `loom init` — text\n",
    "\n",
    "   **Removed surface.** prose\n",
    "\n",
    "   | Surface | Removed because |\n",
    "   |---|---|\n",
    "   | `loom doctor` | because |\n",
    "\n",
);

const SPEC_FR1_TWO_WORKFLOW: &str = concat!(
    "   **Workflow** — group\n",
    "   - `loom plan` — text\n",
    "   - `loom todo` — text\n",
    "\n",
    "   **Inspection** — group\n",
    "   - `loom status` — text\n",
    "\n",
    "   **State** — group\n",
    "   - `loom init` — text\n",
    "\n",
    "   **Removed surface.** prose\n",
    "\n",
    "   | Surface | Removed because |\n",
    "   |---|---|\n",
    "   | `loom doctor` | because |\n",
    "\n",
);

const HELP_GROUPS_MINIMAL: &str = concat!(
    "    (\"Workflow\", &[\"plan\"]),\n",
    "    (\"Inspection\", &[\"status\"]),\n",
    "    (\"State\", &[\"init\"]),\n",
);

const INBOX_MODES_SECTION: &str = concat!(
    "### Inbox Modes\n",
    "\n",
    "| Mode | Invocation | Where it runs |\n",
    "|------|------------|---------------|\n",
    "| List | `loom inbox` / `loom inbox list` | host |\n",
    "| View by number | `loom inbox view <N>` | host |\n",
    "| View by bead | `loom inbox view -b <bead-id>` | host |\n",
    "| View by proposal | `loom inbox view -p <proposal-id>` | host |\n",
    "| Chat queue | `loom inbox chat` | container |\n",
    "| Chat by number | `loom inbox chat <N>` | container |\n",
    "| Chat by bead | `loom inbox chat -b <bead-id>` | container |\n",
    "| Chat by proposal | `loom inbox chat -p <proposal-id>` | container |\n",
    "\n",
    "| Flag | Argument | Purpose |\n",
    "|------|----------|---------|\n",
    "| `-s`, `--spec` | `<label>` | filter |\n",
    "| `-k`, `--kind` | `clarify|blocked|tune` | filter |\n",
    "| `-b`, `--bead` | `<bead-id>` | address |\n",
    "| `-p`, `--proposal` | `<proposal-id>` | address |\n",
);

#[test]
fn surface_conformance_pass_when_spec_and_binary_agree() {
    let ws = make_workspace();
    seed_surface_spec(&ws, SPEC_FR1_MINIMAL);
    seed_surface_main(&ws, HELP_GROUPS_MINIMAL);
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn surface_conformance_validates_inbox_modes() {
    let ws = make_workspace();
    let logs_and_inbox = format!("{LOGS_UX_TABLE}\n{INBOX_MODES_SECTION}");
    seed_surface_spec_with(&ws, SPEC_FR1_MINIMAL, &logs_and_inbox);
    seed_surface_main(&ws, HELP_GROUPS_MINIMAL);
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn surface_conformance_fail_when_inbox_subcommand_missing() {
    let ws = make_workspace();
    let logs_and_inbox = format!("{LOGS_UX_TABLE}\n{INBOX_MODES_SECTION}");
    seed_surface_spec_with(&ws, SPEC_FR1_MINIMAL, &logs_and_inbox);
    seed_surface_main_with(
        &ws,
        HELP_GROUPS_MINIMAL,
        &COMMAND_ENUM_DEFAULT.replace(", Chat(InboxChatArgs)", ""),
    );
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "chat");
}

#[test]
fn surface_conformance_fail_when_removed_inbox_flag_resurfaces() {
    let ws = make_workspace();
    seed_surface_spec(
        &ws,
        &SPEC_FR1_MINIMAL.replace(
            "   | `loom doctor` | because |",
            "   | `loom doctor` | because |\n   | `loom inbox -c` / `loom inbox --chat` | because |",
        ),
    );
    seed_surface_main_with(
        &ws,
        HELP_GROUPS_MINIMAL,
        &COMMAND_ENUM_DEFAULT.replace(
            "struct InboxArgs { action: Option<InboxAction> }",
            "struct InboxArgs { #[arg(long, short = 'c')] chat: bool, action: Option<InboxAction> }",
        ),
    );
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "--chat");
}

#[test]
fn surface_conformance_fail_when_removed_inbox_subcommand_resurfaces() {
    let ws = make_workspace();
    seed_surface_spec(
        &ws,
        &SPEC_FR1_MINIMAL.replace(
            "   | `loom doctor` | because |",
            "   | `loom doctor` | because |\n   | `loom inbox apply` | because |",
        ),
    );
    seed_surface_main_with(
        &ws,
        HELP_GROUPS_MINIMAL,
        &COMMAND_ENUM_DEFAULT.replace(
            "List(InboxListArgs), View(InboxViewArgs), Chat(InboxChatArgs)",
            "List(InboxListArgs), View(InboxViewArgs), Chat(InboxChatArgs), Apply",
        ),
    );
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "loom inbox apply");
}

#[test]
fn surface_conformance_fail_when_removed_top_level_flag_resurfaces() {
    let ws = make_workspace();
    seed_surface_spec(
        &ws,
        &SPEC_FR1_MINIMAL.replace(
            "   | `loom doctor` | because |",
            "   | `loom doctor` | because |\n   | `loom todo --since` | because |",
        ),
    );
    seed_surface_main(&ws, HELP_GROUPS_MINIMAL);
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "loom todo");
}

#[test]
fn surface_conformance_pass_when_removed_top_level_flag_stays_absent() {
    let ws = make_workspace();
    seed_surface_spec(
        &ws,
        &SPEC_FR1_MINIMAL.replace(
            "   | `loom doctor` | because |",
            "   | `loom doctor` | because |\n   | `loom todo --since` | because |",
        ),
    );
    seed_surface_main_with(
        &ws,
        HELP_GROUPS_MINIMAL,
        &COMMAND_ENUM_DEFAULT.replace(
            "    Todo {\n        #[arg(long)]\n        since: Option<String>,\n    },\n",
            "    Todo,\n",
        ),
    );
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn surface_conformance_fail_when_spec_lists_command_binary_does_not() {
    let ws = make_workspace();
    seed_surface_spec(&ws, SPEC_FR1_TWO_WORKFLOW);
    seed_surface_main(&ws, HELP_GROUPS_MINIMAL);
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "`todo`");
}

#[test]
fn surface_conformance_fail_when_binary_lists_command_spec_does_not() {
    let ws = make_workspace();
    seed_surface_spec(&ws, SPEC_FR1_MINIMAL);
    seed_surface_main(
        &ws,
        concat!(
            "    (\"Workflow\", &[\"plan\", \"todo\"]),\n",
            "    (\"Inspection\", &[\"status\"]),\n",
            "    (\"State\", &[\"init\"]),\n",
        ),
    );
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "`todo`");
}

#[test]
fn surface_conformance_fail_when_removed_surface_resurfaces() {
    let ws = make_workspace();
    seed_surface_spec(&ws, SPEC_FR1_MINIMAL);
    seed_surface_main(
        &ws,
        concat!(
            "    (\"Workflow\", &[\"plan\", \"doctor\"]),\n",
            "    (\"Inspection\", &[\"status\"]),\n",
            "    (\"State\", &[\"init\"]),\n",
        ),
    );
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "re-introduces `doctor`");
}

#[test]
fn surface_conformance_fail_when_group_order_differs() {
    let ws = make_workspace();
    seed_surface_spec(&ws, SPEC_FR1_MINIMAL);
    seed_surface_main(
        &ws,
        concat!(
            "    (\"State\", &[\"init\"]),\n",
            "    (\"Workflow\", &[\"plan\"]),\n",
            "    (\"Inspection\", &[\"status\"]),\n",
        ),
    );
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "group order");
}

#[test]
fn surface_conformance_fail_when_spec_logs_flag_missing_from_binary() {
    let ws = make_workspace();
    seed_surface_spec_with(
        &ws,
        SPEC_FR1_MINIMAL,
        concat!(
            "### Logs UX\n",
            "\n",
            "| Flag | Behavior |\n",
            "|------|----------|\n",
            "| `-f` / `--follow` | tail |\n",
            "| `--raw` | raw bytes |\n",
            "| `--ghost` | undeclared |\n",
        ),
    );
    seed_surface_main(&ws, HELP_GROUPS_MINIMAL);
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "`--ghost`");
}

#[test]
fn surface_conformance_fail_when_binary_logs_flag_missing_from_spec() {
    let ws = make_workspace();
    seed_surface_spec(&ws, SPEC_FR1_MINIMAL);
    seed_surface_main_with(
        &ws,
        HELP_GROUPS_MINIMAL,
        concat!(
            "enum Command {\n",
            "    Logs {\n",
            "        #[arg(long, short = 'f')]\n",
            "        follow: bool,\n",
            "        #[arg(long)]\n",
            "        raw: bool,\n",
            "        #[arg(long)]\n",
            "        ghost: bool,\n",
            "    },\n",
            "    Inbox(InboxArgs),\n",
            "}\n",
        ),
    );
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "`--ghost`");
}

#[test]
fn surface_conformance_long_attr_with_explicit_value_is_recognised() {
    let ws = make_workspace();
    seed_surface_spec(&ws, SPEC_FR1_MINIMAL);
    seed_surface_main_with(
        &ws,
        HELP_GROUPS_MINIMAL,
        concat!(
            "enum Command {\n",
            "    Logs {\n",
            "        #[arg(long = \"follow\", short = 'f')]\n",
            "        tail: bool,\n",
            "        #[arg(long)]\n",
            "        raw: bool,\n",
            "    },\n",
            "    Inbox(InboxArgs),\n",
            "}\n",
        ),
    );
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn surface_conformance_fail_when_per_group_command_order_differs() {
    let ws = make_workspace();
    seed_surface_spec(&ws, SPEC_FR1_TWO_WORKFLOW);
    seed_surface_main(
        &ws,
        concat!(
            "    (\"Workflow\", &[\"todo\", \"plan\"]),\n",
            "    (\"Inspection\", &[\"status\"]),\n",
            "    (\"State\", &[\"init\"]),\n",
        ),
    );
    let out = invoke(&["surface_conformance"], Some(ws.path()), None);
    assert_fail(&out, "per-group order differs");
}

// ---------------------------------------------------------------------------
// loom_templates_snapshots_no_crate_root_allow
// ---------------------------------------------------------------------------

#[test]
fn loom_templates_snapshots_no_crate_root_allow_pass() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/tests/snapshots.rs",
        "//! header doc.\n\nuse askama::Template;\n#[test] fn t() {}\n",
    );
    let out = invoke(
        &["loom_templates_snapshots_no_crate_root_allow"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn loom_templates_snapshots_no_crate_root_allow_fail() {
    let ws = make_workspace();
    let body = format!(
        "{allow_attr}\nuse askama::Template;\n#[test] fn t() {{}}\n",
        allow_attr = concat!("#![", "allow(clippy::unwrap_used)]"),
    );
    seed(ws.path(), "crates/loom-templates/tests/snapshots.rs", &body);
    let out = invoke(
        &["loom_templates_snapshots_no_crate_root_allow"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "crate-root `#![allow(...)]`");
}

// ---------------------------------------------------------------------------
// session_trait_in_loom_events
// ---------------------------------------------------------------------------

#[test]
fn session_trait_in_loom_events_pass() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/src/lib.rs",
        "pub trait Session {}\n",
    );
    let out = invoke(&["session_trait_in_loom_events"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn session_trait_in_loom_events_fail_when_missing() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/src/lib.rs",
        "pub fn x() {}\n",
    );
    let out = invoke(&["session_trait_in_loom_events"], Some(ws.path()), None);
    assert_fail(&out, "pub trait Session");
}

#[test]
fn session_trait_in_loom_events_fail_when_defined_in_driver() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/src/lib.rs",
        "pub trait Session {}\n",
    );
    seed(
        ws.path(),
        "crates/loom-driver/src/agent/session.rs",
        "pub trait Session {}\n",
    );
    let out = invoke(&["session_trait_in_loom_events"], Some(ws.path()), None);
    assert_fail(&out, "loom-driver");
}

// ---------------------------------------------------------------------------
// event_sink_in_loom_events
// ---------------------------------------------------------------------------

#[test]
fn event_sink_in_loom_events_pass() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/src/lib.rs",
        "pub trait EventSink {}\n\
         pub enum SessionCommand { Steer(String), Abort(String) }\n",
    );
    let out = invoke(&["event_sink_in_loom_events"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn event_sink_in_loom_events_fail_when_trait_missing() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/src/lib.rs",
        "pub enum SessionCommand { Steer(String), Abort(String) }\n",
    );
    let out = invoke(&["event_sink_in_loom_events"], Some(ws.path()), None);
    assert_fail(&out, "pub trait EventSink");
}

#[test]
fn event_sink_in_loom_events_fail_when_variant_missing() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/src/lib.rs",
        "pub trait EventSink {}\n\
         pub enum SessionCommand { Steer(String) }\n",
    );
    let out = invoke(&["event_sink_in_loom_events"], Some(ws.path()), None);
    assert_fail(&out, "Abort(String)");
}

#[test]
fn event_sink_in_loom_events_fail_when_variant_wrong_type() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/src/lib.rs",
        "pub trait EventSink {}\n\
         pub enum SessionCommand { Steer(u32), Abort(String) }\n",
    );
    let out = invoke(&["event_sink_in_loom_events"], Some(ws.path()), None);
    assert_fail(&out, "Steer(String)");
}

// ---------------------------------------------------------------------------
// public_contract_crates
// ---------------------------------------------------------------------------

fn seed_contract_manifest(ws: &TempDir, name: &str, declare: bool) {
    let body = if declare {
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\
             \n[package.metadata.loom]\npublic_contract = true\n",
        )
    } else {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n")
    };
    seed(ws.path(), &format!("crates/{name}/Cargo.toml"), &body);
}

#[test]
fn public_contract_crates_pass() {
    let ws = make_workspace();
    seed_contract_manifest(&ws, "loom-events", true);
    seed_contract_manifest(&ws, "loom-protocol", true);
    seed_contract_manifest(&ws, "loom-llm", true);
    seed_contract_manifest(&ws, "loom-templates", true);
    seed_contract_manifest(&ws, "loom-skills", true);
    let out = invoke(&["public_contract_crates"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn public_contract_crates_fail_when_missing_marker() {
    let ws = make_workspace();
    seed_contract_manifest(&ws, "loom-events", true);
    seed_contract_manifest(&ws, "loom-protocol", true);
    seed_contract_manifest(&ws, "loom-llm", false);
    seed_contract_manifest(&ws, "loom-templates", true);
    seed_contract_manifest(&ws, "loom-skills", true);
    let out = invoke(&["public_contract_crates"], Some(ws.path()), None);
    assert_fail(&out, "loom-llm");
}

#[test]
fn public_contract_crates_fail_when_manifest_missing() {
    let ws = make_workspace();
    seed_contract_manifest(&ws, "loom-events", true);
    seed_contract_manifest(&ws, "loom-protocol", true);
    seed_contract_manifest(&ws, "loom-llm", true);
    seed_contract_manifest(&ws, "loom-templates", true);
    let out = invoke(&["public_contract_crates"], Some(ws.path()), None);
    assert_fail(&out, "loom-skills");
}

#[test]
fn public_contract_crates_fail_when_extra_crate_declares_marker() {
    let ws = make_workspace();
    seed_contract_manifest(&ws, "loom-events", true);
    seed_contract_manifest(&ws, "loom-protocol", true);
    seed_contract_manifest(&ws, "loom-llm", true);
    seed_contract_manifest(&ws, "loom-templates", true);
    seed_contract_manifest(&ws, "loom-skills", true);
    seed_contract_manifest(&ws, "loom-gate", true);
    let out = invoke(&["public_contract_crates"], Some(ws.path()), None);
    assert_fail(&out, "unexpected");
    assert_fail(&out, "loom-gate");
}

// ---------------------------------------------------------------------------
// loom_templates_public_types
// ---------------------------------------------------------------------------

const TEMPLATES_PUBLIC_TYPES_BODY: &str = "pub struct PreviousFailure;\n\
     pub struct VerifierFailure;\n\
     pub enum BadWalk { Concern { payload: String } }\n\
     pub enum DriverNoticeCause { RetryExhausted }\n\
     pub struct WorkspaceRecovery;\n\
     pub struct RecoveryStash;\n\
     pub enum WorkspaceAlignment { Clean }\n\
     pub struct CriterionStatus;\n\
     pub enum EvidenceState { Missing }\n\
     pub struct CriterionId;\n\
     pub struct CriterionAnnotation;\n\
     pub struct PlanContext;\n\
     pub struct TodoContext;\n\
     pub struct LoopContext;\n\
     pub struct ReviewContext;\n\
     pub struct PinnedContext;\n";

#[test]
fn loom_templates_public_types_pass_when_all_exposed_directly() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        TEMPLATES_PUBLIC_TYPES_BODY,
    );
    let out = invoke(&["loom_templates_public_types"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_templates_public_types_pass_when_reexported_via_pub_use() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "pub mod inner;\n\
         pub use inner::{PreviousFailure, VerifierFailure, BadWalk, DriverNoticeCause, WorkspaceRecovery, RecoveryStash, WorkspaceAlignment, CriterionStatus, EvidenceState, CriterionId, CriterionAnnotation, PlanContext, TodoContext, LoopContext, ReviewContext, PinnedContext};\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/src/inner.rs",
        TEMPLATES_PUBLIC_TYPES_BODY,
    );
    let out = invoke(&["loom_templates_public_types"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_templates_public_types_fail_when_one_missing() {
    let ws = make_workspace();
    let body = "pub struct PreviousFailure;\n\
                pub struct VerifierFailure;\n\
                pub enum BadWalk { Concern { payload: String } }\n\
                pub enum DriverNoticeCause { RetryExhausted }\n\
                pub struct WorkspaceRecovery;\n\
                pub struct RecoveryStash;\n\
                pub enum WorkspaceAlignment { Clean }\n\
                pub struct CriterionStatus;\n\
                pub enum EvidenceState { Missing }\n\
                pub struct CriterionId;\n\
                pub struct CriterionAnnotation;\n\
                pub struct PlanContext;\n\
                pub struct TodoContext;\n\
                pub struct LoopContext;\n\
                pub struct ReviewContext;\n";
    seed(ws.path(), "crates/loom-templates/src/lib.rs", body);
    let out = invoke(&["loom_templates_public_types"], Some(ws.path()), None);
    assert_fail(&out, "PinnedContext");
}

#[test]
fn loom_templates_public_types_fail_when_private() {
    let ws = make_workspace();
    let body = "struct PreviousFailure;\n\
                struct VerifierFailure;\n\
                enum BadWalk { Concern { payload: String } }\n\
                enum DriverNoticeCause { RetryExhausted }\n\
                struct WorkspaceRecovery;\n\
                struct RecoveryStash;\n\
                enum WorkspaceAlignment { Clean }\n\
                struct CriterionStatus;\n\
                enum EvidenceState { Missing }\n\
                struct CriterionId;\n\
                struct CriterionAnnotation;\n\
                struct PlanContext;\n\
                struct TodoContext;\n\
                struct LoopContext;\n\
                struct ReviewContext;\n\
                struct PinnedContext;\n";
    seed(ws.path(), "crates/loom-templates/src/lib.rs", body);
    let out = invoke(&["loom_templates_public_types"], Some(ws.path()), None);
    assert_fail(&out, "PreviousFailure");
}

// ---------------------------------------------------------------------------
// loom_templates_public_partial_constants
// ---------------------------------------------------------------------------

fn seed_partial(ws: &TempDir, name: &str) {
    seed(
        ws.path(),
        &format!("crates/loom-templates/templates/partial/{name}"),
        "partial body\n",
    );
}

#[test]
fn loom_templates_public_partial_constants_pass_each_partial_has_const() {
    let ws = make_workspace();
    seed_partial(&ws, "scratchpad.md");
    seed_partial(&ws, "context_pinning.md");
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "pub const SCRATCHPAD_PARTIAL: &str = include_str!(\"../templates/partial/scratchpad.md\");\n\
         pub const CONTEXT_PINNING_PARTIAL: &str = include_str!(\"../templates/partial/context_pinning.md\");\n",
    );
    let out = invoke(
        &["loom_templates_public_partial_constants"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn loom_templates_public_partial_constants_fail_missing_const() {
    let ws = make_workspace();
    seed_partial(&ws, "scratchpad.md");
    seed_partial(&ws, "style_rules.md");
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "pub const SCRATCHPAD_PARTIAL: &str = include_str!(\"../templates/partial/scratchpad.md\");\n",
    );
    let out = invoke(
        &["loom_templates_public_partial_constants"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "style_rules.md");
}

#[test]
fn loom_templates_public_partial_constants_fail_when_const_is_private() {
    let ws = make_workspace();
    seed_partial(&ws, "scratchpad.md");
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "const SCRATCHPAD_PARTIAL: &str = include_str!(\"../templates/partial/scratchpad.md\");\n",
    );
    let out = invoke(
        &["loom_templates_public_partial_constants"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "scratchpad.md");
}

// ---------------------------------------------------------------------------
// loom_templates_workflow_templates_not_exported
// ---------------------------------------------------------------------------

#[test]
fn loom_templates_workflow_templates_not_exported_pass_when_no_const() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "pub const SCRATCHPAD_PARTIAL: &str = include_str!(\"../templates/partial/scratchpad.md\");\n",
    );
    let out = invoke(
        &["loom_templates_workflow_templates_not_exported"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn loom_templates_workflow_templates_not_exported_pass_when_only_derive() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/src/run.rs",
        "use askama::Template;\n\
         #[derive(Template)]\n\
         #[template(path = \"loop.md\")]\n\
         pub struct LoopContext;\n",
    );
    let out = invoke(
        &["loom_templates_workflow_templates_not_exported"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn loom_templates_workflow_templates_not_exported_fail_when_pub_const_loop() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "pub const LOOP_TEMPLATE: &str = include_str!(\"../templates/loop.md\");\n",
    );
    let out = invoke(
        &["loom_templates_workflow_templates_not_exported"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "loop.md");
}

#[test]
fn loom_templates_workflow_templates_not_exported_fail_when_pub_const_plan() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "pub const PLAN_TEMPLATE: &str = include_str!(\"../templates/plan.md\");\n",
    );
    let out = invoke(
        &["loom_templates_workflow_templates_not_exported"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "plan.md");
}

// ---------------------------------------------------------------------------
// loom_llm_deps
// ---------------------------------------------------------------------------

#[test]
fn loom_llm_deps_pass_when_only_loom_events_internal() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/Cargo.toml",
        "[package]\nname = \"loom-llm\"\n\
         [dependencies]\nloom-events = { workspace = true }\nserde = \"1\"\n",
    );
    let out = invoke(&["loom_llm_deps"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_llm_deps_fail_on_forbidden_internal_dep() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/Cargo.toml",
        "[package]\nname = \"loom-llm\"\n\
         [dependencies]\nloom-events = { workspace = true }\nloom-driver = { workspace = true }\n",
    );
    let out = invoke(&["loom_llm_deps"], Some(ws.path()), None);
    assert_fail(&out, "loom-driver");
}

#[test]
fn loom_llm_deps_fail_when_manifest_missing() {
    let ws = make_workspace();
    let out = invoke(&["loom_llm_deps"], Some(ws.path()), None);
    assert_fail(&out, "not readable");
}

// ---------------------------------------------------------------------------
// loom_llm_has_no_skill_registry_surface
// ---------------------------------------------------------------------------

#[test]
fn loom_llm_has_no_skill_registry_surface_pass_when_clean() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/Cargo.toml",
        "[package]\nname = \"loom-llm\"\n[dependencies]\nloom-events = { workspace = true }\n",
    );
    seed(
        ws.path(),
        "crates/loom-llm/src/lib.rs",
        "pub struct Conversation;\npub trait Tool {}\n",
    );
    let out = invoke(
        &["loom_llm_has_no_skill_registry_surface"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn loom_llm_has_no_skill_registry_surface_fail_on_loom_skills_dep() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/Cargo.toml",
        "[package]\nname = \"loom-llm\"\n[dependencies]\nloom-skills = { workspace = true }\n",
    );
    seed(
        ws.path(),
        "crates/loom-llm/src/lib.rs",
        "pub struct Conversation;\n",
    );
    let out = invoke(
        &["loom_llm_has_no_skill_registry_surface"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "loom-skills");
}

#[test]
fn loom_llm_has_no_skill_registry_surface_fail_on_public_skill_type() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/Cargo.toml",
        "[package]\nname = \"loom-llm\"\n[dependencies]\nloom-events = { workspace = true }\n",
    );
    seed(
        ws.path(),
        "crates/loom-llm/src/lib.rs",
        "pub struct SkillRegistry;\n",
    );
    let out = invoke(
        &["loom_llm_has_no_skill_registry_surface"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "SkillRegistry");
}

// ---------------------------------------------------------------------------
// loom_llm_public_surface
// ---------------------------------------------------------------------------

const LLM_PUBLIC_SURFACE_BODY: &str = "pub type BoxFuture<'a, T> = core::pin::Pin<Box<dyn core::future::Future<Output = T> + Send + 'a>>;\n\
     pub struct CompletionResponse;\n\
     pub trait LlmClient {\n\
         fn schema(&self) -> SchemaKind;\n\
         fn supports(&self, model: &ModelId) -> bool;\n\
         fn complete<'a>(&'a self, req: CompletionRequest) -> BoxFuture<'a, Result<CompletionResponse, LlmError>>;\n\
         fn complete_structured_raw<'a>(&'a self, req: CompletionRequest, schema: serde_json::Value, type_name: String) -> BoxFuture<'a, Result<String, LlmError>>;\n\
     }\n\
     pub trait LlmClientExt: LlmClient {\n\
         fn complete_structured<'a, T>(&'a self, req: CompletionRequest) -> BoxFuture<'a, Result<T, LlmError>> where T: Send + 'static;\n\
     }\n\
     pub struct CompletionRequest;\n\
     pub enum Message { Text }\n\
     pub enum MessageContent { Text(String) }\n\
     pub struct BinaryContent;\n\
     pub struct MimeType;\n\
     pub enum ModelId { Other(String) }\n\
     pub enum SchemaKind { Anthropic }\n\
     pub enum CacheControl { None }\n\
     pub trait Tool {}\n\
     pub struct Conversation;\n\
     pub enum LlmError { Timeout }\n\
     pub enum LlmCapability { MultimodalBinary }\n\
     pub enum RetryAdvice { Retryable }\n\
     pub struct ApiKey;\n";

#[test]
fn loom_llm_public_surface_pass_when_all_exposed_directly() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/lib.rs",
        LLM_PUBLIC_SURFACE_BODY,
    );
    let out = invoke(&["loom_llm_public_surface"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_llm_public_surface_pass_when_reexported_via_pub_use() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/lib.rs",
        "pub mod inner;\n\
         pub use inner::{LlmClient, LlmClientExt, CompletionRequest, Message, MessageContent, BinaryContent, MimeType, ModelId, SchemaKind, CacheControl, Tool, Conversation, LlmError, LlmCapability, RetryAdvice, ApiKey};\n",
    );
    seed(
        ws.path(),
        "crates/loom-llm/src/inner.rs",
        LLM_PUBLIC_SURFACE_BODY,
    );
    let out = invoke(&["loom_llm_public_surface"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_llm_public_surface_fail_when_one_missing() {
    let ws = make_workspace();
    let body = "pub trait LlmClient {}\n\
                pub struct CompletionRequest;\n\
                pub enum Message { Text }\n\
                pub enum ModelId { Other(String) }\n\
                pub enum CacheControl { None }\n\
                pub trait Tool {}\n";
    seed(ws.path(), "crates/loom-llm/src/lib.rs", body);
    let out = invoke(&["loom_llm_public_surface"], Some(ws.path()), None);
    assert_fail(&out, "Conversation");
}

#[test]
fn loom_llm_public_surface_fails_when_generic_structured_is_on_llmclient() {
    let ws = make_workspace();
    let body = LLM_PUBLIC_SURFACE_BODY.replace(
        "fn complete_structured_raw<'a>(&'a self, req: CompletionRequest, schema: serde_json::Value, type_name: String) -> BoxFuture<'a, Result<String, LlmError>>;",
        "fn complete_structured<T>(&self, req: CompletionRequest) -> Result<T, LlmError>;",
    );
    seed(ws.path(), "crates/loom-llm/src/lib.rs", &body);
    let out = invoke(&["loom_llm_public_surface"], Some(ws.path()), None);
    assert_fail(&out, "complete_structured_raw");
    assert_fail(&out, "LlmClient::complete_structured");
}

#[test]
fn loom_llm_public_surface_fails_when_raw_structured_is_generic() {
    let ws = make_workspace();
    let body = LLM_PUBLIC_SURFACE_BODY.replace(
        "fn complete_structured_raw<'a>(&'a self",
        "fn complete_structured_raw<'a, T>(&'a self",
    );
    seed(ws.path(), "crates/loom-llm/src/lib.rs", &body);
    let out = invoke(&["loom_llm_public_surface"], Some(ws.path()), None);
    assert_fail(&out, "type-erased");
}

// ---------------------------------------------------------------------------
// loom_llm_mime_type_no_raw_strings
// ---------------------------------------------------------------------------

#[test]
fn loom_llm_mime_type_no_raw_strings_pass_when_binary_apis_use_mimetype() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/request.rs",
        "pub struct MimeType;\n\
         pub struct BinaryContent;\n\
         impl BinaryContent { pub fn new(mime_type: MimeType, bytes: Vec<u8>) -> Self { Self } }\n\
         pub struct CompletionRequest;\n\
         impl CompletionRequest { pub fn user_binary(self, mime_type: MimeType, bytes: Vec<u8>) -> Self { self } }\n",
    );
    let out = invoke(
        &["loom_llm_mime_type_no_raw_strings"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn loom_llm_mime_type_no_raw_strings_fail_when_binary_api_uses_string() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/request.rs",
        "pub struct CompletionRequest;\n\
         impl CompletionRequest { pub fn user_binary(self, mime_type: impl Into<String>, bytes: Vec<u8>) -> Self { self } }\n",
    );
    let out = invoke(
        &["loom_llm_mime_type_no_raw_strings"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "unvalidated MIME string");
}

// ---------------------------------------------------------------------------
// loom_llm_multimodal_no_provider_wire_types
// ---------------------------------------------------------------------------

#[test]
fn loom_llm_multimodal_no_provider_wire_types_pass_with_owned_types() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/request.rs",
        "pub struct MimeType;\n\
         pub struct BinaryContent { pub mime_type: MimeType, pub bytes: Vec<u8> }\n\
         pub enum MessageContent { Binary(BinaryContent) }\n",
    );
    let out = invoke(
        &["loom_llm_multimodal_no_provider_wire_types"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn loom_llm_multimodal_no_provider_wire_types_fail_on_provider_wire_token() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/request.rs",
        "pub struct BinaryContent { pub wire: genai::chat::ContentPart }\n",
    );
    let out = invoke(
        &["loom_llm_multimodal_no_provider_wire_types"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "provider wire token");
}

// ---------------------------------------------------------------------------
// loom_llm_error_variant_set_multimodal
// ---------------------------------------------------------------------------

const LLM_MULTIMODAL_ERROR_BODY: &str = "#[non_exhaustive]\n\
     pub enum LlmError {\n\
         Transport(String), Timeout, RateLimited, AuthFailed, ProviderHttp,\n\
         MalformedJson, SchemaViolation, IncompatibleModel,\n\
         UnsupportedCapability, IncompatibleRequest, Provider,\n\
     }\n";

#[test]
fn loom_llm_error_variant_set_multimodal_pass() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/client/mod.rs",
        LLM_MULTIMODAL_ERROR_BODY,
    );
    let out = invoke(
        &["loom_llm_error_variant_set_multimodal"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn loom_llm_error_variant_set_multimodal_fail_when_variant_missing() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/client/mod.rs",
        "#[non_exhaustive]\npub enum LlmError { Timeout }\n",
    );
    let out = invoke(
        &["loom_llm_error_variant_set_multimodal"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "UnsupportedCapability");
}

// ---------------------------------------------------------------------------
// loom_llm_no_underlying_crate_reexports
// ---------------------------------------------------------------------------

#[test]
fn loom_llm_no_underlying_crate_reexports_pass_when_types_defined_in_crate() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/lib.rs",
        LLM_PUBLIC_SURFACE_BODY,
    );
    let out = invoke(
        &["loom_llm_no_underlying_crate_reexports"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn loom_llm_no_underlying_crate_reexports_fail_when_only_pub_use_from_external() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/lib.rs",
        "pub use multi_provider::{LlmClient, CompletionRequest, Message, ModelId, CacheControl, Tool, Conversation};\n",
    );
    let out = invoke(
        &["loom_llm_no_underlying_crate_reexports"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "Conversation");
}

// ---------------------------------------------------------------------------
// loom_llm_no_public_genai_types
// ---------------------------------------------------------------------------

#[test]
fn loom_llm_no_public_genai_types_pass_when_public_surface_avoids_genai() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/client/multi_provider.rs",
        "use std::sync::Arc;\n\
         use genai::Client as GenAi;\n\
         pub struct AnthropicClient { inner: Arc<GenAi> }\n\
         impl AnthropicClient {\n\
             pub fn new(api_key: ApiKey) -> Self { Self { inner: Arc::new(GenAi::default()) } }\n\
             pub fn api_key(&self) -> &ApiKey { todo!() }\n\
         }\n\
         pub struct ApiKey(String);\n",
    );
    let out = invoke(&["loom_llm_no_public_genai_types"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_llm_no_public_genai_types_fail_when_pub_fn_returns_genai_type() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/client/multi_provider.rs",
        "pub struct AnthropicClient;\n\
         impl AnthropicClient {\n\
             pub fn from_genai(inner: genai::Client) -> Self { Self }\n\
         }\n",
    );
    let out = invoke(&["loom_llm_no_public_genai_types"], Some(ws.path()), None);
    assert_fail(&out, "from_genai");
}

#[test]
fn loom_llm_no_public_genai_types_fail_when_pub_use_reexports_genai() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/lib.rs",
        "pub use genai::Client;\n",
    );
    let out = invoke(&["loom_llm_no_public_genai_types"], Some(ws.path()), None);
    assert_fail(&out, "pub use");
}

#[test]
fn loom_llm_no_public_genai_types_ignores_non_pub_use_of_genai() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/client/multi_provider.rs",
        "use genai::chat::{ChatRequest, ChatResponse};\n\
         pub struct Foo;\n\
         impl Foo { pub fn new() -> Self { Self } }\n",
    );
    let out = invoke(&["loom_llm_no_public_genai_types"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_llm_no_public_genai_types_ignores_cfg_test_items() {
    let ws = make_workspace();
    let body = format!(
        "pub struct Foo;\n\
         {cfg_test}\n\
         mod tests {{\n\
             pub fn helper() -> genai::Client {{ todo!() }}\n\
         }}\n",
        cfg_test = concat!("#[", "cfg(test)]"),
    );
    seed(ws.path(), "crates/loom-llm/src/lib.rs", &body);
    let out = invoke(&["loom_llm_no_public_genai_types"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_llm_no_public_genai_types_fail_when_pub_trait_method_mentions_genai() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/client/mod.rs",
        "pub trait LlmClient {\n\
             fn underlying(&self) -> &genai::Client;\n\
         }\n",
    );
    let out = invoke(&["loom_llm_no_public_genai_types"], Some(ws.path()), None);
    assert_fail(&out, "LlmClient::underlying");
}

#[test]
fn loom_llm_no_public_genai_types_fail_when_pub_struct_pub_field_holds_genai() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/client/multi_provider.rs",
        "pub struct AnthropicClient { pub inner: std::sync::Arc<genai::Client> }\n",
    );
    let out = invoke(&["loom_llm_no_public_genai_types"], Some(ws.path()), None);
    assert_fail(&out, "AnthropicClient");
}

// ---------------------------------------------------------------------------
// result_hasher_single_call_site
// ---------------------------------------------------------------------------

#[test]
fn result_hasher_single_call_site_pass_when_two_observer_files_reference() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/hasher.rs",
        "pub struct ResultHasher;\nimpl ResultHasher { pub fn hash(_b: &[u8]) -> [u8;16] { [0;16] } }\n",
    );
    seed(
        ws.path(),
        "crates/loom-llm/src/observer/doom_loop.rs",
        "use crate::hasher::ResultHasher;\nfn x() { let _ = ResultHasher::hash(b\"x\"); }\n",
    );
    seed(
        ws.path(),
        "crates/loom-llm/src/observer/duplicate_result.rs",
        "use crate::hasher::ResultHasher;\nfn y() { let _ = ResultHasher::hash(b\"y\"); }\n",
    );
    let out = invoke(&["result_hasher_single_call_site"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn result_hasher_single_call_site_fail_when_a_third_caller_appears() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/hasher.rs",
        "pub struct ResultHasher;\n",
    );
    seed(
        ws.path(),
        "crates/loom-llm/src/observer/doom_loop.rs",
        "use crate::hasher::ResultHasher; fn x() { let _ = ResultHasher; }\n",
    );
    seed(
        ws.path(),
        "crates/loom-llm/src/observer/duplicate_result.rs",
        "use crate::hasher::ResultHasher; fn y() { let _ = ResultHasher; }\n",
    );
    seed(
        ws.path(),
        "crates/loom-llm/src/extra.rs",
        "use crate::hasher::ResultHasher; fn z() { let _ = ResultHasher; }\n",
    );
    let out = invoke(&["result_hasher_single_call_site"], Some(ws.path()), None);
    assert_fail(&out, "expected exactly 2");
}

#[test]
fn result_hasher_single_call_site_fail_when_no_callers() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/hasher.rs",
        "pub struct ResultHasher;\n",
    );
    let out = invoke(&["result_hasher_single_call_site"], Some(ws.path()), None);
    assert_fail(&out, "expected exactly 2");
}

// ---------------------------------------------------------------------------
// observers_in_loom_llm
// ---------------------------------------------------------------------------

#[test]
fn observers_in_loom_llm_pass_when_both_defined_in_llm() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/observer.rs",
        "pub struct DoomLoopObserver;\npub struct DuplicateResultObserver;\n",
    );
    let out = invoke(&["observers_in_loom_llm"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn observers_in_loom_llm_fail_when_missing() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/observer.rs",
        "pub struct DoomLoopObserver;\n",
    );
    let out = invoke(&["observers_in_loom_llm"], Some(ws.path()), None);
    assert_fail(&out, "DuplicateResultObserver");
}

#[test]
fn observers_in_loom_llm_fail_when_duplicated_elsewhere() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-llm/src/observer.rs",
        "pub struct DoomLoopObserver;\npub struct DuplicateResultObserver;\n",
    );
    seed(
        ws.path(),
        "crates/loom-driver/src/dup.rs",
        "pub struct DoomLoopObserver;\n",
    );
    let out = invoke(&["observers_in_loom_llm"], Some(ws.path()), None);
    assert_fail(&out, "also defined outside loom-llm");
}

// ---------------------------------------------------------------------------
// loom_agent_deps
// ---------------------------------------------------------------------------

#[test]
fn loom_agent_deps_pass_when_required_present() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-agent/Cargo.toml",
        "[package]\nname = \"loom-agent\"\n\
         [dependencies]\nloom-events = { workspace = true }\nloom-llm = { workspace = true }\nloom-skills = { workspace = true }\n",
    );
    let out = invoke(&["loom_agent_deps"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_agent_deps_fail_when_loom_llm_missing() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-agent/Cargo.toml",
        "[package]\nname = \"loom-agent\"\n\
         [dependencies]\nloom-events = { workspace = true }\nloom-skills = { workspace = true }\n",
    );
    let out = invoke(&["loom_agent_deps"], Some(ws.path()), None);
    assert_fail(&out, "llm");
}

#[test]
fn loom_agent_deps_fail_when_loom_events_missing() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-agent/Cargo.toml",
        "[package]\nname = \"loom-agent\"\n\
         [dependencies]\nloom-llm = { workspace = true }\nloom-skills = { workspace = true }\n",
    );
    let out = invoke(&["loom_agent_deps"], Some(ws.path()), None);
    assert_fail(&out, "loom-events");
}

#[test]
fn loom_agent_deps_fail_when_loom_skills_missing() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-agent/Cargo.toml",
        "[package]\nname = \"loom-agent\"\n\
         [dependencies]\nloom-events = { workspace = true }\nloom-llm = { workspace = true }\n",
    );
    let out = invoke(&["loom_agent_deps"], Some(ws.path()), None);
    assert_fail(&out, "loom-skills");
}

// ---------------------------------------------------------------------------
// loom_skills_deps
// ---------------------------------------------------------------------------

#[test]
fn loom_skills_deps_pass_with_only_allowed_internal_dep() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-skills/Cargo.toml",
        "[package]\nname = \"loom-skills\"\n\
         [dependencies]\nloom-events = { workspace = true }\nserde = \"1\"\n",
    );
    let out = invoke(&["loom_skills_deps"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_skills_deps_fail_when_loom_events_missing() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-skills/Cargo.toml",
        "[package]\nname = \"loom-skills\"\n[dependencies]\nserde = \"1\"\n",
    );
    let out = invoke(&["loom_skills_deps"], Some(ws.path()), None);
    assert_fail(&out, "loom-events");
}

#[test]
fn loom_skills_deps_fail_on_runtime_dep() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-skills/Cargo.toml",
        "[package]\nname = \"loom-skills\"\n\
         [dependencies]\nloom-events = { workspace = true }\nloom-agent = { workspace = true }\n",
    );
    let out = invoke(&["loom_skills_deps"], Some(ws.path()), None);
    assert_fail(&out, "loom-agent");
}

// ---------------------------------------------------------------------------
// loom_tune_deps
// ---------------------------------------------------------------------------

#[test]
fn loom_tune_deps_pass_with_required_internal_deps() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-tune/Cargo.toml",
        "[package]\nname = \"loom-tune\"\n\
         [dependencies]\nloom-events = { workspace = true }\nloom-skills = { workspace = true }\ntoml = \"1\"\n",
    );
    let out = invoke(&["loom_tune_deps"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_tune_deps_fail_when_loom_skills_missing() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-tune/Cargo.toml",
        "[package]\nname = \"loom-tune\"\n\
         [dependencies]\nloom-events = { workspace = true }\ntoml = \"1\"\n",
    );
    let out = invoke(&["loom_tune_deps"], Some(ws.path()), None);
    assert_fail(&out, "loom-skills");
}

#[test]
fn loom_tune_deps_fail_on_workflow_dep() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-tune/Cargo.toml",
        "[package]\nname = \"loom-tune\"\n\
         [dependencies]\nloom-events = { workspace = true }\nloom-skills = { workspace = true }\nloom-workflow = { workspace = true }\n",
    );
    let out = invoke(&["loom_tune_deps"], Some(ws.path()), None);
    assert_fail(&out, "loom-workflow");
}

// ---------------------------------------------------------------------------
// session_trait_does_not_expose_typestate
// ---------------------------------------------------------------------------

#[test]
fn session_trait_does_not_expose_typestate_pass_when_clean() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/src/lib.rs",
        "pub trait Session {\n\
             fn prompt(&mut self, msg: &str);\n\
             fn steer(&mut self, msg: &str);\n\
         }\n",
    );
    let out = invoke(
        &["session_trait_does_not_expose_typestate"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn session_trait_does_not_expose_typestate_fail_when_idle_in_signature() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/src/lib.rs",
        "pub struct Idle;\n\
         pub trait Session {\n\
             fn prompt(&mut self, idle: Idle);\n\
         }\n",
    );
    let out = invoke(
        &["session_trait_does_not_expose_typestate"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "Idle");
}

#[test]
fn session_trait_does_not_expose_typestate_fail_when_agent_session_in_supertrait() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-events/src/lib.rs",
        "pub trait AgentSession {}\n\
         pub trait Session: AgentSession {\n\
             fn prompt(&mut self, msg: &str);\n\
         }\n",
    );
    let out = invoke(
        &["session_trait_does_not_expose_typestate"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "AgentSession");
}

// ---------------------------------------------------------------------------
// direct_tools_net_new
// ---------------------------------------------------------------------------

#[test]
fn direct_tools_net_new_pass_when_all_six_defined_locally() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-agent/src/direct/tools/mod.rs",
        "pub struct Read;\npub struct Write;\npub struct Edit;\n\
         pub struct Bash;\npub struct Grep;\npub struct Glob;\n",
    );
    let out = invoke(&["direct_tools_net_new"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn direct_tools_net_new_pass_when_split_across_files() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-agent/src/direct/tools/read.rs",
        "pub struct Read;\n",
    );
    seed(
        ws.path(),
        "crates/loom-agent/src/direct/tools/write.rs",
        "pub struct Write;\n",
    );
    seed(
        ws.path(),
        "crates/loom-agent/src/direct/tools/edit.rs",
        "pub struct Edit;\n",
    );
    seed(
        ws.path(),
        "crates/loom-agent/src/direct/tools/bash.rs",
        "pub struct Bash;\n",
    );
    seed(
        ws.path(),
        "crates/loom-agent/src/direct/tools/grep.rs",
        "pub struct Grep;\n",
    );
    seed(
        ws.path(),
        "crates/loom-agent/src/direct/tools/glob.rs",
        "pub struct Glob;\n",
    );
    let out = invoke(&["direct_tools_net_new"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn direct_tools_net_new_fail_when_a_tool_is_only_reexported() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-agent/src/direct/tools/mod.rs",
        "pub struct Read;\npub struct Write;\npub struct Edit;\n\
         pub struct Bash;\npub struct Grep;\n\
         pub use external::Glob;\n",
    );
    let out = invoke(&["direct_tools_net_new"], Some(ws.path()), None);
    assert_fail(&out, "Glob");
}

// ---------------------------------------------------------------------------
// loom_templates_deps
// ---------------------------------------------------------------------------

#[test]
fn loom_templates_deps_pass_when_only_loom_events_internal() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/Cargo.toml",
        "[package]\nname = \"loom-templates\"\n\
         [dependencies]\nloom-events = { workspace = true }\naskama = \"0.16\"\n",
    );
    let out = invoke(&["loom_templates_deps"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn loom_templates_deps_fail_when_loom_driver_present() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/Cargo.toml",
        "[package]\nname = \"loom-templates\"\n\
         [dependencies]\nloom-events = { workspace = true }\nloom-driver = { workspace = true }\n",
    );
    let out = invoke(&["loom_templates_deps"], Some(ws.path()), None);
    assert_fail(&out, "loom-driver");
}

// ---------------------------------------------------------------------------
// template_wire_format_restatement
// ---------------------------------------------------------------------------

#[test]
fn anti_drift_verifier_passes_canonical_partial_layout() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/findings_walk.md",
        "Emit `LOOM_FINDING:` per concern; terminate with \
         `LOOM_CONCERN: {\"summary\":\"...\"}`.\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/review.md",
        "Run the review. {% include \"partial/findings_walk.md\" %}\n",
    );
    let out = invoke(&["template_wire_format_restatement"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn anti_drift_verifier_fails_fixture_with_restated_wire_format() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/findings_walk.md",
        "Emit `LOOM_FINDING:` per concern.\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/review.md",
        "Run the review.\nThen emit LOOM_FINDING: {\"token\":...}.\n",
    );
    let out = invoke(&["template_wire_format_restatement"], Some(ws.path()), None);
    assert_fail(&out, "crates/loom-templates/templates/review.md:2");
}

#[test]
fn template_wire_format_restatement_passes_on_bare_prose_mentions() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/findings_walk.md",
        "Emit `LOOM_FINDING:` per concern.\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/review.md",
        "See the `LOOM_CONCERN` marker for how the walk terminates.\n",
    );
    let out = invoke(&["template_wire_format_restatement"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn template_wire_format_restatement_fails_on_loom_concern_outside_partial() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/findings_walk.md",
        "Emit `LOOM_FINDING:` per concern.\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/templates/loop.md",
        "Run the bead.\nTerminator: LOOM_CONCERN: {\"summary\":\"oops\"}.\n",
    );
    let out = invoke(&["template_wire_format_restatement"], Some(ws.path()), None);
    assert_fail(&out, "crates/loom-templates/templates/loop.md:2");
}

// ---------------------------------------------------------------------------
// no_inline_suppression_comment_contract
// ---------------------------------------------------------------------------

#[test]
fn no_inline_suppression_comment_contract_passes_top_level_toml_registry_path() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "loom.toml",
        "[[suppress]]\nid = \"v1:criterion:verifier-too-narrow:gate#x\"\nreason = \"false positive\"\n",
    );
    seed(
        ws.path(),
        "crates/loom-driver/src/config/mod.rs",
        "pub struct SuppressionConfig { pub id: Option<String>, pub hash: Option<String>, pub reason: String }\npub struct LoomConfig { pub suppress: Vec<SuppressionConfig> }\n",
    );
    seed(
        ws.path(),
        "crates/loom-workflow/src/suppression.rs",
        "fn suppression_matches(id: &str, finding: &str) -> bool { id == finding }\n",
    );
    let out = invoke(
        &["no_inline_suppression_comment_contract"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn no_inline_suppression_comment_contract_fails_on_comment_directive_scanner() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-workflow/src/suppression.rs",
        "fn from_comment(line: &str) -> bool {\n    line.trim_start().starts_with(\"//\") && line.contains(\"loom-suppress\")\n}\n",
    );
    let out = invoke(
        &["no_inline_suppression_comment_contract"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "crates/loom-workflow/src/suppression.rs:2");
}

#[test]
fn no_inline_suppression_comment_contract_honours_loom_files_scope() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-workflow/src/suppression.rs",
        "fn from_comment(line: &str) -> bool { line.contains(\"inline_suppress\") }\n",
    );
    let out = invoke(
        &["no_inline_suppression_comment_contract"],
        Some(ws.path()),
        Some("crates/loom-driver/src/config/mod.rs"),
    );
    assert_pass(&out);
}

#[test]
fn no_inline_suppression_comment_contract_ignores_cfg_test_fixtures() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-workflow/src/suppression.rs",
        "fn production_path() -> bool { false }\n#[cfg(test)]\nmod tests {\n    const FIXTURE: &str = \"// loom-suppress false positive\";\n}\n",
    );
    let out = invoke(
        &["no_inline_suppression_comment_contract"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn no_inline_suppression_comment_contract_checks_after_one_line_cfg_test_item() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-workflow/src/suppression.rs",
        "#[cfg(test)]\nfn fixture() {}\nfn from_comment(line: &str) -> bool { line.trim_start().starts_with(\"//\") && line.contains(\"loom-suppress\") }\n",
    );
    let out = invoke(
        &["no_inline_suppression_comment_contract"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "crates/loom-workflow/src/suppression.rs:3");
}

// ---------------------------------------------------------------------------
// templates_no_removed_surface
// ---------------------------------------------------------------------------

#[test]
fn templates_no_removed_surface_pass_when_renamed_tokens_only() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/example.md",
        "# Example\n\nRun `loom loop` and then `loom gate verify`.\n",
    );
    let out = invoke(&["templates_no_removed_surface"], Some(ws.path()), None);
    assert_pass(&out);
}

#[test]
fn templates_no_removed_surface_fail_when_loom_run_present() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/example.md",
        "# Example\n\nRun `loom run` to start.\n",
    );
    let out = invoke(&["templates_no_removed_surface"], Some(ws.path()), None);
    assert_fail(&out, "loom run");
}

#[test]
fn templates_no_removed_surface_fail_when_loom_check_present() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/partial/example.md",
        "# Example\n\nUse `loom check surface` to audit.\n",
    );
    let out = invoke(&["templates_no_removed_surface"], Some(ws.path()), None);
    assert_fail(&out, "loom check");
}

#[test]
fn templates_no_removed_surface_pass_on_word_extension() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/example.md",
        "# Example\n\nThe loom runner cycles through beads.\n",
    );
    let out = invoke(&["templates_no_removed_surface"], Some(ws.path()), None);
    assert_pass(&out);
}

// ---------------------------------------------------------------------------
// todo_contexts_carry_criterion_status
// ---------------------------------------------------------------------------

const TODO_CRITERION_STATUS_PASS_BODY: &str = "pub struct TodoContext { pub criterion_status: Vec<CriterionStatus> }\n\
     pub struct PlanContext { pub spec_path: String }\n\
     pub struct LoopContext { pub spec_path: String }\n\
     pub struct ReviewContext { pub spec_path: String }\n\
     pub struct InboxContext { pub scratchpad_path: String }\n";

#[test]
fn todo_contexts_carry_criterion_status_pass_when_only_todo_context_has_field() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        TODO_CRITERION_STATUS_PASS_BODY,
    );
    let out = invoke(
        &["todo_contexts_carry_criterion_status"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn todo_contexts_carry_criterion_status_fail_when_todo_missing_field() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "pub struct TodoContext { pub spec_path: String }\n\
         pub struct PlanContext { pub spec_path: String }\n\
         pub struct LoopContext { pub spec_path: String }\n\
         pub struct ReviewContext { pub spec_path: String }\n\
         pub struct InboxContext { pub scratchpad_path: String }\n",
    );
    let out = invoke(
        &["todo_contexts_carry_criterion_status"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "TodoContext` is missing field `criterion_status");
}

#[test]
fn todo_contexts_carry_criterion_status_fail_when_todo_field_has_wrong_type() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "pub struct TodoContext { pub criterion_status: String }\n\
         pub struct PlanContext { pub spec_path: String }\n\
         pub struct LoopContext { pub spec_path: String }\n\
         pub struct ReviewContext { pub spec_path: String }\n\
         pub struct InboxContext { pub scratchpad_path: String }\n",
    );
    let out = invoke(
        &["todo_contexts_carry_criterion_status"],
        Some(ws.path()),
        None,
    );
    assert_fail(
        &out,
        "TodoContext.criterion_status` has wrong type — expected `Vec<CriterionStatus>`",
    );
}

#[test]
fn todo_contexts_carry_criterion_status_fail_when_run_context_carries_field() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "pub struct TodoContext { pub criterion_status: Vec<CriterionStatus> }\n\
         pub struct PlanContext { pub spec_path: String }\n\
         pub struct LoopContext { pub criterion_status: Vec<CriterionStatus> }\n\
         pub struct ReviewContext { pub spec_path: String }\n\
         pub struct InboxContext { pub scratchpad_path: String }\n",
    );
    let out = invoke(
        &["todo_contexts_carry_criterion_status"],
        Some(ws.path()),
        None,
    );
    assert_fail(
        &out,
        "LoopContext` carries field `criterion_status` — only `TodoContext` may",
    );
}

#[test]
fn todo_contexts_carry_criterion_status_finds_structs_split_across_files() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/src/lib.rs",
        "pub mod todo;\npub mod plan;\npub mod run;\npub mod review;\npub mod inbox;\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/src/todo/mod.rs",
        "pub struct TodoContext { pub criterion_status: Vec<CriterionStatus> }\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/src/plan/mod.rs",
        "pub struct PlanContext { pub spec_path: String }\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/src/run/mod.rs",
        "pub struct LoopContext { pub spec_path: String }\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/src/review/mod.rs",
        "pub struct ReviewContext { pub spec_path: String }\n",
    );
    seed(
        ws.path(),
        "crates/loom-templates/src/inbox/mod.rs",
        "pub struct InboxContext { pub scratchpad_path: String }\n",
    );
    let out = invoke(
        &["todo_contexts_carry_criterion_status"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

// ---------------------------------------------------------------------------
// todo_template_uses_driver_created_work_epic
// ---------------------------------------------------------------------------

const TODO_DRIVER_WORK_EPIC_BODY: &str = "# Todo Decomposition\n\
     The driver has already created the driver-created work epic.\n\
     Work epic: {{ work_epic }}\n\
     TASK_ID=$(bd create --title=\"task\" --parent=\"{{ work_epic }}\" --silent)\n";

#[test]
fn todo_template_uses_driver_created_work_epic_passes_when_injected_epic_is_used() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/todo.md",
        TODO_DRIVER_WORK_EPIC_BODY,
    );
    let out = invoke(
        &["todo_template_uses_driver_created_work_epic"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn todo_template_uses_driver_created_work_epic_fails_when_template_missing() {
    let ws = make_workspace();
    let out = invoke(
        &["todo_template_uses_driver_created_work_epic"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "template not found");
}

#[test]
fn todo_template_uses_driver_created_work_epic_fails_when_work_epic_not_rendered() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/todo.md",
        "# Todo\nThe driver has already created the driver-created work epic.\n",
    );
    let out = invoke(
        &["todo_template_uses_driver_created_work_epic"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "does not render");
}

#[test]
fn todo_template_uses_driver_created_work_epic_fails_when_epic_creation_is_requested() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-templates/templates/todo.md",
        "# Todo\nThe driver has already created the driver-created work epic.\n{{ work_epic }}\n--parent=\"{{ work_epic }}\"\nbd create --type=epic\n",
    );
    let out = invoke(
        &["todo_template_uses_driver_created_work_epic"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "instructs the agent to create an epic");
}

// ---------------------------------------------------------------------------
// finding_no_duplicate_definitions
// ---------------------------------------------------------------------------

#[test]
fn finding_no_duplicate_definitions_passes_when_canonical_only() {
    let ws = make_workspace();
    let canonical = seed(
        ws.path(),
        "crates/loom-protocol/src/gate.rs",
        "pub struct Finding { pub token: u32 }\n\
         pub enum ConcernToken { Foo, Bar }\n",
    );
    let consumer = seed(
        ws.path(),
        "crates/loom-workflow/src/lib.rs",
        "pub use loom_protocol::gate::{Finding, ConcernToken};\n\
         pub fn use_finding(_f: Finding) {}\n",
    );
    let scope = format!(
        "{}:{}",
        canonical.to_string_lossy(),
        consumer.to_string_lossy()
    );
    let out = invoke(
        &["finding_no_duplicate_definitions"],
        Some(ws.path()),
        Some(&scope),
    );
    assert_pass(&out);
}

#[test]
fn finding_no_duplicate_definitions_fails_on_second_struct_declaration() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "crates/loom-protocol/src/gate.rs",
        "pub struct Finding { pub token: u32 }\n",
    );
    let dup = seed(
        ws.path(),
        "crates/loom-workflow/src/lib.rs",
        "pub struct Finding { pub other: String }\n",
    );
    let out = invoke(
        &["finding_no_duplicate_definitions"],
        Some(ws.path()),
        Some(&dup.to_string_lossy()),
    );
    assert_fail(&out, "crates/loom-workflow/src/lib.rs:1");
}

#[test]
fn finding_no_duplicate_definitions_fails_on_second_enum_declaration() {
    let ws = make_workspace();
    let dup = seed(
        ws.path(),
        "crates/loom-workflow/src/review.rs",
        "pub enum ConcernToken { Other }\n",
    );
    let out = invoke(
        &["finding_no_duplicate_definitions"],
        Some(ws.path()),
        Some(&dup.to_string_lossy()),
    );
    assert_fail(&out, "ConcernToken");
}

#[test]
fn finding_no_duplicate_definitions_pass_on_re_export_alone() {
    let ws = make_workspace();
    let consumer = seed(
        ws.path(),
        "crates/loom-workflow/src/review.rs",
        "pub use loom_protocol::gate::{Finding, ConcernToken};\n",
    );
    let out = invoke(
        &["finding_no_duplicate_definitions"],
        Some(ws.path()),
        Some(&consumer.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn finding_no_duplicate_definitions_fails_on_duplicate_finding_target_enum() {
    let ws = make_workspace();
    let dup = seed(
        ws.path(),
        "crates/loom-workflow/src/review.rs",
        "pub enum FindingTarget { Criterion }\n",
    );
    let out = invoke(
        &["finding_no_duplicate_definitions"],
        Some(ws.path()),
        Some(&dup.to_string_lossy()),
    );
    assert_fail(&out, "FindingTarget");
}

#[test]
fn finding_no_duplicate_definitions_fails_on_duplicate_walk_output_struct() {
    let ws = make_workspace();
    let dup = seed(
        ws.path(),
        "crates/loom-workflow/src/review.rs",
        "pub struct WalkOutput { findings: Vec<u32> }\n",
    );
    let out = invoke(
        &["finding_no_duplicate_definitions"],
        Some(ws.path()),
        Some(&dup.to_string_lossy()),
    );
    assert_fail(&out, "WalkOutput");
}

#[test]
fn finding_no_duplicate_definitions_fails_on_duplicate_bad_walk_enum() {
    let ws = make_workspace();
    let dup = seed(
        ws.path(),
        "crates/loom-workflow/src/review.rs",
        "pub enum BadWalk { Other }\n",
    );
    let out = invoke(
        &["finding_no_duplicate_definitions"],
        Some(ws.path()),
        Some(&dup.to_string_lossy()),
    );
    assert_fail(&out, "BadWalk");
}

#[test]
fn finding_no_duplicate_definitions_fails_on_duplicate_exit_signal_enum() {
    let ws = make_workspace();
    let dup = seed(
        ws.path(),
        "crates/loom-todo/src/exit.rs",
        "pub enum ExitSignal { Done }\n",
    );
    let out = invoke(
        &["finding_no_duplicate_definitions"],
        Some(ws.path()),
        Some(&dup.to_string_lossy()),
    );
    assert_fail(&out, "ExitSignal");
}

// ---------------------------------------------------------------------------
// audit_makes_no_bd_writes_outside_mint_module
// ---------------------------------------------------------------------------

#[test]
fn audit_makes_no_bd_writes_outside_mint_module_pass_when_only_mint_module_calls() {
    let ws = make_workspace();
    let mint_call = format!(
        "pub async fn dispatch() {{ let _ = {mint_findings_call}.await; }}\n",
        mint_findings_call = concat!("mint_findings", "(&bd, &fs, \"head\")"),
    );
    let inside = seed(
        ws.path(),
        "crates/loom-workflow/src/mint/walk.rs",
        &mint_call,
    );
    let out = invoke(
        &["audit_makes_no_bd_writes_outside_mint_module"],
        Some(ws.path()),
        Some(&inside.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn audit_makes_no_bd_writes_outside_mint_module_pass_when_called_from_main() {
    let ws = make_workspace();
    let body = format!(
        "fn run_gate_mint() {{ let _ = {call}; }}\n",
        call = concat!("mint_finding_with_options", "(&bd, &f, head, &opts)"),
    );
    let allowed = seed(ws.path(), "crates/loom/src/main.rs", &body);
    let out = invoke(
        &["audit_makes_no_bd_writes_outside_mint_module"],
        Some(ws.path()),
        Some(&allowed.to_string_lossy()),
    );
    assert_pass(&out);
}

#[test]
fn audit_makes_no_bd_writes_outside_mint_module_fail_when_called_from_audit_path() {
    let ws = make_workspace();
    let body = format!(
        "fn run_gate_audit() {{ let _ = {call}; }}\n",
        call = concat!("mint_findings", "(&bd, &f, head)"),
    );
    let bad = seed(ws.path(), "crates/loom-workflow/src/audit.rs", &body);
    let out = invoke(
        &["audit_makes_no_bd_writes_outside_mint_module"],
        Some(ws.path()),
        Some(&bad.to_string_lossy()),
    );
    assert_fail(&out, "crates/loom-workflow/src/audit.rs:1");
}

#[test]
fn audit_makes_no_bd_writes_outside_mint_module_fail_when_called_from_review_module() {
    let ws = make_workspace();
    let body = format!(
        "pub fn dispatch() {{ let _ = {call}; }}\n",
        call = concat!("mint_finding_with_options", "(&bd, f, head, opts)"),
    );
    let bad = seed(
        ws.path(),
        "crates/loom-workflow/src/review/runner.rs",
        &body,
    );
    let out = invoke(
        &["audit_makes_no_bd_writes_outside_mint_module"],
        Some(ws.path()),
        Some(&bad.to_string_lossy()),
    );
    assert_fail(&out, "mint_finding_with_options");
}

#[test]
fn audit_makes_no_bd_writes_outside_mint_module_ignores_doc_mentions() {
    let ws = make_workspace();
    let doc = seed(
        ws.path(),
        "crates/loom-workflow/src/lib.rs",
        "//! Module that documents `mint_findings(...)` and `mint_finding_with_options(...)`\n\
         pub fn nothing() {}\n",
    );
    let out = invoke(
        &["audit_makes_no_bd_writes_outside_mint_module"],
        Some(ws.path()),
        Some(&doc.to_string_lossy()),
    );
    assert_pass(&out);
}

// ---------------------------------------------------------------------------
// workspace_compile_checks_exposed_as_flake_checks (harness/tests Nix integration)
// ---------------------------------------------------------------------------

fn seed_workspace_compile_checks_fixture(root: &Path) {
    seed(
        root,
        "nix/flake/checks.nix",
        r#"_:
{
  perSystem = { loom, ... }:
    let
      inherit (loom) clippy nextest;
    in
    {
      checks = {
        loom-clippy = clippy;
        loom-nextest = nextest;
      };
    };
}
"#,
    );
    seed(
        root,
        "nix/workspace.nix",
        r#"{ craneLib }:
let
  commonArgs = { };

  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  clippy = craneLib.cargoClippy (
    commonArgs
    // {
      inherit cargoArtifacts;
    }
  );

  nextest = craneLib.cargoNextest (
    commonArgs
    // {
      inherit cargoArtifacts;
    }
  );
in
{
  inherit
    cargoArtifacts
    clippy
    nextest
    ;
}
"#,
    );
}

#[test]
fn workspace_compile_checks_exposed_as_flake_checks_pass() {
    let ws = make_workspace();
    seed_workspace_compile_checks_fixture(ws.path());
    let out = invoke(
        &["workspace_compile_checks_exposed_as_flake_checks"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn workspace_compile_checks_exposed_as_flake_checks_fail_when_nextest_check_absent() {
    let ws = make_workspace();
    seed_workspace_compile_checks_fixture(ws.path());
    seed(
        ws.path(),
        "nix/flake/checks.nix",
        r#"_:
{
  perSystem = { loom, ... }:
    let
      inherit (loom) clippy nextest;
    in
    {
      checks = {
        loom-clippy = clippy;
      };
    };
}
"#,
    );
    let out = invoke(
        &["workspace_compile_checks_exposed_as_flake_checks"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "checks.loom-nextest");
}

#[test]
fn workspace_compile_checks_exposed_as_flake_checks_fail_when_check_uses_ad_hoc_derivation() {
    let ws = make_workspace();
    seed_workspace_compile_checks_fixture(ws.path());
    seed(
        ws.path(),
        "nix/flake/checks.nix",
        r#"_:
{
  perSystem = { pkgs, loom, ... }:
    let
      inherit (loom) nextest;
    in
    {
      checks = {
        loom-clippy = pkgs.runCommand "loom-clippy" { } "touch $out";
        loom-nextest = nextest;
      };
    };
}
"#,
    );
    let out = invoke(
        &["workspace_compile_checks_exposed_as_flake_checks"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "checks.loom-clippy");
}

#[test]
fn workspace_compile_checks_exposed_as_flake_checks_fail_without_shared_cargo_artifacts() {
    let ws = make_workspace();
    seed_workspace_compile_checks_fixture(ws.path());
    seed(
        ws.path(),
        "nix/workspace.nix",
        r#"{ craneLib }:
let
  commonArgs = { };

  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  clippy = craneLib.cargoClippy (
    commonArgs
    // {
      src = ./.;
    }
  );

  nextest = craneLib.cargoNextest (
    commonArgs
    // {
      inherit cargoArtifacts;
    }
  );
in
{
  inherit
    cargoArtifacts
    clippy
    nextest
    ;
}
"#,
    );
    let out = invoke(
        &["workspace_compile_checks_exposed_as_flake_checks"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "clippy must inherit the shared cargoArtifacts cache");
}

// ---------------------------------------------------------------------------
// loom_gate_check_derivation_exists (pre-commit.md § Fast tier)
// ---------------------------------------------------------------------------

#[test]
fn loom_gate_check_derivation_exists_pass() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "nix/flake/checks.nix",
        "_:\n{\n  perSystem = { pkgs, ... }: {\n    checks.loom-gate-check = pkgs.runCommand \"loom-gate-check\" {} ''loom gate check''; \n  };\n}\n",
    );
    let out = invoke(
        &["loom_gate_check_derivation_exists"],
        Some(ws.path()),
        None,
    );
    assert_pass(&out);
}

#[test]
fn loom_gate_check_derivation_exists_fail_when_absent() {
    let ws = make_workspace();
    seed(
        ws.path(),
        "nix/flake/checks.nix",
        "_:\n{\n  perSystem = _: {\n    checks = { };\n  };\n}\n",
    );
    let out = invoke(
        &["loom_gate_check_derivation_exists"],
        Some(ws.path()),
        None,
    );
    assert_fail(&out, "no derivation runs `loom gate check`");
}
