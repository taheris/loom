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

Inside `nix develop`, the workspace toolchain and `cargo-nextest` are on PATH:

```bash
cd loom
cargo build
cargo nextest run
```

## Workflow

Loom drives a five-phase pipeline per spec:

1. `loom plan -n <label>` — interactive spec interview
2. `loom plan -u <label>` — update an existing spec
3. `loom todo <label>` — decompose the spec into beads
4. `loom run <label>` — execute beads one at a time through the configured agent
5. `loom gate verify` / `loom gate review` — deterministic checks + LLM judge
6. `loom msg <bead>` — resolve `loom:clarify` beads with the Options Format

See `specs/harness.md` for the full command surface and `specs/gate.md` for the
verification model.

## Specs

The behavioural contract lives in [`specs/`](specs/). Each `.md` file is a
self-contained specification; the index — along with the crate map and repo
layout — lives in [`docs/README.md`](docs/README.md).

Author specs per [`docs/spec-conventions.md`](docs/spec-conventions.md); follow
[`docs/style-rules.md`](docs/style-rules.md) for code and tests.

## Using With a Wrapix Sandbox

At runtime, `loom run` shells out to `wrapix run` / `wrapix spawn`. Make sure
`wrapix` is on `PATH` and the image-ref / image-source env vars (or a
SpawnConfig) point at a sandbox image with the right agent runtime.

For the **Direct** backend specifically, the sandbox image must bundle the
`loom-direct-runner` binary. Loom's flake exposes `lib.mkLoom` so wrapix can
build a Linux-targeted runner and hand it to `mkSandbox`:

```nix
{
  inputs.wrapix.url = "github:taheris/wrapix";
  inputs.loom.url   = "github:taheris/loom";

  outputs = inputs: inputs.flake-parts.lib.mkFlake { inherit inputs; } {
    perSystem = { pkgs, system, ... }:
      let
        wrapix     = inputs.wrapix.legacyPackages.${system}.lib;
        loomLinux  = inputs.loom.lib.mkLoom { pkgs = inputs.nixpkgs.legacyPackages.x86_64-linux; };
        sandbox    = wrapix.mkSandbox {
          profile      = wrapix.profiles.rust;
          agent        = "direct";
          directRunner = loomLinux.bin;
        };
      in {
        packages.sandbox = sandbox.package;
      };
  };
}
```

Loom itself has no Nix dependency on wrapix — it just expects the binary on
PATH and the standard env vars.
