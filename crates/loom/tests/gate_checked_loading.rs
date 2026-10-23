//! Ordinary CLI admission loads linked callbacks and rejects executable-looking metadata.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::{Command, Output};

#[path = "common/checked.rs"]
mod checked;
use checked::{TARGET, fixture, native, write};

fn command(root: &Path, lane: &str, scope: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_loom"));
    command
        .args(["--workspace", root.to_str().unwrap(), "gate", lane])
        .args(scope)
        .env_remove("LOOM_INSIDE")
        .env_remove("LOOM_PARENT_DIFF_GATE")
        .env_remove("LOOM_PROFILE_IMAGE_MANIFEST");
    command
}

fn run(root: &Path, lane: &str, scope: &[&str]) -> Output {
    command(root, lane, scope).output().unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn context_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    native(root, "git", &["init", "-q"]);
    write(root, ".gitignore", "/.loom/\n");
    write(
        root,
        "docs/README.md",
        "- [fixture](../specs/fixture/spec.md)\n",
    );
    write(root, "specs/fixture/spec.md", "# Fixture\n");
    write(
        root,
        "specs/fixture/tests.md",
        "# Fixture\n\n## Success Criteria\n- Project text parses [check](loom:project-context)\n",
    );
    dir
}

#[test]
fn cli_pending_owners_cannot_waive_baseline_policy_and_passing_pending_is_unneeded() {
    let dir = context_fixture();
    let context = loom_gate::provider::PROJECT_CONTEXT_TARGET;
    write(
        dir.path(),
        "specs/fixture/tests.md",
        &format!("# Fixture\n\n## Success Criteria\n- Still pending [check?]({context})\n"),
    );
    write(dir.path(), "README.md", "# Invalid document\0\n");
    let failed = run(dir.path(), "verify", &["--tree"]);
    assert_eq!(failed.status.code(), Some(1), "{}", stderr(&failed));
    assert!(
        stderr(&failed).contains("\"verdict\":\"fail\""),
        "{}",
        stderr(&failed)
    );
    assert!(!stderr(&failed).contains("unneeded pending marker"));
    write(dir.path(), "README.md", "# Corrected document\n");
    let passing_pending = run(dir.path(), "verify", &["--tree"]);
    assert_eq!(
        passing_pending.status.code(),
        Some(1),
        "{}",
        stderr(&passing_pending)
    );
    assert!(
        stderr(&passing_pending).contains("\"verdict\":\"pass\""),
        "{}",
        stderr(&passing_pending)
    );
    assert!(
        stderr(&passing_pending).contains("unneeded pending marker"),
        "{}",
        stderr(&passing_pending)
    );
    write(
        dir.path(),
        "specs/fixture/tests.md",
        &format!("# Fixture\n\n## Success Criteria\n- Now implemented [check]({context})\n"),
    );
    let corrected = run(dir.path(), "verify", &["--tree"]);
    assert!(corrected.status.success(), "{}", stderr(&corrected));
}

#[test]
fn cli_document_project_uses_fixed_library_policy_without_rust_or_nix_fallbacks() {
    let dir = context_fixture();
    let root = dir.path();
    let first = run(root, "check", &["--tree"]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(stderr(&first).contains("\"verdict\":\"pass\""));
    write(root, "new.md", "# Invalid\0text\n");
    let failed = run(root, "check", &["--files", "new.md"]);
    assert!(!failed.status.success(), "{}", stderr(&failed));
    assert!(
        stderr(&failed).contains("contains NUL"),
        "{}",
        stderr(&failed)
    );
    write(root, "new.md", "# Corrected\n");
    let corrected = run(root, "check", &["--files", "new.md"]);
    assert!(corrected.status.success(), "{}", stderr(&corrected));
    write(root, "unowned.rs", "pub fn no_native_owner() {}\n");
    let orphan = run(root, "check", &["--tree"]);
    assert!(!orphan.status.success(), "{}", stderr(&orphan));
    assert!(
        stderr(&orphan).contains("unowned.rs"),
        "{}",
        stderr(&orphan)
    );
}

#[test]
fn cli_known_unsupported_definition_fails_before_cold_native_discovery() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "docs/README.md",
        "- [fixture](../specs/fixture/spec.md)\n",
    );
    write(dir.path(), "specs/fixture/spec.md", "# Fixture\n");
    write(
        dir.path(),
        "specs/fixture/tests.md",
        "# Fixture\n\n## Success Criteria\n\n- Unsupported [check](unregistered-check)\n",
    );
    write(
        dir.path(),
        "flake.nix",
        "{ outputs = _: builtins.throw \"cold-native-discovery-must-not-run\"; }\n",
    );
    let output = run(dir.path(), "check", &["--tree"]);
    let diagnostic = stderr(&output);
    assert!(!output.status.success());
    assert!(
        diagnostic.contains("no actual checked definition"),
        "{diagnostic}"
    );
    assert!(
        !diagnostic.contains("capture current verification inputs"),
        "{diagnostic}"
    );
    assert!(
        !diagnostic.contains("cold-native-discovery-must-not-run"),
        "{diagnostic}"
    );
    assert!(!dir.path().join(".loom/state.db").exists());
}

