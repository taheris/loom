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
      includes = path: type: (!composed || root.filter path type) && filter path type;
      selected =
        path: type:
        includes path type
        && (
          type != "directory"
          || builtins.any (entry: selected (path + "/${entry}") (builtins.readDir path).${entry}) (
            builtins.attrNames (builtins.readDir path)
          )
        );
    in
    {
      inherit origin includes;
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
      runtime ? null,
      purpose ? "check",
      flakeEvaluation ? false,
      build,
    }:
    let
      result = build (map ({ path, ... }: path) resources);
      executionRuntime = if runtime == null then result.loom.runtime or "nix" else runtime;
    in
    assert builtins.elem executionRuntime [
      "nix"
      "native-store"
      "native-process"
      "native-cgroup"
      "native-container"
    ];
    assert builtins.elem purpose [
      "check"
      "artifact"
    ];
    assert !flakeEvaluation || purpose == "artifact";
    assert result.type == "derivation";
    result
    // {
      loom =
        (result.loom or { })
        // (if flakeEvaluation then { inherit flakeEvaluation; } else { })
        // {
          version = 1;
          runtime = executionRuntime;
          inherit purpose;
          sources =
            (result.loom.sources or [ ]) ++ map ({ origin, path, ... }: { inherit origin path; }) resources;
          evaluation =
            (result.loom.evaluation or [ ]) ++ map ({ origin, path, ... }: { inherit origin path; }) evaluation;
        };
    };
  artifact = args: check (args // { purpose = "artifact"; });
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
      "native-process"
      "native-cgroup"
      "native-container"
    ];
    assert
      builtins.elem runtime [
        "nix"
        "native-process"
      ]
      || tier == "system";
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
      runtime ? "nix",
      args,
      package ? null,
      compilationInputs ? (args.nativeBuildInputs or [ ]),
    }:
    let
      selection = if package == null then "--workspace" else "--package ${package}";
      commands = {
        build = ''cargo build --offline --locked --manifest-path "$src/Cargo.toml" --workspace --all-targets --profile "$CARGO_PROFILE"'';
        lint = ''cargo clippy --offline --locked --workspace --profile "$CARGO_PROFILE" -- --deny warnings'';
        lint-all-targets = ''cargo clippy --offline --locked --workspace --all-targets --profile "$CARGO_PROFILE" -- --deny warnings'';
        test = ''
          runtime=$(mktemp -d)
          cp -R "''${LOOM_TEST_SOURCE_ROOT:-$src}/." "$runtime"
          chmod -R u+w "$runtime"
          export LOOM_TEST_SOURCE_ROOT="$runtime"
          cd "$runtime"
          git init -q
          git add --all
          git -c user.name='Loom native suite' -c user.email='native@example.invalid' -c commit.gpgsign=false commit -q -m 'Capture native suite source'
          selection=()
          slice='(package(=loom-verification) & binary(=cli)) | (package(=loom) & binary(=integration) & test(=native_system::native_system_scenarios_execute_actual_programs_in_canonical_store))'
          case "''${LOOM_PUBLICATION_CLI_SLICE:-}" in
            selected) selection=(--no-tests=fail -E "$slice");;
            complement) selection=(--no-tests=fail -E '!(package(=loom) & binary(=integration) & test(=native_system::native_system_scenarios_execute_actual_programs_in_canonical_store))');;
          esac
          unset LOOM_PUBLICATION_CLI_SLICE
          bash "$LOOM_PRIVATE_FIXTURE_RUNNER" --build-campaign bash "$LOOM_REPORT_SUPERVISOR" cargo nextest run --cargo-metadata "$cargoArtifacts/cargo.json" --binaries-metadata "$cargoArtifacts/binaries.json" --workspace-remap "$runtime" --target-dir-remap "$cargoArtifacts/target" --ignore-default-filter --no-fail-fast --message-format libtest-json-plus "''${selection[@]}"
        '';
        catalogue = ''
          mkdir -p "$out"
          cargo metadata --offline --locked --manifest-path "$src/Cargo.toml" --format-version 1 > "$out/cargo.json"
          cargo nextest list --offline --locked --manifest-path "$src/Cargo.toml" ${selection} --all-targets --cargo-profile "$CARGO_PROFILE" --run-ignored all --ignore-default-filter --list-type binaries-only --message-format json > "$out/binaries.json"
          listing_platform=$(jq -ce '."rust-build-meta".platforms | if (.targets | length) == 0 then .host.platform elif (.targets | length) == 1 then .targets[0].platform else error("unsupported catalogue targets") end | select((.triple | type) == "string" and (.triple | length) > 0)' "$out/binaries.json")
          listing_target=$(jq -er '.triple' <<< "$listing_platform")
          if jq -e 'has("custom-json")' <<< "$listing_platform" > /dev/null; then
            jq -er '."custom-json"' <<< "$listing_platform" > "$out/listing-target.json"
            listing_target="$out/listing-target.json"
          fi
          listing_build_directory=$(jq -er '."rust-build-meta" | (."build-directory" // ."target-directory") | select(type == "string" and length > 0)' "$out/binaries.json")
          cargo metadata --offline --locked --manifest-path "$src/Cargo.toml" --format-version 1 --all-features --filter-platform "$listing_target" > "$out/listing-cargo.json"
          cargo nextest list --cargo-metadata "$out/listing-cargo.json" --binaries-metadata "$out/binaries.json" --workspace-remap "$src" --target-dir-remap "$out/target" --build-dir-remap "$listing_build_directory" --run-ignored all --ignore-default-filter --message-format json > "$out/nextest.json"
          cargo nextest list --cargo-metadata "$out/listing-cargo.json" --binaries-metadata "$out/binaries.json" --workspace-remap "$src" --target-dir-remap "$out/target" --build-dir-remap "$listing_build_directory" --run-ignored all --message-format json > "$out/default.json"
          printf '%s' "$LOOM_NATIVE_GROUPS" > "$out/groups.txt"
          index=0
          while IFS= read -r group; do
            cargo nextest list --cargo-metadata "$out/listing-cargo.json" --binaries-metadata "$out/binaries.json" --workspace-remap "$src" --target-dir-remap "$out/target" --build-dir-remap "$listing_build_directory" --run-ignored all --ignore-default-filter -E "group(=$group)" --message-format json > "$out/group-$index.json"
            index=$((index + 1))
          done <<< "$LOOM_NATIVE_GROUPS"
        '';
      };
      allowed = [
        "src"
        "cargoArtifacts"
        "CARGO_PROFILE"
        "cargoLock"
        "cargoToml"
        "cargoVendorDir"
        "nativeBuildInputs"
        "meta"
        "LOOM_PREK_BIN"
        "LOOM_ISSUES_BD_BIN"
        "LOOM_TEST_DOLT_BIN"
        "LOOM_NATIVE_NIX_BIN"
        "LOOM_NATIVE_FLOCK_BIN"
        "LOOM_NATIVE_SHELL_BIN"
        "LOOM_NATIVE_STAT_BIN"
        "LOOM_TEST_NIX_REGISTRATION"
        "LOOM_TEST_NIX_INPUTS"
        "LOOM_TEST_SOURCE_ROOT"
        "LOOM_TEST_RUNTIME_PATH"
        "LOOM_NATIVE_RUST_FIXTURE"
        "LOOM_NATIVE_SMOKE_FIXTURE"
        "SSL_CERT_FILE"
        "GIT_CONFIG_GLOBAL"
      ];
      construct =
        op: extra:
        check {
          inherit resources evaluation runtime;
          build =
            _:
            craneLib.mkCargoDerivation (
              args
              // extra
              // {
                buildPhaseCargoCommand = ''
                  export HOME=$(mktemp -d)
                  export GIT_CONFIG_SYSTEM=/dev/null
                  export CARGO_TARGET_DIR="${if op == "catalogue" then "$out" else "$PWD"}/target"
                  ${commands.${op}}
                '';
                doCheck = false;
                # The catalogue builds at its final path so embedded Cargo binary
                # paths remain valid. No operation moves or archives another target.
                doInstallCargoArtifacts = false;
                installPhaseCommand = ''mkdir -p "$out"'';
              }
            );
        };
      # A reporter can stage a distinct runtime projection. Read compiler
      # settings from its exact compilation source, not the runtime projection.
      rootOrigins = builtins.filter (source: source.origin == ".") resources;
      roots =
        if builtins.length rootOrigins == 1 then
          rootOrigins
        else
          builtins.filter (source: source.path == args.src) (
            if rootOrigins == [ ] then resources else rootOrigins
          );
      configuration =
        assert builtins.length roots == 1;
        (builtins.head roots)._nextest;
      catalogue = construct "catalogue" {
        nativeBuildInputs = compilationInputs;
        prePatch = ''export CARGO_TARGET_DIR="$out/target"'';
        dontFixup = true;
        NIX_NO_SELF_RPATH = 1;
        LOOM_TEST_SOURCE_ROOT = "";
        LOOM_TEST_RUNTIME_PATH = "";
        LOOM_TEST_NIX_REGISTRATION = "";
        LOOM_TEST_NIX_INPUTS = [ ];
        LOOM_NATIVE_RUST_FIXTURE = "";
        LOOM_NATIVE_SMOKE_FIXTURE = "";
        GIT_CONFIG_GLOBAL = "/dev/null";
        LOOM_NATIVE_GROUPS = builtins.concatStringsSep "\n" (
          [ "@global" ] ++ builtins.attrNames (configuration.test-groups or { })
        );
      };
      execution = construct operation (
        if operation == "test" then
          {
            cargoArtifacts = catalogue;
            # Execution reads immutable catalogue binaries directly; no writable target is needed.
            prePatch = ''postPatchHooks=("''${postPatchHooks[@]/inheritCargoArtifacts/}")'';
            NIX_NO_SELF_RPATH = 1;
            LOOM_REPORT_SUPERVISOR = ./report-supervisor.sh;
            LOOM_PRIVATE_FIXTURE_RUNNER = ./private-fixture-tests.sh;
            LOOM_FIXTURE_INPUT_LOADER = ./retain-fixture-inputs.sh;
            LOOM_TEST_SCHEDULING = ./test-scheduling.sh;
            NEXTEST_EXPERIMENTAL_LIBTEST_JSON = "1";
          }
        else
          { }
      );
      result = if operation == "test" then asserted execution else execution;
    in
    assert builtins.hasAttr operation commands;
    assert
      package == null
      || (operation == "test" && builtins.match "[A-Za-z0-9_][A-Za-z0-9_-]*" package != null);
    assert runtime != "native-store" || operation == "test";
    assert builtins.all (name: builtins.elem name allowed) (builtins.attrNames args);
    result
    // {
      compilation = if operation == "test" then catalogue else null;
      loom =
        result.loom
        // {
          cargo = operation;
        }
        // (if operation == "test" then { catalogue = catalogue.drvPath; } else { })
        // (if package == null then { } else { cargoPackage = package; });
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
        runtime = package.loom.runtime or "nix";
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
    artifact
    resource
    check
    reported
    cargo
    ;
}
