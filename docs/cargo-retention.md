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

Source-local legacy targets, source checkouts, runtime roots, evidence and the
configured sccache backend are not collection candidates. Consumption on the
same filesystem still affects its free-space check.

## Trusted-native pool

Trusted commands reserve capacity in one pool per effective user/state root at
`$XDG_STATE_HOME/loom/trusted-cargo` (or the standard user-state fallback). All
controlling workspaces using that root share the budget. Worker configuration
cannot divide or enlarge it: `[loom.cargo_retention]` remains the worker policy.
The native pool's operator-owned `policy.json` uses the same policy fields and
numeric defaults, for example:

```json
{
  "max_bytes": 274877906944,
  "reservation_bytes": 536870912,
  "min_free_bytes": 17179869184,
  "max_age_secs": 604800
}
```

The first use creates a private default policy. Operator policy changes require
quiescing allocation/collection or holding that pool's `retention.lock`,
preserving a regular, singly linked, owner-only `0600` file. Missing fields
retain defaults; unknown keys, malformed/oversized files, symlinks, hard links
and unsafe modes refuse admission instead of silently selecting another budget.
Native requests use `reservation_bytes`; optional profile estimates only
increase conservative unknown-generation charges because native commands have no
worker profile.

The same CargoTarget records, durable handoff, accounting, quarantine and hold
implementation serve both trust domains. The native pool's `state.db` contains
artifact ownership/accounting, not workspace evidence or a second safety-history
registry. New targets live under `native/<context>/generation-*/target`.
Repeated commands with the same checkout/toolchain/configuration/driver context
reuse the recorded native target and reservation, after identity and capacity
rechecks. Checkout replacement changes the context. Parallel native admissions
serialize through database close; commands execute after releasing the
allocation fence. No worker target or compiler-cache object seeds these trusted
targets. Source identity is read-only context, not ownership or disposal
authority; source paths are canonicalized before overlap checks. Storage and
control-plane paths still require private, trusted ancestry. Collection
separately requires the strict source-directory check and retains unavailable or
untrusted source ownership.

The budget charges all files in this pool, including older flat contexts and
controller overhead. Unrecorded generations retain conservative reservations;
old flat contexts are charged by allocated blocks and never adopted or deleted.
A native command receives its target only after a synced private dispatch
tombstone and durable record update. Its launcher exit, a free lease or eventual
Cargo completion does not authorize disposal. Unknown or replayed ownership
never makes that output collectible. Native evidence/runtime/operator holds use
the same owner-specific API as worker holds.

As with workers, admission is not a runtime quota: active commands can outgrow
their estimates, and their measured growth constrains later requests. A new
context can be refused while independently admissible existing work continues.
Lifecycle-based collection of **dispatched** trusted output remains pending.

### Before the driver starts

The host `bin/loom` wrapper launches the current checkout's locked Nix package.
It does not create a fresh temporary Cargo target before native admission is
available. Nix owns the immutable package outputs and build lifetimes; the Nix
store remains outside Loom's native pool budget and collection authority. A
failed bootstrap returns its failure without falling back to unmanaged Cargo.
Existing `loom-trusted-bootstrap.*` directories remain untouched and protected;
this change neither adopts nor removes them.

Inside a worker, the same wrapper continues to initialize Cargo patches and run
Cargo against the existing projected target and configured worker cache. Host
bootstrap does not consume that worker output or compiler cache.

## Private-generation permission failures

Worker and native generation boundaries are created with explicit `0700`
permissions, rather than tempfile's default directory permissions. Child-process
umask tests cover both `022` and `077`, independently of the private fixture
runner. Existing directories are validated, never chmod-repaired by preparation.
Mode refusal reports the opened directory's path, observed mode and required
`0700`; the mode comes from the no-follow handle, not a second path lookup.

A restrictive umask only affects new filesystem objects. It cannot repair an
existing recorded namespace or target. Native reuse validates its recorded
namespace and target; admission also validates recorded generations belonging to
other contexts. Thus changing one generation or running a later command under
`077` can still encounter another recorded unsafe generation. Refusal preserves
its data and ownership records. An abandoned attempt that failed before record
creation is different: it remains unrecorded, conservatively charged and never
adopted or automatically repaired. A last observed path alone does not establish
which lifecycle produced it or prove it is the sole failure.

## Current collection boundary

The collector can reclaim only driver-created preparations that were **never
dispatched**, whether worker or trusted-native. The host records dispatch
durably before installing the mount/environment projection. A synced
per-generation projection tombstone lives outside the worker-writable target;
replaying or resetting transient database records cannot turn projected output
into a never-dispatched preparation. There is no tombstone-clear operation. Once
dispatched, a generation remains unreconciled even after launcher exit, Cargo
completion or release of its lease. Older ownership records default to
unreconciled, not never-dispatched.

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
bash scripts/run-private-fixture-tests cargo nextest run -p loom-driver -p loom-workflow \
  -E 'test(cargo_retention) | test(worker_cargo_target_reuse_honors_policy_before_spawn)'
```

Permission regressions also run through the real CLI and offline Cargo:

```bash
cargo test -p loom-driver --lib private_cargo_
cargo test -p loom --test private_cargo_gate
```

These tests set umasks only in child shells, exercise creation/reuse/refusal,
and use disposable state roots rather than an operator's pool.

The private fixture runner gives each suite its own native pool with a one-MiB
reservation for its many tiny checkouts. Actual allocated growth still counts;
production policy defaults and lifetime protection are unchanged. The runner
never consumes or resets an operator's existing native pool.

Bounded fixtures exercise capacity/free-space rejection, distinct pin owners,
unrecorded/legacy/conflicting records, native worker preparation and smaller
request continuation, filesystem substitutions and controller-file provenance. A
real Cargo build script survives launcher SIGKILL while its target stays
protected in both worker and trusted-native pools; even later build completion
does not manufacture teardown authority. Real collector processes are killed at
journal/rename/unlink boundaries and reopened through the production sweep path.
These are Rust/Nextest checks, not host-only scenario scripts or actual Wrix
mount/backend conformance evidence.
