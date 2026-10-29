use super::{Definition, Outcome, Registry, context, fixture, name, native_project, write};
use crate::plan::offered;

#[tokio::test]
async fn actual_native_aliases_share_but_equal_commands_with_distinct_graphs_do_not() {
    let dir = fixture();
    let platform = loom_gate::runner::result::current_platform();
    native_project(&dir, "", true, true);
    let original = std::fs::read_to_string(dir.path().join("flake.nix"))
        .unwrap()
        .replace("x86_64-linux", &platform);
    let aliases = format!(
        r#"
    checks.{platform}.alias = self.checks.{platform}.heading;
    checks.{platform}.distinct = checked.check {{
      resources = [ source ];
      build = sources: builtins.derivation {{
        inherit (self.checks.{platform}.heading) system builder args;
        name = "loom-provider-distinct";
        src = builtins.head sources;
        CONTROL = "distinct-execution-input";
      }};
    }};
"#
    );
    write(
        dir.path(),
        "flake.nix",
        &original.replace(
            &format!("    checks.{platform}.heading ="),
            &(aliases + &format!("    checks.{platform}.heading =")),
        ),
    );
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Accounting [check](accounting)\n- First [test](first)\n- Alias [test](second)\n- Distinct [test](third)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let mut definitions = vec![offered(&inventory, &snapshot)];
    for (target, attribute) in [
        ("first", "heading"),
        ("second", "alias"),
        ("third", "distinct"),
    ] {
        definitions.push(
            Definition::hermetic_baseline(
                &snapshot,
                &inventory,
                name(target),
                &name(&format!("nix:check:flake.nix:{platform}:{attribute}")),
            )
            .unwrap(),
        );
    }
    let first = definitions[1].nix_graph().unwrap();
    let alias = definitions[2].nix_graph().unwrap();
    assert_ne!(first.identity(), alias.identity());
    assert_eq!(first.derivation(), alias.derivation());
    assert_ne!(
        first.derivation(),
        definitions[3].nix_graph().unwrap().derivation()
    );
    let registry = Registry::new(&snapshot, definitions).unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    let scope = loom_gate::scope::Request::Tree
        .resolve(dir.path())
        .await
        .unwrap();
    let selection = plan.select_tiers(&scope, &[loom_gate::Tier::Test]).unwrap();
    for observations in [
        selection
            .invocations()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        selection
            .observations()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
    ] {
        assert_eq!(observations.len(), 3);
        assert_eq!(observations[0].observed().target(), &name("first"));
        assert_eq!(observations[1].observed().target(), &name("second"));
        assert_eq!(observations[2].observed().target(), &name("third"));
        assert_eq!(observations[0].shared_with(), None);
        assert_eq!(observations[1].shared_with(), Some(&name("first")));
        assert_eq!(observations[2].shared_with(), None);
        assert_eq!(observations[0].duration(), observations[1].duration());
        for observed in observations {
            assert_eq!(observed.owners().len(), 1);
            assert_eq!(observed.observed().outcome(), &Outcome::Pass);
        }
    }
}
