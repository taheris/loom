use super::{
    Definition, Error, NixError, Outcome, Registry, context, fixture, name, native_project, write,
};
use crate::provider::plan::offered;

#[tokio::test]
async fn selected_plan_reports_all_cold_inputs_before_any_verifier_executes() {
    let dir = fixture();
    native_project(&dir, "", true, true);
    write(
        dir.path(),
        "report-supervisor.sh",
        include_str!("../../../../../nix/report-supervisor.sh"),
    );
    let platform = loom_gate::runner::result::current_platform();
    let original = std::fs::read_to_string(dir.path().join("flake.nix")).unwrap();
    let unique = dir.path().file_name().unwrap().to_str().unwrap();
    let bash = super::bash();
    let bash_root: std::path::PathBuf = bash.components().take(4).collect();
    let bash_root = serde_json::to_string(bash_root.to_str().unwrap()).unwrap();
    let mkdir = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("mkdir"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    let tools: std::path::PathBuf = mkdir.components().take(4).collect();
    let tools = serde_json::to_string(tools.to_str().unwrap()).unwrap();
    let cold = format!(
        r#"
    checks.{platform}.later = checked.check {{
      resources = [ source ];
      build = sources: builtins.derivation {{
        inherit (self.checks.{platform}.heading) system builder;
        tools = builtins.appendContext {tools} {{ {tools} = {{ path = true; }}; }};
        args = [ "-c" ''
          read -r heading < "$src/README.md"
          [[ "$heading" == '# '* ]] || exit 1
          "$tools/bin/mkdir" -p "$out/bin"
          "$tools/bin/cp" "$BASH" "$out/bin/later"
          # Keep Bash alive until cp finishes closing its logging descriptors.
          exit "$?"
        '' ];
        name = "loom-provider-later-{unique}";
        src = builtins.head sources;
        first = pinned "first";
        second = pinned "second";
      }};
    }};
    checks.{platform}.reporter = checked.reported {{
      evaluation = map (origin: checked.resource {{ root = ./.; inherit origin; }}) [ "producer.sh" "report-supervisor.sh" ];
      runtime = "native-process";
      system = "{platform}";
      bash = builtins.appendContext {bash_root} {{ {bash_root} = {{ path = true; }}; }};
      coreutils = builtins.appendContext {tools} {{ {tools} = {{ path = true; }}; }};
      name = "loom-provider-report-{unique}";
      resources = [ producer ];
      program = "${{self.checks.{platform}.heading.builder}}";
      arguments = [ producer.path (pinned "first") (pinned "second") ];
      targets = [ "probe" ];
      parser = "json-lines";
    }};
    checks.{platform}.alias = self.checks.{platform}.later;
    packages.{platform}.alias = self.checks.{platform}.later;
    apps.{platform}.later = checked.app {{
      pkgs = {{
        bash = builtins.appendContext {bash_root} {{ {bash_root} = {{ path = true; }}; }};
        stdenv.hostPlatform.system = "{platform}";
      }};
      package = self.checks.{platform}.later;
      name = "later";
    }};
"#
    );
    let pinned = format!(
        r#"
    producer = checked.resource {{ root = ./.; origin = "producer.sh"; }};
    pinned = suffix: builtins.derivation {{
      name = "loom-preflight-{unique}-${{suffix}}";
      system = "builtin";
      builder = "builtin:fetchurl";
      url = "https://invalid.example/never-acquire-in-verification";
      outputHashAlgo = "sha256";
      outputHashMode = "flat";
      outputHash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    }};
"#
    );
    write(
        dir.path(),
        "flake.nix",
        &original
            .replace("  in {", &(pinned + "  in {"))
            .replace(
                &format!("    checks.{platform}.heading ="),
                &(cold + &format!("    checks.{platform}.heading =")),
            )
            .replace(
                "checked.check {",
                "checked.check { runtime = \"native-process\"; evaluation = map (origin: checked.resource { root = ./.; inherit origin; }) [ \"producer.sh\" \"report-supervisor.sh\" ];",
            )
            .replace(
                "name = \"loom-provider-heading\";",
                &format!("name = \"loom-provider-heading-{unique}\";"),
            ),
    );
    write(
        dir.path(),
        "producer.sh",
        "printf '%s\\n' '{\"target\":\"probe\",\"pass\":true,\"evidence\":\"fresh report\"}'\n",
    );
    write(
        dir.path(),
        "specs/example/tests.md",
        "## Success Criteria\n- Accounting [check](accounting)\n- Early [test](early)\n- Later [test](later)\n- Alias [test](later-alias)\n- Package [test](later-package)\n- App [test](later-app)\n- Report [test](reporter)\n",
    );
    let (snapshot, inventory) = context(&dir);
    let mut definitions = vec![offered(&inventory, &snapshot)];
    for (target, kind, attribute) in [
        ("early", "check", "heading"),
        ("later", "check", "later"),
        ("later-alias", "check", "alias"),
        ("later-package", "package", "alias"),
        ("later-app", "app", "later"),
        ("reporter", "check", "reporter"),
    ] {
        definitions.push(
            Definition::hermetic_baseline(
                &snapshot,
                &inventory,
                name(target),
                &name(&format!("nix:{kind}:flake.nix:{platform}:{attribute}")),
            )
            .unwrap(),
        );
    }
    let early_outputs = definitions[1]
        .nix_graph()
        .unwrap()
        .outputs()
        .unwrap()
        .clone();
    assert!(
        early_outputs
            .values()
            .all(|output| !std::path::Path::new(output.as_str()).exists())
    );
    let registry = Registry::new(&snapshot, definitions).unwrap();
    let plan = registry.admit(&snapshot, &inventory).unwrap();
    let scope = loom_gate::scope::Request::Tree
        .resolve(dir.path())
        .await
        .unwrap();
    let selection = plan.select_tiers(&scope, &[loom_gate::Tier::Test]).unwrap();
    for error in [
        selection.invocations().err().unwrap(),
        selection.observations().err().unwrap(),
        selection.run().err().unwrap(),
    ] {
        let Error::Nix(NixError::ColdInputs { derivations }) = error else {
            panic!("expected aggregate cold frontier: {error:#}");
        };
        assert_eq!(derivations.len(), 2);
        assert!(
            derivations
                .iter()
                .any(|path| path.as_str().contains("first"))
        );
        assert!(
            derivations
                .iter()
                .any(|path| path.as_str().contains("second"))
        );
    }
    assert!(
        early_outputs
            .values()
            .all(|output| !std::path::Path::new(output.as_str()).exists())
    );
    let exact = loom_gate::scope::Request::Target("early".to_owned())
        .resolve(dir.path())
        .await
        .unwrap();
    let early = plan.select(&exact).unwrap().run().unwrap();
    assert_eq!(early.len(), 1);
    assert_eq!(early[0].observed().outcome(), &Outcome::Pass);
    for suffix in ["first", "second"] {
        let file = dir.path().join(format!("loom-preflight-{unique}-{suffix}"));
        std::fs::write(&file, "").unwrap();
        let output = std::process::Command::new("nix-store")
            .args(["--add-fixed", "sha256"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(output.status.success());
        std::fs::remove_file(file).unwrap();
    }
    let invocations = selection.run().unwrap();
    assert_eq!(invocations.len(), 6);
    assert_eq!(
        invocations
            .iter()
            .filter(|invocation| invocation.shared_with().is_none())
            .count(),
        4
    );
    for invocation in invocations {
        assert_eq!(invocation.observed().outcome(), &Outcome::Pass);
    }
    assert!(
        early_outputs
            .values()
            .all(|output| std::path::Path::new(output.as_str()).exists())
    );
    write(dir.path(), "README.md", "not a heading\n");
    assert!(selection.invocations().is_err());
}
