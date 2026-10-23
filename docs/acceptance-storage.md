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

Test apps use one `target/acceptance-<context>` per checkout and toolchain. The
context includes the immutable toolchain path and actual Rust/Cargo versions;
repeated runs reuse the same writable output, while other checkouts and compiler
versions do not share it. Source or footprint-policy edits do not allocate a new
namespace; Cargo's own fingerprints retain those inputs. This is output
isolation, not evidence or a complete build-input fingerprint. Cargo still
checks source, features, profiles and flags. An old `target/debug` is not
imported into the new context.

The test campaign disables incremental compilation databases and selects
`line-tables-only` debug information for dev/test builds. It does not disable
debug assertions, overflow checks or tests, and it does not change production
optimisation. Source-line backtraces remain available. Compiler caching stays
enabled when configured.

A bounded dependency-free 2,000-function probe measured 38.8 MiB with full debug
and incremental output versus 11.7 MiB with this policy (69.9% less allocated
storage). Both executables returned the same result with debug assertions on,
both retained source-line panic backtraces, and a second compact invocation
reused the same target. This is a mechanism control, not whole-workspace sizing
or attribution of the host's unexplained usage.

Terminal Nix Cargo build/lint/test checks now retain their verdict/report rather
than exporting unconsumed Cargo target archives. Crane may compress these as
incremental chains, so an export is not necessarily another full physical copy;
the bug was retaining compiler data that no downstream build consumes. The
dependency seed and Nextest catalogue still export the compiler artifacts
actually consumed by later builds. Binary-only native drivers and the smoke
acceptance executable likewise install their binaries, not unrelated target
caches. Fresh check execution is still required; no successful verdict is
substituted from a cache.

`E0514` proves that incompatible compiler metadata reached the consumer. It does
not by itself prove cache corruption. In particular, an inherited `RUSTC` can
select a different compiler from the Clippy driver on PATH. Fix selection and
isolate output before considering cache deletion. A cache hit is never a fresh
test verdict.

## Read-only accounting

From the affected checkout:

```bash
python3 scripts/acceptance-storage.py --dead
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
