# Pre-Commit Discipline

Hook composition policy for this project's `.pre-commit-config.yaml`:
which checks fire at commit time vs push time, what each guarantees,
how the agent self-verifies inside its bead container, and how the
content-addressed `MarkerProof` short-circuits redundant pre-push work
on driver-loop integration pushes. Generic hook plumbing (lock, shim
shape, devshell wiring, install lifecycle, `push-verified`, and
`skip-if-missing`) is owned upstream by `wrix.prekHooks`; Loom owns the
repo-local marker-aware policy wrapper at `bin/pre-push-checks`.

## Problem Statement

A project's commit hook is the only check authors run reliably, so the
composition has to satisfy two opposing constraints. Commits stay cheap
(~1s) so authors keep using `git commit` instead of batching; the
integration cost (workspace-scope clippy + tests + review) runs at push
time where occasional latency is acceptable. Two further pressures
shape the architecture:

- **Treefmt drift caught far too late.** Today's contract lets the
  agent emit `LOOM_COMPLETE` without running any local checks; treefmt
  or integrity-gate drift surfaces only after the driver-side verdict
  gate runs, forcing a full recovery iteration to apply formatting the
  agent could have fixed in <1s in-container.
- **5+ min pre-push floor under `nix flake check`.** Bundling the
  workspace Rust compile into `nix flake check` puts a 5+ min cold-cache
  floor on every push — operator-manual and driver-loop alike — which
  pushes operators toward `--no-verify` bypass.

This spec resolves both: the agent's bead container inherits the prek
hook chain so commits self-verify in-session; `nix flake check` is
restructured to a sub-10s fast derivation; workspace cargo and system
coverage run as explicit pre-push hooks outside the flake-check budget;
and a content-addressed `MarkerProof` minted by the driver-side verdict
gate lets prek's pre-push short-circuit redundant covered hooks on
driver-loop integration pushes. Operator-manual pushes pay the full
hook chain once per push against the host cache — a first-class,
supported path, no `--no-verify` required.

## Architecture

### Stage composition

The pre-commit stage provides fast feedback on the staged files: native
whitespace/conflict sanitation, formatting drift, explicit-interpreter safety
for shell self re-exec, and affected deterministic annotation checks. The
pre-push stage composes the fast Nix tier, Rust linting when Rust files changed,
exact pushed-range verification, and the full required suite. Nix-dependent
entries no-op only when Nix is absent, while non-Nix checks continue.

Every pre-push entry is independently marker-aware. There is no standalone
marker gate: the wrapper either proves coverage for that entry or executes it.
The exact hook ids, argv, file selectors, and ordering live only in
`.pre-commit-config.yaml`; the configuration walk verifies that they realize
this policy as one coherent stage graph.

Native prek sanitation avoids the Python bootstrap path, which is not portable
to bare NixOS. The shell-reexec check rejects a shell script that re-executes
itself through its shebang resolution rather than naming the interpreter; this
keeps script fixtures runnable in Nix sandboxes where `/usr/bin/env` is absent.

### Pre-push hook budget and marker coverage

The pre-push chain has independently wrapped hooks. A hook may be
short-circuited only when the marker proves that exact hook entry was
covered for the same tree, config, and push range; otherwise the wrapper
falls through and executes the hook. Coverage, not a fast/slow label,
controls the shortcut.