#[test]
fn cli_structurally_missing_test_fails_before_snapshot_or_native_discovery() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "docs/README.md",
        "- [fixture](../specs/fixture/spec.md)\n",
    );
    write(dir.path(), "specs/fixture/spec.md", "# Fixture\n");
    write(
        dir.path(),
        "specs/fixture/tests.md",
        "# Fixture\n\n## Success Criteria\n\n- Missing test [test](missing_native_body_for_early_feedback)\n",
    );
    let output = run(dir.path(), "test", &["--tree"]);
    let diagnostic = stderr(&output);
    assert!(!output.status.success());
    assert!(
        diagnostic.contains("structural integrity failed"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("missing_native_body_for_early_feedback"),
        "{diagnostic}"
    );
    assert!(
        !diagnostic.contains("capture current verification inputs"),
        "{diagnostic}"
    );
}

#[test]
fn cli_linked_definition_runs_cold_failure_and_correction_without_a_runner() {
    let dir = fixture();
    let root = dir.path();
    // A configured runner is not consulted for either authority or execution.
    write(
        root,
        "loom.toml",
        "[runner.check.hostile]\nmatch='.*'\ncommand='touch should-not-run'\ninputs='touch should-not-query'\n",
    );
    let first = run(root, "check", &["--target", TARGET]);
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(stderr(&first).contains("diagnostic only"));
    assert!(stderr(&first).contains("\"verdict\":\"pass\""));
    assert!(stderr(&first).contains("\"kind\":\"content\""));
    write(
        root,
        "crates/loom-llm/Cargo.toml",
        "[package]\nname='loom-llm'\nversion='0.1.0'\nedition='2024'\n[dependencies]\nloom-driver={path='../loom-driver'}\n",
    );
    native(root, "cargo", &["generate-lockfile", "--offline"]);
    let failed = run(root, "check", &["--target", TARGET]);
    assert!(!failed.status.success());
    assert!(
        stderr(&failed).contains("forbidden internal dep"),
        "{}",
        stderr(&failed)
    );
    write(
        root,
        "crates/loom-llm/Cargo.toml",
        "[package]\nname='loom-llm'\nversion='0.1.0'\nedition='2024'\n",
    );
    native(root, "cargo", &["generate-lockfile", "--offline"]);
    let corrected = run(root, "verify", &["--target", TARGET]);
    assert!(corrected.status.success(), "{}", stderr(&corrected));
    for path in [
        "should-not-run",
        "should-not-query",
        ".loom/state.db",
        ".loom/logs/gate/verifier-results.jsonl",
        ".loom/marker.json",
    ] {
        assert!(!root.join(path).exists(), "diagnostic wrote {path}");
    }
}

#[test]
fn cli_unknown_later_lane_definition_rejects_before_checked_sibling_or_composed_review() {
    // Standalone review inspects source; audit must first pass deterministic admission.
    for lane in ["check", "verify", "audit"] {
        let dir = fixture();
        let root = dir.path();
        write(
            root,
            "specs/fixture/tests.md",
            &format!(
                "# Fixture\n\n## Success Criteria\n\n- Leaf dependency [check]({TARGET})\n- External observation-only provider [system](unregistered-system)\n"
            ),
        );
        write(
            root,
            "loom.toml",
            "[runner.system.external]\nmatch='^unregistered-system$'\ncommand='touch should-not-run'\ninputs='touch should-not-query'\n",
        );
        let output = run(root, lane, &["--tree"]);
        assert!(!output.status.success(), "{lane}");
        let diagnostic = stderr(&output);
        assert!(
            diagnostic.contains("no actual checked definition"),
            "{lane}: {diagnostic}"
        );
        assert!(diagnostic.contains("unregistered-system"));
        assert!(!diagnostic.contains("\"verdict\":\"pass\""));
        for path in [
            "should-not-run",
            "should-not-query",
            ".loom/logs/gate/verifier-results.jsonl",
        ] {
            assert!(!root.join(path).exists(), "{lane} wrote {path}");
        }
    }
}

