# Generated Cargo storage admission

[Workspaces](../specs/workspaces.md#generated-cargo-retention) owns retention.
Worker preparation now accounts storage and reserves capacity before dependency
seeding or target projection. This is admission control, not a filesystem quota:
preparation or an active build can exceed its estimate. Seeded usage is checked
again before worker projection; an oversized unlaunched preparation rejects the
request and can be collected by subsequent pressure admission. Dispatched output
remains protected and its measured usage constrains subsequent admissions.

## Configuration

The defaults are:

```toml
[loom.cargo_retention]
max_bytes = 274877906944       # 256 GiB
reservation_bytes = 536870912 # 512 MiB per request
min_free_bytes = 17179869184  # 16 GiB remaining after the reservation
max_age_secs = 604800         # seven days
```

Optional per-profile estimates select a request size without dividing the shared
storage budget or creating separate compiler-cache pools:

```toml
[loom.cargo_retention.profile_reservations]
rust = 8589934592
base = 134217728
```

An unavailable large reservation uses the existing preflight infrastructure
retry queue, not semantic worker retries or a global Loop failure. Independently
admissible requests can continue. Zero age makes otherwise eligible abandoned
preparations immediately age-eligible; it does not weaken lifetime protection.
All quantities are bytes, except `max_age_secs`. Unknown keys, negative values
and invalid profile names fail parsing.

The namespace budget covers `.loom/cargo-artifacts`, including unrecorded output
and interrupted quarantines. Outstanding reservations survive controller
restart. Unrecorded generations remain protected and receive a conservative
default reservation no smaller than the configured default/profile estimates.
File blocks are counted once per inode within each namespace; cross-namespace
sharing is conservatively charged separately. These counters do not claim CoW
extent sharing or predict physically reclaimed volume space. Free-space checks
use the storage filesystem's actual available blocks.

Trusted verification outputs, legacy targets, source checkouts, runtime roots,
evidence and the configured sccache backend are not collection candidates.
Consumption on the same filesystem still affects its free-space check. This
policy does **not** yet provide lifecycle-based retention of trusted native
Cargo output.

## Current collection boundary

The collector can reclaim only driver-created preparations that were **never
projected to a worker**. The host records dispatch durably before installing the
mount/environment projection. A synced per-generation projection tombstone lives
outside the worker-writable target; replaying or resetting transient database
records cannot turn projected output into a never-dispatched preparation. There
is no tombstone-clear operation. Once dispatched, a generation remains
unreconciled even after launcher exit, Cargo completion or release of its lease.
Older ownership records default to unreconciled, not never-dispatched.

Completed-worker collection is intentionally not implemented: pinned Wrix lacks
the required trusted generation-bound lifetime receipt and restart observation.
Neither `.wrix/log`, Beads closure, a missing/reused PID nor worker-authored
metadata supplies that authority. An unavailable runtime observation therefore
retains output and may exhaust admission capacity rather than delete it.

Native holds identify both purpose and an immutable owner reference; releasing
one evidence or runtime owner's hold cannot release another owner's protection.
Allocation, holds and collection use the same controller fence. Quarantine
records intent before rename and removal, checks namespace/target/lease
identities and current protections, refuses indirect or writable controller
state, and reconciles interrupted placement conservatively. Missing or
conflicting records never authorize collection. There is no legacy-target
adoption, source-clone removal, broad pruning or safety-state reset operation.

## Native verification

```bash
bash scripts/configure-cargo-patches.sh
cargo nextest run -p loom-driver -p loom-workflow \
  -E 'test(cargo_retention) | test(worker_cargo_target_reuse_honors_policy_before_spawn)'
```

Bounded fixtures exercise capacity/free-space rejection, distinct pin owners,
unrecorded/legacy/conflicting records, native worker preparation and smaller
request continuation, filesystem substitutions and controller-file provenance. A
real Cargo build script survives launcher SIGKILL while its target stays
protected; even later build completion does not manufacture teardown authority.
Real collector processes are killed at journal/rename/unlink boundaries and
reopened through the production sweep path. These are Rust/Nextest checks, not
host-only scenario scripts or actual Wrix mount/backend conformance evidence.
