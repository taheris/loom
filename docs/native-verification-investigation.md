# Native verification investigation

Status: coordinated `lm-fb9j.12`, `lm-fb9j.14`, and `lm-fb9j.15` remain in
progress. This report is historical investigation evidence, not a verification
receipt or completion claim. The draft is rebased onto integration `e4c9c686`.
At the user's request, VM definitions, exported VM packages and automatic VM
routing have been removed. Suites execute Nextest directly, including tests
excluded by its default scheduling filter. Native isolation runs directly and
returns its actual failure/unavailable status; it cannot boot a fallback VM.
Historical logs, failed fixtures and machine state are preserved. No source push
or coordinated landing has occurred.

## Findings

### The worker restriction is a proc mount, not absent user namespaces

Private user, mount, network, IPC, UTS, and PID namespaces can be created in the
worker. The actual Nix builder nevertheless fails when mounting its private
`/proc`, with `EPERM`. A second probe using `NOSUID|NODEV|NOEXEC` also fails.
Consequently, a successful `unshare` probe does not establish Nix availability.
The unchanged native execution correctly reports unavailable rather than pass.

The trace and failed fixtures are retained under
`.loom/logs/coordinated-resume/private-userns-1/`. No sandbox disablement,
security-policy exception, device fabrication, or privilege expansion was used.

### A private software kernel provides a working execution environment

The software-virtualized verifier uses QEMU TCG without requiring KVM. Its Nix
configuration enforces sandboxing, prohibits sandbox fallback, and has no
substituters. The guest has its own writable store state; the host store export
is read-only. The virtiofs server drops unsupported capabilities rather than
acquiring host capabilities. Short private temporary paths avoid the UNIX socket
length limit.

`native-verifier-vm-3/status` records both driver build and actual VM execution
as zero. Its actual `native-isolation` output contains passing `cold` and
`warm-rebuild` observations, each with both expected outputs. The controls keep
an undeclared file and live listener available outside the unit's Nix sandbox.
This establishes that the original source-worker mount failure is recoverable
through software isolation; it does not establish full coordinated acceptance.

An earlier required-suite draft routed the fixed native verifier to this VM
after direct execution reported capability-unavailable. That routing and the
later whole-workspace VM runner have been removed at the user's request. The
following evidence describes historical runs, not the current execution path. VM
state and reports are retained. The Rust revisions restricted guest networking
and exercised ordinary CLI binding. Revisions 4 and 5 failed while constructing
the Rust fixture; revision 5 exposed Crane's required `cargoArtifacts` argument.
The corrected fixture explicitly supplies it and constructs Build,
production/all-target Lint, and Test producers. Revision 6 failed on a cold
Bison source derivation, despite the compiled dependency output being available.
The realization-frontier repair now checks only dependencies needed to build the
requested outputs: an available dependency output stops traversal, but a warm
requested check never bypasses its own dependency checks. Four regression
controls pass. Revision 7 reached actual Cargo/Nextest preparation but failed on
a required cold bzip2 source derivation. Revision 9 still failed after
provisioning bzip2. Comparing the actual catalogue realization graph with the VM
closure exposed the missing `zstd.dev` and `stdenvNoCC` outputs: their absence
forced traversal into historical bootstrap sources. Adding those exact
prerequisites leaves zero cold fixed-output inputs in the frontier diagnostic;
no guard changed. Revision 8 was cancelled as obsolete. Revision 10 passed
native isolation but was cancelled during initial fixture locking, with its
state and cancellation status retained. Revision 11 uses a fixture lock whose
hashes are computed from the declared immutable inputs during construction. An
actual offline no-update lock check preserves its bytes and completes in 0.025
seconds on the worker. The guest has progressed past locking into inventory
discovery. None establishes a Rust/profile verdict.

An earlier queued follow-up never ran: its running shell driver was edited in
place and failed with a syntax error. The failed driver and logs are retained.
Subsequent drivers are separate files, syntax-checked before launch. The latest
fixture also declares its actual evaluator inputs explicitly. Compiler and
Nextest remain immutable runtime inputs with their original store contexts;
unused duplicate nonflake inputs have been removed rather than treating large
runtime artifacts as project source inputs.

### Compiler facts and test verdicts are different boundaries

The native Rust catalogue companion runs actual Cargo metadata and unfiltered
Nextest collection. It retains compiler artifacts for the subsequent complete
native test execution. A prepared catalogue has no caller-listing constructor or
deserializer and does not discharge a test obligation.