#[test]
fn cli_malformed_current_native_definition_fails_before_callback_and_correction_is_fresh() {
    let dir = fixture();
    let root = dir.path();
    write(root, "crates/loom-llm/Cargo.toml", "[package\n");
    let bad = run(root, "check", &["--target", TARGET]);
    assert!(!bad.status.success());
    assert!(
        stderr(&bad).contains("independent native verification obligations"),
        "{}",
        stderr(&bad)
    );
    assert!(!stderr(&bad).contains("\"verdict\":\"pass\""));
    write(
        root,
        "crates/loom-llm/Cargo.toml",
        "[package]\nname='loom-llm'\nversion='0.1.0'\nedition='2024'\n",
    );
    let corrected = run(root, "check", &["--target", TARGET]);
    assert!(corrected.status.success(), "{}", stderr(&corrected));
}

#[test]
fn cli_executable_looking_targets_cannot_extend_a_supported_constructor() {
    for target in [
        "cargo run -p loom-walk -- loom_llm_deps --all",
        "cargo run -p loom-walk -- loom_llm_deps; touch should-not-run",
        "nix run .#arbitrary-app -- should-not-run",
        "engine-info --command should-not-run",
    ] {
        let dir = fixture();
        let root = dir.path();
        write(
            root,
            "specs/fixture/tests.md",
            &format!(
                "# Fixture\n\n## Success Criteria\n\n- Unsupported operation [check]({target})\n"
            ),
        );
        let output = run(root, "check", &["--target", target]);
        assert!(!output.status.success(), "{target}");
        assert!(
            stderr(&output).contains("no actual checked definition"),
            "{}",
            stderr(&output)
        );
        assert!(!stderr(&output).contains("\"verdict\":\"pass\""));
        assert!(!root.join("should-not-run").exists());
    }
}

#[test]
fn cli_checked_pending_readiness_never_becomes_ordinary_pass_evidence() {
    let dir = fixture();
    let root = dir.path();
    write(
        root,
        "specs/fixture/tests.md",
        &format!(
            "# Fixture\n\n## Success Criteria\n\n- Pending leaf dependency [check?]({TARGET})\n"
        ),
    );
    let ready = run(root, "check", &["--target", TARGET]);
    assert!(!ready.status.success());
    assert!(
        stderr(&ready).contains("unneeded pending marker"),
        "{}",
        stderr(&ready)
    );
    write(
        root,
        "crates/loom-llm/Cargo.toml",
        "[package]\nname='loom-llm'\nversion='0.1.0'\nedition='2024'\n[dependencies]\nloom-driver={path='../loom-driver'}\n",
    );
    native(root, "cargo", &["generate-lockfile", "--offline"]);
    let pending = run(root, "check", &["--target", TARGET]);
    assert!(pending.status.success(), "{}", stderr(&pending));
    assert!(stderr(&pending).contains("\"verdict\":\"fail\""));
    assert!(!root.join(".loom/state.db").exists());
    assert!(!root.join(".loom/logs/gate/verifier-results.jsonl").exists());
}

fn shared_owner(root: &Path, modifier: &str) {
    write(
        root,
        "docs/README.md",
        "- [fixture](../specs/fixture/spec.md)\n- [second](../specs/second/spec.md)\n",
    );
    write(root, "specs/second/spec.md", "# Second owner\n");
    write(
        root,
        "specs/second/tests.md",
        &format!(
            "# Second owner\n\n## Success Criteria\n\n- Shared dependency [{modifier}]({TARGET})\n"
        ),
    );
}

