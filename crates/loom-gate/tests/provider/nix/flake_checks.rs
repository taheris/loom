use super::{Definition, Registry, context, fixture, name, native_project, write};
use crate::provider::plan::offered;
use loom_driver::git::{commit_all_in, sync_head_commit_sha};
use loom_gate::Tier;
use loom_gate::provider::nix::flake_checks::Error;
use loom_gate::provider::plan::selection::Stage;
use loom_gate::scope::Request;

fn add_raw_root(dir: &tempfile::TempDir, script: &str) {
    native_project(dir, "", true, true);
    let system = loom_gate::runner::result::current_platform();
    let heading = format!("checks.{system}.heading");
    let source = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    write(dir.path(), "flake.nix", &source.replace(&heading, &format!(
        "checks.{system}.raw = builtins.derivation {{ name = \"flake-residual\"; inherit (self.{heading}) builder system; args = [ \"-c\" \"{script}\" ]; }};\n    {heading}"
    )));
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Context [check](accounting)\n",
    );
}

async fn publication_range(dir: &tempfile::TempDir) -> loom_gate::scope::Resolved {
    let root = dir.path();
    commit_all_in(root, "Before flake inputs change").unwrap();
    let prior = sync_head_commit_sha(root).unwrap();
    write(
        root,
        "README.md",
        &format!("# Changed input {}\n", root.display()),
    );
    commit_all_in(root, "After flake inputs change").unwrap();
    let head = sync_head_commit_sha(root).unwrap();
    Request::Diff(format!("{prior}..{head}"))
        .resolve(root)
        .await
        .unwrap()
}

fn plan(dir: &tempfile::TempDir) -> loom_gate::provider::plan::Plan {
    let (snapshot, inventory) = context(dir);
    let system = loom_gate::runner::result::current_platform();
    let check = Definition::hermetic_baseline(
        &snapshot,
        &inventory,
        name("heading"),
        &name(&format!("nix:check:flake.nix:{system}:heading")),
    )
    .unwrap();
    Registry::new(&snapshot, vec![offered(&inventory, &snapshot), check])
        .unwrap()
        .admit(&snapshot, &inventory)
        .unwrap()
}

#[tokio::test]
async fn flake_handoff_requires_the_exact_fresh_output_of_the_selected_check() {
    let dir = fixture();
    add_raw_root(&dir, ": > $out");
    let scope = publication_range(&dir).await;
    let plan = plan(&dir);
    let selection = plan
        .select_at(&scope, &[Tier::Check], Stage::Publication)
        .unwrap();
    let output = plan
        .units()
        .iter()
        .find(|unit| unit.target() == &name("heading"))
        .unwrap()
        .nix_graph()
        .unwrap()
        .outputs()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert!(!std::path::Path::new(output.as_str()).exists());
    let delegated = selection.start_root_flake_checks().unwrap();
    assert!(
        !std::path::Path::new(output.as_str()).exists(),
        "the flake prelude must not realize the exact delegated check"
    );
    assert!(
        matches!(delegated.finish(), Err(Error::NotDischarged { target, .. }) if target == name("heading"))
    );

    let mut checks = selection.start_root_flake_checks().unwrap();
    for observation in selection.observations().unwrap() {
        checks.observe(observation.unwrap().observed()).unwrap();
    }
    checks.finish().unwrap();
    assert!(std::path::Path::new(output.as_str()).exists());
}

#[tokio::test]
async fn unresolved_exact_range_uses_the_complete_stock_flake_build() {
    let dir = fixture();
    add_raw_root(&dir, ": > $out");
    publication_range(&dir).await;
    let plan = plan(&dir);
    let output = plan
        .units()
        .iter()
        .find(|unit| unit.target() == &name("heading"))
        .unwrap()
        .nix_graph()
        .unwrap()
        .outputs()
        .unwrap()
        .values()
        .next()
        .unwrap();
    let scope = Request::Diff("HEAD^..HEAD".into())
        .resolve(dir.path())
        .await
        .unwrap();
    let selection = plan
        .select_at(&scope, &[Tier::Check], Stage::Publication)
        .unwrap();
    assert!(!std::path::Path::new(output.as_str()).exists());
    selection
        .start_root_flake_checks()
        .unwrap()
        .finish()
        .unwrap();
    assert!(std::path::Path::new(output.as_str()).exists());
}

#[tokio::test]
async fn unchecked_raw_flake_failure_is_not_lost_when_other_builds_are_delegated() {
    let dir = fixture();
    add_raw_root(&dir, "exit 63");
    // An unqualified flake selector searches packages.<system> before the
    // actual top-level checks. This valid package has a same-named subattr.
    let system = loom_gate::runner::result::current_platform();
    let marker = format!("    checks.{system}.raw =");
    let flake = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    let shadow = format!(
        r#"
    packages.{system}.checks = (builtins.derivation {{
      name = "flake-unrelated-package";
      inherit (self.checks.{system}.heading) builder system;
      args = [ "-c" ": > $out" ];
    }}) // {{ {system}.heading = self.checks.{system}.heading; }};
"#
    );
    assert!(flake.contains(&marker));
    write(
        dir.path(),
        "flake.nix",
        &flake.replace(&marker, &(shadow + &marker)),
    );
    let scope = publication_range(&dir).await;
    let plan = plan(&dir);
    let selection = plan
        .select_at(&scope, &[Tier::Check], Stage::Publication)
        .unwrap();
    let Err(Error::Nix(error)) = selection.start_root_flake_checks() else {
        panic!("the unclaimed raw check must still be built");
    };
    assert!(error.to_string().contains("flake-residual"), "{error}");
}
