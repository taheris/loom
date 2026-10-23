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

## Discovery and disposable tests

Driver-owned Cargo metadata and inventory queries deny native Cargo output at
`/dev/null/loom-cargo-discovery`. They disable compiler wrappers/cache overrides
without opening the native pool, reserving capacity or creating a generation. A
discovery wrapper that attempts compilation fails; it does not gain permission
to use ambient output. This is an explicit operation boundary, not a
command-name exemption. Arbitrary input helpers and verifier commands can
compile, including through wrappers, and still require an execution guard and
native admission.

Direct verifier invocations of the build-pinned Nix executable use an enforced
Nix-owned output contract: `build`, `run`, `eval`, read-only inspection and
flake checks cannot use native Cargo output. Nix owns store builds and their
lifetimes; all three native Cargo output aliases point at the denied discovery
path and compiler caching remains disabled. This does not assert that a Nix
application is hermetic or grant input/evidence admission. A runtime native
Cargo write fails rather than escaping to ambient worker or canonical output.
Applications that need native compilation must use a managed native runner, for
example an opaque `env nix run ...` or shell wrapper; `nix shell` and
`nix develop` remain managed.

The command constructor resolves the selected executable before spawning and
pins it only when it matches the immutable Nix tool supplied by the build.
Mutable/shadowed front ends, unsupported invocation shapes and absent pins keep
normal native admission. Arguments, cwd, scope environment, cleared environments
and standard I/O are not reconstructed for the Nix-owned path. Repeated queries,
execution and failures create no target, policy or native allocation record.
Native Cargo initialization and Nix packages supply `LOOM_NATIVE_NIX_BIN`; an
ambient executable name or runtime pin environment value cannot select this
contract.

The ordinary Nextest profile wraps listing and execution with
`scripts/run-private-fixture-tests`. Each invocation gets private temporary and
state roots. Runtime Cargo targets point at a private path that is created only
if used. Explicit `--build-campaign` preserves the primary compilation's
admitted Cargo target and warm artifacts; Nextest still isolates every test
subprocess. It must not clear a target supplied by a compiling caller's
execution guard. Empty compiler-wrapper values disable Cargo-configured wrappers
(unsetting them would re-enable configuration). Compiler-cache directories and
server sockets are private too. The runner propagates the child's exit status
and removes its disposable fixture root on success or failure. Descendants
inherit that state selection. Nix suite wrappers may still wrap the whole
campaign as well.

The `test-support` feature also gives unwrapped trusted-command calls a private
fixture root owned by their execution guard, rather than falling back to the
operator pool. Keep that guard until subprocess reaping, including errors.
`LOOM_TEST_STORAGE=1` selects an explicitly owned fixture state root instead;
the private runner exports it, and standalone lifecycle fixtures set it together
with their disposable `XDG_STATE_HOME`. It is a test-only convention, not a
production lifetime or collection authority. Production builds without that
feature continue using the operator pool. For standalone Cargo tests, prefer
`bash scripts/run-private-fixture-tests cargo test ...` to isolate all process
state, not just managed Cargo commands.

Marker CLI/hook tests live in `crates/loom/tests/marker_cli.rs`, where Cargo
supplies the current prebuilt CLI. The former gate-library `cargo run` fallback
is removed: it allocated native generations and could rebuild Loom in each
fixture. Its generated shell launcher also dropped the execution guard before
launch. The replacement still drives the real CLI, mint and hook paths, with
repeated execution and assertions of zero generated storage; no runtime Loom
build or stale sibling-binary lookup is used.

### Focused pollution reproduction

A disposable-pool run of the pre-change Gate test binary demonstrated that
`binary_print_inputs_protocol_parses_json_inputs` and
`run_check_spawns_subprocess_and_parses_verdict_from_stdout` each added one
native generation per run. Ten runs each left 20 records/generations occupying
380,928 allocated bytes. The fixture policy reserved only 1 MiB each; under the
operator default, those same 20 reservations would charge at least 10 GiB. The
synthetic scope fixture added no generation; temporary source names alone were
not treated as attribution evidence.

