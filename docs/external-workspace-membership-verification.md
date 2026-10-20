# External membership implementation and verification

Baseline: Loom `2c6e9869`, native x86_64 Linux. This is a development report,
not admitted Gate evidence or publication authority.

## Existing support and changes

The baseline has invocation-local operator/integration observations,
layout-based resolution of the controlling `.loom/state.db`, private
execution-clone checks, dirty-work recovery and native cleanup admission. It has
no persistent native workspace key, reciprocal binding registry or relocation
transaction.

The change adds explicit host-local adapter selection, bounded status-only
membership admission, and ordering in public initialization and Git preparation.
It does not add an identity/history authority, external cleanup callback,
toolkit dependency, Wrix context delivery, host-verification bridge or evidence
importer.

An intact operator tree can move with its nested integration and native state.
Selected integration also repairs a reused bead's stale absolute origin after
admission. Moving integration outside the native layout and linked/shared-Git
execution checkouts remain unsupported. No native continuity claim is made for
copies or replacements between invocations.

Self-review found and addressed selection persistence before CLI rebuild's
tracker query, retention of native observations across clone preparation,
standalone absence handling, and the repository panic scanner's requirement for
explicit test guards in a separate test source file. No independent code review
or external-provider acceptance is claimed.

## Verification

Native Cargo's pinned patches were initialized with
`bash scripts/configure-cargo-patches.sh`. Resource-capped retries use the same
pinned tools and coverage; only scheduling changes.

| Check                                                                           | Result                                                                      |
| ------------------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| `nix fmt`                                                                       | Passed                                                                      |
| `cargo build` (`CARGO_BUILD_JOBS=2` retry)                                      | Passed                                                                      |
| `cargo clippy --workspace -- -D warnings`                                       | Passed                                                                      |
| `cargo clippy --workspace --all-targets -- -D warnings`                         | Passed separately                                                           |
| `cargo nextest run --test-threads 8 --no-fail-fast` with two build jobs         | 3,469 passed; six existing skips, including three default-filter exclusions |
| Focused CLI, driver, process and fixture tests after final explicit test guards | 23 passed                                                                   |
| `nix build --cores 2 --max-jobs 2`                                              | Passed, including final production/test source                              |
| `nix flake check --cores 2 --max-jobs 2`                                        | Passed on x86_64 Linux                                                      |
| `loom-walk no_panics_in_production`                                             | Passed after explicit test-guard correction                                 |
| Normal commit hooks and signing                                                 | Passed; SSH signature verified; no bypasses                                 |

The full native suite passed twice. The final test-only guard annotations do not
change production behavior; the focused tests and both Clippy configurations
were rerun after that correction.

Public CLI tests cover selection before/after native initialization,
confirmation without operator enrollment, failed initialization/rebuild
persistence, integration rejection before configuration/state changes, moved
native state, explicit disabling and the worker guard. Driver tests use real
disposable Git repositories and native SQLite state for fresh/reused
preparation, dirty-work stashing, conflicting membership, moves and stale
capabilities, replacement, linked-worktree refusal, tune preparation and native
cleanup. Process components exercise closed stdin, both output limits and actual
process-group termination with idempotent retry after a simulated committed
external effect.

The fixture adapter has a protocol self-test, but its checkout-local test
registrations are not a real host identity registry. These results do **not**
establish Weld interoperability, external-store concurrency/continuity security,
Darwin/aarch64 execution, or a complete Wrix worker launch and workflow cleanup.
Those claims require separate acceptance with the real consumer and runtime.

## Negative controls and retained evidence

A deliberate mutation bypassing target admission for bead paths caused
`external_membership_rejects_fresh_clone_before_identity_hooks_or_mount_setup`
to fail as expected. The mutation was removed, and the focused suite passed.
Other negative cases check unchanged configuration, dirty bytes and membership,
not merely a nonzero return.

The first uncapped broad build reported a linker killed by signal 9; the first
Nix build exited 137. Their owned process groups were stopped before capped
retries. An intermediate CLI ordering assertion assumed a fixed number of
operator confirmations; it was corrected to assert first operator confirmation
and eventual integration admission. An initial flake check rejected unguarded
panic syntax in a test-only source file; explicit test guards corrected that
without weakening the scanner.

Local logs are retained under `/tmp/lm-gyv2-*.log`, including failed attempts,
the mutation control, full native runs and final checks. Failed fixture trees
were deliberately retained at `/tmp/.tmp3VtXJE` and `/tmp/.tmps07Vxc` with their
Git/configuration evidence. Successful new fixtures clean their temporary roots;
CLI locks use disposable state homes. No broad lock/worktree pruning was run.

snapshot updated because: `loom init` now exposes explicit adapter selection and
disabling flags.

No source branch was pushed, and no driver or adapter was installed, published
or activated. The bead checkout is retained for review.
