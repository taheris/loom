# Real Nix over fixture-owned physical bytes, retaining canonical logical paths.
{ pkgs }:
pkgs.runCommand "fixture-private-nix" { } ''
    mkdir -p "$out/bin"
    for command in nix nix-store; do
      cat > "$out/bin/$command" <<'TOOL'
  #!${pkgs.bash}/bin/bash
  set -euo pipefail
  real="''${NIX_STATE_DIR:?fixture must own its Nix state}/store"
  store="local?real=$real"
  command="''${0##*/}"
  # Only the physical backend changes; the subject still owns all Nix policy.
  if [[ "$command" == nix-store && "''${1:-}" == --load-db ]]; then
    ${pkgs.nix}/bin/nix-store --store "$store" "$@"
    paths=$(${pkgs.nix}/bin/nix --store "$store" path-info --all)
    while IFS= read -r path; do
      [[ -n "$path" ]] || continue
      if [[ ! -e "$real/''${path##*/}" && ! -L "$real/''${path##*/}" ]]; then
        ${pkgs.coreutils}/bin/cp -a --no-preserve=ownership --reflink=auto "$path" "$real/"
      fi
    done <<< "$paths"
    ${pkgs.nix}/bin/nix-store --store "$store" --verify --check-contents
  else
    exec ${pkgs.nix}/bin/"$command" --store "$store" "$@"
  fi
  TOOL
      chmod +x "$out/bin/$command"
    done
''
