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

| Stage      | Wall-time target          | Hooks |
|------------|---------------------------|-------|
| pre-commit | ~1s target; bounded by staged-file annotation selectivity | `repo: builtin` `trailing-whitespace`, `end-of-file-fixer` (excludes `.beads/config.yaml`), `check-merge-conflict`; `treefmt --fail-on-change`; `shell-reexec-explicit-interpreter`; `loom gate verify --files` (spec annotation lane only: affected `[check]` / `[test]` verifiers scoped to staged files per [gate.md § Verifier inputs](gate.md); `[system]` excluded) |
| pre-push   | <10s fast tier + full required suite | repo-local `bin/pre-push-checks` wrapping `skip-if-missing nix -- nix flake check` (fast derivations; no-ops in the bead container with no `nix`; marker-aware like the rest of the push path); `bin/pre-push-checks` wrapping `cargo clippy --workspace --all-targets -- -D warnings` (file-gated on `\.rs$`); `bin/pre-push-checks` wrapping `loom gate verify --diff <push-range>` (scope-derived project pre-commit lane + affected `[check]` / `[test]`; `[system]` excluded unless added as an explicit hook); `bin/pre-push-checks` wrapping `skip-if-missing nix -- nix run .#test` (full suite: fast flake tier + workspace clippy + full nextest + gate/system checks; always runs when not marker-covered) |

There is no standalone `loom gate verify-marker` hook; the marker is
consulted by the `pre-push-checks` wrapper per-hook (see *Marker
integration* for the lifecycle and the rationale).

The pre-commit trio uses `repo: builtin` — prek's native Rust
implementations of the standard pre-commit hooks. The upstream
`pre-commit/pre-commit-hooks` Python repo is rejected because prek
installs Python hooks via `uv`, and `uv` downloads a glibc-linked
binary that fails ld validation on bare NixOS. `repo: builtin`
sidesteps Python entirely.

The `shell-reexec-explicit-interpreter` hook id wraps
`scripts/check-shell-reexec` as a local `language: system` hook.

### Pre-push hook budget and marker coverage

The pre-push chain has independently wrapped hooks. A hook may be
short-circuited only when the marker proves that exact hook entry was
covered for the same tree, config, and push range; otherwise the wrapper
falls through and executes the hook. Coverage, not a fast/slow label,
controls the shortcut.

