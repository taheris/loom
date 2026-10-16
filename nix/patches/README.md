# Dependency patches

## Hook termination status

`prek-hook-status.patch` retains native Unix hook signals as `128 + signal`
instead of collapsing them to semantic exit 1. Batched hook execution keeps
completed semantic failures dominant over infrastructure failures, normalizing
combined semantic failures to exit 1 instead of bitwise-ORing them into reserved
infrastructure statuses. The runner propagates its native aggregate exit status
across projects and hook groups; Gate never derives outcomes from display text.
Gate uses this pinned runner through the same generated Cargo configuration as
the parser patch, and Nix builds embed the identical runner path. Ambient
`PREK_QUIET` is removed for the Gate-owned invocation so exit reports remain
available. Actual SIGTERM/SIGKILL hooks, explicit infrastructure exits, mixed
semantic failures, and quiet environments are exercised by `gate_cli_scope`
against real prek. Bootstrap builds made with an older image's Cargo
configuration can still run provisioning commands. Their diff-hook verification
returns infrastructure exit 78 until current Cargo initialization supplies the
pinned runner; they cannot silently fall back to an ambient runner.

Remove the runner patch after an upstream release preserves these outcomes.

## Standalone nextest filterset parsing

`nextest-filtering-parse.patch` makes `ParsedExpr::parse` return upstream
diagnostics when parser recovery produces an AST with errors. The grammar, AST,
and diagnostic messages and spans remain unchanged. In particular, an unpatched
`ParsedExpr::parse("test(")` recovers an empty contains matcher while discarding
`InvalidString` and `ExpectedCloseParenthesis` diagnostics. That recovered AST
is not accepted syntax at Loom’s strict boundary. Keep these negative fixtures;
empty regexes (`test(//)`), wildcard globs (`test(#*)`), and escaped closing
parentheses are valid edge syntax and have positive coverage.

`../nextest-filtering-overlay.nix` overlays Crane's `downloadCargoPackage` to
apply this patch when unpacking the registry crate. `../workspace.nix` uses that
Crane instance for dependency artifacts, packages, Clippy, Nextest, and the
focused `nextest-filtering-syntax` check, including downstream `mkLoom`
consumers. No third-party source is checked into this repository.

Cargo dependencies use compatible version requirements. `Cargo.lock` retains the
published crate's registry source and checksum; `cargo update` can select new
compatible releases. Native Cargo uses the same patched source through a
generated `paths` override, which does not alter registry resolution or the
dependency graph. The dev shells, test apps, and pre-push wrapper initialize
`.cargo/nix-patches.toml`; only its optional include is tracked in Git.

To initialize or refresh native Cargo after updating dependencies:

```bash
bash scripts/configure-cargo-patches.sh
```

The helper selects the crate version and checksum from the current `Cargo.lock`
and retains its Nix store closure. `cargo update` still resolves compatible
registry releases; subsequent push hooks refresh the override automatically.
Loom's Rust worker images also carry this exact dependency closure and a Cargo
wrapper that initializes the override before build/test commands, without a
dev-shell hook or host execution bridge. The wrapper checks the dispatched
lockfile's version and checksum against its bundled source; a changed identity
rebuilds the override in the worker's private Nix store. Cargo health/help
probes and unrelated repositories do not initialize an override. Outside a
prepared checkout, native Cargo still uses the upstream parser.

Run the public-entrypoint regressions against the Nix-patched dependency:

```bash
nix build .#checks.x86_64-linux.nextest-filtering-syntax
nix build .#checks.x86_64-linux.native-cargo-patches
```

The native check starts with unpatched registry sources, verifies the strict
regression fails, initializes the override, and runs the parser and strategy
tests through ordinary Cargo without changing the lockfile.

Remove the overlay and native override once an upstream release passes these
regressions without them. A patch-application failure after a dependency update
requires review rather than silently falling back to the recovering parser.
