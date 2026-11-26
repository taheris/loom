use super::{Definition, Outcome, Registry, context, fixture, name, native_project, write};
use crate::provider::plan::offered;
use loom_gate::Tier;
use loom_gate::provider::plan::selection::Stage;

#[tokio::test]
async fn confined_observation_batch_keeps_artifacts_fresh_checks_aliases_and_order() {
    let dir = fixture();
    let root = dir.path();
    let platform = loom_gate::runner::result::current_platform();
    native_project(&dir, "", true, true);
    let flake = std::fs::read_to_string(root.join("flake.nix")).unwrap();
    let extras = format!(
        r#"
    checks.{platform}.alias = self.checks.{platform}.heading;
    packages.{platform}.artifact = checked.artifact {{
      resources = [ source ];
      build = sources: builtins.derivation {{
        inherit (self.checks.{platform}.heading) system builder args;
        name = "loom-provider-batched-artifact";
        src = builtins.head sources;
      }};
    }};
    checks.{platform}.other = checked.check {{
      resources = [ source ];
      build = sources: builtins.derivation {{
        inherit (self.checks.{platform}.heading) system builder args;
        name = "loom-provider-independent-heading";
        src = builtins.head sources;
      }};
    }};
"#
    );
    let heading = format!("    checks.{platform}.heading =");
    assert!(flake.contains(&heading));
    write(
        root,
        "flake.nix",
        &flake.replace(&heading, &(extras + &heading)),
    );
    write(
        root,
        "specs/example/tests.md",
        "## Success Criteria\n- Accounting [check](accounting)\n- First [check](first)\n- Alias [check](first-alias)\n- Artifact [check](interleaved-artifact)\n- Second [check](second)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let bindings = [
        ("first", "heading"),
        ("first-alias", "alias"),
        ("second", "other"),
    ]
    .map(|(target, attr)| {
        (
            name(target),
            name(&format!("nix:check:flake.nix:{platform}:{attr}")),
        )
    });
    let mut definitions = vec![offered(&inventory, &snapshot)];
    definitions.extend(Definition::hermetic_baselines(&snapshot, &inventory, &bindings).unwrap());
    definitions.push(
        Definition::hermetic_baseline(
            &snapshot,
            &inventory,
            name("interleaved-artifact"),
            &name(&format!("nix:package:flake.nix:{platform}:artifact")),
        )
        .unwrap(),
    );
    let registry = Registry::new(&snapshot, definitions).unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    let scope = loom_gate::scope::Request::Tree.resolve(root).await.unwrap();
    let mut reference = None;
    for batch in [false, true, true, false] {
        let selection = plan
            .select_at(&scope, &[Tier::Check], Stage::Publication)
            .unwrap();
        let invocations = if batch {
            selection
                .observations()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
        } else {
            selection
                .invocations()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
        }
        .unwrap();
        let observed = invocations
            .into_iter()
            .filter(|invocation| invocation.observed().target() != &name("accounting"))
            .collect::<Vec<_>>();
        assert_eq!(observed.len(), 4);
        for (invocation, target) in
            observed
                .iter()
                .zip(["first", "first-alias", "interleaved-artifact", "second"])
        {
            assert_eq!(invocation.observed().target(), &name(target));
            assert_eq!(invocation.observed().outcome(), &Outcome::Pass);
            assert_eq!(invocation.owners().len(), 1);
        }
        assert_eq!(observed[0].shared_with(), None);
        assert_eq!(observed[1].shared_with(), Some(&name("first")));
        assert_eq!(observed[2].shared_with(), None);
        assert_eq!(observed[3].shared_with(), None);
        let outcomes = observed
            .iter()
            .map(|invocation| serde_json::to_value(invocation.observed()).unwrap())
            .collect::<Vec<_>>();
        if let Some(previous) = &reference {
            assert_eq!(&outcomes, previous);
        } else {
            reference = Some(outcomes);
        }
    }
}
