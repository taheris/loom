# Pre-Commit Discipline

Hook composition policy for this project's `.pre-commit-config.yaml`:
which checks fire at commit time vs push time, what each guarantees,
how the agent self-verifies inside its bead container, and how the
content-addressed `MarkerProof` short-circuits redundant pre-push work
on driver-loop integration pushes. The hook plumbing (lock, shim shape,
devshell wiring, install lifecycle, marker-check wrapper) is owned
upstream by `wrapix.prekHooks`.

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
restructured to a sub-10s fast tier; the workspace cargo dance runs as
per-hook prek entries against the host's warm cache; and a content-
addressed `MarkerProof` minted by the driver-side verdict gate lets
prek's pre-push short-circuit redundant audit work on driver-loop
integration pushes. Operator-manual pushes pay the slow tier exactly
once per push (~30–90s warm) against the host cache — a first-class,
supported path, no `--no-verify` required.

## Architecture

### Stage composition

| Stage      | Wall-time target          | Hooks |
|------------|---------------------------|-------|
| pre-commit | ~1s                       | `repo: builtin` `trailing-whitespace`, `end-of-file-fixer` (excludes `.beads/config.yaml`), `check-merge-conflict`; `treefmt --fail-on-change`; `shell-reexec-explicit-interpreter`; `loom gate verify --files` (integrity gate + `[check]`-tier verifiers whose declared inputs intersect staged files) |
| pre-push   | <10s fast + ~30–90s slow  | `skip-if-missing nix -- nix flake check` (the <10s fast tier — treefmt-check derivation + `loom gate check` derivation; **no Rust workspace compile**; no-ops in the bead container which has no `nix`); `pre-push-checks cargo build --workspace` (file-gated on `\.rs$`); `pre-push-checks cargo clippy --workspace --all-targets -- -D warnings` (file-gated on `\.rs$`); `pre-push-checks cargo nextest run --workspace` (file-gated on `\.rs$`); `pre-push-checks loom gate verify --diff <range>` (deterministic-tier verifiers whose declared inputs intersect the diff); `pre-push-checks skip-if-missing nix -- nix run .#test` (`container-smoke`; gated on `^crates/[^/]+/tests/properties\.rs$`; the nested wrappers compose — `pre-push-checks` short-circuits on a valid marker; on marker miss it execs the wrapped command, which is itself wrapped by `skip-if-missing` to no-op when `nix` is absent) |

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

### Fast tier vs slow tier

The pre-push chain has two tiers: a fast tier that always runs, and
a slow tier whose hooks are individually wrapped to short-circuit on
a valid marker.

