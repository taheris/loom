# Cargo target reuse validation

[Workspaces](../specs/workspaces.md#cargo-target-seeding) owns the deployed
reuse policy. This document records portable validation and bounded
measurements, not new cache trust, verification or disposal authority.

## Rust/Nextest coverage

Run ordinary coverage inside a source worker:

```bash
bash scripts/configure-cargo-patches.sh
cargo nextest run -p loom-shared
```

Policy, donor permissions, unsupported-clone fallback, clone I/O failures,
private-cache exclusion, independent writes, Cargo locks, symlink rejection,
existing-target preservation, publication races and interruption recovery need
no host access. A real child process is killed after staging readiness to prove
SIGKILL leaves only inert staging and an independent retry works. Its launcher
owns and reaps the child process group; no sleeps or host privileges are needed.

The clone syscall is the narrow external capability boundary. Deterministic
adapters supply independent files, unsupported operations and I/O failures to
exercise every policy branch on any supported filesystem. A shared adapter
contract checks native success or explicit native unavailability; an unsupported
native call is **not** reported as evidence that physical CoW sharing works.
These adapters do not replace policy, donor selection, staging or publication.

The native Cargo fixture creates independent Git clones from one disposable
source and uses a local Git dependency, an isolated Cargo home and one compiler
job. Wrapper/cache variables are cleared only for the fixture. Actual Cargo JSON
artifact messages prove that reused external dependencies are fresh while the
application recompiles for its own source/path. Concurrent builds execute
divergent application code, forced invalidation rebuilds the dependency, a new
dependency revision changes output, and donor deletion leaves the recipient
usable. Bounded subprocess capture terminates/reaps each command group after
sixty seconds; in-process tests own policy assertions.

No fixture establishes a production safety-history association, uses live
Beads/Dolt, invokes a real agent or accesses canonical runtime artifacts.

## On-demand measurements

```bash
cargo run -p loom-shared --example cargo-target-reuse -- /writable/fixture-parent
```

The optional parent selects the filesystem to measure; its default is the system
temporary directory. The example creates and removes only its own unique
temporary fixture. It does not accept an existing target or collector path. JSON
on stdout records platform/toolchain, seeding and build times, Cargo freshness,
logical transferred bytes, and allocated external-library file blocks.

Storage is counted both per path and once per `(device, inode)` across the
donor/recipient pair. Shared-inode bytes therefore quantify hard-link sharing.
Allocated file-block sums **do not** quantify CoW shared extents or total volume
reclamation; substrate-native evidence is needed for those claims. Metadata,
application binaries, Git data and Cargo home are outside this library-only
accounting. Every mode retains a private application target.

### Linux/ext4 observation

Five disposable fixtures ran inside the Linux source worker on 2026-10-20, using
x86_64 Rust/Cargo 1.99.1 and an ext4-mounted parent. Each fixture compiled a
small application and a local Git dependency with a 256 KiB static payload. The
fixed order was off, CoW-only, then hard-link-permitted, so filesystem/page
cache warming is not controlled. These are advisory tiny-fixture observations,
not a production speedup estimate or an acceptance timing quota.

| Policy            | Seed + build median (range), ms | External dependency fresh | Shared allocated library bytes |
| ----------------- | ------------------------------: | ------------------------: | -----------------------------: |
| `off`             |                   145 (140–150) |                       0/5 |                              0 |
| `copy-on-write`   |                   132 (122–136) |                       0/5 |                              0 |
| `allow-hardlinks` |                   105 (102–108) |                       5/5 |                        798,720 |

Each hard-link-permitted seed linked 797,226 logical library bytes. Inode-aware
accounting and a subsequent Cargo build confirmed 798,720 allocated library
bytes shared with its donor. CoW was unavailable on this ext4 mount; default
reuse discarded staging and built cold without silently copying or hard-linking
libraries. The differences between the two cold timings are not attributed to
reuse. Raw reports are retained under `.loom/logs/lm-xmbu/ext4-*.json`; the
example and native fixture are the replayable evidence.

## Minimal platform-only remainder

The remaining Darwin/APFS check needs the actual Wrix Linux-worker shared-mount
substrate: verify native cloning really shares physical storage there, writes
remain independent, and the recipient Linux worker accepts compatible Linux
artifacts. Record the selected image, toolchain and mount/filesystem details. Do
not infer APFS extent sharing from generic block counters or treat native Darwin
artifacts as Linux donors.

All ordinary policy, locking, failure, retry and Cargo-correctness cases above
already run in Nextest; they are not additional host-only scenarios. No new host
bridge, writable canonical target or privileged mount is needed or authorized.
