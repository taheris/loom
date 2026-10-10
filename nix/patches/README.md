# Dependency patches

## Standalone nextest filterset parsing

`nextest-filtering-parse.patch` makes `ParsedExpr::parse` return upstream
diagnostics when parser recovery produces an AST with errors. The grammar, AST,
and diagnostic messages and spans remain unchanged.

`../nextest-filtering-overlay.nix` overlays Crane's `downloadCargoPackage` to
apply this patch when unpacking the registry crate. `../workspace.nix` uses that
Crane instance for dependency artifacts, packages, Clippy, Nextest, and the
focused `nextest-filtering-syntax` check, including downstream `mkLoom`
consumers. No third-party source is checked into this repository.

Cargo dependencies use compatible version requirements. `Cargo.lock` retains the
published crate's registry source and checksum; `cargo update` can select new
compatible releases without a path override. The patch applies only to Nix
builds: bare Cargo uses the unpatched upstream parser.

Run the public-entrypoint regressions against the Nix-patched dependency:

```bash
nix build .#checks.x86_64-linux.nextest-filtering-syntax
```

Remove the overlay and patch once an upstream release passes these regressions
without them. A patch-application failure after a dependency update requires
review rather than silently falling back to the recovering parser.
