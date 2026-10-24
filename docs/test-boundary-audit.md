# Test ownership and execution audit

This records historical implementation checkpoints and the continuation of
`lm-510x`, not final acceptance. The cleanup and native-provider implementation
landed together; automatic hook restoration remains outstanding. A passing
source walk is not runtime/backend conformance.

## Responsibility inventory

| Surface                                                                         | Disposition                                      | Retained Loom coverage                                                                                                                                   |
| ------------------------------------------------------------------------------- | ------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Real Wrix/Podman Cargo projection campaign and its scripts                      | Removed, not deferred                            | Actual namespace admission, exact mount/env serialization and Wrix wire roundtrip; real native Cargo canonical-output isolation                          |
| Wrix private-Nix, entrypoint, transport, container-health and sandbox campaigns | Removed from scripts, apps, checks and fixtures  | Loom backend protocol/parsing, wrapper composition, capability/result handling and source checks                                                         |
| Cargo/sccache hit, eviction, corruption and cross-checkout cache experiments    | Removed, including replay packages               | Configured writable mount and wrapper/env alignment; trusted Cargo rejects worker cache state; historical research is explicitly non-current             |
| Podman canonical-filesystem mutation campaign                                   | Removed with its example, probe and replay route | Admission rejects all five canonical projections; operator/integration association, config/wire handling and current worker feedback remain native tests |
| Signed profile-factory bootstrap                                                | Retained with small descriptors, not full images | Authenticated source selection, rejected authority, source immutability and provisioning/error policy                                                    |
| Native Cargo retention, leases, recovery and namespace fences                   | Retained                                         | Normal native units remain automatic; delegated-cgroup acceptance remains an explicit Loom-owned host obligation                                         |
| Native Beads adapter and workflow admission                                     | Retained                                         | Real API observations, graph/metadata handling and lost-acknowledgement recovery; fixture-owned isolated Dolt servers are reaped on normal teardown      |
| One mock-agent assembly smoke                                                   | Retained separately                              | Loom's assembled driver/agent handoff, not a second upstream runtime suite; Darwin unavailability exits 69 rather than passing                           |
| Nextest parser/capture patch controls                                           | Retained                                         | The pinned consumer boundary and local patch require their focused regression tests                                                                      |

The shared native tracker fixture no longer reopens an embedded engine for every
command. It clears ambient tracker/Git configuration, uses a private socket and
database, and emits server diagnostics on startup failure. It does not fake
Beads results.

Pure test-support consumers no longer pull the runtime through the acceptance
helper: that helper has an explicit feature. The tune fixture snapshots the
reviewed working-tree delta rather than validating only an older commit.
Template validation still compiles the actual candidate and regenerates real
snapshots. Its three conformance walks reuse the registered Rust rules instead
of rebuilding a separate checker inside the candidate. The shared walk API
accepts its caller's input authority; it does not create live-filesystem input
inside checked analysis.

## Execution composition

- The full native app runs format, production Nix build, fast flake checks,
  native build, both separate Clippy configurations and the native suite. It
  does not recursively invoke whole-tree Gate system verification afterward.
- The required app does not repeat preceding format/Nix/Clippy work. Its native
  run explicitly bypasses default filters. The repository default profile now
  selects all tests; none of the three former exact exclusions remain.
- The reconciled fast source check directly dispatches the registered analyses
  selected from shipped annotations, without recursive Gate or Cargo execution.
  Checked-source/admission tests still reject raw I/O, omitted inputs, stale
  descriptions and undeclared definitions.
- Full and required apps each retain the native-unit acceptance app once. Its
  real isolation, reporting and compiler-catalogue checks are not replaced by
  ordinary unit success or an unavailable result.
- Phase-context and task-dispatch Nix checks share one union execution rather
  than rerunning the dispatch group. The public diagnostic selectors remain.
