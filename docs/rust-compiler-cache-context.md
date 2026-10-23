# Historical compiler-cache context and trust controls

The compiler/sccache replay campaign has been removed from Loom, not moved to a
manual test. Backend conformance belongs upstream. These retained measurements
are historical advice, not current automated coverage. Loom keeps focused
configuration, namespace, result-admission and [retention](cargo-retention.md)
verification.

These are bounded measurements, not a new compiler-cache, mount or verification
policy. No production cache, key hashing, environment or trusted artifact was
modified. A cache hit accelerates compilation; it neither eliminates local
outputs nor proves that a current verifier ran successfully.

## Controlled measurements

The retired experiment used actual local Git clones, pinned offline Rust/Cargo,
one compilation job and a fresh private sccache server per case. All inherited
`SCCACHE_*` settings are removed before assigning the private cache directory,
bound and loopback endpoint, preventing an inherited remote backend or socket
from selecting a production cache. Each server also receives a fresh private
empty configuration file and must report the exact private local backend and cap
before any compilation. A deliberately invalid inherited configuration, foreign
cache path and unusable parent socket are ignored by the actual control; the
unselected sentinel directory remains absent. Generated files are bounded to 256
MiB; each private cache has a 128 KiB logical limit. Source, flag, feature and
dependency changes must produce their distinct expected executable results, not
merely a successful compiler exit or a metadata cache hit.

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

## Ownership and retained evidence

Loom no longer ships this backend experiment or a replay package. The recorded
coherent-substitution result explains why cache hits cannot establish source
identity or trusted execution. Current Loom tests exercise its actual admitted
namespace configuration, override replacement and result/lifetime boundaries,
without asserting sccache hit rates, eviction or corruption recovery.

Production profile/mount work remains owned by `lm-gzdt`; trusted artifact
lifecycle and protected namespace collection remain owned by `lm-nasp`. Neither
can safely substitute cache-key normalization for those boundaries.
