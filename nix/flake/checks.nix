{ inputs, ... }:
{
  perSystem =
    {
      pkgs,
      loom,
      system,
      wrixLauncher,
      ...
    }:
    let
      inherit (loom)
        bin
        cargoArtifacts
        craneLib
        stagedSrc
        ;
      checked = import ../verification.nix;
      resource =
        origin:
        checked.resource {
          root = ../..;
          inherit origin;
        };
      evaluation = import ../native-context.nix;
      check = resources: build: checked.check { inherit resources evaluation build; };
      scriptCheck =
        name: resources: nativeBuildInputs: script:
        check resources (
          paths:
          pkgs.runCommand name { inherit nativeBuildInputs; } ''
            set -euo pipefail
            ${script paths}
            touch "$out"
          ''
        );
      testsDeriv = import ../../tests/default.nix {
        inherit pkgs wrixLauncher;
        loomPackage = loom;
      };
      checkedNativeCargo = import ../native-cargo.nix {
        inherit pkgs;
        config = loom.cargoConfig;
        toolchain = loom.toolchain;
      };
      bootstrapConfig =
        (pkgs.writeText "bootstrap-cargo-patches.toml" ''
          paths = []
          [env]
          LOOM_PREK_BIN = { value = "${loom.hookRunner}/bin/prek", force = true }
          LOOM_ISSUES_BD_BIN = { value = "${pkgs.beads}/bin/bd", force = true }
          LOOM_NATIVE_NIX_BIN = { value = "${pkgs.nix}/bin/nix", force = true }
          ${pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
            LOOM_NATIVE_FLOCK_BIN = { value = "${pkgs.util-linux}/bin/flock", force = true }
            LOOM_NATIVE_SHELL_BIN = { value = "${pkgs.bash}/bin/bash", force = true }
            LOOM_NATIVE_STAT_BIN = { value = "${pkgs.coreutils}/bin/stat", force = true }
          ''}
        '').overrideAttrs
          {
            passthru = {
              nextestVersion = "0.23.0";
              nextestChecksum = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
            };
          };
      bootstrapCargo = import ../native-cargo.nix {
        inherit pkgs;
        config = bootstrapConfig;
        toolchain = pkgs.writeShellScriptBin "cargo" ''
          set -euo pipefail
          printf '%s\n' "$@"
        '';
      };
      fakeLoomBin = pkgs.writeShellApplication {
        name = "loom";
        text = ''
          if [[ "''${1:-}" == --launch-inputs ]]; then
            printf 'git=%s\nraw=%s\n' "''${LOOM_WRIX_BIN:-}" "''${LOOM_WRIX_SPAWN_BIN:-}"
          elif [[ "''${1:-}" == --provisioned-inputs ]]; then
            printf '%s\n' "''${LOOM_PROVISIONED_PROFILES_MANIFEST:-}"
          else
            printf 'loom 0.0.0\n'
          fi
        '';
      };
      fakeUnprofiledWrix = pkgs.writeShellScriptBin "wrix" ''printf 'unprofiled wrix %s\n' "$*"'';
      fakeProfiledWrix =
        (pkgs.writeShellScriptBin "wrix" ''
          exec ${fakeUnprofiledWrix}/bin/wrix --profile-config /nix/store/fake-profile.json "$@"
        '').overrideAttrs
          (old: {
            passthru = (old.passthru or { }) // {
              launcher = fakeUnprofiledWrix;
            };
          });
      fakeProfileManifest = pkgs.writeText "profile-images.json" "{}";
      fakeLoomWrix = (import ../lib.nix).mkLoomBin {
        inherit pkgs;
        loomBuild.bin = fakeLoomBin;
        wrixLauncher = fakeProfiledWrix;
        profileManifest = fakeProfileManifest;
      };
      fakeSmokeNix = pkgs.writeShellScriptBin "nix" ''
        touch "$SMOKE_NIX_CALLED"
        exit 1
      '';
      darwinSmokeText =
        (import ../../tests/loom/default.nix {
          pkgs = {
            inherit (pkgs) lib;
            stdenv.hostPlatform.isLinux = false;
            writeShellApplication = { text, ... }: { inherit text; };
          };
          loomPackage = throw "unavailable smoke must not force a native build";
        }).loom-smoke.text;
      llmCompile =
        flags:
        check loom.testResources (
          _:
          craneLib.mkCargoDerivation {
            pname = "loom-llm-feature-check";
            version = "0.0.0";
            src = stagedSrc;
            inherit cargoArtifacts;
            inherit (loom) cargoVendorDir;
            doCheck = false;
            doInstallCargoArtifacts = false;
            buildPhaseCargoCommand = ''cargo check --offline --locked --profile "$CARGO_PROFILE" -p loom-llm ${flags}'';
            installPhaseCommand = "touch $out";
          }
        );
      llmCompatCases = map (name: "client::openai_compat::tests::${name}") [
        "openai_compat_multimodal_returns_unsupported_without_network"
        "openai_compat_client_sends_chat_completions_shape_to_configured_url"
        "openai_compat_client_rejects_non_compat_modelids"
        "openai_compat_wiremock_contract_covers_status_classes"
      ];
      llmCompatArchive = craneLib.mkCargoDerivation {
        pname = "loom-llm-compat-archive";
        version = "0.0.0";
        src = stagedSrc;
        inherit cargoArtifacts;
        inherit (loom) cargoVendorDir;
        nativeBuildInputs = [ pkgs.cargo-nextest ];
        doCheck = false;
        doInstallCargoArtifacts = false;
        buildPhaseCargoCommand = ''cargo nextest archive --offline --locked --cargo-profile "$CARGO_PROFILE" -p loom-llm --features openai-compat --lib --archive-file "$TMPDIR/llm-compat.tar.zst"'';
        installPhaseCommand = ''mkdir -p "$out"; cp "$TMPDIR/llm-compat.tar.zst" "$out/archive.tar.zst"'';
      };
      llmCompatProgram = pkgs.writeShellApplication {
        name = "llm-compat-report";
        runtimeInputs = [
          pkgs.bash
          pkgs.cargo-nextest
          pkgs.coreutils
        ];
        text = ''
          export HOME
          HOME=$(mktemp -d)
          mkdir -p "$HOME/tests"
          export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
          export NIX_SSL_CERT_FILE="$SSL_CERT_FILE"
          export NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1
          exec cargo-nextest nextest run --archive-file ${llmCompatArchive}/archive.tar.zst \
            --extract-to "$HOME/tests" --workspace-remap ${stagedSrc} \
            --ignore-default-filter --test-threads 2 --no-fail-fast \
            -E '${builtins.concatStringsSep " | " (map (name: "test(=${name})") llmCompatCases)}' \
            --message-format libtest-json-plus
        '';
      };
      checks = {
        smoke-unavailable = scriptCheck "smoke-unavailable" [ ] [ pkgs.bash pkgs.gnugrep ] (_: ''
          status=0
          bash -euo pipefail ${pkgs.writeText "unavailable-smoke.sh" darwinSmokeText} >"$TMPDIR/smoke.out" 2>&1 || status=$?
          [[ "$status" == 69 ]]
          grep -Fq 'container smoke not available on Darwin' "$TMPDIR/smoke.out"
        '');
        native-check-discovery = check [ ] (
          _:
          pkgs.writeText "native-check-discovery.json" (
            builtins.toJSON (import ../../tests/verify/native-check-discovery.nix { lib = inputs.nixpkgs.lib; })
          )
        );
        workspace-build-identity = checked.check {
          resources = [ ];
          evaluation = evaluation ++ loom.testResources;
          build =
            _:
            import ../tests/workspace-identity.nix {
              inherit pkgs loom;
              inherit (inputs) crane fenix;
              src = ../..;
            };
        };
        workspace-source-includes-git-policy-scripts =
          scriptCheck "workspace-source-includes-git-policy-scripts"
            ([ (resource "crates/loom-test-support/src/git_policy") ] ++ loom.testResources)
            [ ]
            (paths: ''
              for src in ${bin.src} ${stagedSrc}; do
                for script in wrix.sh sign.sh; do
                  cmp "${builtins.head paths}/$script" "$src/crates/loom-test-support/src/git_policy/$script"
                done
              done
            '');
        workspace-source-includes-nix-support =
          scriptCheck "workspace-source-includes-nix-support"
            (
              [
                (resource "nix")
                (resource "bin/loom")
              ]
              ++ loom.testResources
            )
            [ pkgs.diffutils ]
            (paths: ''
              diff --recursive ${builtins.elemAt paths 0} ${stagedSrc}/nix
              cmp ${builtins.elemAt paths 1} ${stagedSrc}/bin/loom
            '');
        nextest-cargo-home-isolation =
          scriptCheck "nextest-cargo-home-isolation"
            [ (resource "tests/agent/nextest-home.sh") ]
            [ pkgs.cargo pkgs.rustc pkgs.stdenv.cc ]
            (paths: ''
              bash ${builtins.head paths} ${pkgs.writeText "nextest-pre-unpack.sh" pkgs.cargo-nextest.preUnpack}
              bash ${builtins.head paths} ${
                pkgs.writeText "prek-pre-unpack.sh" (import ../prek.nix { inherit pkgs; }).preUnpack
              }
            '');
        acceptance-storage-report =
          scriptCheck "acceptance-storage-report"
            (map resource [
              "tests/agent/acceptance-storage-test.py"
              "scripts/acceptance-storage.py"
            ])
            [ pkgs.python3 ]
            (paths: ''
              python3 ${builtins.elemAt paths 0} ${builtins.elemAt paths 1}
            '');
        native-stage-integrity = check (loom.testResources ++ [ (resource "bin/pre-push-checks") ]) (
          _:
          assert !loom.verificationBuild.doInstallCargoArtifacts;
          assert !loom.clippy.doInstallCargoArtifacts;
          assert !loom.clippyAllTargets.doInstallCargoArtifacts;
          assert loom.clippy.cargoArtifacts == loom.cargoArtifacts;
          assert loom.clippyAllTargets.cargoArtifacts == loom.cargoArtifacts;
          assert loom.clippy.loom.cargo == "lint";
          assert loom.clippyAllTargets.loom.cargo == "lint-all-targets";
          assert !loom.nextest.loom.reporter.doInstallCargoArtifacts;
          assert loom.nextest.loom.reporter.cargoArtifacts.doInstallCargoArtifacts;
          pkgs.runCommand "native-staged-source-integrity" { } ''
            test -x ${stagedSrc}/bin/pre-push-checks
            cmp ${stagedSrc}/bin/pre-push-checks ${(resource "bin/pre-push-checks").path}
            cmp ${stagedSrc}/crates/loom-agent/src/pi/parser/terminal_provider_error.jsonl \
              ${(resource "crates/loom-agent/src/pi/parser/terminal_provider_error.jsonl").path}
            ${pkgs.python3}/bin/python3 ${stagedSrc}/tests/verify/native-resources-test.py
            touch "$out"
          ''
        );
        llm-compat-compile = llmCompile "--features openai-compat";
        llm-native-compile = llmCompile "--no-default-features";
        llm-compat-report = checked.reported {
          inherit system evaluation;
          name = "loom-llm-compat-report";
          bash = pkgs.bash;
          coreutils = pkgs.coreutils;
          resources = loom.testResources;
          program = "${llmCompatProgram}/bin/llm-compat-report";
          parser = "libtest-json";
          targets = map (name: "loom-llm::loom_llm$" + name) llmCompatCases;
        };
        nextest-filtering-syntax = check loom.testResources (
          _:
          craneLib.mkCargoDerivation {
            pname = "nextest-filtering-syntax";
            version = "0.0.0";
            src = stagedSrc;
            inherit cargoArtifacts;
            inherit (loom) cargoVendorDir;
            doCheck = true;
            doInstallCargoArtifacts = false;
            buildPhaseCargoCommand = ''cargo test --locked --profile "$CARGO_PROFILE" -p loom-gate --test nextest_filtering --no-run'';
            checkPhaseCargoCommand = ''cargo test --locked --profile "$CARGO_PROFILE" -p loom-gate --test nextest_filtering'';
            installPhaseCommand = "touch $out";
          }
        );
        native-cargo-bootstrap =
          scriptCheck "native-cargo-bootstrap" loom.testResources
            [ pkgs.git (pkgs.lib.getBin pkgs.nix) pkgs.coreutils ]
            (_: ''
              cp -r ${stagedSrc} source
              chmod -R u+w source
              cd source
              git init -q
              bash tests/agent/native-cargo.sh ${bootstrapCargo}/bin/cargo \
                ${bootstrapConfig} ${bootstrapConfig.nextestVersion} ${bootstrapConfig.nextestChecksum} ${loom.hookRunner}/bin/prek
            '');
        native-cargo-patches = check loom.testResources (
          _:
          craneLib.mkCargoDerivation {
            pname = "native-cargo-patches";
            version = "0.0.0";
            src = stagedSrc;
            inherit cargoArtifacts;
            cargoVendorDir = (inputs.crane.mkLib pkgs).vendorCargoDeps {
              cargoLock = ../../Cargo.lock;
              cargoToml = ../../Cargo.toml;
            };
            nativeBuildInputs = [ (pkgs.lib.getBin pkgs.nix) ];
            doCheck = true;
            doInstallCargoArtifacts = false;
            preBuild = ''
              cp Cargo.lock "$TMPDIR/original-Cargo.lock"
              if cargo test --locked --profile "$CARGO_PROFILE" -p loom-gate --test nextest_filtering nextest_filter_grammar_errors_retain_upstream_diagnostics > "$TMPDIR/unpatched.log" 2>&1; then
                echo "unpatched parser unexpectedly passed the strict regression" >&2
                exit 1
              fi
              grep -F 'test nextest_filter_grammar_errors_retain_upstream_diagnostics ... FAILED' "$TMPDIR/unpatched.log"
              ${checkedNativeCargo}/bin/cargo --version
              test ! -e .cargo/nix-patches.toml
              ${checkedNativeCargo}/bin/cargo test --locked --profile "$CARGO_PROFILE" -p loom-gate --test nextest_filtering nextest_filter_grammar_errors_retain_upstream_diagnostics
              test -L .cargo/nix-patches.toml
            '';
            buildPhaseCargoCommand = ''cargo test --locked --profile "$CARGO_PROFILE" -p loom-gate --test nextest_filtering --test strategy'';
            checkPhaseCargoCommand = ''cmp Cargo.lock "$TMPDIR/original-Cargo.lock"'';
            installPhaseCommand = "touch $out";
          }
        );
        loom-gate-check = check loom.testResources (
          _:
          craneLib.mkCargoDerivation {
            pname = "loom-source-walks";
            version = "0.0.0";
            src = stagedSrc;
            inherit cargoArtifacts;
            inherit (loom) cargoVendorDir;
            doCheck = true;
            doInstallCargoArtifacts = false;
            nativeBuildInputs = [
              pkgs.git
              pkgs.nix
              pkgs.cacert
              pkgs.cargo-nextest
              pkgs.jq
              pkgs.python3
            ];
            buildPhaseCargoCommand = "cargo build --offline --locked -p loom-walk --bin loom-walk --release";
            postUnpack = ''
              (
                export HOME=$(mktemp -d)
                export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
                cd "$sourceRoot"
                test ! -e .git && test ! -e target && test ! -e .cargo-home
                git init -q -b main
                printf '/target/\n/.cargo-home/\n' >> .git/info/exclude
                git add --all
                GIT_AUTHOR_DATE="@1 +0000" GIT_COMMITTER_DATE="@1 +0000" \
                  git -c user.name="Loom native source fixture" -c user.email="native-source-fixture@example.invalid" \
                    -c commit.gpgsign=false commit -q -m "Seal staged verification source"
              )
            '';
            preCheck = ''
              export HOME=$(mktemp -d)
              export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
              export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
            '';
            checkPhaseCargoCommand = "bash ${../../scripts/run-private-fixture-tests} target/release/loom-walk --from-specs";
          }
        );
        worker-native-preflight =
          scriptCheck "worker-native-preflight"
            (map resource [
              "tests/sandbox/native-preflight-test.sh"
              "scripts/worker-native-preflight.sh"
            ])
            [ pkgs.bash pkgs.coreutils pkgs.gnugrep ]
            (paths: ''
              bash ${builtins.elemAt paths 0} ${builtins.elemAt paths 1}
            '');
        test-app-ignores-host-git-signing = testsDeriv.test-app-ignores-host-git-signing;
        smoke-preflight-skips-runtime-build =
          scriptCheck "smoke-preflight-skips-runtime-build" [ (resource "scripts/run-smoke.sh") ] [ ]
            (paths: ''
              export PATH="${fakeSmokeNix}/bin:$PATH"
              export SMOKE_NIX_CALLED="$TMPDIR/nix-called"
              status=0
              bash ${builtins.head paths} "$TMPDIR/missing-fuse" "$TMPDIR/missing-tun" .#smoke-runtime > "$TMPDIR/output" 2>&1 || status=$?
              [[ "$status" == 77 ]]
              [[ ! -e "$SMOKE_NIX_CALLED" ]]
              grep -F 'skip: container runtime devices' "$TMPDIR/output"
            '');
        loom-wrapper-preserves-spaced-output-path =
          scriptCheck "loom-wrapper-preserves-spaced-output-path" [ ] [ pkgs.makeWrapper ]
            (_: ''
              (
                out="$TMPDIR/loom output"
                ${fakeLoomWrix.buildCommand}
                [[ "$("$out/bin/loom" --version)" == "loom 0.0.0" ]]
              )
            '');
        loom-wrix-does-not-default-launcher-override =
          scriptCheck "loom-wrix-does-not-default-launcher-override" [ ] [ ]
            (_: ''
              unset LOOM_WRIX_BIN LOOM_WRIX_SPAWN_BIN
              export LOOM_PROVISIONED_PROFILES_MANIFEST=/stale/operator/environment.json
              [[ "$(${fakeLoomWrix}/bin/loom --provisioned-inputs)" == ${fakeProfileManifest} ]]
              expected=$(printf 'git=%s\nraw=\n' ${fakeProfiledWrix}/bin/wrix)
              [[ "$(${fakeLoomWrix}/bin/loom --launch-inputs)" == "$expected" ]]
              export LOOM_WRIX_SPAWN_BIN=${fakeUnprofiledWrix}/bin/wrix
              expected=$(printf 'git=%s\nraw=%s\n' ${fakeProfiledWrix}/bin/wrix "$LOOM_WRIX_SPAWN_BIN")
              [[ "$(${fakeLoomWrix}/bin/loom --launch-inputs)" == "$expected" ]]
            '');
      };
    in
    {
      checks = checks // {
        smoke-host-gate = testsDeriv.smoke-host-gate;
        checks-match-host-system = checked.check {
          resources = [ ];
          evaluation = evaluation ++ loom.testResources;
          build =
            _:
            assert pkgs.lib.all (candidate: candidate.system == system) (builtins.attrValues checks);
            pkgs.runCommand "checks-match-host-system" { } ''touch "$out"'';
        };
      };
    };
}
