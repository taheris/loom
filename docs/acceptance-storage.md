# Acceptance storage and recovery

Do not repeat the full suite to diagnose disk pressure or incompatible Rust
metadata. Keep the failure transcript, stop the failed run, measure first, and
resolve the narrow failure before requesting fresh full-tree acceptance.

## Compiler and target selection

The shared test-app constructor declares the immutable Rust toolchain and native
C/C++ compiler. Before Cargo, its context initializer selects that toolchain's
`rustc` and `rustdoc`, rejects a foreign Cargo executable, and removes an
inherited rustup toolchain selector. It leaves compiler-cache wrappers and cache
backends intact.

Test apps use one reusable acceptance target per checkout and toolchain. On
Linux the target lives under
`$XDG_CACHE_HOME/loom/test-targets/<checkout>/acceptance-<context>` (the
standard user cache fallback applies when unset); other platforms retain
`target/acceptance-<context>` in the checkout. The checkout key includes its
canonical path and filesystem identity. Cache ancestry must be owned by the
caller or root without unsafe write permissions; symlinks and non-directory
ancestors are rejected. No cache is imported from another checkout and no
automatic collection authority is added. The compiler context includes the
immutable toolchain path and actual Rust/Cargo versions; repeated runs reuse the
same writable output, while other checkouts and compiler versions do not share
it. Source or footprint-policy edits do not allocate a new namespace; Cargo's
own fingerprints retain those inputs. This is output isolation, not evidence or
a complete build-input fingerprint. Local Linux execution lets the existing
kernel-protected running-image identity cache work without weakening its
deliberate refusal to cache mutable FUSE/remote images. Cargo still checks
source, features, profiles and flags. An old `target/debug` is not imported into
the new context.

Ordinary test apps select bounded I/O overlap from available CPUs and the
smallest visible memory limit, reserving two GiB of headroom and 768 MiB per
executing test. Small workers can overlap up to eight fixtures; unknown memory
retains the CPU default, and explicit `NEXTEST_TEST_THREADS` is respected. This
changes scheduling, not cases, filters, outcomes or evidence. The same declared
helper is used by the fixed Cargo report constructor. Disposable Go clients and
servers default to one worker each, respecting explicit `GOMAXPROCS`; the outer
campaign supplies concurrency instead of multiplying backend workers. One-shot
workflow-fixture clients avoid repeated garbage collections, respecting explicit
`GOGC`; their long-lived SQL servers and native adapter-conformance checks
retain ordinary collection. Long-running workflow library cases are scheduled
ahead of the remaining suite so their tail can overlap other real work; no case
or filter changes.

The workspace dev profile disables incremental compilation databases and selects
`line-tables-only` debug information. Cargo's test profile inherits both
settings, so direct `cargo build`, `cargo test` and Clippy invocations do not
accumulate incremental databases outside the test-app wrapper. The wrapper also
enforces these settings for its campaign. It does not disable debug assertions,
overflow checks or tests, and it does not change production optimisation.
Source-line backtraces remain available. Compiler caching stays enabled when
configured.

A dependency-free 2,000-function probe measured 38.8 MiB with full
debug/incremental output versus 11.7 MiB with the compact-debug policy (69.9%
less allocated storage). Both executables returned the same result with debug
assertions on, both retained source-line panic backtraces, and a second compact
invocation reused the same target. This is a mechanism control, not
whole-workspace sizing or attribution of the host's unexplained usage.

The actual workspace-profile regression builds and tests another dependency-free
2,000-function probe with plain Cargo: compact dev/test output allocated
12,550,144 bytes versus 30,334,976 bytes with incremental compilation explicitly
re-enabled (58.6% less), counting each hardlinked inode once. Both builds
execute with debug assertions, unchanged builds add no target bytes, and a
source edit changes executable behavior. Re-enabling incremental compilation in
the manifest makes the regression fail. These are isolated mechanism
measurements, not whole-project disk or latency acceptance; existing generated
databases are not automatically deleted.

