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

## Darwin/APFS through Wrix

The substrate-only check ran on 2026-10-22 on native arm64 macOS 26.5.2 (Darwin
25.5.0), against Loom `df4e3cfd`. Disposable sources and targets lived under the
bead's own `.loom/logs/lm-c5b0/apfs-wrix-4837uhb0/`, on the writable APFS Data
volume (`disk3s1`, 4,096-byte allocation blocks). No existing source, canonical
target, service or lifecycle record was removed or repurposed.

### Runtime and fixture

Actual `wrix spawn` used the installed host CLI
`/nix/store/yglf40fwspspz2zlghvrr5qz5i2mpqq0-wrix-rust-pi/bin/wrix` and Apple
Container (client 1.5.0, running server 1.1.0). The supported `mkSandbox` direct
`agentPkg` seam selected Linux Bash instead of an LLM agent, leaving the Rust
profile and normal entrypoint/network enforcement intact. The generated profile
was `/nix/store/m12jgbmk7x8kfrf2lziyxlsf2mjdg8mv-wrix-rust-profile-config.json`:

- Image: `wrix-rust:a61430f2`, digest
  `sha256:86333c373aa52b11bd4235626c3bc29ea2398dc14499825c2ce20e807b2c79cd`.
- Archive: `/nix/store/41f0v78a8hdsyy8cdh4a8cdvpwc8cbii-wrix-rust.tar.gz`.
- Worker: aarch64 Linux 6.14.9; rustc 1.99.1
  (`58cf8f9c35b355199727627d07ae34a4e2846d67`), Cargo 1.99.1 (`5f94df478`), LLVM
  23.1.1.
- `findmnt -T /workspace`: `virtiofs`, `rw,relatime`; the sole source projection
  was the disposable fixture root. No additional host mount or bridge was added.
- Git deploy/sign grants were false and runtime API credentials were removed
  from the launcher environment. Normal worker tool-cache sharing was enabled,
  consistent with donor eligibility; fixture Cargo independently used one job,
  its own `/workspace/cargo-home`, and explicitly cleared both compiler wrappers
  and `CARGO_TARGET_DIR` for donor and recipient builds.

The fixture created independent Git clones and a local Git dependency with a 256
KiB static payload. Both donor and recipient artifacts were built inside
separate launches of that same Linux image; native Darwin artifacts were never
used as Linux donors. Between launches, a disposable host probe called the
actual `loom_shared::artifact::prepare` with `Reuse::default()` and the
immutable image profile identity. Only the probe's own direct bead siblings were
eligible.

### Physical sharing, writes and freshness

Default CoW seeding reported **796,934 cloned library bytes**, **zero linked
bytes**, and 1,039 privately copied metadata bytes. APFS `getattrlist` with
`FSOPT_ATTR_CMN_EXTENDED` returned the following before the recipient build:

| Library                              |   Bytes | Donor inode | Recipient inode | Shared clone ID | Full-clone refcount, each |
| ------------------------------------ | ------: | ----------: | --------------: | --------------: | ------------------------: |
| `libexternal-b18124012ae92e29.rlib`  | 532,044 |    41166305 |        41166553 |        41166305 |                         2 |
| `libexternal-b18124012ae92e29.rmeta` | 264,890 |    41166290 |        41166554 |        41166290 |                         2 |

Both pairs were on device 16777233, with equal SHA-256 contents but distinct
inodes. Returned extended flags were `0x43` for donors and `0x41` for
recipients: both include **`EF_SHARES_ALL_BLOCKS` (`0x40`)**. Equal
`ATTR_CMNEXT_CLONEID`, `ATTR_CMNEXT_CLONE_REFCNT = 2` and the full-sharing flag
establish native APFS physical sharing, not an inference from sizes or
`st_blocks`. These attributes' contracts are documented by Darwin's
`getattrlist(2)`; no physical-address query, root access or allocated-block
approximation was needed.

Recipient Cargo JSON reported `external: fresh=true` and
`application: fresh=false`. Executed outputs were
`7:/workspace/operator/.loom/beads/lm-aaaa` for the donor and
`7:/workspace/operator/.loom/beads/lm-bbbb` for the recipient, confirming its
application was compiled for its own checkout. After this freshness check, a
Linux-worker write and fsync at recipient library offset 4,096 changed only its
hash; the donor hash remained unchanged. A host write and fsync at donor offset
8,192 then changed only the donor hash, preserving the modified recipient. After
those writes, APFS reported distinct clone IDs (41166305 and 41175340),
refcounts of 1 and cleared full-sharing flags (`0x01`) on the modified pair.
These intentionally damaged disposable files were not reused as successful Cargo
results or verification evidence.

### Unavailable-CoW cold fallback

Inside the same actual Linux-worker virtiofs projection, `FICLONE` returned
`EOPNOTSUPP` (95). A separately compiled Linux probe using an unchanged copy of
`loom-shared`'s production sources then prepared a third compatible recipient
with the default policy. It returned `Cold`; before Cargo, the target contained
only `.loom-shared-context`, not privately copied or hard-linked libraries. The
subsequent offline Cargo build reported both external dependency and application
`fresh=false`, and executed the third checkout's own application. Thus host-side
APFS cloning works, while an unavailable clone syscall in the worker safely
builds cold. This does not add in-worker seeding to dispatch.

### Retained evidence and portable verification

Raw image construction expression, disposable host probe source, orchestration
script, spawn configurations, stdout/stderr, Cargo artifact JSON, native APFS
attribute snapshots and summaries are retained under
`.loom/beads/lm-c5b0/.loom/logs/lm-c5b0/`. `apfs-before-build.json`,
`apfs-after-write.json`, `summary.json` and `cold-summary.json` belong to
`apfs-wrix-4837uhb0/`; the orchestration script is `lm-c5b0-conformance.py`.
These are bounded investigation fixtures, not an installed external seeding tool
or new cache/disposal authority.

The existing 17 `loom-shared` Nextest cases also passed natively on Darwin,
including unsupported-clone policy and native Cargo fixtures. This run exposed
an existing fixture-only `/var` versus `/private/var` path mismatch: Cargo
canonicalizes `CARGO_MANIFEST_DIR`, so the fixture now canonicalizes its
temporary root before creating checkout paths. The full Darwin flake check also
exposed a test control that spoofed `/tmp` while the safety runner correctly
queried its canonical `/private/tmp` alias; that control now spoofs the resolved
system temporary directory. Neither production seeding nor temporary-root safety
checks were weakened.

Ordinary policy, locks, failure, retry and Cargo-correctness coverage remains
portable Nextest work, not an expanded host-only suite. Lifecycle/retention and
admitted generated-target/shared-sccache projection are separate tasks; this
check neither claims their completion nor authorizes collection. Coordination
notes were added to `lm-ss26` and `lm-ucim` without claiming either bead.
