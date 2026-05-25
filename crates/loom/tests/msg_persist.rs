//! End-to-end `loom msg -a <choice> -i <id>` reply-persistence tests.
//!
//! Drives the real `loom` binary against the `bd-shim` test helper on
//! `PATH`. The shim mutates a per-test state directory on every
//! `bd update` so a subsequent `bd show <id> --json` returns the same
//! record the production code wrote — verifying the persist path
//! end-to-end. No CommandRunner mocks; the test exercises the same
//! `tokio::process::Command::new("bd")` invocation path the production
//! driver uses.
//!
//! A prior bug in the `UpdateOpts` literal in `loom/src/main.rs` fed
//! the composed note into `println!` but never into the bd call. The
//! `msg::reply::tests::integer_choice_resolves_to_option_note` unit
//! test passed throughout. This integration test exists so that bug
//! shape cannot recur silently.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Description for a clarify bead with two well-formed options per the
/// Options Format Contract. `build_fast_reply` composes
/// `"Chose option 1 — Choose A: Detail A"` for input `1`.
const CLARIFY_DESC: &str = "## Options — pick a path

### Option 1 — Choose A
Detail A

### Option 2 — Choose B
Detail B
";

/// Seed one bead into the shim's state dir. Each bead is a directory
/// under `BD_STATE_DIR` with one file per field; labels live one per
/// line in `labels`; `notes` starts absent (empty in JSON output) and
/// gets written by the shim on `bd update --notes <text>`.
fn seed_bead(state_dir: &Path, id: &str, title: &str, description: &str, labels: &[&str]) {
    let bead_dir = state_dir.join(id);
    std::fs::create_dir_all(&bead_dir).expect("mkdir bead dir");
    std::fs::write(bead_dir.join("title"), title).expect("write title");
    std::fs::write(bead_dir.join("description"), description).expect("write description");
    std::fs::write(bead_dir.join("status"), "open").expect("write status");
    std::fs::write(bead_dir.join("priority"), "2").expect("write priority");
    std::fs::write(bead_dir.join("issue_type"), "task").expect("write issue_type");
    let body = labels.join("\n");
    std::fs::write(bead_dir.join("labels"), body).expect("write labels");
}

/// Install the `bd-shim` binary as `<dir>/bd-bin/bd` so PATH lookup
/// inside the loom subprocess resolves `bd` to the shim. Cargo
/// publishes the shim binary path via `CARGO_BIN_EXE_bd-shim`; we
/// symlink (preferred) or fall back to copying on filesystems that
/// reject symlinks across mount points.
fn install_bd_shim(dir: &Path) -> PathBuf {
    let bin_dir = dir.join("bd-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bd-bin");
    let bd_path = bin_dir.join("bd");
    let source = PathBuf::from(env!("CARGO_BIN_EXE_bd-shim"));
    match std::os::unix::fs::symlink(&source, &bd_path) {
        Ok(_) => {}
        Err(_) => {
            std::fs::copy(&source, &bd_path).expect("copy bd-shim into bin dir");
            let mut perm = std::fs::metadata(&bd_path).expect("stat bd").permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(&bd_path, perm).expect("chmod bd");
        }
    }
    bin_dir
}

/// `loom msg` calls `ProfileImageManifest::from_env()` even though the
/// `_manifest` binding is unused for msg flows. Write a minimum-viable
/// manifest pointing at an empty tar so the call succeeds.
fn write_minimal_manifest(dir: &Path) -> PathBuf {
    let source = dir.join("base.tar");
    std::fs::write(&source, "").expect("write base.tar");
    let manifest = dir.join("profile-images.json");
    let body = format!(
        r#"{{"base": {{"ref":"localhost/wrapix-base:test","source":{source:?}}}}}"#,
        source = source.display().to_string(),
    );
    std::fs::write(&manifest, body).expect("write manifest");
    manifest
}

