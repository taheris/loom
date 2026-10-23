# Historical Rust artifact storage investigation

The Cargo/sccache backend replay campaigns have been removed from Loom. Wrix and
the compiler/cache projects own backend behavior; Loom tests its adapter,
admission, lifetime and retention policy. The measurements below are historical,
not current automated verdicts. See [Cargo retention](cargo-retention.md) for
Loom's implemented boundary and current verification.

This is measured engineering advice, not a new cleanup contract or deployed
collector. [Workspaces](../specs/workspaces.md#cleanup-admission) still owns
checkout disposal; [Evidence](../specs/evidence.md#retained-safety-evidence)
still protects safety records and required witnesses. No real checkout, target,
compiler cache, Nix root, container image, source, stash, log or authority
record was removed for this investigation.

## Accounting and scope

The worker-visible snapshot began **2026-10-17 03:48:39 UTC**. GNU
`du -sx --block-size=1` measured allocated blocks; Podman and sccache supplied
their own logical accounting. These figures describe the mounted execution
environment, not every operator-side recovery copy or the entire underlying
filesystem. Queries were sequential, not one atomic filesystem snapshot.

| Visible resource                              | Allocated bytes |    GiB |
| --------------------------------------------- | --------------: | -----: |
| Seven bead `target` directories               | 301,426,720,768 | 280.73 |
| Integration `target`                          |  25,884,577,792 |  24.11 |
| `/workspace/.loom/sccache`                    |  53,829,070,848 |  50.13 |
| Effective harness `/home/wrix/.cache/sccache` |  53,848,768,512 |  50.15 |
| `/nix/store`                                  |  83,122,663,424 |  77.41 |
| Visible Podman storage                        |   5,817,237,504 |   5.42 |

The largest visible bead target was 95,142,301,696 bytes (about 88.6 GiB). The
filesystem reported 3,864,181,223,424 total bytes and 1,080,183,779,328
available bytes. Filesystem totals include unrelated data; these resource rows
are not an exhaustive partition or an additive explanation of all filesystem
use. Hard links and directory accounting also require care.

`/workspace/target` and `/workspace/.loom/archives` were absent in this view.
That does **not** establish that operator recovery data is empty. Earlier task
records measured about 1.4 TiB overall, 654 GiB of bead checkouts, 180 GiB of
integration targets, 332 GiB of recovery copies and 162 GiB of operator targets.
Those are historical measurements, not current totals or a matched time series.
The recorded earlier cleanup reclaimed about 399 GiB of inactive generated
outputs plus 21 GiB through verified recovery archival; this investigation did
not repeat that cleanup or infer new disposal authority from it.

The read-only Podman query succeeded: five images, zero containers and zero
local volumes; reported image `RawSize` and `RawReclaimable` were both
5,478,634,965 bytes. Reclaimable is an engine accounting category, **not**
permission to remove an image or runtime root. Layer sharing and metadata
explain why that number need not equal `du`. No prune, image removal or Nix GC
ran.

### Cache routing is an observed boundary

The configured shared directory and effective harness directory had different
inodes on the same device, not one directory reached through an alias. Measuring
only `.loom/sccache` would miss another approximately 50 GiB in this
environment. This does not prove every normal Loom launch has the wrong mount:
Loop already threads its configured cache mount and environment through worker
preparation. The execution profile actually selected must be audited.

One effective global sccache server snapshot reported 53,686,736,575 cached
bytes against its 53,687,091,200-byte limit: the logical 50 GiB bound was
working. Its counters showed 1,109 Rust misses and no Rust hits, plus 30
C/assembler hits; 571 of 1,734 requests were non-cacheable. These cumulative
counters are not a universal hit rate, a before/after comparison, or proof of a
particular miss cause. On-disk metadata can exceed the logical cache limit. The
two directory measurements do not authorize merging their contents or changing
trust domains.

## Bounded real multi-checkout experiment

The retired experiment constructed two actual local Git clones and a small
dependency-free Rust package. It used Rust 1.99.1, offline Cargo, one
compilation job, private source/cache/output directories and a 256 MiB
generated-file bound. It accepted no production cleanup path. Its JSON
measurement report was not verification or disposal authority. At measurement
time it ran through the actual Gate dispatcher with an immutable script copy;
that replay wiring is no longer part of Loom's checks or packages.

Observed worker fixture results:

| Case                                       | Observation                                                                                                                                          |
| ------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| Two private output directories             | 4,608,000 allocated file bytes each; equal sample executable hashes, distinct inodes                                                                 |
| Shared writable target                     | A build from clone B changed the executable reached by clone A's previously trusted output path from `one` to `two`                                  |
| Concurrent shared target                   | Both real builds finished; Cargo emitted package/build-directory lock waits                                                                          |
| Configuration namespace                    | A feature-specific native build returned `variant`; the separately retained A executable still returned `one`                                        |
| Compiler cache rematerialization           | Same-path rebuild registered a Rust cache hit, but still materialized 4,608,000 local output bytes                                                   |
| Fresh target/checkout contexts             | Cache misses were observed despite the identical sample executable; no normalization fix is assumed                                                  |
| Incremental variant                        | 163,840 additional allocated incremental-file bytes in this tiny fixture                                                                             |
| Six changing-source cache epochs           | Logical cache bytes stayed around 4.5 KiB under an 8 KiB cap, exercising actual eviction; every execution returned its changed source value          |
| Deliberately corrupted private cache entry | The real compiler wrapper recorded a cache read/error counter and recomputed the correct executable                                                  |
| Active output lease                        | A separate process received the explicit nonblocking-lock rejection while the parent held the kernel lease                                           |
| Collector interruption                     | An actual collector subprocess was killed after quarantine rename; a new process reacquired its released lease and removed only the owned quarantine |
| Source protection and continuation         | Git HEAD, stash list, dirty state and notes remained identical; retained executables still ran and an independent build continued                    |

The final worker run also checked mode 0700 controller/namespace boundaries and
mode 0555 protected fixture executable permissions. An actual UID 65534 process
could not overwrite that private controller path. Unprivileged Nix builders
report the unavailable cross-UID control explicitly instead of manufacturing a
UID result; the worker-side control is separate measured evidence. This is not a
claim that DAC protects a host target from a root worker sharing that target's
writable mount. The overwrite experiment demonstrates precisely why it cannot.

The inherited profile rejected an initial attempt to enable incremental
compilation through its compiler wrapper. Controlled incremental and uncached
comparisons clear wrapper variables **only inside the synthetic fixture**;
production profile settings remain unchanged. Corruption recovery is not
cryptographic cache authentication: a coherently forged compiled object remains
a reason not to share worker-writable caches with trusted compilation domains.
Failed fixtures and their logs were retained; successful private fixtures clean
up only their own generated files after writing their report.

Times are advisory, and a tiny standard-library fixture cannot size a real Loom
build. Example namespace hashes include toolchain, flags, lockfile, profile,
features and trust domain, but are not a complete build-input fingerprint or a
provenance/admission mechanism. The interruption test covers process death, not
power-loss durability. No production collector or disk-pressure scheduler is
installed by this investigation.

## Recommended ownership and automatic policy

**Do not share one writable `target` with the operator, integration checkout or
canonical driver.** Cargo locking serializes builds; it does not protect the
meaning of an executable pathname after another build finishes. Compiler-cache
hits likewise do not eliminate local outputs or establish fresh verifier
results. Keep trusted executables in immutable installed roots, never in an
ordinary evictable worker target.

Loom should own artifact admission and lifecycle: selected trust domain,
namespace generation, reservations, live process ownership, evidence/runtime
pins, accounting, restart reconciliation and independent-work scheduling. Use
existing Loop/Gate ownership and association boundaries; do not create another
safety registry or let agent-authored metadata assert collection eligibility.

Wrix should own the corresponding profile mechanics: private target mounts,
cache mount/environment consistency, filesystem/cgroup limits, immutable runtime
mounting and toolchain/configuration isolation. A writable compiler cache shared
within an allowed domain is still untrusted compiled data, not an execution or
evidence authority. Cross-domain reuse needs a separately verified immutable
publication path, not a common writable directory.

A concrete starting policy for the measured large Rust workload is:

1. Reserve **128 GiB per full Rust namespace**, adjustable from measured prior
   high-water usage plus headroom. Do not derive production capacity from the
   tiny fixture. Decline or queue new reservations when unavailable; continue
   independently admissible smaller work through the normal scheduler.
2. Use a configurable **256 GiB soft generated-output pool**, with a lower
   reclaim target to avoid oscillation. Active sessions may exceed a soft
   per-namespace target within their reservation; never evict their live output
   simply to force a nominal bound.
3. Prefer idle, known generated namespaces after **48 hours** or pool pressure.
   Begin disk-pressure recovery below **15% free or 64 GiB**, and fence new
   reservations below **5% free or 16 GiB**. These are proposed initial knobs,
   not newly enforced limits. If safe candidates cannot satisfy the budget,
   return explicit infrastructure unavailability, not a passing or semantically
   failed verifier.
4. Retain every live, held, pinned, unknown or ambiguously owned namespace.
   Kernel leases alone are insufficient after a driver crash if an owned build
   survives: reconcile the actual worker/build process generation and owned
   cgroup before releasing a reservation. Do not trust PID existence, mtimes,
   Beads closure or a worker-written `inactive` flag as disposal authority.
5. Quarantine only a broker-owned generated namespace under the artifact root;
   fence new allocation, reject foreign paths/symlinks, recheck ownership and
   pins, and journal durable transitions before unlinking. Resume a known
   quarantine after restart, never infer that arbitrary old directories are
   disposable. Unknown restart state is preserved with diagnostics.
6. Preserve source clones, commits, refs, stashes, logs, notes, dependency
   state, the canonical driver, admitted runtime roots, safety records and
   required witnesses. Build-cache eviction is not checkout cleanup. Archive
   workflows should omit or separately handle disposable generated outputs only
   after identifying protected evidence and active uses; never silently discard
   recovery content or run broad pruning.

The bounded experiment exercised the mechanics and failure cases behind this
recommendation. Production implementation still requires trusted lifecycle and
mount integration; it cannot be obtained by copying a prototype boolean or
running `rm -rf` over a glob of old checkouts.

## Retained evidence and implementation ownership

Loom no longer ships the upstream backend experiment or an on-demand replay
package. Private-fixture root, permissions and reservation-policy controls
remain in `tests/fixtures/private-fixture-tests.sh`; native artifact admission
and lifetime tests remain in the driver.

Read-only accounting uses `df -B1`, GNU `du -sx --block-size=1` on the selected
existing target/cache/store roots, `sccache --show-stats --stats-format=json`
under the selected profile, and `podman system df --format json`. Record missing
paths, scope, timestamp, tool versions and the effective cache location; do not
turn missing access into zero bytes. Cache-hit counts, directory totals and the
six-epoch fixture are different observations, not one growth curve.

Research evidence is retained with the owning task's
`.loom/logs/lm-jzd3/accounting-1`, `experiment-*` and `gate-experiment-*` logs.
Implementation follow-ups are recorded in Beads:

- `lm-nasp`: trusted generated-artifact reservation, retention and restart
  integration in existing Loop/Gate lifecycle.
- `lm-gzdt`: private target and effective compiler-cache profile/mount
  alignment, including Wrix integration ownership.
- `lm-q1eh`: measured Rust cache-key/miss diagnosis across isolated roots
  without weakening provenance, isolation or fresh execution.

None is a blanket prerequisite for unrelated work or `.90` admission. This
report does not claim those production changes are already implemented.