#[test]
fn cli_shared_checked_pass_retains_a_later_pending_owner() {
    let dir = fixture();
    let root = dir.path();
    shared_owner(root, "check?");
    let output = run(root, "check", &["--target", TARGET]);
    let diagnostic = stderr(&output);
    assert!(!output.status.success(), "{diagnostic}");
    assert!(
        diagnostic.contains("unneeded pending marker"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("specs/second/tests.md"), "{diagnostic}");
    assert_eq!(
        diagnostic.matches("\"verdict\":\"pass\"").count(),
        1,
        "{diagnostic}"
    );
    assert!(!root.join(".loom/state.db").exists());
}

#[test]
fn cli_shared_checked_failure_retains_a_later_ordinary_owner() {
    let dir = fixture();
    let root = dir.path();
    write(
        root,
        "specs/fixture/tests.md",
        &format!("# Fixture\n\n## Success Criteria\n\n- Pending dependency [check?]({TARGET})\n"),
    );
    shared_owner(root, "check");
    write(
        root,
        "crates/loom-llm/Cargo.toml",
        "[package]\nname='loom-llm'\nversion='0.1.0'\nedition='2024'\n[dependencies]\nloom-driver={path='../loom-driver'}\n",
    );
    native(root, "cargo", &["generate-lockfile", "--offline"]);
    let output = run(root, "verify", &["--target", TARGET]);
    let diagnostic = stderr(&output);
    assert!(!output.status.success(), "{diagnostic}");
    assert!(
        diagnostic.contains("forbidden internal dep"),
        "{diagnostic}"
    );
    assert_eq!(
        diagnostic
            .lines()
            .filter(|line| line.starts_with(&format!("loom gate [{TARGET}]: ")))
            .filter(|line| line.contains("\"verdict\":\"fail\""))
            .count(),
        1,
        "{diagnostic}"
    );
    assert_eq!(
        diagnostic.matches("loom gate [check] FAIL:").count(),
        1,
        "{diagnostic}"
    );
    assert!(diagnostic.contains("specs/second/tests.md"), "{diagnostic}");
    assert!(!root.join(".loom/state.db").exists());
}

#[test]
fn cli_native_nix_target_uses_a_checked_graph_not_the_configured_command() {
    native_target("checks");
}

#[test]
fn cli_native_package_target_uses_its_actual_namespace() {
    native_target("packages");
}

#[test]
fn cli_finite_scope_cannot_hide_independent_native_coverage_before_project_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    native(root, "git", &["init", "-q"]);
    write(root, "loom.toml", "");
    // The configuration now has a genuine context reader. Keep this control
    // about a real uncovered compiler duty, not invented configuration duties.
    write(
        root,
        "Cargo.toml",
        "[package]\nname='uncovered'\nversion='0.1.0'\nedition='2024'\n",
    );
    write(root, "src/lib.rs", "pub fn value() {}\n");
    native(root, "cargo", &["generate-lockfile", "--offline"]);
    write(
        root,
        "docs/README.md",
        "- [fixture](../specs/fixture/spec.md)\n",
    );
    write(root, "specs/fixture/spec.md", "# Fixture\n");
    let target = format!(
        "nix build .#checks.{}.configuration",
        loom_gate::runner::result::current_platform()
    );
    write(
        root,
        "specs/fixture/tests.md",
        &format!(
            "## Success Criteria\n- Deferred [check]({target})\n  ```loom-verify\n  defer_until = \"publication\"\n  ```\n- Eligible [check]({target})\n"
        ),
    );
    let tools = tempfile::tempdir().unwrap();
    let marker = root.join("project-hook-must-not-run");
    project_hook(
        root,
        tools.path(),
        &format!(
            "#!/usr/bin/env bash\nset -euo pipefail\ntouch '{}'\n",
            marker.display()
        ),
    );
    let output = run(root, "check", &["--files", ".pre-commit-config.yaml"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let diagnostic = stderr(&output);
    assert!(
        diagnostic.contains("missing required Build coverage"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("loom gate plan:"), "{diagnostic}");
    assert!(!marker.exists());
}

fn native_target(namespace: &str) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    native(root, "git", &["init"]);
    write(root, ".gitignore", "/.loom/\n");
    let bash = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("bash"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    let store: std::path::PathBuf = bash.components().take(4).collect();
    let builder = format!(
        "builtins.appendContext {} {{ {} = {{ path = true; }}; }}",
        serde_json::to_string(bash.to_str().unwrap()).unwrap(),
        serde_json::to_string(store.to_str().unwrap()).unwrap()
    );
    let system = loom_gate::runner::result::current_platform();
    let target = format!("nix build .#{namespace}.{system}.native");
    write(
        root,
        "verification.nix",
        include_str!("../../../nix/verification.nix"),
    );
    write(root, "README.md", "# Native checked source\n");
    write(
        root,
        "flake.nix",
        &format!(
            r#"{{ outputs = {{ self }}: let
checked = import ./verification.nix;
source = checked.resource {{
  root = ./.;
  filter = path: type: type == "directory" || path == toString (./. + "/README.md");
}};
in {{ {namespace}.{system}.native = checked.check {{
resources = [ source ];
build = sources: builtins.derivation {{
  name = "loom-cli-native-checked"; system = "{system}"; builder = {builder};
  src = builtins.head sources;
  args = [ "-c" "read -r heading < \"$src/README.md\"; [[ \"$heading\" == '# '* ]] && printf checked > \"$out\"" ];
}};
}}; }}; }}
"#
        ),
    );
    write(
        root,
        "docs/README.md",
        "- [fixture](../specs/fixture/spec.md)\n",
    );
    write(root, "specs/fixture/spec.md", "# Native fixture\n");
    write(
        root,
        "specs/fixture/tests.md",
        &format!("# Fixture\n\n## Success Criteria\n\n- Native checked target [check]({target})\n"),
    );
    write(
        root,
        "loom.toml",
        "[runner.check.hostile]\nmatch='.*'\ncommand='touch should-not-run'\ninputs='touch should-not-query'\n",
    );
    let output = run(root, "check", &["--target", &target]);
    let diagnostic = stderr(&output);
    assert!(
        diagnostic.contains("\"kind\":\"execution-graph\""),
        "{diagnostic}"
    );
    if diagnostic.contains("\"verdict\":\"unavailable\"") {
        assert_eq!(output.status.code(), Some(78), "{diagnostic}");
        assert!(diagnostic.contains("\"capability\":\"nix-sandbox\""));
        assert!(!diagnostic.contains("\"verdict\":\"pass\""));
    } else {
        assert!(output.status.success(), "{diagnostic}");
        assert!(diagnostic.contains("\"verdict\":\"pass\""));
    }
    assert!(!root.join("should-not-run").exists());
    assert!(!root.join("should-not-query").exists());
    assert!(!root.join(".loom/state.db").exists());
}

fn baseline_commit(root: &Path) {
    native(root, "git", &["add", "."]);
    native(
        root,
        "git",
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "Fixture",
        ],
    );
}