fn run_loom_msg(
    workspace: &Path,
    bin_dir: &Path,
    state_dir: &Path,
    manifest: &Path,
    args: &[&str],
) -> std::process::Output {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let mut entries: Vec<PathBuf> = vec![bin_dir.to_path_buf()];
    entries.extend(std::env::split_paths(&path_var));
    let new_path = std::env::join_paths(entries).expect("join PATH");

    let loom_bin = env!("CARGO_BIN_EXE_loom");
    Command::new(loom_bin)
        .arg("--workspace")
        .arg(workspace)
        .arg("msg")
        .args(args)
        .env("PATH", new_path)
        .env("LOOM_PROFILES_MANIFEST", manifest)
        .env("BD_STATE_DIR", state_dir)
        .env("XDG_STATE_HOME", workspace.join(".loom-state"))
        // The nested-loom guard refuses mutating `loom msg` invocations
        // when LOOM_INSIDE=1. The cargo test runner inherits LOOM_INSIDE
        // when this suite is executed inside a loom-managed container,
        // which would block the child `loom msg` invocation before it
        // reached the persist path under test.
        .env_remove("LOOM_INSIDE")
        .output()
        .expect("spawn loom")
}

fn run_bd_show(bin_dir: &Path, state_dir: &Path, id: &str) -> serde_json::Value {
    let output = Command::new(bin_dir.join("bd"))
        .arg("show")
        .arg(id)
        .arg("--json")
        .env("BD_STATE_DIR", state_dir)
        .output()
        .expect("spawn bd-shim");
    assert!(
        output.status.success(),
        "bd show {id} must exit 0. stderr={:?}",
        String::from_utf8_lossy(&output.stderr),
    );
    let arr: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "bd show stdout not JSON: {e}\nstdout={:?}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    arr.as_array()
        .expect("bd show returns array")
        .first()
        .cloned()
        .expect("bd show array non-empty")
}

/// Read the shim's invocation log for debug attribution on failure.
fn read_invocation_log(state_dir: &Path) -> String {
    std::fs::read_to_string(state_dir.join(".invocations.log")).unwrap_or_default()
}

/// Description for a blocked bead carrying a free-form prompt — no
/// Options section. `build_fast_reply` always returns `FastReply::Verbatim`
/// for blocked kind regardless of input shape (integer-looking or not).
const BLOCKED_DESC: &str = "Need a deploy key for the runner image — \
which key path should it mount?
";

// -------------------------------------------------------------------
// B3 — `test_msg_option_validates`
// -------------------------------------------------------------------

