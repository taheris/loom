# Quint acceptance harness

`nix run .#test-quint -- provisioning-and-batching` provisions Quint 0.32.0, its
Rust evaluator 0.6.0, Apalache 0.56.1 (including its solver), and Java from the
existing `flake.lock`. Cargo pins upstream Quint Connect and its macros to 0.1.2
through `Cargo.lock`. No tool is installed during a campaign. The Nix package's
immutable `QUINT_HOME` contains the evaluator and solver distribution; missing
packaged resources fail before generation.

This app is acceptance infrastructure, not another operational verification or
publication route. It certifies only the small production-connected bridge:
named actions and nondeterministic arguments drive `runner::check_zero_match`,
and upstream `State` comparison checks every observation. Whole-gate scenarios
remain explicitly unimplemented, not successful empty runs. Extend the same
`scripts/test-quint.sh` dispatcher for subsequent scenarios, including
`workspace-safety-history`; do not add another app or hook.

## Campaigns and budgets

`tests/fixtures/quint/campaign.json` is the versioned input. Ordinary callers
need no settings. Generation/discovery runs once per seed, not per trace or
criterion. Connect owns trace decoding, action dispatch support, and comparison;
the wrapper only counts declared work and enforces limits.

| Campaign | Seeds                    | Traces per seed | Transitions per trace | Deadline per seed |
| -------- | ------------------------ | --------------- | --------------------- | ----------------- |
| Routine  | 42, 20250308             | 8               | 20                    | 30 seconds        |
| Deeper   | Routine + 314159, 271828 | 32              | 80                    | 120 seconds       |
| Replay   | 42                       | 1               | 1                     | 30 seconds        |

Routine processes 16 traces / 336 observations; deeper processes 128 traces /
10,368 observations. Both also execute the checked-in zero-match replay through
`replayStep`, whose nondeterministic domain is a singleton. It is portable Quint
source, not a machine-local trace or safety-history resolution token. `deeper`
explicitly requests the larger campaign. A trace includes its initial state.
Exact completion, including reaching the declared count, is bounded success.
Short/empty/extra batches, generation errors, unknown actions, missing
arguments, projection differences, deadlines, and interrupted guards are
failures. Nothing shrinks the campaign or changes applicability in response to
latency.

Initial profiling on Linux x86_64 with preprovisioned tools, a debug-built
adapter, no result reuse, and verbose upstream traces captured to a temporary
file measured 2.84 seconds for routine plus replay and 7.28 seconds for deeper
plus replay. The 30/120-second seed guards leave startup/scheduling headroom;
they are deliberately not targets or adoption thresholds. These measurements
cover bridge execution only, not full hook costs, cold builds, or either
consumer's eventual pilot adoption. Reprofile the composed model with its
required witnesses and mutations before changing its finite definitions; do not
reduce coverage to fit this bridge's latency.

The process-group timeout bounds even a hanging generator/adapter; per-seed
in-process guards and final count checks prevent a partially explored campaign
from emitting a receipt. Success prints bounded receipts identifying tools,
property, model/input digest, seeds, bounds, and counts. Failure prints upstream
verbose diagnostics, including the seed, trace, and state difference. Input
identity hashes logical paths and contents, not workspace paths; seed changes
invalidate identity. No successful evidence is imported into Gate by this app.

## Upstream compatibility boundary

Connect 0.1.2 exposes `Driver`, `State`, `Step`, and `runner::run_test` with
`RunConfig`. Its ITF file replay and `Step` constructors are private. Also,
`TestConfig` does not pass `--mbt`: with Quint 0.32.0 its test traces omit
`mbt::actionTaken` and `mbt::nondetPicks`, so Connect rejects them. This
unsupported integration is not replaced by a bespoke ITF decoder. Checked-in
named replay actions use the compatible `RunConfig` route instead. `bridge.qnt`
retains a Quint test witness for inspecting this upstream limitation, but it is
not counted as implementation conformance.

`nix build .#checks.x86_64-linux.quint-harness` tests provisioning and batching
against the real tools and exercises invalid models, divergence, unsupported
actions, short/empty generation, missing tools, empty receipts, and timeouts.
Native `cargo test -p loom-gate --test quint` covers exact-count/early-guard
classification, invalid campaigns, and identity invalidation without needing
Quint on PATH. Shell fixtures mock external processes only; production gate
logic and upstream comparison remain real.