#[cfg(unix)]
fn project_hook(root: &Path, tools: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    write(tools, "fixture-hook", script);
    let entry = tools.join("fixture-hook");
    std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o755)).unwrap();
    write(
        root,
        ".pre-commit-config.yaml",
        &format!(
            "repos:\n- repo: local\n  hooks:\n  - id: fixture-hook\n    name: Fixture hook\n    language: system\n    entry: {}\n    always_run: true\n    pass_filenames: false\n",
            serde_json::to_string(entry.to_str().unwrap()).unwrap()
        ),
    );
    hook_configuration_consumer(root);
}

fn hook_configuration_consumer(root: &Path) {
    if !root.join(".gitignore").exists() {
        write(root, ".gitignore", "/.loom/\n");
    }
    let bash = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("bash"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    let store: std::path::PathBuf = bash.components().take(4).collect();
    let system = loom_gate::runner::result::current_platform();
    write(
        root,
        "verification.nix",
        include_str!("../../../nix/verification.nix"),
    );
    write(
        root,
        "flake.nix",
        &format!(
            r#"{{ outputs = {{ self }}: let
      checked = import ./verification.nix;
      runtime = checked.resource {{
        root = ./.;
        filter = path: type: type == "directory" || builtins.elem path [ (toString (./. + "/.pre-commit-config.yaml")) (toString (./. + "/.gitignore")) ];
      }};
      evaluator = checked.resource {{
        root = ./.;
        filter = path: type: type == "directory" || builtins.match ".*[.]nix" (toString path) != null;
      }};
    in {{ checks.{system}.configuration = checked.check {{
      resources = [ runtime ]; evaluation = [ evaluator ];
      build = sources: builtins.derivation {{
        name = "loom-hook-configuration"; system = "{system}";
        builder = builtins.appendContext {bash} {{ {store} = {{ path = true; }}; }};
        src = builtins.head sources;
        args = [ "-c" "read -r heading < \"$src/.pre-commit-config.yaml\"; read -r ignored < \"$src/.gitignore\"; [[ \"$heading\" == repos: && -n \"$ignored\" ]] && printf checked > \"$out\"" ];
      }};
    }}; }}; }}"#,
            bash = serde_json::to_string(bash.to_str().unwrap()).unwrap(),
            store = serde_json::to_string(store.to_str().unwrap()).unwrap(),
        ),
    );
}