/// `-a 1` on a clarify bead with two options composes `"Chose option 1
/// — Choose A: Detail A"` and persists it via `bd update --notes`, in
/// the same call that removes the `loom:clarify` label. A subsequent
/// `bd show --json | .notes` returns the composed text.
#[test]
fn msg_option_fast_reply_persists_note_via_bd_show() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();

    seed_bead(
        &state_dir,
        "wx-testa",
        "pick a path",
        CLARIFY_DESC,
        &["loom:clarify"],
    );

    let bin_dir = install_bd_shim(workspace);
    let manifest = write_minimal_manifest(workspace);

    let output = run_loom_msg(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        &["-o", "1", "-b", "wx-testa"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = read_invocation_log(&state_dir);
    assert!(
        output.status.success(),
        "loom msg -a 1 must exit 0.\nstdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );

    let bead = run_bd_show(&bin_dir, &state_dir, "wx-testa");
    let notes = bead
        .get("notes")
        .and_then(serde_json::Value::as_str)
        .expect("notes field present and string");
    assert_eq!(
        notes, "Chose option 1 — Choose A: Detail A",
        "bd show --json .notes must carry the composed note.\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );

    let labels = bead
        .get("labels")
        .and_then(serde_json::Value::as_array)
        .expect("labels field is array");
    let has_clarify = labels.iter().any(|v| v.as_str() == Some("loom:clarify"));
    assert!(
        !has_clarify,
        "loom:clarify label must be removed by the same update call. labels={labels:?}\n\
         bd-shim log:\n{log}",
    );
}

/// `-a 99` on a clarify bead with only two options errors non-zero AND
/// leaves the bead untouched (notes empty, label still present). Pins
/// both halves of the invariant — validation must happen before any
/// bd-side state mutation.
#[test]
fn msg_option_out_of_range_errors_and_leaves_bead_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();

    seed_bead(
        &state_dir,
        "wx-testb",
        "pick a path",
        CLARIFY_DESC,
        &["loom:clarify"],
    );

    let bin_dir = install_bd_shim(workspace);
    let manifest = write_minimal_manifest(workspace);

    let output = run_loom_msg(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        &["-o", "99", "-b", "wx-testb"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = read_invocation_log(&state_dir);
    assert!(
        !output.status.success(),
        "loom msg -a 99 must exit non-zero.\nstdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );
    assert!(
        stderr.contains("99") && (stderr.contains("option") || stderr.contains("Option")),
        "stderr must explain the out-of-range index. stderr={stderr}",
    );

    let bead = run_bd_show(&bin_dir, &state_dir, "wx-testb");
    let notes = bead
        .get("notes")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    assert!(
        notes.is_empty(),
        "bead.notes must remain empty on validation failure (got {notes:?}).\n\
         stderr={stderr}\nbd-shim log:\n{log}",
    );
    let labels = bead
        .get("labels")
        .and_then(serde_json::Value::as_array)
        .expect("labels field is array");
    let has_clarify = labels.iter().any(|v| v.as_str() == Some("loom:clarify"));
    assert!(
        has_clarify,
        "loom:clarify must remain when validation fails (no label drift on error). labels={labels:?}\n\
         bd-shim log:\n{log}",
    );
}

// -------------------------------------------------------------------
// B4 — `test_msg_reply_verbatim` integration half
// -------------------------------------------------------------------

/// `-a "<free text>"` on a `loom:blocked` bead persists the verbatim
/// text as notes and clears the `loom:blocked` label in the same
/// update. The unit test `blocked_free_form_passes_through_unchanged`
/// only proved `build_fast_reply` returns the right `FastReply` enum;
/// this test pins that the enum's note actually reaches the bead.
#[test]
fn msg_blocked_verbatim_text_persists_via_bd_show() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();

    seed_bead(
        &state_dir,
        "wx-blockt",
        "deploy key path",
        BLOCKED_DESC,
        &["loom:blocked"],
    );

    let bin_dir = install_bd_shim(workspace);
    let manifest = write_minimal_manifest(workspace);

    let output = run_loom_msg(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        &["-r", "use /run/secrets/deploy_key", "-b", "wx-blockt"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = read_invocation_log(&state_dir);
    assert!(
        output.status.success(),
        "loom msg -a \"<text>\" on blocked bead must exit 0.\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );

    let bead = run_bd_show(&bin_dir, &state_dir, "wx-blockt");
    let notes = bead
        .get("notes")
        .and_then(serde_json::Value::as_str)
        .expect("notes field present and string");
    assert_eq!(
        notes, "use /run/secrets/deploy_key",
        "bd show --json .notes must carry the verbatim free-form reply.\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );

    let labels = bead
        .get("labels")
        .and_then(serde_json::Value::as_array)
        .expect("labels field is array");
    let has_blocked = labels.iter().any(|v| v.as_str() == Some("loom:blocked"));
    assert!(
        !has_blocked,
        "loom:blocked label must be removed by the same update call. labels={labels:?}\n\
         bd-shim log:\n{log}",
    );
}

/// `-r "1"` on a `loom:blocked` bead does NOT resolve as integer option
/// (blocked beads have no Options section); the literal `"1"` is
/// persisted as verbatim per `blocked_integer_choice_is_always_verbatim`.
/// Pins the kind-discriminator in the persist path — integer-looking
/// input on a blocked bead must not accidentally trigger the option
/// validator.
#[test]
fn msg_blocked_integer_input_persists_verbatim_not_as_option() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();

    seed_bead(
        &state_dir,
        "wx-blocki",
        "free-form blocked prompt",
        BLOCKED_DESC,
        &["loom:blocked"],
    );

    let bin_dir = install_bd_shim(workspace);
    let manifest = write_minimal_manifest(workspace);

    let output = run_loom_msg(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        &["-r", "1", "-b", "wx-blocki"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = read_invocation_log(&state_dir);
    assert!(
        output.status.success(),
        "loom msg -a 1 on blocked bead must exit 0 (verbatim, not option).\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );

    let bead = run_bd_show(&bin_dir, &state_dir, "wx-blocki");
    let notes = bead
        .get("notes")
        .and_then(serde_json::Value::as_str)
        .expect("notes field present and string");
    assert_eq!(
        notes, "1",
        "blocked-bead integer input must persist verbatim, not as Option N composition.\n\
         stdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );
}

// -------------------------------------------------------------------
// `test_msg_flag_exclusivity`
// -------------------------------------------------------------------
//
// `loom msg --chat` dispatch is exercised end-to-end against the mock
// pi agent in `crates/loom/tests/msg_chat.rs` (launches container,
// writes notes, partial progress, exit signals, scope filtering).

/// `clap` enforces mutual exclusion at parse time: `-o` xor `-r`, neither
/// with `-d`, `-n` xor `-b`. Each forbidden combination must exit
/// non-zero with a parse error, BEFORE any bd call. The shim doesn't
/// even get invoked.
#[test]
fn msg_flag_exclusivity_enforced_at_parse_time() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let bin_dir = install_bd_shim(workspace);
    let manifest = write_minimal_manifest(workspace);

    // Each row exercises one of the mutual-exclusion rules.
    let forbidden: &[&[&str]] = &[
        &["-o", "1", "-r", "foo", "-b", "wx-aaa"],
        &["-o", "1", "-d", "-b", "wx-aaa"],
        &["-r", "foo", "-d", "-b", "wx-aaa"],
        &["-n", "1", "-b", "wx-aaa", "-r", "foo"],
    ];

    for argv in forbidden {
        let output = run_loom_msg(workspace, &bin_dir, &state_dir, &manifest, argv);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "loom msg {argv:?} must reject mutually exclusive flags.\n\
             stdout={stdout}\nstderr={stderr}",
        );
        // clap's error mentions the conflicting argument by name.
        let combined = format!("{stdout}{stderr}");
        let mentions_conflict = combined.contains("cannot be used")
            || combined.contains("conflicts with")
            || combined.contains("cannot");
        assert!(
            mentions_conflict,
            "stderr must explain the conflict (clap-shape error).\nargs={argv:?}\nstderr={stderr}",
        );
    }

    // Sanity: a *legal* combination still parses (the bd shim returns
    // no beads → "(no outstanding clarify or blocked beads)" exit 0).
    let output = run_loom_msg(workspace, &bin_dir, &state_dir, &manifest, &[]);
    assert!(
        output.status.success(),
        "bare `loom msg` (no flags) must parse cleanly.\nstderr={:?}",
        String::from_utf8_lossy(&output.stderr),
    );
}

// -------------------------------------------------------------------
// `test_msg_dismiss` — `-d` writes DISMISS_NOTE and removes the label
// -------------------------------------------------------------------

#[test]
fn msg_dismiss_writes_canonical_note_and_clears_label() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();

    seed_bead(
        &state_dir,
        "wx-dism",
        "dismiss path",
        CLARIFY_DESC,
        &["loom:clarify"],
    );

    let bin_dir = install_bd_shim(workspace);
    let manifest = write_minimal_manifest(workspace);

    let output = run_loom_msg(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        &["-b", "wx-dism", "-d"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = read_invocation_log(&state_dir);
    assert!(
        output.status.success(),
        "loom msg -d must exit 0.\nstdout={stdout}\nstderr={stderr}\nbd-shim log:\n{log}",
    );

    let bead = run_bd_show(&bin_dir, &state_dir, "wx-dism");
    let notes = bead
        .get("notes")
        .and_then(serde_json::Value::as_str)
        .expect("notes field present and string");
    assert!(
        notes.contains("Dismissed via loom msg"),
        "bd show --json .notes must carry the canonical dismiss note. got={notes:?}\nlog={log}",
    );
    let labels = bead
        .get("labels")
        .and_then(serde_json::Value::as_array)
        .expect("labels field is array");
    let has_clarify = labels.iter().any(|v| v.as_str() == Some("loom:clarify"));
    assert!(
        !has_clarify,
        "loom:clarify must be removed by the dismiss update. labels={labels:?}\nlog={log}",
    );
}

// -------------------------------------------------------------------
// `test_msg_spec_filter` — `-s <label>` narrows list output to spec
// -------------------------------------------------------------------

#[test]
fn msg_spec_filter_narrows_list_to_matching_spec() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();

    seed_bead(
        &state_dir,
        "wx-sfa",
        "alpha clarify",
        CLARIFY_DESC,
        &["loom:clarify", "spec:alpha"],
    );
    seed_bead(
        &state_dir,
        "wx-sfb",
        "beta clarify",
        CLARIFY_DESC,
        &["loom:clarify", "spec:beta"],
    );

    let bin_dir = install_bd_shim(workspace);
    let manifest = write_minimal_manifest(workspace);

    // Bare invocation lists both.
    let output = run_loom_msg(workspace, &bin_dir, &state_dir, &manifest, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "bare loom msg must exit 0");
    assert!(
        stdout.contains("wx-sfa") && stdout.contains("wx-sfb"),
        "cross-spec list must show both beads. stdout={stdout}",
    );

    // -s alpha filters out the beta bead.
    let output = run_loom_msg(workspace, &bin_dir, &state_dir, &manifest, &["-s", "alpha"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom msg -s alpha must exit 0.\nstdout={stdout}\nstderr={stderr}",
    );
    assert!(
        stdout.contains("wx-sfa"),
        "spec-filter must include alpha bead. stdout={stdout}",
    );
    assert!(
        !stdout.contains("wx-sfb"),
        "spec-filter must exclude beta bead. stdout={stdout}",
    );
}

// -------------------------------------------------------------------
// `test_msg_view_modes` — `-n <N>` and `-b <id>` render the bead host-side
// -------------------------------------------------------------------

#[test]
fn msg_view_modes_render_bead_host_side() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    let state_dir = workspace.join("bd-state");
    std::fs::create_dir_all(&state_dir).unwrap();

    seed_bead(
        &state_dir,
        "wx-viewa",
        "alpha view",
        CLARIFY_DESC,
        &["loom:clarify", "spec:alpha"],
    );

    let bin_dir = install_bd_shim(workspace);
    let manifest = write_minimal_manifest(workspace);

    // -b <id> view: prints title, description, and does not mutate state.
    let output = run_loom_msg(
        workspace,
        &bin_dir,
        &state_dir,
        &manifest,
        &["-b", "wx-viewa"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let log = read_invocation_log(&state_dir);
    assert!(
        output.status.success(),
        "loom msg -b view must exit 0.\nstdout={stdout}\nstderr={stderr}",
    );
    assert!(
        stdout.contains("wx-viewa"),
        "view output must include bead id. stdout={stdout}",
    );
    assert!(
        stdout.contains("alpha view"),
        "view output must include bead title. stdout={stdout}",
    );
    assert!(
        stdout.contains("### Option 1"),
        "view output must include the description body. stdout={stdout}",
    );

    // -b view must NOT call bd update — only bd list.
    assert!(
        !log.contains("update wx-viewa"),
        "view mode must not mutate the bead. log={log}",
    );

    // Label remains.
    let bead = run_bd_show(&bin_dir, &state_dir, "wx-viewa");
    let labels = bead
        .get("labels")
        .and_then(serde_json::Value::as_array)
        .expect("labels array");
    assert!(
        labels.iter().any(|v| v.as_str() == Some("loom:clarify")),
        "view must leave loom:clarify label intact. labels={labels:?}",
    );

    // -n 1 view: same content via 1-based addressing.
    let output = run_loom_msg(workspace, &bin_dir, &state_dir, &manifest, &["-n", "1"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "loom msg -n 1 view must exit 0.\nstdout={stdout}\nstderr={stderr}",
    );
    assert!(
        stdout.contains("wx-viewa") && stdout.contains("### Option 1"),
        "view by -n must render the same bead body. stdout={stdout}",
    );
}