`nix flake check` is budgeted as the first pre-push hook. Its test-tier
composition relative to the full-suite app is owned by [tests.md](tests.md).
On pre-push it is routed through repo-local `bin/pre-push-checks`, so a driver-minted
marker may skip it only if the marker's `GateSuccess` proves the hook
ran and passed. Its derivation chain contains checks that don't require
compiling the workspace under test: the treefmt-check derivation plus a
`loom gate check` derivation that runs `[check]`-tier verifiers + the
integrity gate + the surface-conformance audit. The loom binary itself
is precompiled (via crane's `cargoArtifacts` chain, reused), so the
derivation's wall-clock budget is the checks themselves, not the build.
Target: <10s warm when it runs.

The full required suite and container smoke composition are owned by
[tests.md — Nix Integration](tests.md#nix-integration). This spec owns only
their hook placement: `nix run .#test` is a pre-push hook rather than a
CI-only surface because this repository has no separate CI safety net.

The targeted hooks are clippy + `loom gate verify --diff <push-range>`
+ `nix run .#test`. Prek exports the pushed endpoints as
`PRE_COMMIT_FROM_REF` / `PRE_COMMIT_TO_REF`; `pre-push-checks` appends
that exact range to the gate hook instead of deriving it from the branch
upstream. `loom gate verify --diff` uses the scope-derived contract in
[gate.md](gate.md): project pre-commit hooks for the range, then affected
`[check]` / `[test]` annotations; no `LOOM_VERIFY_TIERS` environment
override exists. Each hook runs as an independent prek entry
with its own `files:` regex where applicable so file-pattern selectivity
composes with marker-aware fallthrough. On the driver-loop integration
push the wrapper can short-circuit every covered hook in sub-second
time; on operator-manual pushes (no marker present in the operator's
clone) the wrapper falls through and the hooks run against the host's
warm cache.

### Agent self-verify in the bead container

Bead-workspace placement and canonical hook-path creation/repair are owned by
[harness.md § Bead Dispatch](harness.md#bead-dispatch). This spec consumes that
workspace guarantee: an agent's ordinary `git commit` traverses the same
configured pre-commit policy as a host commit. The shared `test-sandbox`
verifier consumes the
[tests-owned packaged-agent health policy](tests.md#assembled-system-checks);
the hook-chain checks do not drive an agent conversation. Workers do not push.
Final in-session feedback follows the contract in
[templates.md — Loop completion self-check and self-review](templates.md#loop-completion-self-check-and-self-review).

What this catches in-session:

- Treefmt drift (pre-commit hook auto-applies; commit blocked until
  the agent stages the formatter's diff).
- Integrity-gate findings on staged files (pre-commit hook fails;
  agent fixes annotations or stages the corrective change).
- `[check]`-tier verifier failures whose inputs intersect staged
  files (same pre-commit hook).
- Project pre-commit failures and affected `[check]` / `[test]`
  failures on the final loop self-check range defined by
  [templates.md — Loop completion self-check and self-review](templates.md#loop-completion-self-check-and-self-review),
  including hooks such as clippy when the project config places them in
  the pre-commit lane.
  Full workspace nextest and `[system]` verifiers are explicit
  pre-push/full-suite or operator-invoked responsibilities.

What it does *not* do: act as the trust source for the driver-side
verdict gate. The agent's hook chain is **feedback only**. The driver
runs its own independent post-integration verify and molecule push gate
(per [harness.md § Verdict Gate](harness.md#verdict-gate)) after
`LOOM_COMPLETE`; the agent cannot mint a `MarkerProof` and cannot
bypass driver verification by emitting a structured "I verified" report.

The bead container has no `nix`. Hooks whose entry runs `nix` are
wrapped as `entry: skip-if-missing nix -- <command>` in
`.pre-commit-config.yaml` (the wrapper is shipped from
`wrix.prekHooks` per *Plumbing ownership split* below); inside the bead
container the wrapper observes `nix` absent on `PATH` and exits 0 silently,
no-op-ing the hook. Outside the bead container (host devShell, pre-push, and full-suite
contexts) the same wrapper finds `nix` and execs normally. Non-`nix`
pre-commit hooks run uniformly across host and bead-container contexts.

### Marker integration

`MarkerProof` is the content-addressed trust-bearing artifact whose
lifecycle, validation fields, mint trigger, and diagnostic
`loom gate verify-marker` contract are canonical in
[gate.md § Marker](gate.md#marker). This spec only binds that
Gate-owned marker surface to the pre-push hook composition:

- The driver push gate runs the configured pre-push chain for the
  actual push range before Gate mints a marker.
- Each pre-push hook entry in `.pre-commit-config.yaml` invokes the
  repo-local `bin/pre-push-checks` wrapper with stable `--hook-id` and
  `--hook-entry` metadata. The wrapper asks Gate to validate the marker
  for the current hook and short-circuits only on covered success;
  marker absence, mismatch, dirty tree, config/range drift, missing
  evidence, or uncovered hooks fall through to the underlying command.
- `.pre-commit-config.yaml` does not register `loom gate verify-marker`
  as a standalone hook. That subcommand remains a diagnostic and wrapper
  dependency per Gate; marker absence is a fall-through condition for
  operator pushes, not a hook failure.

### Plumbing ownership split

`core.hooksPath`, the hook shim scripts, the flock that serializes
prek's stash/restore window across overlapping commits, the
`push-verified` SHA stamp (the pre-push shim writes it on overall
pre-push success; the user's git-push re-run consumes it instantly
to decouple from SSH latency), and the `skip-if-missing` wrapper
(PATH-conditional exec for hooks whose binary may be absent in some
contexts, notably `nix` inside the bead container) are packaged in the
`wrix.prekHooks` derivation and installed by `wrixLib.mkDevShell` when
this project's `nix develop` is entered. The marker-aware
`pre-push-checks` wrapper is repo-local at `bin/pre-push-checks`, and
pre-push hook entries invoke it by that path rather than relying on
ambient PATH. Loom owns that wrapper's hook-id, hook-entry, push-range,
marker-validation, and fall-through policy; wrix owns the reusable hook
shim, lock, install, stamp, and skip-if-missing plumbing around it. The same
packaged hooks are present in the worker image, while
[harness.md § Bead Dispatch](harness.md#bead-dispatch) exclusively owns where
Loom installs and repairs their `core.hooksPath`. Given that placement, an
agent commit consumes this repo-local policy uniformly with a host commit.
The downstream project maintains its marker wrapper and policy config, but not
hook shims, lock scripts, or installation logic.

`pre-push-checks` and `push-verified` are **complementary**, not
successor and predecessor: the SHA stamp short-circuits the entire
prek pre-push chain on the user's second `git push` attempt after a
successful first attempt (SSH-decoupling); `pre-push-checks` is a
per-hook short-circuit inside the chain itself, gated on the
content-addressed `MarkerProof` written by the driver-side verdict
gate (see *Marker integration* above). Both stay shipped; both stay
active.

`skip-if-missing` is the upstream mechanism for hooks whose
underlying binary is not guaranteed in every context — the bead
container has no `nix`, so any hook that runs `nix` is wrapped as
`entry: skip-if-missing nix -- <command>`. The wrapper exits 0
silently when the named binary isn't on `PATH` and execs the
command otherwise. Wrix does **not** maintain a hook-id skip
list, and does not stub `nix` on the container `PATH`; the absence
is observable at the hook's entry, tagged via the wrapper at the
point of use.

### Source-of-truth files

`.pre-commit-config.yaml` is the **single source of truth** for what
runs at pre-commit / pre-push. It has three mandatory consumers plus
any external CI that opts into the same hook chain:

1. The bead container's prek (pre-commit on agent commits, plus any
   manual pre-push the agent invokes, for feedback only).
2. The operator's host prek (pre-commit + pre-push on operator's
   commits/pushes).
3. The driver's push gate, which invokes `prek run --hook-stage
   pre-push --from-ref <base> --to-ref <head>` programmatically against
   the loom workspace's actual push range.
4. Optional external CI, when configured, which invokes the same
   prek-configured hooks in its sandbox.

This spec owns:

- `.pre-commit-config.yaml`
- `bin/pre-push-checks`
- `scripts/check-shell-reexec`

`loom gate review` is the driver-only addition — not a prek hook. It
runs at the verdict gate's molecule-completion audit; it does not run in
the bead container (self-review is structurally weak) and does not run on
operator-manual pushes. Operator-manual pushes get deterministic pre-push
coverage; review coverage requires the driver verdict gate or an
external CI that invokes review.

### Plan-only commits and the integrity gate

`loom plan` sessions produce commits whose diff is `specs/**.md`
only. These commits routinely add Success Criteria bullets whose
`[check]` / `[test]` / `[system]` / `[judge]` annotations name
verifiers that will be implemented by a follow-on `loom loop`
bead, not by the plan commit itself. Naively, every such
annotation would fail the integrity gate inside `nix flake check`
at pre-push time, blocking the plan commit from shipping and
forcing operators toward `--no-verify` or an external allowlist —
neither acceptable.

The mechanism that makes plan-only commits clear the pre-push gate
is the **pending modifier** `?` on the annotation itself, owned
and described by [gate.md — Pending modifier](gate.md#pending-modifier).
The plan-stage rubric obligates the agent to mark every new
annotation whose target won't resolve at commit time as
`[tier?](target)`; the integrity gate then accepts the pending
annotation silently and continues to refuse genuinely-broken
unmarked annotations. The implementing diff drops the `?` at the
same moment it lands the verifier, enforced structurally by the
`UnneededPendingMarker` finding.

This spec therefore does **not** file-scope `nix flake check` to
skip on spec-only pushes, **nor** add a pre-commit hook for spec
syntactic checks. The pending-modifier mechanism solves the
failure mode at the annotation layer; both stages of the hook
chain run unchanged, against a tree where pending claims are
declared as such.

## Success Criteria

### Configuration

- The configured pre-commit stage binds native sanitation, formatter,
  shell-reexec, and affected-file gate entries to their intended files and
  stage; no isolated key can satisfy the verifier without its related entry
  [check](cargo run -p loom-walk -- pre_push_config_marker_wrapper_contract)
- The configured pre-push stage starts with the fast tier, preserves Rust-file
  selectivity, appends prek's exact pushed range, keeps the full suite
  always-on, wraps Nix for absent-tool environments, and routes every entry
  through matching marker metadata
  [check](cargo run -p loom-walk -- pre_push_config_marker_wrapper_contract)
- The pre-push gate receives prek's concrete pushed endpoints rather than a
  potentially stale branch upstream, and uses scope-derived tiers
  [test](pre_push_config_runs_verify_diff_for_pushed_range)
- Rust-file selection and pushed-range verification compose without a
  `LOOM_VERIFY_TIERS` override
  [test](pre_push_config_runs_clippy_and_verify_diff_without_loom_verify_tiers)
- Nix workspace staging includes the repo-local marker-policy wrapper
  [check](grep -q '"bin/pre-push-checks"' nix/workspace.nix)

### Shell re-exec discipline

- A shell self re-exec that names its interpreter is accepted
  [test](shell_reexec_check_accepts_explicit_interpreter)
- A shell self re-exec that relies on executing `$0` directly is rejected with
  an actionable diagnostic
  [test](shell_reexec_check_rejects_implicit_self_exec)

### Fast tier composition

- `loom gate check` is exposed as a flake-check derivation distinct
  from the targeted pre-push hooks
  [check](cargo run -p loom-walk -- loom_gate_check_derivation_exists)

### Agent self-verify

- The production Rust sandbox image, canonical Wrix hook installation, real
  project config, and prek chain jointly cause an ordinary agent-style commit
  to run the configured pre-commit checks
  [system](nix run .#test-sandbox)
- The same sandbox contains no Nix executable: Nix-dependent pre-push entries
  skip through the packaged wrapper while a non-Nix gate entry still executes
  [system](nix run .#test-sandbox)

### Marker integration

The marker schema and diagnostic CLI outcomes are owned and verified by
[gate.md § Marker](gate.md#marker). This section verifies only pre-push
consumption.
- The `pre-push-checks` wrapper exits 0 without execing its argument
  command only when marker validation proves matching tree/config/range
  and hook coverage for the wrapped hook id/entry
  [test](marker::tests::pre_push_checks_short_circuits_only_on_covered_marker)
- The `pre-push-checks` wrapper execs its argument command when marker
  validation fails because the marker is stale, dirty, config/range-
  mismatched, missing gate-log evidence, or does not cover the wrapped
  hook
  [test](marker::tests::pre_push_checks_falls_through_on_uncovered_or_invalid_marker)
- The `pre-push-checks` wrapper execs its argument command (does not
  fail the push) when the marker file is absent, so operator-manual
  pushes from a clone with no `.loom/marker.json` progress through the
  hook chain rather than aborting
  [test](marker::tests::pre_push_checks_falls_through_on_missing_marker)

## Requirements

### Functional

1. **Stage composition.** Pre-commit provides staged-file sanitation,
   formatting, shell self-reexec safety, and affected deterministic feedback.
   Pre-push composes the fast tier, language-selective lint, exact pushed-range
   verification, and full-suite coverage. Each pre-push entry routes through
   marker-aware fallthrough; the exact configuration is authoritative in
   `.pre-commit-config.yaml`.

2. **Native sanitation.** Prek's built-in sanitation implementations avoid a
   non-portable Python bootstrap while preserving the required staged-file
   cleanup and conflict checks.

3. **Single source of truth.** `.pre-commit-config.yaml` is the
   canonical SOT for deterministic project hooks, consumed uniformly by:
   the bead container's pre-commit feedback, the operator's host prek,
   the driver-side push gate (`prek run --hook-stage pre-push --from-ref
   <base> --to-ref <head>`), and any external CI that invokes the same
   hook chain.

4. **Agent self-verify.** The hook-path placement guarantee is owned by
   [harness.md § Bead Dispatch](harness.md#bead-dispatch). Given that
   placement, the assembled sandbox executes this spec's configured commit
   policy. The final self-check command and range are owned by
   [templates.md — Loop completion self-check and self-review](templates.md#loop-completion-self-check-and-self-review).
   This is feedback, not the pre-push trust source.

5. **Marker handoff.** `MarkerProof` is the content-addressed
   trust artifact defined in [gate.md § Marker](gate.md#marker). Mint
   authority lives driver-side; prek consumes the marker through the
   `pre-push-checks` wrapper around each hook. The wrapper validates
   both workspace fingerprint and hook coverage for the wrapped entry.
   Marker absence or uncovered hooks are normal fall-through conditions,
   not push failures. The marker is not minted by the bead-container
   agent.

### Non-Functional

1. **Cheap commits.** Pre-commit wall-time targets ~1s for ordinary
   staged-file changes so authors keep using `git commit` frequently
   rather than batching changes to avoid the hook. Unknown-input
   `[check]` / `[test]` annotations may run conservatively until their
   verifiers implement input queries; that is a verifier-contract issue,
   not a hook-bypass reason.

2. **Marker-fast driver-loop pre-push.** On the driver-loop integration
   push, the marker short-circuits covered hooks so pre-push completes
   in marker-validation time for those hooks and normal runtime for any
   uncovered hook. Operator-manual pushes pay the full hook chain,
   including the full-suite app, accepted as the rare-case latency.

3. **Forgery-resistant trust.** The bead-container agent cannot mint
   a `MarkerProof`. The driver-side verdict gate is the sole mint
   authority. Agent self-verify is feedback only — an agent that
   skips hooks or fabricates a "verified" claim cannot bypass driver
   verification.

## Out of Scope

- **Generic hook plumbing.** Locks, shim shape, install lifecycle, the
  `push-verified` SHA stamp (SSH-decoupling mechanism, written by the
  pre-push shim on overall pre-push success), and the
  `skip-if-missing` wrapper (PATH-conditional exec for hooks whose
  binary may be absent in some contexts) are owned upstream by
  `wrix.prekHooks`. Failures in serialization, prek shim
  regeneration, generic wrapper-script behaviour, or stamp lifecycle
  belong to that project. The repo-local marker-aware policy wrapper is
  not part of this out-of-scope set; this spec owns it via
  `bin/pre-push-checks` and the Marker integration contract above.
  Loom-owned `core.hooksPath` placement in `.loom/integration` and bead
  clones is specified in [harness.md](harness.md); this spec consumes
  the canonical path and hook wrappers by name in
  `.pre-commit-config.yaml`.

- **Per-user `pre-commit install`.** Installation flows through
  `wrixLib.mkDevShell` exclusively. The
  `.pre-commit-config.yaml` shape is portable but documenting a
  manual-install path is not this spec's concern.

- **Full test and smoke runners.** `nix run .#test` and
  `nix run .#smoke` are owned by [tests.md](tests.md); this spec only
  specifies when the pre-push hook fires the full suite.

- **Binary cache for `nix flake check`.** A wrix-side binary cache
  (cachix / attic / S3 substituter) would let CI and cold operator
  workstations hit precomputed derivations, eliminating cold-cache
  latency on `nix flake check`. The configuration (substituter URL,
  trusted public keys, populator workflow) is a wrix concern and not
  part of this spec's contract. No `nixConfig.substituters` is
  currently declared in this project's flake; that's a known gap, not
  a contract.

- **sccache configuration.** Cross-context cargo cache warmth via
  sccache is governed by `[loom] sccache_dir` (see
  [harness.md](harness.md)); this spec only consumes the resulting
  warmth, not the configuration mechanism.

- **`MarkerProof` schema, type-safety mechanism, and `loom gate
  verify-marker` subcommand contract.** Defined in [gate.md §
  Marker](gate.md#marker). This spec only specifies *when* and
  *where* the marker is consumed at the hook chain, not its internal
  shape.

- **`loom gate review` invocation.** Driver-only; not a prek hook.
  Composed into the verdict gate by [harness.md § Verdict
  Gate](harness.md#verdict-gate) as the LLM-judged half of the audit.
