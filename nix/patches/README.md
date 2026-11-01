# Dependency patches

## Restricted procfs for nested Nix sandboxes

`nix-pid-only-procfs.patch` retains native Nix sandboxing in rootless workers
whose outer container masks non-process `/proc` entries. Linux rejects a new
full procfs mount there with `EPERM` ("Mount too revealing"), even though
private user, mount, PID and network namespaces work. Both Nix's namespace probe
and its actual builder retry that specific mount denial with `subset=pid` and
`nosuid,nodev,noexec`. Other mount failures remain failures; kernels without
PID-only procfs support cannot silently disable sandboxing. The restricted mount
exposes process information for the sandbox's own PID namespace, not the masked
global procfs entries or the parent's processes.

Inside PID-only procfs, the user-namespace sysctl files are absent. Their
absence is no longer mistaken for disabled user namespaces: Nix still rejects a
visible zero sysctl and actually attempts `CLONE_NEWUSER` before declaring
support.

`../nix-overlay.nix` applies the Linux-only patch to the Nix executable used by
Loom's compiled verifier pin, test fixtures and generated worker images. No host
socket, capability grant, unmasked parent procfs, VM, extra sandbox path or
unsandboxed execution is introduced. Programs requiring unavailable global
procfs entries still fail; their results are not reclassified as passes.

The overlay also handles rootless namespaces that deny `setgroups`: a denied
attempt to drop supplementary groups is accepted only when `getgroups` proves
there are already zero. Nonempty groups and query failures still block when
group dropping is required; that policy remains enabled in the regression.

The upstream store unit-test harness explicitly selects its private store's
single-user build mode and drops supplementary groups before entering its own
network-test user namespace, rather than assuming a host `nixbld` group or
permission to drop groups afterward. Its complete test inventory remains
enabled. The supplementary-group functional fixture checks the actual kernel
list: with default and explicit required-drop policy, already-empty groups
execute the build, while nonempty groups still require the original denial.
These fixture settings do not alter the shipped Nix configuration.

`nix-gc-startup-warnings.patch` installs Nix's existing Boehm warning-to-debug
handler before collector initialization. With PID-only procfs, Boehm's optional
CPU-count probe cannot open `/proc/stat`; initializing before registering that
handler leaked the diagnostic into ordinary CLI stderr. The handler and its
logging policy are unchanged, and no CLI test output is filtered or skipped.
Remove this patch when upstream registers the handler before initialization.

The `nix-pid-only-procfs` check creates an actual PID-only parent procfs, proves
unpatched Nix rejects constrained sandbox setup (namespace support or required
group dropping), then uses patched Nix for cold and forced warm builds with
sandbox fallback disabled. It checks inaccessible undeclared parent files,
private PID/network/UTS namespace identities, a fixed private hostname, and
absent global procfs entries. It also runs a real CLI evaluation and requires
correct stdout and completely empty stderr under the same restricted procfs. Its
`NIX_TEST_NONEMPTY_GROUPS=1` mode retains a supplementary group before entering
the user namespace, for a fresh worker to prove the required-drop rejection
independently of the empty-group success. The worker launcher must permit
kernel-checked `sethostname` and `setdomainname` without granting
parent-namespace `SYS_ADMIN`; existing launch filters cannot be repaired from
inside a running worker.

Remove this patch when upstream Nix supports this constrained procfs directly.

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

The standalone and required test apps also export the immutable runner path
explicitly for native Cargo compilation, just as the packaged builds do. A
missing optional Cargo include must not leave their CLI binaries unpinned.
`test-app-hook-runner` exercises both app constructors with real Cargo, Clippy
and Nextest, an absent optional include, and absent/conflicting ambient pins.
Only the unrelated Nix/system stages use fixture tools in that focused check.

Native tracking conformance also needs the immutable Beads executable. Both test
apps export `LOOM_ISSUES_BD_BIN`, and the same focused check verifies its
compile-time value with missing and conflicting ambient pins. The project's
pre-commit lane initializes the generated Cargo configuration before Gate runs
its tiers, including when an installed driver verifies a fresh integration
checkout without going through `bin/loom` or the worker's Cargo wrapper.
`native-cargo-bootstrap` exercises that real hook with absent and legacy
configurations. Initialization failure fails the lane, not skips native tests.

The same initialization and both test-app constructors pin
`LOOM_NATIVE_NIX_BIN`. Direct verifiers can select Nix-owned output only for
that immutable executable; arbitrary PATH entries and runtime environment values
cannot replace it. Native bootstrap tests refresh legacy configurations missing
the pin, and the test-app check covers absent/conflicting ambient pins. This
output contract denies native Cargo targets; it does not admit verifier inputs
or assert application hermeticity. See
[storage admission](../../docs/cargo-retention.md).

Remove the runner patch after an upstream release preserves these outcomes.

## Nextest capture handoff and Cargo home

`nextest-capture-handoff.patch` prevents unrelated pre-exec children from
retaining capture writers. `../nextest-overlay.nix` retains the Linux capture
handoff regression in the packaged runner's check phase.