- Automatic `checks.quint-model` executes all 113 witnesses for both declared
  seeds and 64 complete 80-step traces per seed. The pinned upstream test API
  batches witness setup; each test still receives fresh state and RNG semantics.
  Seven semantic mutants must pass their unmodified witness and fail its actual
  expect assertion. Parser, missing-work, timeout, trace, exhaustive-budget and
  replay controls remain executable.
- The model check's source bundle excludes unrelated Rust crates and agent/cache
  experiments. This is input scoping, not a saved-verdict shortcut.
- Private fixture commands clear ambient compiler wrappers and cache settings so
  a shared daemon cannot capture a temporary directory that the fixture removes.
  Conformance tests inject conflicting sentinels and verify this isolation.

## Checkpoint measurements

These are individual observations, not cross-machine guarantees or a comparison
between different feature trees. Compilation/provisioning is separate from test
execution unless explicitly included.

| Work                                                                 | Observation                                                                                                                                |
| -------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| Focused Cargo projection boundary                                    | 1 passed, 0.07 seconds; separate cold crate compilation was 134 seconds                                                                    |
| Template candidate validation                                        | 23.63 seconds isolated, 23.07 seconds in the later full run; historical isolated baseline 186 seconds                                      |
| Native source-readmission                                            | 59.41 seconds isolated, 50.18 seconds in the later full run; historical isolated baseline 113 seconds                                      |
| Separately audited decision inventory                                | 65.30 seconds isolated, 51.08 seconds in the later full run; no longer default-excluded                                                    |
| Batched 113-witness execution                                        | 0.95 seconds for one seed                                                                                                                  |
| Restored model/source-closure/unavailable/private-fixture Nix checks | 57.36 seconds together, including the changed native tool build; historical model-harness baseline alone was 129 seconds                   |
| Default native suite after repairs                                   | 3,559 passed in 193.01 seconds across 107 binaries; five existing ignored tests, no default exclusions; preceding compilation 0.74 seconds |

The earlier diagnostic run had 22 failures. Their repaired hook/source fixtures
and checked-admission surfaces passed focused reruns before the fresh green
whole-native run. That diagnostic run was never treated as a passing result.
Native tracking conformance remains a genuine long-tail cost (97.68 seconds in
that full run); it is neither filtered out nor replaced by a cache-hit
assertion.

Three ignored tests are re-executed process entrypoints. The other two require
actual Linux delegated cgroup2 and pinned native helpers: native lifetime
sealing/stale-entry cancellation, and restart/process-generation reconciliation.
Their explicit verification scripts remain; the ordinary suite does not claim
that managed-host acceptance passed.

The Evidence-owned shared-history admission scenario remains pending. Removing a
container mount probe does not implement that separate safety-fact contract.

## Native-provider reconciliation

The cleanup is now combined with the preserved `lm-fb9j.12` draft. Original
source, index and recovery objects remain protected; no delivery commit or push
was made. Closed native constructors and independent source/consumer analysis
remain. The removed Wrix checks are not retained as fixture-specific discovery
obligations. The shipped graph test reuses the inventory's freshly constructed
immutable native graphs instead of reconstructing them for each assertion.

Marker CLI/wrapper conformance now belongs to the CLI integration suite and uses
Cargo's actual Loom binary. It no longer recursively builds the CLI from library
tests. Joint mint/wrapper, malformed-receipt, dirty/tree rejection and exact
hook/entry/range assertions remain. Two former 160–161-second cases took
0.48–0.75 seconds in the focused rerun. The complete shipped native graph and
responsibility test passed in 96.91 seconds. These host measurements were not
isolated from other build activity and are not hook-latency guarantees.

The first reconciled default-suite diagnostic ran 3,692 tests: 3,683 passed,
nine failed, five existing tests were ignored. It is not a passing aggregate.
Repairs include fresh immutable compiler fixtures after the SDK runner changed,
complete captured evaluator inputs, native suite composition and bounded
owned-server readiness. Focused reruns do not replace a fresh whole-suite run.

At the profile-alignment checkpoint, native build, production Clippy, strict
no-IFD check evaluation for all three systems, Nix build and all 33 host flake
checks passed. Native Nix checks now select the same Cargo profile as their
compiler seed, rather than silently compiling a second development-profile tree.
Foreign-system checks were evaluated, not built.