#[cfg(unix)]
#[test]
fn cli_unknown_definition_fails_before_project_hook_effects() {
    let dir = fixture();
    let root = dir.path();
    write(
        root,
        "specs/fixture/tests.md",
        "# Fixture\n\n## Success Criteria\n\n- Unknown checked definition [system](unregistered-system)\n",
    );
    baseline_commit(root);
    let tools = tempfile::tempdir().unwrap();
    project_hook(
        root,
        tools.path(),
        "#!/usr/bin/env bash\nset -euo pipefail\nprintf ran > \"$HOOK_MARKER\"\n",
    );
    let marker = tools.path().join("hook-marker");
    let output = command(root, "verify", &["--diff", "HEAD..HEAD"])
        .env("HOOK_MARKER", &marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no actual checked definition"),
        "{}",
        stderr(&output)
    );
    assert!(!marker.exists(), "project hook ran before admission");
}

#[cfg(unix)]
#[cfg(unix)]
#[test]
fn cli_missing_native_discovery_tool_is_unavailable_not_semantic_failure_or_pass() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    native(root, "git", &["init", "-q"]);
    write(root, "loom.toml", "");
    let tools = tempfile::tempdir().unwrap();
    project_hook(
        root,
        tools.path(),
        "#!/usr/bin/env bash\nset -euo pipefail\nexit 0\n",
    );
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("git"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    std::os::unix::fs::symlink(git, tools.path().join("git")).unwrap();
    let output = command(root, "check", &["--tree"])
        .env("PATH", tools.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(78), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("required discovery tool `nix` is unavailable"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn cli_native_baseline_without_annotations_is_not_lost_in_cli_selection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    native(root, "git", &["init"]);
    let tools = tempfile::tempdir().unwrap();
    project_hook(
        root,
        tools.path(),
        "#!/usr/bin/env bash\nset -euo pipefail\nexit 0\n",
    );
    let output = run(root, "check", &["--tree"]);
    let diagnostic = stderr(&output);
    assert!(
        diagnostic.contains("\"kind\":\"execution-graph\""),
        "{diagnostic}"
    );
    if diagnostic.contains("\"verdict\":\"unavailable\"") {
        assert_eq!(output.status.code(), Some(78), "{diagnostic}");
    } else {
        assert!(output.status.success(), "{diagnostic}");
        assert!(diagnostic.contains("\"verdict\":\"pass\""), "{diagnostic}");
    }
}

#[cfg(unix)]
#[test]
fn cli_project_hook_source_change_requires_a_fresh_invocation_before_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    native(root, "git", &["init"]);
    baseline_commit(root);
    let tools = tempfile::tempdir().unwrap();
    project_hook(
        root,
        tools.path(),
        "#!/usr/bin/env bash\nset -euo pipefail\nprintf 'pub fn unowned() {}\\n' > unowned.rs\nprintf ran > \"$HOOK_MARKER\"\n",
    );
    let marker = tools.path().join("hook-marker");
    let output = command(root, "verify", &["--diff", "HEAD..HEAD"])
        .env("HOOK_MARKER", &marker)
        .output()
        .unwrap();
    assert!(marker.exists(), "{}", stderr(&output));
    assert!(
        !output.status.success(),
        "hook cannot silently replace the admitted candidate"
    );
    assert!(root.join("unowned.rs").exists());
    assert!(
        stderr(&output).contains("tree-modified-by-hook"),
        "{}",
        stderr(&output)
    );
    assert!(
        !stderr(&output).contains("loom gate plan:"),
        "dispatch followed a candidate change"
    );
}
