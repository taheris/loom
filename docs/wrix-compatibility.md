# Consuming Loom from Wrix

Loom's Nix library is available through `import "${loomSrc}/nix/lib.nix"`.
Builds and focused consumer checks share one workspace construction:

```nix
let
  workspace = loomLib.mkLoom {
    inherit pkgs crane fenix;
    src = loomSrc;
    # Optional explicit metadata and vendor inputs:
    cargoToml = "${loomSrc}/Cargo.toml";
    cargoLock = "${loomSrc}/Cargo.lock";
    # cargoVendorDir = consumerVendor;
  };
in {
  packages.loom = workspace.bin;
  checks.loom-spawn-contract = workspace.mkCheck {
    cargoNextestExtraArgs = "-p loom-driver -E 'test(wrix_) | test(profile_manifest)'";
  };
  checks.loom-runner-inputs = workspace.mkCheck {
    cargoNextestExtraArgs = "-p loom-gate -E 'test(inputs::tests)'";
  };
}
```

`mkCheck` runs actual Nextest tests, not a fixture-derived platform verdict. It
accepts Crane check options, including additional native build inputs, while
retaining Loom's source declarations. It does not accept a replacement `src`;
construct another workspace for a different source. `checkArgs` is the explicit
argument set for consumers needing a lower-level Crane check. `cargoToml`,
`cargoLock`, and `cargoVendorDir` are also exported separately. Production and
all-target Clippy remain distinct checks.

## Cold evaluation

`stagedSrc` is a **derivation output** named `loom-src-with-extras`, not an
already materialized source directory. Passing only it and `cargoArtifacts` to
Crane's `cargoNextest` causes Crane's package-name/version and vendor inference
to inspect `stagedSrc/Cargo.toml`. A cold evaluator cannot read that output
without realizing it. This is an evaluation dependency, not evidence of store
corruption; prebuilding the source only hides it.

Use `workspace.mkCheck` or pass `workspace.checkArgs` to Crane. Both retain the
explicit source-local Cargo metadata and vendor derivation rather than inferring
them from the staged output. `loom-consumer-api` uses a fresh private Nix
evaluation store with import-from-derivation disabled: it reproduces the old
call's failure and checks the supported interface without realizing the staged
source.

## Data and executable authority

Generated profile/image entries have four fields: `ref`, `source`,
`source_kind`, and `profile_config`. The image source and immutable profile
configuration keep their Nix string contexts and matching image provenance.
Legacy `launcher` fields are ignored; they cannot select a process. Optional
legacy digest/runtime metadata is still checked/read, not executable authority.

The raw Wrix launcher comes from trusted caller context, in this order:

1. An explicit API override (`SpawnConfig.wrix_launcher` or interactive option).
2. `LOOM_WRIX_SPAWN_BIN`.
3. `LOOM_WRIX_BIN`.
4. `WRIX_BIN`.
5. Host `PATH` (`wrix`).

These must identify raw launchers, not wrappers with a baked profile. Loom does
not unwrap scripts. Every image-backed launch still passes exactly the selected
immutable `--profile-config`; changing the executable does not select an image.

`mkProfileCatalogue` exports a schema-versioned, recipe-only
`profile-catalogue`. The driver discovers it from clean, signature-verified
published operator source; no profile-manifest or catalogue environment variable
is supported. Shell entry and `mkLoomBin` do not materialize an image matrix or
carry profile metadata. Dispatch validates the selected recipe/output binding,
realizes just its single-entry manifest with signature enforcement, and retains
catalogue and selected output roots in private external state. Source and
association changes fail closed around every bounded provisioning command.

Selected manifests carry image-owned `native_preflight` metadata alongside the
exact immutable profile configuration. Native availability requests worker
readiness probes; it is not accepted verification and cannot substitute another
project's image source, ref, digest, or configuration.

## Spawn extension boundary

Wrix launch inputs remain top-level: image source/ref/kind, workspace, env,
agent arguments, and mounts. Opaque Loom session state belongs exclusively under
`extensions.loom`. Host-only launcher/profile paths, credential paths in
`launcher_env`, skill registries, and host event metadata are not serialized.
The Direct runner uses the same decoder as the host tests. Missing, null, or
non-object namespaced payloads are errors, even when legacy session fields are
present at the top level. Launch fields cannot be supplied by the session
extension, and top-level fields cannot override session state.

## Consumer migration

Remove the three Loom compatibility patches and the local manifest-builder
replacement. Keep unrelated host CLI pruning if desired. Direct profiles must
still receive the Linux `loom-direct-runner` binary: use the unpruned Linux
`workspace.bin` as `directAgentPkg`, or provide that runner separately. Replace
custom staged-source Nextest construction with `workspace.mkCheck` (or its
explicit `checkArgs`). If constructing launcher environments manually, pin a raw
Wrix executable in caller context and export the native capability output
separately. Do not restore manifest executable fields or a legacy
session-decoder fallback.

Check/System runner-input batching is already upstream. Its failure, omission,
caching, pending-target, group/tier isolation, and per-target fallback tests
remain in `loom-gate`; no batching implementation is duplicated here.
