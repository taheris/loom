# Rust compiler-cache context and trust controls

These are bounded measurements, not a new compiler-cache, mount or verification
policy. No production cache, key hashing, environment or trusted artifact was
modified. A cache hit accelerates compilation; it neither eliminates local
outputs nor proves that a current verifier ran successfully.

## Controlled measurements

`tests/agent/rust-cache-context.py` uses actual local Git clones, pinned offline
Rust/Cargo, one compilation job and a fresh private sccache server per case. All
inherited `SCCACHE_*` settings are removed before assigning the private cache
directory, bound and loopback endpoint, preventing an inherited remote backend
or socket from selecting a production cache. Each server also receives a fresh
private empty configuration file and must report the exact private local backend
and cap before any compilation. A deliberately invalid inherited configuration,
foreign cache path and unusable parent socket are ignored by the actual control;
the unselected sentinel directory remains absent. Generated files are bounded to
256 MiB; each private cache has a 128 KiB logical limit. Source, flag, feature
and dependency changes must produce their distinct expected executable results,
not merely a successful compiler exit or a metadata cache hit.

The measured Rust 1.99.1 / sccache 0.18.0 fixture produced:

| Target directory transport     | Basedir normalization  | Same-path rematerialization Rust hits | New-target Rust hits | Other-checkout Rust hits |
| ------------------------------ | ---------------------- | ------------------------------------: | -------------------: | -----------------------: |
| `CARGO_TARGET_DIR` environment | none                   |                                     1 |                    0 |                        0 |
| `CARGO_TARGET_DIR` environment | requested and reported |                                     1 |                    0 |                        0 |
| Cargo `--target-dir` argument  | none                   |                                     1 |                    1 |                        0 |
| Cargo `--target-dir` argument  | requested and reported |                                     1 |                    1 |                        0 |

Every new target still materialized 4,608,000 allocated file bytes. The complete
controlled worker run allocated about 85.9 MiB, below its 256 MiB bound. These
small-package figures are not production sizing, a universal speedup, or a
whole-workspace hit rate. Timings, actual compiler invocations, flags, versions,
cache counters and outputs are retained in the JSON report.

For this fixture, changing output location through a Cargo argument gained a
same-source/new-target hit while retaining separate writable outputs. This is
not permission to remove `CARGO_TARGET_DIR` from arbitrary workloads: it is a
compiler-observable environment input. The source-level controls demonstrate why
a universal environment/key normalization would be unsound.

## Observable context is part of semantics

The fixture compiles actual `env!("CARGO_MANIFEST_DIR")` and
`option_env!("CARGO_TARGET_DIR")` uses. Native execution confirms that:

- The manifest directory changes with the source checkout.
- Environment-selected target directories can appear in compiled program data.
- Choosing the directory only through Cargo's `--target-dir` leaves the optional
  target-directory environment macro unset instead.

Consequently an environment-to-argument migration can change program behavior,
not just cache performance. Test code commonly uses its manifest directory to
find fixtures; blindly rewriting that value can make a reused test executable
inspect a different checkout. Dependency, build-script and procedural-macro
behavior also belongs to the actual compiler context. Do not discard an input
from a cache key simply because this tiny package does not use it.

Recorded rustc invocations also expose the real manifest path, process-root
context, output/dependency paths, feature configuration and metadata arguments.
Multiple inputs differ across checkouts, so the remaining misses are not
attributed to one unisolated variable. Configured basedirs alone did not produce
cross-checkout hits in this experiment. A global server's cumulative counters
are not a controlled causal comparison.

The useful production direction is stable, immutable, content-addressed build
inputs and compatible toolchain/configuration contexts, with any required
runtime source/fixture paths supplied correctly. Preserve actual environment
semantics and isolated writable outputs. Benchmark the real selected producer
before changing defaults; a fixture-only improvement is not a deployed fix.

A separate real path dependency changes its implementation while retaining the
same lockfile, then changes its package version and lockfile. Both rebuilt
executables must return the new dependency result. Neither body changes nor
lock/version changes may reuse an incompatible object merely because the parent
source remains unchanged.

## Coherent cached bytes are not authenticated authority

A separate deliberately unsafe **private synthetic** configuration builds two
valid library objects whose bodies return `one` and `two`. It replaces the first
cache key's data with the second valid cache entry, restores source `one`,
removes only the generated private target and compiles again.

With the measured wrapper, the executable returned **`two` while the source
expected `one`**. A fresh semantic comparison rejects that result. The
separately retained executable still returned `one`; restoring the original
private entry and rebuilding recovered `one`. No real shared cache or installed
driver was touched. The registered control also permits a future authenticated
wrapper to reject the substitution and recompute `one`; it never treats a wrong
native result as matching source evidence.

This is stronger than malformed-cache recovery: valid compiled objects can be
coherently substituted without being authenticated to the requested key. It is
not evidence that current Loom operator and worker cache domains are actually
shared. In particular, two similarly sized cache directories may be intentional
trust-domain separation, not an error to fix by merging them.

Keep worker-writable compiler caches away from trusted compilation domains and
canonical driver/runtime artifacts. Filesystem namespace keys and cache hits are
not provenance, fresh verifier outcomes or publication authority. Cargo's
build-directory lock does not prevent a later build from replacing a shared
executable pathname; writable target isolation remains required even when
compiler caching is useful.

## Automatic replay and ownership

`checks.<system>.rust-cache-context-fixture` invokes the same fixed experiment
through actual `loom gate check --tree` with a mode 0444 script copy. Source,
flag, feature, macro, cache-substitution, retained-executable and recovery
controls run unattended with existing source-profile capabilities. There is no
manual host-test handoff, new host bridge or privilege expansion.

Normal automation can replay the pinned check:

```bash
bash scripts/configure-cargo-patches.sh
nix build --no-link .#checks.x86_64-linux.rust-cache-context-fixture
```

Failed fixtures retain their paths and diagnostics; successful private fixtures
write `rust-cache-context-report.json` before removing their own generated
files. The registered derivation retains that report as its output. The source
is the replayable coherent-substitution witness, not an expiring transcript
assertion or a mutable positive-result cache.

Production profile/mount work remains owned by `lm-gzdt`; trusted artifact
lifecycle and protected namespace collection remain owned by `lm-nasp`. Neither
can safely substitute cache-key normalization for those boundaries.
