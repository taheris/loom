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

## Crates

| Crate | Purpose |
|-------|---------|
| `loom` | CLI entry point and process plumbing |
| `loom-driver` | State store (SQLite), bd shim, lock manager, scratchpads, git client |
| `loom-events` | Typed event identifiers (`BeadId`, `SpecLabel`, `MoleculeId`, …) |
| `loom-agent` | Backend abstraction: pi-mono RPC, Claude stream-json, and Direct |
| `loom-llm` | Public-contract LLM primitives: `LlmClient`, `Conversation`, observers |
| `loom-templates` | Askama prompt templates with typed contexts |
| `loom-gate` | Quality gate: `loom gate verify` (deterministic) + `loom gate review` (LLM judge) |
| `loom-workflow` | Phase implementations: `plan`, `todo`, `run`, `gate`, `msg` |
| `loom-render` | Streaming output formatters and event sinks |
| `loom-walk` | Spec-annotation walker for `[verify]` / `[check]` / `[system]` |
| `loom-direct-runner` | Sandbox-aware tool runtime for the Direct backend |
| `loom-test-support` | Shared test fixtures and helpers |

## Specs

The behavioural contract lives in [`specs/`](specs/). Each `.md` file is a
self-contained specification; the index lives in
[`docs/README.md`](docs/README.md).

| Spec | Code | Purpose |
|------|------|---------|
| [harness.md](specs/harness.md) | [`crates/`](crates/) | Platform: crate structure, lints, process architecture, state store, command set |
| [agent.md](specs/agent.md) | [`crates/loom-agent/`](crates/loom-agent/) | Backend abstraction (pi RPC, Claude stream-json, Direct) |
| [templates.md](specs/templates.md) | [`crates/loom-templates/`](crates/loom-templates/) | Askama templates, partials, per-phase pinning policy |
| [llm.md](specs/llm.md) | [`crates/loom-llm/`](crates/loom-llm/) | Public LLM primitives, observer chain, agent-loop |
| [gate.md](specs/gate.md) | [`crates/loom-gate/`](crates/loom-gate/) | Conformance + style + test-quality verification |
| [tests.md](specs/tests.md) | [`tests/`](tests/) | Test strategy: unit, integration, system |

Author specs per [`docs/spec-conventions.md`](docs/spec-conventions.md); follow
[`docs/style-rules.md`](docs/style-rules.md) for code and tests.

## Repo Layout

```
.
├── Cargo.toml         # Workspace manifest
├── Cargo.lock
├── clippy.toml        # Workspace-wide clippy config
├── crates/            # Rust crates (see table above)
├── specs/             # Behavioural specifications
├── docs/              # Spec-authoring conventions, style rules
├── tests/
│   ├── mock-pi/       # Mock pi binary for protocol tests
│   ├── mock-claude/   # Mock claude binary for stream-json tests
│   └── judges/        # LLM judge rubrics ([judge] annotations)
└── flake.nix
```

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

## Workflow

Loom drives a five-phase pipeline per spec:

1. `loom plan -n <label>` — interactive spec interview, writes `specs/<label>.md`
2. `loom plan -u <label>` — update an existing spec; rewrites `## Implementation Notes`
3. `loom todo <label>` — decompose the spec into beads via `bd create`
4. `loom run <label>` — execute beads one at a time through the configured agent
5. `loom gate verify` / `loom gate review` — deterministic checks + LLM judge
6. `loom msg <bead>` — resolve `loom:clarify` beads with the Options Format

See `specs/harness.md` for the full command surface and `specs/gate.md` for the
verification model.