The **fast tier** is `nix flake check`. It runs on every push and on
every CI build, unconditionally — no marker shortcut, no file gating.
Its derivation chain contains only checks that don't require compiling
the workspace under test: the treefmt-check derivation plus a
`loom gate check` derivation that runs `[check]`-tier verifiers + the
integrity gate + the surface-conformance audit. The loom binary itself
is precompiled (via crane's `cargoArtifacts` chain, reused), so the
derivation's wall-clock budget is the checks themselves, not the
build. Target: <10s warm.

The **slow tier** is the cargo dance + `loom gate verify --diff <range>`
+ `container-smoke`. Each runs as an independent prek hook with its
own `files:` regex so file-pattern selectivity composes with the
marker-aware short-circuit. Each entry routes through a wrapper
(`pre-push-checks` — owned upstream, see *Plumbing*) that consults the
`MarkerProof` and exits 0 on valid marker, otherwise execs the
underlying command. On the
driver-loop integration push the wrapper short-circuits the entire
slow tier in sub-second time; on operator-manual pushes (no marker
present in the operator's clone) the wrapper falls through and the
slow tier runs against the host's warm `target/` cache.

CI is the immovable defence-in-depth: it invokes the same prek hooks
plus `loom gate review` in a nix-pure sandbox with no marker shortcut,
so the per-PR cycle re-derives every check fresh.

### Agent self-verify in the bead container

The bead workspace is a `git clone --local` of the loom workspace, and
the container's bind-mounted `/workspace` includes the cloned
`.git/`. The bead container inherits `core.hooksPath` from the loom
workspace's prek installation (same `wrapix.prekHooks`-shipped path),
so prek's hook chain fires on every `git commit` and `git push` the
agent invokes. The agent is a developer using git; prek is the
orchestrator of "what to verify when."

What this catches in-session:

- Treefmt drift (pre-commit hook auto-applies; commit blocked until
  the agent stages the formatter's diff).
- Integrity-gate findings on staged files (pre-commit hook fails;
  agent fixes annotations or stages the corrective change).
- `[check]`-tier verifier failures whose inputs intersect staged
  files (same pre-commit hook).
- Cargo build / clippy / nextest failures on the bead branch's push
  to the loom workspace (pre-push hooks fire against the bead tree
  warm with the agent's session work).

What it does *not* do: act as the trust source for the driver-side
verdict gate. The agent's hook chain is **feedback only**. The driver
runs its own independent audit (per [harness.md § Verdict
Gate](harness.md#verdict-gate)) post-`LOOM_COMPLETE`; the agent
cannot mint a `MarkerProof` and cannot bypass driver verification by
emitting a structured "I verified" report.

The bead container has no `nix`. Hooks whose entry runs `nix` are
wrapped as `entry: skip-if-missing nix -- <command>` in
`.pre-commit-config.yaml` (the wrapper is shipped from
`wrapix.prekHooks` per *Plumbing* below); inside the bead container
the wrapper observes `nix` absent on `PATH` and exits 0 silently,
no-op-ing the hook. Outside the bead container (host devShell + CI)
the same wrapper finds `nix` and execs normally. Everything else in
the host's pre-commit / pre-push chain runs uniformly across both
contexts.

### Marker integration

`MarkerProof` is the content-addressed trust-bearing artifact defined
in [gate.md § Marker](gate.md#marker). Lifecycle within this spec's
scope:

1. Driver-side verdict gate runs the full audit (`prek run
   --hook-stage pre-push --all-files` + `loom gate review`) at the
   loom workspace post-rebase, inside the `index.lock` critical
   section.
2. On audit-pass, the verdict gate mints `MarkerProof` from the sealed
   `GateSuccess` and atomically writes it to
   `.loom/marker.json` in the loom workspace.
3. The loom workspace runs `git push origin <integration-branch>` —
   still inside the critical section.
4. prek's pre-push chain fires. The `nix flake check` fast tier runs
   first, unconditionally (no marker consultation). Each slow-tier
   hook's entry then routes through the `pre-push-checks` wrapper,
   which deserializes the marker, computes the workspace fingerprint
   (tree OID at HEAD + porcelain-clean precondition), compares
   against the marker's tree OID, and exits 0 without execing the
   underlying command on match. On marker absent / mismatch / dirty
   tree, the wrapper falls through and execs the underlying command,
   so the slow tier runs.

There is no standalone `loom gate verify-marker` prek hook. The
binary stays as a callable subcommand (used by the wrapper, available
for diagnostic invocation), but registering it as its own gating
first hook would block operator-manual pushes — the wrapper is the
sole hook-chain consumer of the marker, and "no marker" is a normal
operator-push condition the wrapper handles by falling through, not a
failure that should stop the push.

Operator-manual pushes from the operator's `/workspace` clone have no
`MarkerProof` (different clone — the verdict gate never wrote a marker
there). The wrapper's marker check returns "no valid marker", and
because the wrapper treats that as fall-through (not failure), prek's
pre-push chain runs the fast tier + the full slow tier against the
operator's host cache. No `--no-verify` is required — the
operator-manual push is a first-class, supported path that pays the
slow tier exactly once per push.

### Plumbing (owned upstream)

`core.hooksPath`, the hook shim scripts, the flock that serializes
prek's stash/restore window across overlapping commits, the
`push-verified` SHA stamp (the pre-push shim writes it on overall
pre-push success; the user's git-push re-run consumes it instantly
to decouple from SSH latency), the `pre-push-checks` wrapper script
(per-hook marker-aware short-circuit inside the prek chain), and the
`skip-if-missing` wrapper (PATH-conditional exec for hooks whose
binary may be absent in some contexts, notably `nix` inside the
bead container) are all packaged in the `wrapix.prekHooks`
derivation and installed by `wrapixLib.mkDevShell` when this
project's `nix develop` is entered. **The same installation applies
inside the bead container**: bead-container entrypoints from profile
images that build on `wrapixLib` inherit `core.hooksPath` and put
both wrappers on `PATH`, so the agent's `git commit` and `git push`
fire the prek chain uniformly with the host. The downstream project
does not maintain its own hook shims, lock script, wrappers, or
installation logic.

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
command otherwise. Wrapix does **not** maintain a hook-id skip
list, and does not stub `nix` on the container `PATH`; the absence
is observable at the hook's entry, tagged via the wrapper at the
point of use.

### Source-of-truth files

`.pre-commit-config.yaml` is the **single source of truth** for what
runs at pre-commit / pre-push. It has four consumers:

1. The bead container's prek (pre-commit + pre-push on agent's session,
   for feedback).
2. The operator's host prek (pre-commit + pre-push on operator's
   commits/pushes).
3. The driver's verdict gate, which invokes `prek run --hook-stage
   pre-push --all-files` programmatically against the loom workspace's
   integrated tree as part of the audit.
4. CI, which invokes the same `prek run` path plus `loom gate review`
   in a nix-pure sandbox.

This spec owns:

- `.pre-commit-config.yaml`
- `scripts/check-shell-reexec`

`loom gate review` is the driver-only addition — not a prek hook. It
runs at the verdict gate's molecule-completion audit and at CI; it
does not run in the bead container (self-review is structurally weak)
and does not run on operator-manual pushes (operators get review
coverage from CI on the next PR).

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
  [check?](grep -q 'loom gate verify --files' .pre-commit-config.yaml)
  (pending lm-nzjp.28: the `loom-gate-verify-files` hook is currently
  commented out in `.pre-commit-config.yaml` awaiting variadic
  `--files <PATH>...` support in the binary; promote back to `[check]`
  once the hook is reinstated)
- The pre-push stage's first hook is the `nix flake check` fast tier
  [check](awk '/^      - id:/{last=$0} /stages:.*pre-push/{print last; exit}' .pre-commit-config.yaml | grep -q 'nix flake check\|flake-check')
- `.pre-commit-config.yaml` does not register `loom gate verify-marker`
  as a prek hook (the marker is consulted by the `pre-push-checks`
  wrapper per-hook; a standalone gating hook would block
  operator-manual pushes that legitimately have no marker)
  [check](awk 'BEGIN{found=0} /^[[:space:]]*-[[:space:]]*id:.*verify-marker/{found=1} END{exit found}' .pre-commit-config.yaml)
- The pre-push stage includes a `nix flake check` hook with
  `always_run: true`
  [check](grep -q 'nix flake check' .pre-commit-config.yaml)
- The pre-push stage includes per-hook `cargo build`, `cargo clippy`,
  and `cargo nextest` entries, each file-gated on `\.rs$`
  [check](grep -E -q 'cargo (build|clippy|nextest)' .pre-commit-config.yaml)
- Each slow-tier pre-push hook's `entry` routes through the
  `pre-push-checks` wrapper from `wrapix.prekHooks`
  [check](grep -q 'pre-push-checks' .pre-commit-config.yaml)
- Hooks whose entry runs `nix` are wrapped with
  `skip-if-missing nix --` so they no-op in the bead container
  (which has no `nix`) while running normally on the host devShell
  and in CI
  [check](grep -q 'skip-if-missing nix --' .pre-commit-config.yaml)
- The pre-push stage runs `loom gate verify --diff` for the deterministic
  verifier tier against the pushed range
  [check](grep -q 'loom gate verify --diff' .pre-commit-config.yaml)
- The pre-push stage includes a container-smoke hook gated on
  `crates/*/tests/properties.rs`
  [check](grep -q 'tests/properties.rs' .pre-commit-config.yaml)

### Shell re-exec discipline

- `scripts/check-shell-reexec` exists and is executable
  [check](test -x scripts/check-shell-reexec)

### Fast tier composition

- `nix flake check` performs no Rust workspace compile of the project
  under test (the loom-deps / loom-0.1.0 / clippy / nextest derivations
  are excluded from `flake check`; their work moves to dedicated
  pre-push hooks against the host cache)
  [check](cargo run -p loom-walk -- nix_flake_check_excludes_workspace_compile)
- `loom gate check` is exposed as a flake-check derivation distinct
  from the slow-tier hooks
  [check](cargo run -p loom-walk -- loom_gate_check_derivation_exists)

### Agent self-verify

- The bead container inherits `core.hooksPath` from the
  `wrapix.prekHooks` installation so prek fires on agent commits
  [test?](loom_workflow::tests::bead_container_inherits_hooks_path)
- Bead-container pre-commit hooks fire on the agent's `git commit`
  invocations
  [test?](loom_workflow::tests::agent_commit_runs_pre_commit_chain)
- Bead-container pre-push hooks fire on the bead branch's push to the
  loom workspace
  [test?](loom_workflow::tests::bead_push_runs_pre_push_chain)
- The bead container has no `nix`, so `nix flake check` is skipped
  inside the bead container's prek invocation (other hooks still fire)
  [test?](loom_workflow::tests::bead_container_skips_nix_flake_check)

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
  command when `loom gate verify-marker` exits 0
  [test](marker::tests::pre_push_checks_short_circuits_on_valid_marker)
- The `pre-push-checks` wrapper execs its argument command when
  `loom gate verify-marker` exits non-zero
  [test](marker::tests::pre_push_checks_falls_through_on_invalid_marker)
- The `pre-push-checks` wrapper execs its argument command (does not
  fail the push) when the marker file is absent, so operator-manual
  pushes from a clone with no `.loom/marker.json` progress through the
  slow tier rather than aborting
  [test](marker::tests::pre_push_checks_falls_through_on_missing_marker)

## Requirements

### Functional

1. **Stage composition.** Pre-commit runs the builtin trio
   (`trailing-whitespace`, `end-of-file-fixer` with `.beads/config.yaml`
   excluded, `check-merge-conflict`), `treefmt --fail-on-change`,
   `shell-reexec-explicit-interpreter`, and `loom gate verify --files`
   against staged files. Pre-push runs `nix flake check` first (the
   <10s fast tier — no workspace Rust compile), then the slow tier as
   per-hook entries (`cargo build`, `cargo clippy`, `cargo nextest`,
   `loom gate verify --diff <range>`, `container-smoke`), each routed
   through the `pre-push-checks` wrapper. A valid marker short-circuits
   the slow tier on driver-loop integration pushes; on operator-manual
   pushes (no marker present) the wrapper falls through and the slow
   tier runs against the host's warm cache. `loom gate verify-marker`
   is not itself a prek hook — it is the wrapper's marker-validation
   subcommand, available for diagnostic invocation but never gating
   the push chain on its own.

2. **Builtin trio over Python.** The trailing-whitespace,
   end-of-file-fixer, and check-merge-conflict hooks use `repo: builtin`
   so prek's native implementations run, not the
   `pre-commit/pre-commit-hooks` Python repo.

3. **Single source of truth.** `.pre-commit-config.yaml` is the
   canonical SOT for deterministic hooks, consumed uniformly by: the
   bead container's prek, the operator's host prek, the driver-side
   verdict gate (`prek run --hook-stage pre-push --all-files`), and CI.

4. **Agent self-verify.** The bead container inherits `core.hooksPath`
   so prek fires on agent commits and bead-branch pushes. This is the
   in-session feedback layer; it is **not** the trust source for
   pre-push (see *Non-Functional #3*).

5. **Marker handoff.** `MarkerProof` is the content-addressed
   trust artifact defined in [gate.md § Marker](gate.md#marker). Mint
   authority lives driver-side; prek consumes the marker through the
   `pre-push-checks` wrapper around each slow-tier hook (the
   wrapper invokes `loom gate verify-marker` or equivalent
   marker-validation logic). Marker absence is a normal condition for
   operator-manual pushes, not a push failure. The marker is not
   minted by the bead-container agent.

### Non-Functional

1. **Cheap commits.** Pre-commit wall-time targets ~1s so authors keep
   using `git commit` frequently rather than batching changes into
   larger commits to avoid the hook.

2. **Sub-1-min driver-loop pre-push.** On the driver-loop integration
   push, the marker short-circuits the slow tier so pre-push completes
   in `nix flake check` time (<10s) plus marker validation
   (sub-second). Operator-manual pushes pay the full slow tier
   (~30–90s warm), accepted as the rare-case latency.

3. **Forgery-resistant trust.** The bead-container agent cannot mint
   a `MarkerProof`. The driver-side verdict gate is the sole mint
   authority. Agent self-verify is feedback only — an agent that
   skips hooks or fabricates a "verified" claim cannot bypass driver
   verification.

## Out of Scope

- **Hook plumbing.** Locks, shim shape, install lifecycle, the
  `push-verified` SHA stamp (SSH-decoupling mechanism, written by the
  pre-push shim on overall pre-push success), the `pre-push-checks`
  wrapper script (per-hook marker-aware short-circuit), and the
  `skip-if-missing` wrapper (PATH-conditional exec for hooks whose
  binary may be absent in some contexts) are all owned upstream by
  `wrapix.prekHooks`. Failures in serialization, prek shim
  regeneration, `core.hooksPath` wiring, wrapper-script behaviour,
  or stamp lifecycle belong to that project. This spec consumes
  these surfaces by name in `.pre-commit-config.yaml`; it does not
  redefine them.

- **Per-user `pre-commit install`.** Installation flows through
  `wrapixLib.mkDevShell` exclusively. The
  `.pre-commit-config.yaml` shape is portable but documenting a
  manual-install path is not this spec's concern.

- **Container smoke runner.** `nix run .#test` is owned by
  [tests.md](tests.md); this spec only specifies when the hook fires
  it.

- **Binary cache for `nix flake check`.** A wrapix-side binary cache
  (cachix / attic / S3 substituter) would let CI and cold operator
  workstations hit precomputed derivations, eliminating cold-cache
  latency on `nix flake check`. The configuration (substituter URL,
  trusted public keys, populator workflow) is a wrapix concern and not
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
