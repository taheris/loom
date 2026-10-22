# Generated Cargo storage admission

[Workspaces](../specs/workspaces.md#generated-cargo-retention) owns retention.
Preparation accounts storage and reserves capacity before dependency seeding or
target projection. Admission is not a filesystem quota: an active build can
exceed its estimate. Materialized usage is checked again before projection;
measured growth constrains later requests. Only known generated output with
positive lifetime reconciliation is collectible.

## Configuration

```toml
[loom.cargo_retention]
max_bytes = 274877906944       # 256 GiB
reservation_bytes = 536870912 # 512 MiB per request
min_free_bytes = 17179869184  # 16 GiB remaining after reservation
max_age_secs = 604800         # seven days

# Optional estimates, not separate budgets or compiler-cache pools:
[loom.cargo_retention.profile_reservations]
rust = 8589934592
base = 134217728
```

Unavailable capacity uses the existing preflight infrastructure retry queue, not
semantic worker retries or a global Loop failure. Independently admissible
smaller work can continue. Zero age does not weaken lifetime protection.
Quantities are bytes except `max_age_secs`; unknown keys, negative quantities
and invalid profile names fail parsing.

The worker budget covers `.loom/cargo-artifacts`, unrecorded output,
reservations, private receipts and interrupted quarantines. Unknown generations
receive a conservative reservation no smaller than the default/profile
estimates. Inodes are charged once within each namespace; cross-namespace
sharing is conservatively charged separately. Counters do not claim CoW extent
sharing or predict physically reclaimed space. Free-space checks use actual
available filesystem blocks; failed accounting is never zero usage.

Source-local legacy targets, source/Git/recovery state, runtime roots, evidence
and configured sccache backends are not collection candidates. Their filesystem
consumption still affects the free-space floor.

## Trusted lifetimes

New version-3 generations have private durable protection outside the
worker-writable target. It binds source, target, namespace, lease, profile and
context identities independently of transient ownership/accounting records.
Dispatch becomes unknown before any projection is exposed. Worker-authored
completion, `.wrix/log`, launcher exit, free leases, absent/reused PIDs, age and
Beads closure never supply retirement authority.

### Linux workers

The pinned Wrix protocol advertises `features.execution_tickets`. Loom reserves
a host-only one-shot ticket and passes it through launcher environment, never
worker SpawnConfig environment or JSON. The immutable issuer identity is
rechecked. Wrix binds an exact full Podman container ID durably before starting
it. Reconciliation requires an unambiguously exited exact object, successful
non-force removal returning that same ID, and durable retirement. A missing
object, ambiguous/unavailable observation or interrupted removal remains
protected.

Normal Wrix workspace admission still applies. Existing operator integration or
explicit standalone acknowledgment must already authorize namespace creation;
Loom never acknowledges an operator's scope automatically. Missing admission or
unsupported profiles leave output unknown, with diagnostics. No host control
socket, extra worker mount or privilege is introduced.

### Trusted-native commands

One pool per effective user/state root lives at
`$XDG_STATE_HOME/loom/trusted-cargo` (standard user-state fallback when unset).
All controlling workspaces share its operator-owned `policy.json`, with the same
fields/defaults above. Worker configuration cannot enlarge this budget. Policy
updates require quiescing admission or holding `retention.lock`; symlinks, hard
links, unsafe modes and malformed/oversized policy refuse admission. Native
requests use the default estimate, not a worker profile.

Supported Linux hosts use an already delegated, user-owned cgroup2 subtree
beneath the invoking process's current scope. No controllers, ancestor limits or
privileges are changed. Context includes that parent scope's identity. Pinned
flock/Bash/stat helpers join a unique group before the original command runs.
Shared admission leases protect pending commands; an exclusive execution lock
serializes reuse of one context/target/reservation. Host callers retain the
must-use execution guard until reaping the subprocess.

Retirement requires matching boot, filesystem and cgroup identities, an empty
`cgroup.events`, and successful removal of that exact empty group. Completion or
absence alone is insufficient. A retired target can be readmitted with a fresh
group and immutable entry script. Stale queued commands cannot join the new
generation. Held targets and the executing runtime are not reused for writes.
Worker targets/compiler-cache objects never seed native targets; compiler
wrappers and ambient sccache configuration are disabled.

Unsupported platforms, missing delegation/helpers, Nix builds, worker execution
and dynamic-loader overrides do not acquire this lifetime authority. Their
output remains unreconciled. Trusted-native commands retain their existing host
trust: this mechanism is not a sandbox against compromised same-user host code.
Shared/delegated service output and cache roots are not promoted into
collectible generations by a container receipt.

Older flat contexts and unrecorded output are charged, never adopted or deleted.
Legacy dispatched output remains protected. New native generations use
`native/<context>/generation-*/target`; allocation reuses the existing artifact
records, fence, accounting, quarantine and owner-specific hold API, not a new
evidence registry.

### Before the driver starts

The host `bin/loom` wrapper launches the checkout's locked Nix package, not an
unmanaged temporary Cargo target. Nix owns those immutable outputs and build
lifetimes; its store is outside Loom collection. Failure does not fall back to
unmanaged Cargo. Existing `loom-trusted-bootstrap.*` output remains untouched.
Workers continue using their projected target/cache and initialize Cargo patches
first.

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

## Holds and crash recovery

Evidence/runtime/operator holds identify immutable owners. Durable protection
preserves independent owners through cache replay; releasing one cannot release
another. Legacy hold operations add conservative durable protection, never
manufacture a reconciled lifetime. Cache downgrades cannot bypass these records.

Allocation, hold changes and collection share a fence held through database
teardown. Eligible collection rechecks source/namespace/target/lease identities,
current holds and positive lifetime authority. Before quarantine, a terminal
receipt is synced outside the recursively removed directory. It remains the
authoritative hold/retirement record across interrupted rename or partial
unlink, including loss of the in-directory marker. It is removed only after data
unlink and ownership-record removal. Missing/conflicting proof retains output;
there is no tombstone-clear escape hatch, source-clone removal, broad pruning or
safety-state reset.

## Verification

```bash
bash scripts/configure-cargo-patches.sh
bash scripts/run-private-fixture-tests cargo nextest run -p loom-driver -p loom-workflow \
  -E 'test(cargo_retention) | test(worker_cargo_target_reuse_honors_policy_before_spawn)'
bash scripts/verify-native-cargo-lifetime
LOOM_PROFILES_MANIFEST=/nix/store/...-profile-images.json \
  bash scripts/verify-wrix-cargo-projection
```

Permission regressions also run through the real CLI and offline Cargo:

```bash
cargo test -p loom-driver --lib private_cargo_
cargo test -p loom --test private_cargo_gate
```

These tests set umasks only in child shells, exercise creation/reuse/refusal,
and use disposable state roots rather than an operator's pool.

Portable Rust tests cover pressure, free-space/accounting failures, independent
holds, cache replay/downgrade, issuer replacement, path substitutions and real
collector SIGKILL at quarantine boundaries. Explicit Linux system tests require
real delegated cgroups and actual Wrix/Podman: real Cargo/build-script
descendants survive launcher SIGKILL, retain output through pressure/restart,
and become collectible only after exact teardown. Default-suite skips are not
runtime proof. The private fixture runner uses its own native pool with small
fixture estimates; production limits and existing operator pools are unchanged.
