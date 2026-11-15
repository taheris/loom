# Shared evaluator inputs for native output constructors, not execution authority.
let
  checked = import ./verification.nix;
  resource =
    origin:
    checked.resource {
      root = ../.;
      inherit origin;
    };
in
[
  # A flake source subject includes its complete Nix control plane, including
  # test modules outside nix/. This is evaluator input, never runtime access.
  (checked.resource {
    root = ../.;
    filter = path: type: type == "directory" || builtins.match ".*[.]nix" (toString path) != null;
  })
]
++ map resource [
  "nix"
  "flake.lock"
  "rust-toolchain.toml"
  "Cargo.toml"
  "Cargo.lock"
  # Crane's dummy workspace scans nested Cargo manifests and inferred targets,
  # including this independent conformance workspace outside Cargo members.
  "tests/fixtures/catalogue-reuse"
  ".cargo/config.toml"
]
