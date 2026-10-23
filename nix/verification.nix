# Native sources retain their repository origin through filtering and execution.
let
  resource =
    {
      root,
      origin ? ".",
      name ? "loom-source",
      filter ? (_path: _type: true),
    }:
    let
      composed = root._isLibCleanSourceWith or false;
      original = if composed then root.origSrc else root;
      selectedRoot = if origin == "." then original else original + "/${origin}";
      selected =
        path: type:
        (!composed || root.filter path type)
        && filter path type
        && (
          type != "directory"
          || builtins.any (entry: selected (path + "/${entry}") (builtins.readDir path).${entry}) (
            builtins.attrNames (builtins.readDir path)
          )
        );
    in
    {
      inherit origin;
      _nextest =
        let
          directory = selectedRoot + "/.config";
          file = directory + "/nextest.toml";
        in
        if builtins.pathExists file && selected directory "directory" && selected file "regular" then
          builtins.fromTOML (builtins.readFile file)
        else
          { };
      path = builtins.path {
        path = selectedRoot;
        inherit name;
        filter = selected;
      };
    };

  check =
    {
      resources,
      evaluation ? [ ],
      build,
    }:
    let
      result = build (map ({ path, ... }: path) resources);
    in
    assert result.type == "derivation";
    result
    // {
      loom = (result.loom or { }) // {
        version = 1;
        sources =
          (result.loom.sources or [ ]) ++ map ({ origin, path, ... }: { inherit origin path; }) resources;
        evaluation =
          (result.loom.evaluation or [ ]) ++ map ({ origin, path, ... }: { inherit origin path; }) evaluation;
      };
    };
  asserted =
    reporter:
    (reporter.overrideAttrs (_: {
      args = [ ./report-supervisor.sh ];
      LOOM_NATIVE_REPORT = reporter;
      LOOM_NATIVE_REPORT_INPUTS = reporter.drvAttrs.args;
    }))
    // {
      loom = reporter.loom // {
        inherit reporter;
      };
    };
  nativeDerivation =
    attributes:
    let
      result = builtins.derivation attributes;
    in
    result
    // {
      drvAttrs = attributes;
      overrideAttrs = update: nativeDerivation (attributes // update attributes);
    };
  reported =
    {
      system,
      bash,
      coreutils,
      name,
      resources,
      evaluation ? [ ],
      program,
      arguments ? [ ],
      parser,
      targets,
      tier ? "check",
      runtime ? "nix",
    }:
    assert builtins.elem runtime [
      "nix"
      "native-cgroup"
    ];
    assert runtime == "nix" || tier == "system";
    assert
      targets != [ ] || throw "native report ${name}: declare at least one complete result identity";
    assert
      builtins.length targets == builtins.length (
        builtins.attrNames (
          builtins.listToAttrs (
            map (name: {
              inherit name;
              value = null;
            }) targets
          )
        )
      )
      || throw "native report ${name}: duplicate result identities are not permitted";
    let
      contract = {
        inherit parser tier;
        targets = builtins.sort builtins.lessThan targets;
      };
      result = check {
        inherit resources evaluation;
        build =
          _:
          nativeDerivation {
            inherit name system;
            builder = "${bash}/bin/bash";
            args = [
              ./report-supervisor.sh
              program
            ]
            ++ arguments;
            LOOM_REPORT_CONTRACT = builtins.toJSON contract;
            PATH = "${coreutils}/bin";
          };
      };
      verified = asserted result;
    in
    verified
    // {
      loom = verified.loom // {
        report = contract;
        inherit runtime;
      };
    };
  cargo =
    {
      craneLib,
      operation,
      resources,
      evaluation ? [ ],
      args,
    }:
    let
      commands = {
        build = ''cargo build --offline --locked --workspace --all-targets --profile "$CARGO_PROFILE"'';
        lint = ''cargo clippy --offline --locked --workspace --profile "$CARGO_PROFILE" -- --deny warnings'';
        lint-all-targets = ''cargo clippy --offline --locked --workspace --all-targets --profile "$CARGO_PROFILE" -- --deny warnings'';
        test = ''bash "$LOOM_PRIVATE_FIXTURE_RUNNER" --build-campaign bash "$LOOM_REPORT_SUPERVISOR" cargo nextest run --offline --locked --workspace --all-targets --cargo-profile "$CARGO_PROFILE" --ignore-default-filter --test-threads 2 --no-fail-fast --message-format libtest-json-plus'';
        catalogue = ''
          mkdir -p "$out"
          cargo metadata --offline --locked --format-version 1 > "$out/cargo.json"
          cargo nextest list --offline --locked --workspace --all-targets --cargo-profile "$CARGO_PROFILE" --run-ignored all --ignore-default-filter --message-format json > "$out/nextest.json"
          cargo nextest list --offline --locked --workspace --all-targets --cargo-profile "$CARGO_PROFILE" --run-ignored all --message-format json > "$out/default.json"
          printf '%s' "$LOOM_NATIVE_GROUPS" > "$out/groups.txt"
          index=0
          while IFS= read -r group; do
            cargo nextest list --offline --locked --workspace --all-targets --cargo-profile "$CARGO_PROFILE" --run-ignored all --ignore-default-filter -E "group(=$group)" --message-format json > "$out/group-$index.json"
            index=$((index + 1))
          done <<< "$LOOM_NATIVE_GROUPS"
        '';
      };
      allowed = [
        "src"
        "cargoArtifacts"
        "cargoLock"
        "cargoToml"
        "cargoVendorDir"
        "nativeBuildInputs"
        "meta"
        "LOOM_PREK_BIN"
        "LOOM_ISSUES_BD_BIN"
        "LOOM_NATIVE_NIX_BIN"
        "LOOM_NATIVE_FLOCK_BIN"
        "LOOM_NATIVE_SHELL_BIN"
        "LOOM_NATIVE_STAT_BIN"
        "SSL_CERT_FILE"
        "GIT_CONFIG_GLOBAL"
      ];
      construct =
        op: extra:
        check {
          inherit resources evaluation;
          build =
            _:
            craneLib.mkCargoDerivation (
              args
              // extra
              // {
                buildPhaseCargoCommand = ''
                  export HOME=$(mktemp -d)
                  export GIT_CONFIG_SYSTEM=/dev/null
                  ${commands.${op}}
                '';
                doCheck = false;
                # Only the catalogue is consumed as a reusable compiler seed.
                # Terminal verdict/report outputs must not copy the whole target.
                doInstallCargoArtifacts = op == "catalogue";
                installPhaseCommand = ''mkdir -p "$out"'';
              }
            );
        };
      roots = builtins.filter (source: source.origin == "." || source.path == args.src) resources;
      configuration =
        assert builtins.length roots == 1;
        (builtins.head roots)._nextest;
      catalogue = construct "catalogue" {
        LOOM_NATIVE_GROUPS = builtins.concatStringsSep "\n" (
          [ "@global" ] ++ builtins.attrNames (configuration.test-groups or { })
        );
      };
      execution = construct operation (
        if operation == "test" then
          {
            cargoArtifacts = catalogue;
            LOOM_REPORT_SUPERVISOR = ./report-supervisor.sh;
            LOOM_PRIVATE_FIXTURE_RUNNER = ./private-fixture-tests.sh;
            NEXTEST_EXPERIMENTAL_LIBTEST_JSON = "1";
          }
        else
          { }
      );
      result = if operation == "test" then asserted execution else execution;
    in
    assert builtins.hasAttr operation commands;
    assert builtins.all (name: builtins.elem name allowed) (builtins.attrNames args);
    result
    // {
      loom =
        result.loom
        // {
          cargo = operation;
        }
        // (if operation == "test" then { catalogue = catalogue.drvPath; } else { });
    };
  app =
    {
      pkgs,
      package,
      name,
    }:
    let
      program = "${package}/bin/${name}";
      definition = check {
        resources = package.loom.sources;
        evaluation = package.loom.evaluation;
        build =
          _:
          builtins.derivation {
            name = "loom-app-definition";
            system = pkgs.stdenv.hostPlatform.system;
            builder = "${pkgs.bash}/bin/bash";
            args = [
              "-eu"
              "-c"
              "test -f \"$LOOM_APP_PROGRAM\"; test -x \"$LOOM_APP_PROGRAM\"; : > \"$out\""
            ];
            LOOM_APP_PROGRAM = program;
          };
      };
    in
    {
      type = "app";
      inherit program;
      meta.loom = { inherit definition; };
    };
in
{
  inherit
    app
    resource
    check
    reported
    cargo
    ;
}