Terminal Nix Cargo build/lint/test checks now retain their verdict/report rather
than exporting unconsumed Cargo target archives. Crane may compress these as
incremental chains, so an export is not necessarily another full physical copy;
the bug was retaining compiler data that no downstream build consumes. The
dependency seed and Nextest catalogue still export the compiler artifacts
actually consumed by later builds. The catalogue uses explicit compilation
tools, not the full execution tool list: merely listing Loom tests must not
build the Wrix runtime. Runtime-only tools remain declared on actual test
execution and enter `PATH` only inside test-process wrappers, after compilation.
Fixture-store registration uses the declared absolute Nix tool before execution;
it does not add runtime tools to the compiler search path. Runtime tools cannot
shadow existing compiler tools. Catalogue and execution retain identical
compiler input lists; the independent equality guard is unchanged. The real
constructor regression proves that adding a runtime-only tool leaves the
compiler derivation unchanged, changes the execution derivation, and retains the
tool there; restoring the coupled list fails that regression. Dev dependency
seeds use Crane's directory/symlink mode. The catalogue inherits shared
dependency libraries and builds directly at its final immutable target path,
preserving Cargo's embedded executable paths without relocating a complete
target. Execution reads the catalogue's binary metadata without invoking
compilation, and Nextest remaps working directories to the complete current
runtime source. The disposable fixture store registers compiler sources and the
running catalogue, so re-importing a store path cannot replace a live test
working directory. The catalogue's locked, offline, all-target workspace/profile
construction and the independent compiler-input equality check remain mandatory.
Build consumers that may recompile workspace crates materialize only their own
inherited library links; unrelated dependency libraries remain shared. A real
Rust mutation control proves recompilation leaves the retained store bytes
unchanged. Successful compilation is not test evidence. The catalogue disables
package fixups and unused self-output RPATHs: it retains compiler data, not
installed executables that need another round of `patchelf`. On Linux, a
compiler-only setup hook losslessly compresses catalogue ELF debug sections with
zlib at link time. A real 2,000-function probe shrank from 5,206,072 to
1,602,944 bytes, with identical `.text`/`.rodata`, debug assertions, and actual
source-line/function-name panic backtraces. Removing the compression flag makes
the control fail. This is a mechanism measurement, not whole-project sizing. An
earlier compression-only catalogue qualification occupied 2.7 GiB versus 6.8 GiB
without compression, with identical 3,785 listed cases, five ignored cases and
123 binary entries. Sampled peak native growth was 3.8 versus 7.9 GiB; cold
compilation took 202 versus 154 seconds. These are compiler/storage
measurements, not passing test verdicts or an end-to-end latency benchmark.
Shared backing space also reflects other writers and previously allocated guest
blocks. The production release seed keeps its existing compressed format.
Binary-only native drivers and the smoke acceptance executable likewise install
their binaries, not unrelated target caches. Fresh check execution is still
required; no successful verdict is substituted from a cache.

`E0514` proves that incompatible compiler metadata reached the consumer. It does
not by itself prove cache corruption. In particular, an inherited `RUSTC` can
select a different compiler from the Clippy driver on PATH. Fix selection and
isolate output before considering cache deletion. A cache hit is never a fresh
test verdict.

## Read-only accounting

From the affected checkout, with the Rust tool provisioned by the development
shell (or `nix run .#verification-tool -- storage --dead`):

```bash
loom-verification storage --dead
```

This reads Nix's path metadata and asks its collector to **print**, not delete,
dead paths and roots. It does not recursively walk `/nix/store`, build packages,
prune profiles, or remove source, targets, caches or evidence. The report and
raw metadata are retained under `.loom/logs/storage-*`.

The report also records Nix's failed-build retention setting and lists matching
top-level temporary build directories without recursively scanning them. Failed
Nix build trees are outside the store and are not reclaimed by store GC. A
listed name is not proof that it is inactive or disposable; inspect and preserve
needed diagnostics before any explicitly scoped operator cleanup.

NAR sizes are logical serialized sizes, not allocated filesystem blocks or a
promise of physically reclaimable bytes. Store deduplication, filesystem
accounting and concurrent changes can differ. Missing size data is explicit, not
zero. Compare the store report with filesystem free space and scoped target,
cache and container measurements before attributing terabytes to one component.

## Deliberate cleanup

Finish or stop builds before cleanup. Keep required report outputs rooted or
export their diagnostic files first; a pathname in a transcript is not a Nix GC
root. Review the reporter's `gc-roots` and `largest_dead_paths` before
requesting native collection:

```bash
nix-store --gc --max-freed 107374182400
```

This requests collection of dead store paths until the native collector reaches
its 100-GiB accounting target; it may overshoot by a whole path. Nix roots
remain protected. It is not profile-generation deletion or permission to
manually remove directories from `/nix/store`. Recheck filesystem free space
afterwards.

For the explicitly identified checkout-local generated `target` only, after
stopping every build or executable using it and retaining any needed generated
witnesses, `cargo clean --target-dir "$PWD/target"` removes that target.
Initialize native Cargo patches first as required by `AGENTS.md`. Do not apply
this to managed trusted/worker namespaces, other checkouts or a symlinked
target, and do not repeatedly clean the reusable acceptance context on routine
retries.

Do not merge trust-domain caches or erase the shared sccache backend to fix one
compiler-selection failure. Its approximately 50-GiB configured cache can be
working normally. Never infer that source clones, logs, safety state, runtime
holds or dispatched managed output are garbage from age, size or a failed test.
[Cargo retention](cargo-retention.md) owns managed namespace collection.

The coordinated host handoff requires 128 GiB free on both source and store
filesystems before requesting a full run and configures Nix's dead-path GC
low/high watermarks at 32/64 GiB. During each fixed acceptance stage, its
monitor samples free space once per second and cancels only that stage's owned
process group below 32 GiB, returning unavailable rather than continuing into
the 16-GiB native admission reserve. It records space samples with the
transcript. These are admission headroom, cancellation and GC triggers, not a
hard quota for active builds or permission to interrupt unrelated jobs.
Insufficient space refuses execution rather than shrinking coverage or accepting
an unavailable check.
