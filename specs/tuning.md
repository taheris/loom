# Artifact tuning

Runs the bounded SkillOpt-style checker pipeline and stages validated artifact
proposals for human review.

## Problem Statement

Prompt and skill improvements need reproducible evaluation without silently
changing trusted behavior. Tuning bounds candidate search and validation, then
stages evidence-backed source proposals for human review.

## Architecture

Checkers evaluate candidates in isolated proposal workspaces; Inbox owns the
human decision and trusted apply handoff. Related owners: [skills](skills.md),
[templates](templates.md), [inbox](inbox.md), [workspaces](workspaces.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Tune Modes

[Acceptance](#success-criteria).

[Tuning — Tune Command Surface](#tune-command-surface) and its proposal-worktree
section own tuning. [Workspaces](workspaces.md) supplies `tune.lock`;
[Inbox](inbox.md) owns queue routing and the trusted post-chat apply handoff.

### Template Tuning Proposals

[Acceptance](#success-criteria).

Loom's workflow templates remain compiled source rather than runtime overrides.
[Tuning — Tune Proposal Worktrees and Beads](#tune-proposal-worktrees-and-beads)
owns proposal isolation, candidate validation, and inbox-exposure policy for
phase and partial tuning. [Templates](templates.md) owns the compile-time
surface those validators exercise.

### SkillOpt-Style Tuning Loop

[Acceptance](#skillopt-style-tuning-loop-1).

`loom tune` adapts SkillOpt's text-optimization discipline to Loom artifacts:

1. **Harvest** evidence from the workspace plus explicitly configured external
   roots.
2. **Mine** recurring tasks, failures, review findings, verifier outcomes, and
   human corrections into training evidence and checkable behavioral cases where
   possible.
3. **Load tuning guidance** from `docs/tuning.md` and applicable package
   `tuning.md` files. Prose guides candidate generation; `loom-case` blocks are
   parsed as declared regression cases.
4. **Select and freeze** a checker plan from the internal machine-readable
   checker registry, requested level (`fast`, `run`, or `full`), budgets,
   evidence pools, and seed. The candidate generator cannot add or remove
   checkers after proposing edits.
5. **Replay current behavior** for selected behavioral cases when the requested
   level runs behavior (`run` / `full`).
6. **Reflect** over training evidence and declared guidance to propose bounded
   edits.
7. **Select** edits under an edit budget, analogous to a textual learning rate.
8. **Apply** edits to a candidate artifact in an isolated proposal worktree.
9. **Gate** the candidate with preflight validators plus selected behavioral
   cases, comparing current and candidate hard/soft scores.
10. **Stage** a tune bead for human review through `loom inbox`.

No automatic background tuning, scheduled tuning, automatic adoption, or
`auto_adopt` config exists in v1.

### Tuning Documentation and Declared Cases

[Acceptance](#success-criteria).

`docs/tuning.md` is Loom's repo-wide tuning document. In the Loom repository it
also documents the tuning system itself; the normative owner remains this spec
(`spec:tuning`). Consumer repositories may commit their own `docs/tuning.md` as
repo-wide tuning guidance.

Package-form skills may also include an optional git-tracked `tuning.md` next to
`skill.md`. Both basenames are matched case-insensitively and generated in
lowercase; duplicate case variants of either basename in one package directory
are hard errors. A package `tuning.md` is loaded only when the owning package
skill is applicable and in the tune target set. Every case in a package
`tuning.md` must include the owning `skill:<name>` target. Loose single-file
skills have no adjacent tuning document in v1.

Tuning markdown prose is optimizer context for all levels (`fast`, `run`,
`full`). Fenced `loom-case` blocks are removed from prose context and parsed as
strict TOML. `loom-case` syntax, path rules, target selectors, and case id rules
are specified in [docs/tuning.md](../docs/tuning.md). Loaded cases are parsed
and validated at every level, including `fast` and `--dry-run`.

### Checker Portfolio

[Acceptance](#success-criteria).

Tune validation uses an internal machine-readable checker registry rather than
ad-hoc checks invented after a candidate diff exists. The registry is not part
of the public `loom-skill` crate. In v1 the authoritative registry is typed Rust
metadata in the internal `loom-tune` crate and is serializable for docs,
snapshots, and `loom tune checker` output. Checker metadata includes id, title,
summary, status, applicable target kinds, supported levels, cost,
mandatory/disableable policy, case schemas, scoring rules, retirement guidance,
and implementation key.

Checker ids are stable compatibility surface and use exactly three dotted
segments:

```text
<kind>.<domain>.<name>
```

V1 kinds are `preflight` and `behavior`. V1 domains are `skill`, `template`,
`review`, `todo`, `loop`, `inbox`, `tune`, `agent`, and `gate`. Unknown kind,
unknown domain, retired checker id, or unregistered checker id is a hard error
when referenced. Retired checker ids remain in the registry with migration or
replacement guidance.

Preflight validators prove candidate legality and run automatically by
applicability. Mandatory preflight validators cannot be disabled and are not
usable in `loom-case` blocks. V1 starts with coarse, stable preflight ids:

- `preflight.skill.registry` — skill parse/frontmatter/name/duplicate/override
  registry legality.
- `preflight.skill.materialization` — safe materialization paths and backend
  disclosure/registration inputs.
- `preflight.skill.protocol-boundary` — skill content cannot weaken compiled
  phase protocol, terminal markers, gate rules, or safety contracts.
- `preflight.template.compile` — candidate phase/partial templates compile
  against typed Askama contexts.
- `preflight.template.conformance` — include graph, marker ownership,
  options/findings wire-format, and surface-reference walkers pass.
- `preflight.tune.case-validation` — loaded `docs/tuning.md` / package
  `tuning.md` cases parse, validate, and reference known active/inactive targets
  legally.

Behavioral checkers are SkillOpt-style task evaluators: run the target
agent/workflow against a case, score behavior with `hard` and `soft` metrics,
compare current vs candidate, and classify the outcome as `improved`,
`regressed`, `persistent-fail`, or `stable-success`.

Initial behavioral checker families are:

- `behavior.review.finding-recall` — run review on a known diff and score
  whether expected `LOOM_FINDING` predicates are present.
- `behavior.todo.decomposition` — run todo decomposition for a known request and
  score parseable/scoped `LOOM_TODO` output.
- `behavior.loop.verify-after-edit` — run a loop fixture and verify that a
  relevant verifier command actually ran after the final relevant edit.
- `behavior.loop.scope-discipline` — run a trap fixture and score that only
  allowed paths changed while the requested task was solved.
- `behavior.inbox.resolution-path` — run an inbox fixture and score that chat,
  not removed host-side mutation commands, resolves the item.
- `behavior.tune.apply-handoff` — run an accepted tune-proposal fixture and
  score a valid `LOOM_APPLY` handoff without chat-side push/integration edits.
- `behavior.agent.context-before-edit` — run a fixture and verify required
  context files were read before the first relevant edit.

Behavioral scores use parsed review/todo/apply protocols, final repository
byte/permission changes (including untracked and agent-committed edits), and
paired backend tool-call/result events. Final-answer claims are not execution
evidence. Read/edit ordering requires completed reads before edit starts;
verification requires successful commands after the last successful relevant
edit. Unknown tools, opaque shell execution where safety/ordering is required,
and mutations without corresponding edit events are reported unavailable, not
certified. Scope checks enforce allowed/forbidden globs and file-count limits.
Inbox `must_update_beads` is currently unavailable without isolated state
transition evidence. Mined text currently has no expected-result oracle:
selected mined cases block as not evaluated before launching a replay, rather
than awarding nonempty text a passing score. Checker IDs and schemas remain
accepted so this limitation is explicit rather than silently skipping selected
cases.

Checker-specific `loom-case` schemas are strict typed TOML structs defined one
checker at a time. V1 schemas stay minimal and deterministic; they do not expose
arbitrary shell scripts, command DSLs, or repo-authored checker implementations.

Behavioral fixture cases use tracked, self-contained fixture directories:

```text
fixture/
  repo/          # files copied into the isolated checker checkout
  state.toml     # optional bead/inbox/tune setup state
  input.md       # optional user/task text
```

Checker implementations own execution. Fixture files are evidence inputs, not
programs to run. Current and candidate sessions receive independent disposable
Git checkouts of the same frozen tracked-file bytes and permissions, without
remotes or borrowed Git/Beads/Loom metadata. `repo/` is copied as files;
`input.md` supplies task context, not flattened repository contents. Unsafe
symlink referents are rejected. Nonempty `state.toml` currently blocks
evaluation: isolated Beads/inbox/tune state provisioning is not implemented and
operator state is never substituted. Replay events persist under
`.loom/logs/tune/` after replay checkouts are removed, including
failure/cancellation paths. Launcher cancellation kills its Unix process group;
detached processes and container cleanup remain the sandbox launcher's
responsibility.

Checker levels:

- `fast` creates a proposal after preflight, tuning-doc validation, and case
  validation only. It runs no behavioral rollouts.
- `run` is the normal bounded behavioral validation level: selected declared
  regression cases plus a small mined selection-evidence sample.
- `full` runs all applicable declared regression cases, then broader mined
  selection evidence until hard caps are reached.

```toml
[tune.checks]
max_behavior_cases = 3
max_wall_time_secs = 1800
max_llm_judge_calls = 10
# Optional checker ids may be disabled; mandatory preflight validators cannot.
# disabled = ["behavior.review.finding-recall"]

[tune.evidence]
selection_fraction = 0.34
external_roots = [
  # "~/.claude/projects",
  # "~/.codex/archived_sessions",
]
```

The wall cap is one deadline shared by candidate preflights, fixture setup, and
behavioral replays, starting when candidate checks begin (not harvesting or
candidate generation). `max_llm_judge_calls` retains its config spelling but
counts evaluation session starts: current and candidate each reserve one credit
before launch. It is not a model-turn/token cap. Exhaustion blocks the proposal
and records incomplete validation, never success; no further evaluation starts.
Every frozen mandatory preflight runs against candidate files, and any failure
suppresses behavioral replay. Candidate tuning cases cannot change the frozen
regressions.

Only optional checkers can be disabled. Disabling a mandatory preflight
validator is a configuration error. A loaded `loom-case` that names a disabled
behavioral checker is also a hard error; Loom does not silently skip explicit
regressions. `selection_fraction` defaults to `0.34` and must satisfy
`0.0 < selection_fraction < 1.0`.

`fast`, `run`, and `full` are explicit command levels; there is no default level
config. `run` treats `max_behavior_cases` as a hard cap, so declared regression
cases may be sampled when too many apply. `full` runs every applicable declared
regression case before sampling mined cases. Only selected cases can block a
proposal, but skipped declared regressions are reported loudly with guidance to
use `full` or raise caps.

Checker planning is deterministic given the targets, loaded cases/evidence,
registered checker metadata, config, and seed. `loom tune ... --seed <n>` pins
the sampling seed; otherwise Loom generates and records one. The seed controls
sampling within stable pools, not mined train/selection split membership.
`loom tune ... --dry-run` prints loaded tuning docs, evidence roots, seed,
candidate case pool, selected/skipped cases, and the frozen checker plan, then
exits before candidate generation.

If targets are invalid during preflight, `loom tune` fails without creating a
bead. If targets are valid but planning/generation later determines the scope is
too broad, incompatible, or cannot fit configured budgets, the run creates a
blocked tune bead that explains the problem and suggests narrower commands. V1
has no `max_targets` / `max_files` knobs; checker budgets and coherence
determine refusal.

### Evidence Roots and Splits

[Acceptance](#success-criteria).

By default, tuning sees only the current workspace (`/workspace` in Loom-managed
containers). V1 mines Loom-owned evidence first: JSONL events under
`.loom/logs/`, gate/review outputs, bead state, git diffs, criterion evidence,
review findings, workspace-contained agent transcripts, and loaded tuning docs.
Evidence is redacted before persistence in proposal artifacts.

External transcript roots are never harvested implicitly. Users may add explicit
external roots in `[tune.evidence].external_roots`; `loom tune` prints every
evidence root before it reads from them.

Mined evidence uses stable `train` / `selection` splits in v1. Split assignment
uses SHA-256 over `repo_or_workspace_salt || evidence_item_id`, maps the digest
to `[0,1)`, and assigns the item to `selection` when the value is less than
`selection_fraction`; all other items are `train`. The salt is an opaque stable
repository/workspace identity owned by the mining algorithm. Reports record only
the salt id; local manifests may record workspace/cache paths separately for
resume/debug, but never as salt material. The seed used for a tune run does not
affect split membership. Reports/manifests also record the split algorithm
version and selection fraction. Training evidence may be shown to candidate
generation. Selection evidence is withheld and used for behavioral
checking/gating. There is no mined `test` split in v1. Declared `loom-case`
cases are tracked regression cases, not hidden selection evidence.

Acceptance policy:

- Preflight failure blocks staging.
- Regression on a selected declared regression case blocks the tune bead.
- Worse aggregate score on selected mined selection evidence blocks the tune
  bead.
- Mixed mined evidence with no aggregate regression remains pending but is
  prominently flagged.
- All adoption still requires human review through `loom inbox`.

The default soft-score regression epsilon is `0.01`. A checker may override it
in metadata. Regression is `candidate.hard < current.hard`, or equal hard with
`candidate.soft < current.soft - epsilon`; improvement mirrors that relation. V1
aggregate scores use equal case weights.

### Tune Proposal Worktrees and Beads

[Acceptance](#success-criteria).

One `loom tune ...` invocation creates one tune proposal bead and one local
proposal envelope, even when multiple skills/templates/partials are targeted.
The proposal id is the tune bead id.

```text
.loom/tune/<bead-id>/
  repo/                 # isolated proposal checkout on branch loom/tune/<bead-id>
  manifest.json         # local execution manifest/cache
  evidence.md           # local expanded evidence appendix
  logs/
  evidence/
```

The tune bead is the canonical durable review record. It carries labels such as
`loom:tune` plus relevant `spec:<label>` labels (`spec:skills` for skill-only
proposals, `spec:templates` for template/partial proposals, both for mixed
proposals). Its body contains the durable human report: state, tuned targets,
proposal branch, base/head commits, tune level, seed, checker-plan hash,
summary, validation table, risks, and inbox-chat context. Bead metadata carries
machine-readable `loom.tune.*` fields for the same canonical state, including:

- `loom.tune.id`
- `loom.tune.state`
- `loom.tune.targets`
- `loom.tune.level`
- `loom.tune.seed`
- `loom.tune.base_commit`
- `loom.tune.proposal_branch`
- `loom.tune.proposal_head`
- `loom.tune.plan_hash`
- `loom.tune.case_counts`
- `loom.tune.outcome_counts`
- `loom.tune.apply_failure` when relevant

`.loom/tune/<id>/` is local and disposable. `manifest.json` is a resume/debug
cache containing the structured checker plan/results and local path map;
`evidence.md` may contain larger excerpts and checker output tails. Manifest
fields include schema version, proposal/bead id, workspace path, state at write,
target kind/names/files, git base/branch/head/commit ids, tune level/seed/ plan
hash/plan/results/caps, and local paths. The bead and proposal branch are
canonical; if manifest and bead disagree, the tune item blocks for review.
`loom inbox view -p <id>` must still work from the tune bead body and local
proposal repo when `evidence.md` is absent. If `.loom/tune/<id>/` is missing or
corrupt but bead metadata and the proposal branch/head still exist, Loom may
regenerate local manifest/evidence artifacts on demand. If the proposal branch
or identified commits are missing/unreachable, the tune item remains
`kind = tune` but moves to blocked state for chat review with repair/drop
options. Corrupt tune items are never silently skipped.

Tune proposal states are:

```text
pending       # valid proposal awaiting review
blocked       # proposal/run/artifact needs human decision before adoption
accepted      # human authorized inclusion in the next apply batch
applied       # batch passed gates and pushed to origin
rejected      # human decided not to adopt/drop it
apply_failed  # accepted, but batch apply/gate/push failed
```

State mirrors to bead status: `pending` and transient `accepted` are open;
`blocked` and `apply_failed` are blocked; `applied` and `rejected` are closed.
No `archived` or `deferred` state exists in v1.

`loom tune` does not push proposal branches in v1 and does not modify the
operator checkout. The local proposal branch lives inside `.loom/tune/<id>/repo`
only. Remote/asynchronous proposal publication is deferred.

Skill tuning proposals modify existing repo/configured skill files or create
tracked built-in overrides under `.loom-override/skills/`.

Phase/partial tuning proposals modify template source files in the proposal
worktree. Before a template proposal enters the inbox, Loom validates it in that
worktree by compiling the Askama templates, rendering representative snapshots,
and running template conformance walkers. Askama type safety is useful only when
candidate templates are compiled against the typed contexts; therefore candidate
validation is a required tuning stage, not an optional post-review step.

### Tune Command Surface

[Acceptance](#success-criteria).

`loom tune` with no subcommand prints command help and exits without tuning.
Listing commands are read-only and do not create beads:

| Command             | Meaning                                                                                                                   |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `loom tune skill`   | List tuneable skills.                                                                                                     |
| `loom tune phase`   | List tuneable phase templates.                                                                                            |
| `loom tune partial` | List tuneable partials.                                                                                                   |
| `loom tune checker` | List registered tuning checkers with id, status, target kinds, levels, cost, mandatory/disableable policy, and summaries. |
| `loom tune all`     | List all tuneable surfaces and counts.                                                                                    |

Proposal creation requires an explicit level:

| Command                 | Meaning |
| ----------------------- | ------- |
| `loom tune skill fast   | run     | full [<skill-name>...]`   | Tune all applicable skills when no names are supplied, or the named skills when supplied.                                        |
| `loom tune phase fast   | run     | full [<phase-name>...]`   | Tune all phase templates when no names are supplied, or named phase templates such as `plan`, `todo`, `loop`, `review`, `inbox`. |
| `loom tune partial fast | run     | full [<partial-name>...]` | Tune all partials when no names are supplied, or named partials such as `review_rubric`.                                         |
| `loom tune all fast     | run     | full`                     | Tune skills, phase templates, and partials in one proposal. Target names are not accepted after `all`.                           |

There are no plural aliases (`skills`, `phases`, `partials`), no `template`
umbrella command, and no `msg` phase target in v1. Template target names use
phase names and partial filenames without `.md`.

Each proposal-creating invocation creates one proposal bead. Mixed surfaces are
allowed only through `loom tune all fast|run|full` in v1. Proposal branches may
contain one or more commits; one commit total is the default expectation unless
the tuning agent has a strong reason to split. A proposal command with no target
names tunes every target on that surface; if the requested scope is too broad
for the checker budget or cannot form one coherent proposal, Loom blocks the
tune bead with split guidance rather than silently creating multiple beads.

Common tune flags for proposal-creating commands:

| Flag         | Meaning                                                                                                                                                    |
| ------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--dry-run`  | Print loaded tuning docs, evidence roots, seed, case pool, selected/skipped cases, and frozen checker plan; create no candidate. Invalid on list commands. |
| `--seed <n>` | Use a deterministic checker-plan seed; generated and recorded when absent.                                                                                 |

### Human Review Through Inbox

[Acceptance](#success-criteria).

The inbox command modes, addressing, filters, queue ordering, interactive
resolution authority, terminal markers, and trusted apply batch are defined once
in [Inbox — Inbox Modes](inbox.md#inbox-modes). This spec does not restate that
shared workflow contract.

Tuning contributes tune-kind proposal records to that authoritative inbox flow.
Each record and local envelope must satisfy
[Tune Proposal Worktrees and Beads](#tune-proposal-worktrees-and-beads), so the
inbox can review the candidate and hand any authorized adoption to the trusted
driver without making tuning a second resolution authority.

## Success Criteria

### Workflow commands

- The tune CLI surface owned by [Tuning](#tune-command-surface) is wired into
  the binary, and read-only tune invocations do not allocate tune proposal
  envelopes [test](loom_tune_bare_prints_help_without_proposal)

- Tune dry-runs and proposal creation dispatch through the same
  `loom-workflow::tune` planning integration, with dry-run stopping before
  candidate generation; evidence policy and checker-plan semantics are owned by
  [Tuning](#skillopt-style-tuning-loop)
  [test](loom_tune_level_seed_dry_run_shape_plan)

### Auxiliary commands

<!-- prettier-ignore -->
- `loom sync` remains absent, but `loom tune` is present as the manual
  SkillOpt-style proposal command; the surface-conformance walk rejects any
  reintroduction of sync and validates the tune subcommand shape [check](cargo run -p loom-walk -- tune_surface_conformance)

### Acceptance

- `loom tune` with no subcommand prints help; `loom tune skill`, `phase`,
  `partial`, `checker`, and `all` list surfaces/checkers; proposal creation
  requires explicit `fast`, `run`, or `full` after
  `skill`/`phase`/`partial`/`all`; `--dry-run` and `--seed` apply only to
  proposal-creating commands [test](loom_tune_cli_surface)

- Each tuning invocation creates one tune bead plus one isolated
  `.loom/tune/<bead-id>/` envelope with `repo/`, `manifest.json`, `evidence.md`,
  candidate commit(s), and no changes to the invoking checkout
  [test](loom_tune_subcommands_create_isolated_proposals)

- Tune checker planning freezes a deterministic registered-checker plan from the
  internal typed `loom-tune` registry before candidate generation, records the
  level/seed/case-pool/selected/skipped plan/results in the bead/manifest, and
  rejects post-candidate checker changes as validation evidence
  [test](tune_checker_plan_freeze_contract)

- Skill tuning reads workspace evidence by default, reads explicit
  `[tune.evidence].external_roots` only when configured, loads `docs/tuning.md`
  plus applicable package `tuning.md` files, validates `loom-case` blocks,
  prints evidence roots before harvesting, and gates candidate edits with
  selected behavioral cases before inbox exposure
  [test](skill_tune_evidence_roots_and_gate)

- Phase and partial tuning validates candidate templates in the proposal
  worktree by compiling Askama templates, rendering representative snapshots,
  and running template conformance walkers before inbox exposure
  [test](template_tune_candidate_validation)

### SkillOpt-Style Tuning Loop

- Candidate preflights inspect actual artifact files and preserve registry and
  tuning-case invariants.
  [test](candidate_preflights_read_files_and_preserve_registry_and_case_invariants)

- A failed mandatory candidate preflight prevents behavioral replay and blocks
  the proposal. [test](tune_failed_mandatory_preflight_suppresses_replays)

- The tuning wall budget is shared across evaluation work and cancels in-flight
  replay when exhausted.
  [test](wall_budget_is_shared_and_cancels_in_flight_work)

- A zero tuning wall budget starts no replay.
  [test](tune_zero_wall_budget_starts_no_replay)

- Evaluation budgets reserve baseline and candidate work separately and cannot
  admit incomplete comparisons.
  [test](tune_evaluation_cap_reserves_each_side_and_blocks_incomplete_results)

- Unsupported tuning fixture state blocks evaluation rather than being flattened
  or ignored.
  [test](unsupported_fixture_state_is_not_silently_flattened_or_ignored)

- Replay timeout terminates the mutating launcher and its descendants so timed-
  out work cannot continue changing the fixture.
  [test](tune_wall_timeout_terminates_mutating_launcher_and_descendant)

- A nonzero replay result cleans up the isolated fixture and blocks proposal
  admission. [test](tune_nonzero_replay_cleans_up_and_blocks)

- Baseline and candidate replays launch independent fixtures and preserve their
  production event evidence.
  [test](tune_replay_launches_independent_fixtures_and_preserves_events)

- Scope regression scoring compares repository bytes even when an agent commits
  its edits.
  [test](tune_scope_regression_uses_repository_bytes_even_after_agent_commits)

- Mined nonempty text without behavioral observation is explicitly reported as
  not evaluated. [test](mined_nonempty_text_is_explicitly_not_evaluated)

- Todo behavioral scoring counts parsed beads and checks required and forbidden
  spec assignments, not prose claims.
  [test](todo_counts_parsed_beads_and_checks_required_and_forbidden_specs)

- Apply scoring requires parsed proposal identities and respects the declared
  integration and push permissions.
  [test](apply_requires_parsed_ids_and_honors_push_and_integration_flags)

- Inbox safety scoring relies on observed actions, never asserted command
  execution or bead-update prose.
  [test](inbox_safety_flags_never_accept_command_or_bead_update_claims)

- Context scoring requires completed required reads before the first observed
  edit. [test](context_requires_completed_reads_before_the_first_edit)

- Verification scoring requires a successful verifier command after the final
  observed edit.
  [test](verify_requires_successful_command_after_final_observed_edit)

- Scope scoring uses observed paths rather than positive or forbidden words in
  agent prose.
  [test](scope_scores_observed_paths_not_positive_or_forbidden_text)

## Requirements

### Command entrypoint

[Acceptance](#success-criteria).

- `loom tune` — manual SkillOpt-style tuning surface. The command and proposal
  creation/isolation contracts are owned by
  [Tuning — Tune Command Surface](#tune-command-surface) and
  [Tuning — Tune Proposal Worktrees and Beads](#tune-proposal-worktrees-and-beads);
  this tuning spec owns its workflow placement, while [Inbox](inbox.md) owns
  human review and the trusted apply handoff.

### Functional

1. **Internal tuning engine.** The SkillOpt-style tuning engine remains internal
   in v1, with registry/case/evidence/scoring/metadata types housed in the
   internal `loom-tune` crate. Public tuning APIs are out of scope until the
   evidence, task, replay, gate, and proposal types stabilize.

2. **Manual tuning.** `loom tune` with no subcommand prints help and never
   starts tuning. `loom tune skill` / `phase` / `partial` / `checker` / `all`
   are listing commands. Tuning starts only when `fast`, `run`, or `full`
   follows `skill`, `phase`, `partial`, or `all`. Omitted names tune every
   target on that surface.
3. **Checker portfolio.** Tuning uses Loom-registered internal checkers in
   `fast`, `run`, or `full` levels. Repo config may set budgets and disable
   optional checker ids, but mandatory preflight validators remain enabled; v1
   has no arbitrary tune-specific checker commands and no public checker
   registry API.
4. **Proposal bead and isolation.** Tuning creates one tune bead and one local
   `.loom/tune/<bead-id>/` envelope per invocation. Proposal commits live in
   `repo/` on branch `loom/tune/<bead-id>` and never modify the invoking
   checkout or push automatically.
5. **Template validation.** Phase and partial template proposals must validate
   in their proposal worktree before entering `loom inbox` as pending.
6. **Inbox ownership.** Tuning emits tune-kind proposal records; the command
   surface and resolution authority are owned exclusively by
   [Inbox — Inbox Modes](inbox.md#inbox-modes).
7. **Tune apply handoff.** Tune adoption follows the trusted apply contract in
   [Inbox — Inbox Modes](inbox.md#inbox-modes); tuning does not define a second
   apply path.
8. **Workspace-first evidence.** Tuning evidence defaults to the workspace;
   external transcript roots require `[tune.evidence].external_roots` and are
   printed before use. Mined evidence is stably split into `train` and
   `selection` using `[tune.evidence].selection_fraction`.

### Non-Functional

1. **Privacy.** Loom never implicitly reads home-directory transcript stores.
   Evidence roots outside the workspace are explicit configuration.
2. **Safety.** Skill tuning cannot weaken phase protocol. Template tuning
   follows [Templates — Out of Scope](templates.md#out-of-scope). Native
   registration failure is fatal when native registration was selected.

## Out of Scope

- Automatic background tuning/adoption and implicit home-directory transcript
  harvesting are excluded. Inbox owns human approval and trusted apply handoff.

- Automatic background tuning, scheduled tuning, auto-adoption, and `auto_adopt`
  config.
- Implicit harvesting of `~/.claude`, `~/.codex`, or any other path outside the
  workspace.