`nix flake check` is still budgeted as the fast derivation set. It runs
as the first pre-push hook and inside the full-suite app, but it does
not expose full workspace clippy or nextest checks. On pre-push it is
routed through repo-local `bin/pre-push-checks`, so a driver-minted
marker may skip it only if the marker's `GateSuccess` proves the hook
ran and passed. Its derivation chain contains checks that don't require
compiling the workspace under test: the treefmt-check derivation plus a
`loom gate check` derivation that runs `[check]`-tier verifiers + the
integrity gate + the surface-conformance audit. The loom binary itself
is precompiled (via crane's `cargoArtifacts` chain, reused), so the
derivation's wall-clock budget is the checks themselves, not the build.
Target: <10s warm when it runs.

The full required suite lives at `nix run .#test`, not in flake checks.
That app runs the fast flake tier, `cargo clippy --workspace
--all-targets -- -D warnings`, full workspace `cargo nextest run
--workspace`, and `loom gate system --tree`. The current container smoke
surface is the separate `nix run .#smoke` app owned by [tests.md](tests.md)
and reached through the system tier. This repository has no separate CI
safety net, so the full-suite app is a pre-push hook rather than a
CI-only surface.

The targeted hooks are clippy + `loom gate verify --diff <push-range>`
+ `nix run .#test`. `loom gate verify --diff` uses the scope-derived
contract in [gate.md](gate.md): project pre-commit hooks for the range,
then affected `[check]` / `[test]` annotations; no `LOOM_VERIFY_TIERS`
environment override exists. Each hook runs as an independent prek entry
with its own `files:` regex where applicable so file-pattern selectivity
composes with marker-aware fallthrough. On the driver-loop integration
push the wrapper can short-circuit every covered hook in sub-second
time; on operator-manual pushes (no marker present in the operator's
clone) the wrapper falls through and the hooks run against the host's
warm cache.

### Agent self-verify in the bead container

The bead workspace is a `git clone --local` of the loom workspace, and
the container's bind-mounted `/workspace` includes the cloned `.git/`.
The driver configures the bead clone's `core.hooksPath` to the
canonical `wrix.prekHooks` path before spawning the agent, so prek's
pre-commit chain fires on every `git commit` the agent invokes. The
agent is a developer using git; prek is the orchestrator of "what to
verify when." Workers do not push to the loom workspace; final
in-session trust feedback comes from the prompt-required
`loom gate verify --diff <bead-base>..HEAD` self-check.

What this catches in-session:

- Treefmt drift (pre-commit hook auto-applies; commit blocked until
  the agent stages the formatter's diff).
- Integrity-gate findings on staged files (pre-commit hook fails;
  agent fixes annotations or stages the corrective change).
- `[check]`-tier verifier failures whose inputs intersect staged
  files (same pre-commit hook).
- Project pre-commit failures and affected `[check]` / `[test]`
  failures on the final self-check range
  (`loom gate verify --diff <bead-base>..HEAD`), including hooks such
  as clippy when the project config places them in the pre-commit lane.
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
shim, lock, install, stamp, and skip-if-missing plumbing around it. **The
same packaged hooks are available inside the bead container**:
bead-container entrypoints from profile images that build on `wrixLib`
put the packaged wrappers on `PATH`, and Loom configures
the bead clone's `core.hooksPath` before spawning the agent. The agent's
`git commit` fires the prek pre-commit chain uniformly with the host;
workers do not rely on `git push` for final self-verification. The
downstream project maintains its repo-local marker wrapper, but does not
maintain hook shims, lock scripts, or installation logic. Loom consumes
the canonical installed path:
`loom init` writes it into `.loom/integration`, the molecule push gate
repairs stale `.loom/integration` store paths before verification, and
bead workspace creation writes or repairs it in each `.loom/beads/<id>`
clone before an agent is spawned. The operator checkout's current git
config is not the source of truth.

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

- `.pre-commit-config.yaml` declares the builtin trio via `repo: builtin`
  [check](grep -q 'repo: builtin' .pre-commit-config.yaml)
- `end-of-file-fixer` excludes `.beads/config.yaml`
  [check](grep -q '\.beads/config\.yaml' .pre-commit-config.yaml)
- `treefmt --fail-on-change` runs at the `pre-commit` stage
  [check](grep -q 'treefmt --fail-on-change' .pre-commit-config.yaml)
- The shell-reexec hook invokes `scripts/check-shell-reexec`
  [check](grep -q 'scripts/check-shell-reexec' .pre-commit-config.yaml)
- The pre-commit stage runs `loom gate verify --files` against staged
  files
  [check](grep -q 'loom gate verify --files' .pre-commit-config.yaml)
- The pre-push stage's first hook is the `nix flake check` fast tier
  [check](cargo run -p loom-walk -- pre_push_config_marker_wrapper_contract)
- `.pre-commit-config.yaml` does not register `loom gate verify-marker`
  as a prek hook (the marker is consulted by the `pre-push-checks`
  wrapper per-hook; a standalone gating hook would block
  operator-manual pushes that legitimately have no marker)
  [check](awk 'BEGIN{found=0} /^[[:space:]]*-[[:space:]]*id:.*verify-marker/{found=1} END{exit found}' .pre-commit-config.yaml)
- The pre-push stage includes a `nix flake check` hook with
  `always_run: true`
  [check](grep -q 'nix flake check' .pre-commit-config.yaml)
- The pre-push stage includes `cargo clippy` file-gated on `\.rs$` and
  targeted `loom gate verify --diff` without any `LOOM_VERIFY_TIERS`
  environment override
  [test?](pre_push_config_runs_clippy_and_verify_diff_without_loom_verify_tiers)
- Each pre-push hook entry routes through the repo-local
  `bin/pre-push-checks` wrapper so marker coverage can be checked per
  hook without relying on ambient PATH
  [check](cargo run -p loom-walk -- pre_push_config_marker_wrapper_contract)
- Nix workspace staging includes the repo-local `bin/pre-push-checks`
  wrapper so flake checks and verifier builds can read the Loom-owned
  marker policy
  [check](grep -q '"bin/pre-push-checks"' nix/workspace.nix)
- Hooks whose entry runs `nix` are wrapped with
  `skip-if-missing nix --` so they no-op in the bead container
  (which has no `nix`) while running normally on host devShell and
  pre-push/full-suite contexts
  [check](cargo run -p loom-walk -- pre_push_config_marker_wrapper_contract)
- The pre-push stage runs `loom gate verify --diff` against the pushed
  range; scope-derived gate policy excludes `[system]` by default and
  project-specific hook selection stays in `.pre-commit-config.yaml`
  [test?](pre_push_config_runs_verify_diff_for_pushed_range)
- The pre-push stage includes an always-run full-suite hook that invokes
  `nix run .#test`
  [check](grep -q 'full-test-suite' .pre-commit-config.yaml)

### Shell re-exec discipline

- `scripts/check-shell-reexec` exists and is executable
  [check](test -x scripts/check-shell-reexec)

### Fast tier composition

- `nix flake check` omits workspace clippy and nextest from the flake
  checks set, while `nix run .#test` covers workspace clippy, full
  nextest, and system verifiers
  [check](cargo run -p loom-walk -- workspace_compile_checks_are_full_test_app_only)
- `loom gate check` is exposed as a flake-check derivation distinct
  from the targeted pre-push hooks
  [check](cargo run -p loom-walk -- loom_gate_check_derivation_exists)

### Agent self-verify

- The bead container's workspace has `core.hooksPath` configured from
  the canonical `wrix.prekHooks` installation so prek fires on agent
  commits
  [test](bead_workspace_configures_and_repairs_hooks_path)
- Bead-container pre-commit hooks fire on the agent's `git commit`
  invocations
  [test](loom_workflow::tests::agent_commit_runs_pre_commit_chain)
- The loop prompt requires a final self-check using
  `loom gate verify --diff <bead-base>..HEAD`; workers do not rely on a
  bead-workspace push to trigger pre-push hooks
  [test](run_template_uses_injected_self_check_range_not_head_shorthand)
- The bead container has no `nix`, so `nix flake check` is skipped
  inside the bead container's prek invocation (other hooks still fire)
  [test](loom_workflow::tests::bead_container_skips_nix_flake_check)

### Marker integration

- `loom gate verify-marker` exits 0 when the marker's tree OID matches
  HEAD's tree OID and porcelain is clean
  [test](marker::tests::verify_marker_exits_zero_on_match)
- `loom gate verify-marker` exits non-zero when the marker file is
  absent
  [test](marker::tests::verify_marker_exits_nonzero_on_missing)
- `loom gate verify-marker` exits non-zero when the marker's tree OID
  does not match HEAD's tree OID
  [test](marker::tests::verify_marker_exits_nonzero_on_tree_mismatch)
- `loom gate verify-marker` exits non-zero when porcelain is non-empty
  even if the tree OID matches
  [test](marker::tests::verify_marker_exits_nonzero_on_dirty_tree)
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

1. **Stage composition.** Pre-commit runs the builtin trio
   (`trailing-whitespace`, `end-of-file-fixer` with `.beads/config.yaml`
   excluded, `check-merge-conflict`), `treefmt --fail-on-change`,
   `shell-reexec-explicit-interpreter`, and `loom gate verify --files`
   against staged files. Pre-push runs per-hook entries (`nix flake
   check`, `cargo clippy`, `loom gate verify --diff <push-range>`,
   `full-test-suite`), each routed through the `pre-push-checks`
   wrapper. A valid marker short-circuits only hooks covered by typed
   `GateSuccess` evidence for the same tree/config/range; on operator-
   manual pushes (no marker present) the wrapper falls through and the
   hooks run against the host's warm cache. `loom gate verify-marker`
   is not itself a prek hook — it is a diagnostic marker-validation
   subcommand, never a standalone gate in the push chain.

2. **Builtin trio over Python.** The trailing-whitespace,
   end-of-file-fixer, and check-merge-conflict hooks use `repo: builtin`
   so prek's native implementations run, not the
   `pre-commit/pre-commit-hooks` Python repo.

3. **Single source of truth.** `.pre-commit-config.yaml` is the
   canonical SOT for deterministic project hooks, consumed uniformly by:
   the bead container's pre-commit feedback, the operator's host prek,
   the driver-side push gate (`prek run --hook-stage pre-push --from-ref
   <base> --to-ref <head>`), and any external CI that invokes the same
   hook chain.

4. **Agent self-verify.** The bead container has `core.hooksPath`
   configured to the canonical `wrix.prekHooks` path so prek fires on
   agent commits. The loop prompt also requires
   `loom gate verify --diff <bead-base>..HEAD` before completion. This
   is the in-session feedback layer; it is **not** the trust source for
   pre-push (see *Non-Functional #3*).

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