Named Rust binding requires an actual nonignored, unambiguous compiled test
runnable in the producer's actual mode, the same snapshot and producer, and the
producer's actual Rust responsibility. Unknown test kinds and out-of-mode cases
cannot authorize named execution. Aliases execute the complete native suite,
with warm outputs rebuilt; they do not execute caller command strings. The fixed
test operation ignores the project default filter so that a named case cannot be
excluded while another case produces a passing suite result.

This alias implementation is not the required per-case result bridge. A failing
case currently gives every suite alias the aggregate failure; the checked CLI
also does not retain the legacy per-case skip and structured-result fields.
There is no retained report output from a failed native test derivation. An
independent read-only review confirms that the remaining CLI failures are not
only stale fixtures. Completing them requires a constructor-owned supervised
report contract, fresh execution-report sharing with individual result
projections, and the existing interpretation/persistence policy. Independent
aggregate baselines must remain separate from case projections. Restoring raw
runners or changing positive tests into unavailable outcomes would not repair
these gaps. The review is retained in `readonly-integration-review/report.txt`;
it is analysis, not acceptance evidence.

The added isolated probe exercises missing and ambiguous identities, ignored
cases, changed snapshots, cold/warm CLI execution, a genuine assertion failure,
and correction recovery. It now also checks actual Nextest group/default
membership, the broad unmatched suite, whole-plan CLI execution, and rejection
of unknown or empty property groups before unrelated-file selection. The
catalogue compiles these contexts with the same fixed native Nextest preparation
rather than inventing a global group or an all-tests default. Profile bytes
participate in catalogue freshness; classification reports are serialize-only
data, not verdicts. Its source and evaluation are present; this report does not
substitute for its live results.

### Configuration responsibility follows real consumers

Project context owns and parses its known project configuration. Native fixture
and generated data retain their declared compiler consumers instead of becoming
project policy documents. Unknown configuration without a reader is rejected.
Rust Build, Lint, and Test claims require their corresponding fixed Cargo
operation. A configuration file is not assigned universal compiler duties merely
because a Cargo manifest exists.

Strengthened shipped-graph checks exposed missing native UI resources and the
old universal-configuration assumption. Both UI directories are now declared.
The complete captured shipped-graph/Rust-duty test and the retained upstream
private-policy boundary test pass in `native-catalogue-1/core-3.stderr.log`. The
finite-scope CLI control now uses a genuinely uncovered Rust Build duty, not an
invented configuration Evaluate duty; all 17 checked-loading tests pass in
`native-catalogue-1/cli-2.stderr.log`. The subsequent library/inventory/provider
matrix passes all 512 tests with zero skips in
`native-catalogue-1/profile-core-3.stderr.log`. It includes pending-owner
retention and rejection of pending-only independent coverage. The subsequent
19-case checked-CLI matrix passes in
`native-catalogue-1/profile-cli-5.stderr.log`, with 15 unrelated filter
exclusions. It proves real failure/correction for pending mandatory policy and
that a nested parent skip does not emit pass evidence; the same callback fails
without the parent guard. Both separate workspace Clippy commands pass in
`native-catalogue-1/profile-workspace-clippy.stderr.log` and
`profile-workspace-clippy-all.stderr.log`. These do not establish complete
workspace acceptance.

The current library/inventory/provider matrix passes 517 tests with zero skips
in `native-catalogue-1/lane-core.stderr.log`. A new regression checks that Check
precedes Test regardless of lexical target names or caller lane order. Shared
executions remain attached to their earliest eligible lane. The isolated Rust
probe also checks this ordering and now contains an actual post-integration
diff/hook/Check/Test phase; its live result is still outstanding. Both shipped
native graph and private-policy boundary controls pass independently in
`native-catalogue-1/current-native-policy-2.stderr.log`. Twelve real-prek and
scope-policy controls pass in `native-catalogue-1/prek-migration-5.stderr.log`.
These exercise actual hooks, grouped/direct infrastructure statuses, signals,
formatter recovery, nested configuration ownership, and semantic-failure
precedence. A foreign-platform control retains its system owner without
converting inapplicability into verification. Hook stdout cannot override native
process status. Completed semantic exit 2 is retained by the new aggregator; an
independent semantic failure still dominates infrastructure. Both workspace
Clippy configurations pass separately in
`native-catalogue-1/lane-clippy.stderr.log` and
`native-catalogue-1/lane-clippy-all.stderr.log`.

Review remains source inspection rather than a second deterministic gate. The
new eager native-admission call has been removed from this read-only lane;
verify and mint retain their mandatory admission. The real renamed-built-in
source publication/review control passes in
`native-catalogue-1/readonly-review.stderr.log`, without changing the fixture or
weakening deterministic admission.

