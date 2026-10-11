{
  pkgs,
  config,
  toolchain,
}:

pkgs.lib.hiPrio (
  pkgs.writeShellScriptBin "cargo" ''
    set -euo pipefail
    case "''${1:-}" in
      --version | -V | -vV | -Vv | --help | -h | version | help) ;;
      *)
        repo_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd) # best-effort: Cargo also runs outside Git checkouts.
        if [[ -f "$repo_root/scripts/configure-cargo-patches.sh" && -f "$repo_root/.cargo/config.toml" ]]; then
          ${pkgs.bash}/bin/bash "$repo_root/scripts/configure-cargo-patches.sh" \
            ${config} ${config.nextestVersion} ${config.nextestChecksum}
        fi
        ;;
    esac
    exec ${toolchain}/bin/cargo "$@"
  ''
)