After isolation, ten direct runs of each input/dispatch fixture and each real
scope/inventory metadata regression left zero files in the inherited disposable
operator-state directory. The real host pool stayed at 504 records and 509
generations. Advisory warm medians, including libtest process startup, were
45.18 ms (input fixture), 45.34 ms (dispatch fixture), 65.59 ms (scope metadata)
and 64.98 ms (inventory metadata). Each metadata regression performs two
successful queries and one failure. The pre-change input/dispatch artifact's
medians were 27.88/30.93 ms, without disposable allocation cleanup. These are
focused local observations, not a Weld hook benchmark or timing gate. The
ordinary eight-regression Nextest run passed in 0.554 seconds after compilation.

The Nix-owned regression additionally runs real input probes and Gate dispatch
against a disposable zero-byte native policy. Ten warm campaigns, each with 18
Nix invocations (including failures), had a median of 322.95 ms and maximum of
336.04 ms. No native database or generation was created; the host remained at
504 records / 509 generations. A separate real `nix run` app fixture repeats
success/failure without compiling anything; its private Nix store is collected
through native `nix store gc`, not permission repair or deletion of host output.
These are Loom execution-path measurements, not claims about total Weld hook
latency.

## Legacy pressure recovery and steady state

Free disk space does not override reservation admission. Five hundred protected
512-MiB reservations charge 250 GiB even if materialized output occupies only a
few GiB. The reservation is an estimate, not a per-build allocation or
filesystem quota; `max_bytes` bounds aggregate retained usage/reservations, not
the size of one build. Raising an exhausted budget is pressure relief, not
retirement proof.

For an existing pool:

1. Preserve the policy backup, generation data, lifetime records and independent
   holds. Stop fixture pollution before considering capacity changes.
2. Inventory through read-only inspection. Do not infer retirement from age,
   missing processes/sources, completed tests or Beads closure.
3. Use native lifetime reconciliation only for generations with matching trusted
   execution authority. Missing authority remains protected. Current native
   cgroup reconciliation cannot retrospectively certify legacy version-2
   dispatched output; no supported bulk retirement/adoption operation exists.
4. Release a hold only through its owner's native lifecycle, never by editing
   records. Neither removing source checkouts nor rebuilding the transient cache
   grants generation disposal authority.
5. Keep the current budget while protected charges exceed the intended budget.
   Any later operator policy update must quiesce admission or hold
   `retention.lock`, preserve unrelated fields and remain above protected
   charges plus admitted headroom. Do not restore an older smaller policy
   blindly.

A reasonable initial steady-state target remains an aggregate 256-GiB pool,
512-MiB default reservation, 16-GiB free-space floor and seven-day _eligibility_
threshold, with actual build measurements driving larger estimates (for example,
8 GiB for a measured Rust build). A 50-GiB per-build requirement is not implied.
These estimates do not enforce a hard per-build ceiling. Supported trusted
lifetimes and positive retirement, not a larger budget or an age threshold,
allow retained generations to become collectible. Until legacy authority can be
established by a supported lifecycle, their protected charge is irreducible;
there is no safe promise that this pool can return to 256 GiB immediately.

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
bash scripts/run-private-fixture-tests --build-campaign cargo nextest run -p loom-driver -p loom-workflow \
  -E 'test(cargo_retention) | test(worker_cargo_target_reuse_honors_policy_before_spawn)'
bash scripts/verify-native-cargo-lifetime
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
real delegated cgroups: real Cargo/build-script descendants survive launcher
SIGKILL, retain output through pressure/restart, and become collectible only
after exact teardown. Default-suite skips are not runtime proof. Wrix owns
container mount, cache-backend and runtime-ticket conformance; Loom tests its
requested mounts, configuration serialization and handling of
ticket/reconciliation responses without launching Wrix containers. The private
fixture runner uses its own native pool with small fixture estimates; production
limits and existing operator pools are unchanged.
