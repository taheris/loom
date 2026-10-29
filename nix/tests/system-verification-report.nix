# Exercise the production emitter, not a duplicate implementation of its protocol.
{ pkgs, system }:
let
  source = pkgs.writeTextDir "tests/fixtures/git/test-gitconfig" ''
    [user]
      name = Native report fixture
      email = native-report@example.invalid
  '';
  verify = import ../system-verification.nix {
    inherit pkgs system;
    loom = {
      stagedSrc = source;
      testResources = [ ];
    };
  };
  exercise =
    status:
    let
      scenario = pkgs.writeShellScript "wire-scenario-${toString status}" ''
        printf 'scenario stdout\n'
        printf 'scenario stderr\n' >&2
        exit ${toString status}
      '';
      report = verify {
        name = "wire-${toString status}";
        program = scenario;
      };
      producer = builtins.elemAt report.loom.reporter.drvAttrs.args 1;
      outcome = if status == 0 then "passed" else "failed";
      protocolStatus = if status == 0 then 0 else 1;
    in
    ''
      work=$(mktemp -d)
      mkdir "$work/report"
      status=0
      out="$work/report" ${producer} > "$work/record" || status=$?
      test "$status" = '${toString protocolStatus}'
      test "$(cat "$work/report/scenario-status")" = '${toString status}'
      test "$(cat "$work/report/scenario-stdout")" = 'scenario stdout'
      test "$(cat "$work/report/scenario-stderr")" = 'scenario stderr'
      jq -se --arg platform '${system}' '
        length == 1 and (.[0] |
          .target == "wire-${toString status}" and .outcome == "${outcome}" and
          .execution == {platform: $platform, platforms: [$platform], capabilities: []} and
          (.evidence | length > 0) and
          (has("skip_reason") | not))
      ' "$work/record" > /dev/null
    '';
  registration = pkgs.closureInfo { rootPaths = [ source ]; };
  sdkScenario = pkgs.writeShellScript "isolated-native-sdk" ''
    set -euo pipefail
    [[ "$NIX_REMOTE" == local ]]
    ${pkgs.nix}/bin/nix path-info --offline ${source}
    if ${pkgs.nix}/bin/nix-store --check-validity ${builtins.unsafeDiscardStringContext (toString pkgs.hello)}; then
      printf 'private SDK admitted an undeclared store object\n' >&2
      exit 1
    fi
  '';
  sdkReport = verify {
    name = "wire-sdk";
    program = sdkScenario;
    nixRegistration = "${registration}/registration";
  };
  sdkProducer = builtins.elemAt sdkReport.loom.reporter.drvAttrs.args 1;
in
pkgs.runCommand "system-verification-report"
  {
    nativeBuildInputs = [
      pkgs.jq
      pkgs.hello
    ];
  }
  ''
    set -euo pipefail
    ${pkgs.lib.concatMapStrings exercise [
      0
      1
      77
      78
    ]}
    work=$(mktemp -d)
    mkdir "$work/report"
    out="$work/report" ${sdkProducer} > "$work/record"
    jq -se 'length == 1 and .[0].target == "wire-sdk" and .[0].outcome == "passed"' "$work/record" >/dev/null
    touch "$out"
  ''