### Pure evaluation and runtime patching are separate

The `lm-qgli` repair loads immutable Wrix source modules without importing a
patch derivation's output. Runtime helpers use the corresponding patched store
inputs. Import-from-derivation has not been enabled. Passing graph construction
is evidence for pure evaluation, not evidence that every private-Nix runtime
compatibility check has executed; `lm-qgli` remains open for that distinction.

Strict evaluation exposed further source-stage reads in native, Quint, parser,
and smoke helper derivations. These now receive the same vendor directory built
from the original pinned Cargo inputs rather than asking Crane to read a
staged-source derivation during evaluation. The unpatched-vendor parser control
still uses unpatched Crane with the actual repository lock and manifest. The
Wrix module adapter retains Fenix's own file/directory, profile, component, and
cross-target logic. Only its fixed-hash channel-manifest fetch uses Nix's
evaluation fetch primitive; compiler components remain ordinary derivations.
Service/Weld APIs and versions are unchanged. Cargo manifest locators avoid
implicitly recopying the entire source tree before reading those source files.

`current-nix-graph/checks-no-ifd-14.json` resolves all 48 current-system checks
with IFD disabled, and `stderr-14.log` records successful full graph-only flake
checking under that restriction. Actual builds of `worker-private-nix-api`,
`known-hosts-preserves-store`, and `profile-manifest-keeps-runtime-path-context`
pass in `runtime-policy-14.stderr.log`. These focused policy controls and graph
evaluation do not establish private-Nix scenario execution or whole-tree
acceptance. The default Nix build also passes with IFD disabled in
`current-nix-build/stderr-14.log`; the remaining full gates have not passed.

### Report/result bridge continuation

The draft now retains constructor-supervised native reports and projects actual
individual case verdicts through the ordinary CLI. Current-run sharing retains
the independent aggregate baseline. Report parsing uses the existing
contradiction, JUnit, structured-status and deny-by-default skip rules. Separate
assertion derivations preserve ordinary Nix failure status even when report
outputs must survive a failed test producer. These changes are implemented, not
yet isolated end-to-end acceptance.

`report-retry-gate-tests.log` records 677 passing loom-gate tests with no skips;
`report-retry-clippy-2.log` and `report-retry-clippy-all.log` record both
separate workspace Clippy configurations passing. `report-retry-graph-2.log`
records strict graph-only checking with IFD disabled. Cold read-only evaluation
also exposed repeated source filtering and Nextest-configuration reads through
unmaterialized filtered paths. Source composition and configuration readers now
retain the original path and apply the same source filters before reading.

VM11 reached a genuine six-case catalogue and a passing cold exact-target CLI
execution, then timed out during the warm phase. Its status is failure, not
runtime acceptance. The native Rust probe now scopes its initial compiler facts
so they are released before independent CLI processes, retains its stale-facts
negative control, and checks mixed case results against a failing independent
baseline. The VM phase assertions also include the existing post-integration
phase rather than omitting it.

VM12 built its immutable driver with IFD disabled, passed native isolation and
prepared the genuine six-case catalogue, but timed out during the cold CLI
phase. `native-verifier-vm-12/status` records runtime failure and an unchanged
source tree. Its source, index, unstaged patch, logs and machine state are
retained. Scoping the initial catalogue was insufficient to meet the runtime
budget; another timeout is not positive report/projection evidence.

## Evidence limits

Paths above are relative to `.loom/logs/coordinated-resume/` unless a full
relative path is shown. Failed attempts, source patches, fixture state, recovery
refs, drafts, and the separately delivered fixture work are preserved.

The latest complete diagnostic, `report-bridge-full-diagnostic`, ran 3,481
tests: 3,442 passed and 39 failed, with three existing tests skipped. Cargo
build passed; source and staged-index comparisons both remained unchanged. The
failures still preclude acceptance. `report-retry-final-gate-tests.log` records
679 loom-gate tests passing without skips, and both separate final workspace
Clippy configurations pass. These focused results do not replace the failing
complete suite. The preceding diagnostic ran 3,462 tests with 52 failures and
also correctly detected source edits. Its private-policy control subsequently
passed on stable source. Focused subsequent results cannot be summed into a new
full suite verdict. Actual profile/property binding is implemented but its
isolated end-to-end validation is unfinished. The implemented checked
report/result projection still needs isolated end-to-end validation. Remaining
CLI skip-policy integration, equivalent fixture migrations, runtime
compatibility, and frozen whole-tree gates remain within their owning open
beads. No unavailable result, timeout, skip, cached verdict, or manual-host-test
promise establishes delivery.