The pinned nixpkgs Cargo build hook and this runner's check phase invoke Cargo
without setting `CARGO_HOME`. Even an offline, dependency-free Cargo build
creates `.global-cache` and package lock files under `$HOME/.cargo`. With Nix's
unsandboxed default `HOME=/homeless-shelter`, that makes subsequent derivations
fail the home-directory purity guard.

Both the Nextest overlay and the pinned prek runner prepend the shared
`../cargo-home.nix` hook before unpacking. It sets `HOME` and `CARGO_HOME`
inside the derivation's `TMPDIR`, covering build hooks and Cargo subprocesses
throughout the build while preserving upstream hooks. No purity guard or
regression is disabled, and worker startup never deletes an occupied guard. The
`nextest-cargo-home-isolation` flake check runs both actual hooks with fresh
homes and real offline Cargo build/test commands, including a control that
reproduces the cache writer in a disposable home.

```bash
nix build .#checks.x86_64-linux.nextest-cargo-home-isolation
```

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

## Restricted native-container proc

`../native-container-nix.nix` reuses the configured Nix overlay's restricted
procfs and required supplementary-group handling rather than applying a second,
conflicting procfs patch. The appliance's restricted parent procfs still
requires the kernel-enforced PID-only child mount; sandbox fallback remains
disabled. The package retains its complete upstream tests, declaring their shell
interpreters and gzip pipeline directly rather than relying on `/bin/sh` inside
a closed builder. Existing upstream skips are not native smoke coverage.
CLI-only test SDK inputs select Nix's binary output explicitly; unused
development headers are not execution prerequisites.

## Nextest compiler archive reproducibility

`../normalize-nextest-archive.sh` canonicalizes packaging headers for the
retention and compatibility compiler archives. Nextest records wall-clock mtimes
on freshly linked binaries and its metadata JSON; otherwise identical rebuilds
therefore differ. The compiler archive recipes also request Cargo's standard
release symbol stripping: LLVM's unused anonymous symbol suffixes can vary even
when every runtime section is identical. This does not omit tests or change
runtime code. Packaging normalization then preserves the compiler's payload
bytes, paths, links and executable modes. The reproducibility check varies input
timestamps, compares normalized archives, verifies restored
contents/modes/links, and rejects malformed input. Ordinary Gate rebuild
validation remains enabled; no nondeterminism exemption or cached behavioral
verdict is introduced.

## Native appliance kernel

`linux-kconfig-shell.patch` supplies immutable Bash to Kconfig shell execution
and header archive generation. Source enumeration uses Perl's argument-vector
pipe rather than an implicit `/bin/sh`. OID generation uses core Perl arbitrary-
precision binary conversion instead of unchecked shell/`bc` pipelines. Without
that repair a closed builder can silently generate incorrect OIDs and reject
valid module-signature containers. `../tests/kernel-oid-registry.py`
independently checks every encoded OID, offset, lookup hash and lookup ordering
before the kernel is built. Module-signature verification remains enabled.

`../native-container-kernel.nix` declares the finite appliance module inventory,
retains the full kernel configuration, and rejects changes to enabled security
settings when pruning unrelated drivers. It requires a healthy guest firewall,
not a disabled firewall or a missing-module exemption. The native KVM kernel is
the fixed launch boundary; there is no emulation or failed-sandbox fallback. Its
static `../native-store-projection.c` constructor runs only in that guest,
projects the already admitted read-only store, and fails closed on empty,
writable or unsupported input views. It replaces duplicate per-object service
setup without granting access to other host store objects. Successful private
working disks receive a lossless QCOW2 archive and logical-image comparison;
only that invocation's verified working representation is retired.

The disposable smoke SDK adapters in `../native-smoke-nix.nix` and
`../native-smoke-beads.nix` do not modify ordinary Nix or Beads packages. They
retain real strict builds and upstream scoped credential/sync handling. Outage
injection only adds namespace-denial restrictions to a real Nix child; it never
manufactures a passing result.

## Upstream Wrix sources

`wrix-storage-options.patch` preserves the actual Podman graph driver options
when constructing Skopeo's explicit store reference, including executable-valued
mount-program metadata. An explicit store otherwise discards the configured
single-caller ownership mode. Malformed options do not invent a store reference;
normal configured storage remains the existing default. The immutable runtime
package, Clippy and Nextest producers receive the same source patch without
import-from-derivation. The smoke declares single-caller storage before import;
no subordinate mapping helper or runtime retry is enabled.

Wrix `2ac6d00adaeb117b05c6e999d9ac8e048ec6ab26` incorporates the Git-helper Bash
invocation, immutable-store known-hosts handling and private-Nix test
portability fixes. Import its pinned modules directly; generated patched-source
imports create an unnecessary import-from-derivation boundary. The downstream
Wrix patches are removed, not the independent Cargo, Nextest or prek patches.

Claude permission prompts are client-specific. Loom's Claude backend answers
`control_request` messages on stdin and adds
`"agent_args": ["--permission-prompt-tool", "stdio", ...]` to its spawn config.
Generic Wrix entrypoints do not force that transport. The entrypoint contract
checks both the unchanged default and explicit client argument forwarding,
including a dropped-argument mutation control.
