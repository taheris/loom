# Loom

Rust workflow orchestrator for spec-driven AI development. Loom turns a
specification into a sequence of bead-tracked tasks and dispatches them to an
agent backend (pi-mono RPC, Claude Code stream-json, or the in-process Direct
backend over `loom-llm`).

## Quick Start

The flake supports `aarch64-darwin`, `aarch64-linux`, and `x86_64-linux`. Intel
Darwin (`x86_64-darwin`) is not supported.

```bash
nix build              # Build the loom binary
nix run -- --help      # CLI overview
nix flake check        # Fast checks
nix run .#test         # Full suite
```

Inside `nix develop`, the pinned Rust toolchain (`rust-toolchain.toml`),
`cargo-nextest`, the formatter, Wrix launcher, profile-image manifest, and
`loom` binary are on PATH. The default shell is image-backed so `loom loop`
works without extra environment setup:

```bash
cargo build
cargo nextest run
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
loom loop
```

## Workflow

Loom drives a five-phase pipeline per spec:

1. `loom plan [SPEC_LABEL ...]` — interactive spec interview with optional
   anchors
2. `loom todo` — decompose changed specs into beads
3. `loom loop [BEAD_OR_EPIC_ID ...]` — execute beads through the configured
   agent
4. `loom gate verify` / `loom gate review` — deterministic checks + LLM judge
5. `loom inbox` / `loom inbox chat` — resolve clarifies, blockers, and tune
   proposals

See [Harness](specs/harness.md) for the command index and [Gate](specs/gate.md)
for verification/review composition and publication.

## Specs

The behavioural contract lives in [`specs/`](specs/), with one
`specs/<label>.md` per owner and inline Success Criteria, at most 2,000 lines
per spec. The ownership index lives in [`docs/README.md`](docs/README.md). The
intended `spec.md` / `tests.md` package split follows tested tooling support;
see the
[cutover rule](docs/spec-conventions.md#bootstrap-authoring-before-cutover).

Author specs per [`docs/spec-conventions.md`](docs/spec-conventions.md); follow
[`docs/style-rules.md`](docs/style-rules.md) for code and tests.

## Using Loom

The flake's default `loom` package is the host-native CLI only. It does not pull
Wrix sandbox images or profile manifests into the package closure, so it is
suitable for Home Manager and system profiles:

```nix
{ inputs', ... }:
{
  home.packages = [
    inputs'.loom.packages.loom
  ];
}
```

For image-backed workflows, Loom also exposes `loom-wrix`: a wrapped binary with
`wrix` on its internal PATH and `LOOM_PROFILES_MANIFEST` defaulted to a
base/rust/python × claude/pi/direct manifest. Entries are data-only: image ref,
source, source kind, and immutable profile config. The raw launcher comes from
an explicit API override, `LOOM_WRIX_SPAWN_BIN`, `LOOM_WRIX_BIN`, `WRIX_BIN`, or
host PATH, in that order. Every launch still passes the selected immutable
`--profile-config`; executable overrides never select an image. Use raw
launchers, not wrappers with baked profile configuration. Legacy manifest
`launcher` fields are ignored. See
[Wrix consumption](docs/wrix-compatibility.md) for namespaced sessions, native
availability metadata, and cold-evaluable Nix build/check interfaces. Add the
wrapped package to a Wrix devshell for end-to-end use:

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
{ inputs, inputs', pkgs, ... }:
let
  wrixLib = inputs'.wrix.legacyPackages.lib;
  manifest = inputs.loom.lib.mkProfileManifest {
    inherit pkgs wrixLib;
    profiles = { inherit (wrixLib.profiles) base rust; };
  };
in
{
  devShells.default = wrixLib.mkDevShell {
    shellHook = ''
      export LOOM_PROFILES_MANIFEST=${manifest}
      export LOOM_NATIVE_PREFLIGHT_MANIFEST=${manifest.nativePreflight}
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
        loomLinux  = inputs.loom.lib.mkLoom { pkgs = inputs.loom.inputs.nixpkgs.legacyPackages.x86_64-linux; };
        sandbox    = wrix.mkSandbox {
          profile      = wrix.profiles.rust;
          agent        = "direct";
          agentPkg = loomLinux.bin.overrideAttrs (old: {
            meta = old.meta // { mainProgram = "loom-direct-runner"; };
          });
        };
      in {
        packages.wrix = wrix.rustPackage.wrix;
        packages.image = sandbox.image.source;
        packages.profile-config = sandbox.image.profileConfig;
      };
  };
}
```

Programmatic launches pass the selected image's immutable `profile-config` to
the independent host `wrix` CLI. Loom's `sandbox` and `debug` packages are
dedicated launch apps: `nix run .#sandbox -- .` runs the selected image in the
current checkout. They are not configured `wrix` CLIs for service commands.

Loom communicates with Wrix through its CLI; consumer Nix glue selects the
launcher package and immutable profile data explicitly.
