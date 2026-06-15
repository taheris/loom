# Loom

Rust workflow orchestrator for spec-driven AI development. Loom turns a
specification into a sequence of bead-tracked tasks and dispatches them to an
agent backend (pi-mono RPC, Claude Code stream-json, or the in-process Direct
backend over `loom-llm`).

## Quick Start

```bash
nix build              # Build the loom binary
nix run -- --help      # CLI overview
nix flake check        # Clippy + nextest
```

Inside `nix develop`, the pinned Rust toolchain (`rust-toolchain.toml`),
`cargo-nextest`, the formatter, and the host-native `loom` binary are on PATH:

```bash
cargo build
cargo nextest run
```

The image-backed development shell remains available explicitly:

```bash
nix develop .#wrix
```

## Workflow

Loom drives a five-phase pipeline per spec:

1. `loom plan [SPEC_LABEL ...]` — interactive spec interview with optional anchors
2. `loom todo` — decompose changed specs into beads
3. `loom loop [BEAD_OR_EPIC_ID ...]` — execute beads through the configured agent
4. `loom gate verify` / `loom gate review` — deterministic checks + LLM judge
5. `loom msg <bead>` — resolve `loom:clarify` beads with the Options Format

See `specs/harness.md` for the full command surface and `specs/gate.md` for the
verification model.

## Specs

The behavioural contract lives in [`specs/`](specs/). Each `.md` file is a
self-contained specification; the index — along with the crate map and repo
layout — lives in [`docs/README.md`](docs/README.md).

Author specs per [`docs/spec-conventions.md`](docs/spec-conventions.md); follow
[`docs/style-rules.md`](docs/style-rules.md) for code and tests.

## Using Loom

The flake's default `loom` package is the host-native CLI only. It does not
pull Wrix sandbox images or profile manifests into the package closure, so it
is suitable for Home Manager and system profiles:

```nix
{ inputs', ... }:
{
  home.packages = [
    inputs'.loom.packages.loom
  ];
}
```

For image-backed workflows, Loom also exposes `loom-wrix`: a wrapped binary
with `wrix` on its internal PATH and `LOOM_PROFILES_MANIFEST` defaulted to a
base/rust/python manifest. Add that explicit package to a wrix devshell when
you want `loom plan` to work end-to-end without setting the env vars yourself:

```nix
{
  inputs = {
    wrix.url = "github:taheris/wrix";
    loom.url   = "github:taheris/loom";
  };

  outputs = inputs: inputs.flake-parts.lib.mkFlake { inherit inputs; } {
    perSystem = { inputs', ... }: {
      devShells.default = inputs'.wrix.legacyPackages.lib.mkDevShell {
        packages = [
          inputs'.loom.packages.loom-wrix
          # ... your other dev tools
        ];
      };
    };
  };
}
```

### Custom profile sets

`--set-default` only fires if `LOOM_PROFILES_MANIFEST` is unset, so a consumer
exporting their own manifest wins. Build one via `lib.mkProfileManifest`:

```nix
let
  wrixLib = inputs'.wrix.legacyPackages.lib;
  manifest  = inputs.loom.lib.mkProfileManifest {
    inherit wrixLib;
    profiles = { inherit (wrixLib.profiles) base rust; };
  };
in
{
  devShells.default = wrixLib.mkDevShell {
    shellHook = ''
      export LOOM_PROFILES_MANIFEST=${manifest}
    '';
    packages = [ inputs'.loom.packages.loom ];
  };
}
```

### Direct backend (embedding `loom-direct-runner` in a sandbox image)

For the Direct agent backend, the sandbox image must bundle the
`loom-direct-runner` binary. `lib.mkLoom` builds a Linux-targeted runner that
wrix can hand to `mkSandbox`:

```nix
{
  inputs.wrix.url = "github:taheris/wrix";
  inputs.loom.url   = "github:taheris/loom";

  outputs = inputs: inputs.flake-parts.lib.mkFlake { inherit inputs; } {
    perSystem = { pkgs, system, ... }:
      let
        wrix     = inputs.wrix.legacyPackages.${system}.lib;
        loomLinux  = inputs.loom.lib.mkLoom { pkgs = inputs.nixpkgs.legacyPackages.x86_64-linux; };
        sandbox    = wrix.mkSandbox {
          profile      = wrix.profiles.rust;
          agent        = "direct";
          directRunner = loomLinux.bin;
        };
      in {
        packages.sandbox = sandbox.package;
      };
  };
}
```

Loom itself has no Nix dependency on wrix — it just expects the binary on
PATH and the standard env vars.