The two retained Cargo-retention bindings now use a checked system report with a
closed delegated-native profile. Compiler archives are prepared in normal Nix
sandboxes; the existing trusted driver runs the actual retention cases in a
bounded user-service sandbox. Home, runtime sockets, network and PID visibility
are isolated; only an owned output and the verifier's own real kernel cgroup are
writable. Private owned ancestors surround that delegation without relaxing
native ownership guards. Source-selected environments never affect the host
launcher, and executable identities and fresh reports remain checked.

The actual routed scenario passed all 36 ordinary retention cases and both real
managed lifetime/restart cases. Each required ignored case is separately
selected so removing one cannot turn the other's success into complete
acceptance. This is not an inference from ordinary unit success or a host-script
wrapper. Gate hook restoration and its separate latency obligations remain under
the consolidated cleanup bead.

## Automatic-hook continuation

The rebased audit shares successful contract-membership discovery only between
clones of the same immutable snapshot. A changed checkout still fails current
context checks; a new snapshot rediscovers additions and malformed packages.
Admission resolves each target/contract pair once while retaining every owning
criterion. Platform checks share validation within one pure pass and use the
existing kernel-protected running-image identity, not a mutable-file timestamp
cache. The CLI no longer repeats the iterator's whole-plan check immediately
before that iterator performs the same check; diagnostic dispatch retains its
sibling checks. The ordinary-commit readiness fixture now uses Cargo's immutable
compiled hook-runner capability rather than assuming that capability is also
exported in the test process environment.

Normal plan output reports actual execution groups, aliases, lane/owner counts,
affected-input reasons and excluded-unit counts without repeating every complete
input closure per alias. The full explanation API remains available; explicit
`RUST_LOG=loom::gate::plan=debug` diagnostics include its complete definitions.
Neither representation changes selection, freshness, dispatch or verdicts.

Measurements below are diagnostics on a shared x86_64 Linux development host,
with concurrent compilation, not isolated latency guarantees:

| Work                                                | Result                                                                                                                                                        |
| --------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Initial debug files diagnostic                      | Interrupted after 317.568 seconds, no completed result                                                                                                        |
| Intermediate debug files diagnostic                 | Interrupted after 329.437 seconds, no completed result                                                                                                        |
| Actual prek files hook before compact reporting     | Interrupted after 505.928 seconds; Gate was blocked writing the multi-gigabyte plan line to prek, with about 3.4 GiB resident memory                          |
| Separate cold Nix CLI build after compact reporting | Passed in 202.745 seconds; provisioning/compilation, not hook execution                                                                                       |
| Actual prek files hook after compact reporting      | Failed after 490.116 seconds; 1,256 admitted units, 1,130 selected units, 1,028 shared aliases, 102 fresh execution groups; plan line reduced to 32,343 bytes |

The last run reached real execution instead of blocking on plan output. It did
not pass: Nix advertises `aarch64-linux` in `extra-platforms`, but an actual ARM
builder fails to execute its present Bash binary. The registered interpreter is
`/run/binfmt/aarch64-linux` with flag `P`, without `F`. Host Nix configuration
includes that path, but the closed native runner deliberately clears both
sandbox-path settings and remote builders. The interpreter therefore cannot be
resolved inside this stricter sandbox; ordinary host Nix configuration alone
does not establish its capability. The subsequent ARM Rust standard-library
input is also cold under the offline verification boundary. These are not
successful foreign-platform checks.

No binfmt registration, kernel privilege, sandbox exception, platform exclusion,
VM fallback or automatic hook stage was changed to conceal those failures. The
remaining host capability/configuration repair, completed warm files/diff and
actual commit/push measurements, and automatic-stage/fixture restoration still
block closure. A full native diagnostic also caught the compact report omitting
lane labels; those labels are restored rather than weakening the scope-policy
assertion. Focused passes and interrupted/failed full runs do not establish
complete acceptance.
