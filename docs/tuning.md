# Tuning

This document has two roles:

1. It defines Loom's tuning model, built-in checker contract, and consumer
   `docs/tuning.md` format.
2. When this repository tunes itself, it is also the repo-wide tuning guidance
   loaded by `loom tune`.

The normative owner is [`specs/skills.md`](../specs/skills.md) / `spec:skills`.
Changes to checker ids, checker domains, case syntax, or tuning acceptance rules
are planned through that spec.

## SkillOpt Adaptation

Loom tuning follows SkillOpt's discipline, but with Loom-specific artifacts and
human review:

```text
harvest evidence -> mine checkable cases -> generate bounded edits
  -> compare current vs candidate behavior -> stage tune bead -> inbox review
```

Static checks are **preflight validators**. They prove candidate legality, such
as frontmatter shape or template compilation. They are not the primary quality
signal.

Behavioral checkers are the SkillOpt-style signal. A behavioral checker runs a
representative task with the current artifact and the candidate artifact, scores
both with `hard` and `soft` metrics, and records the delta.

## Tuning Documents

Loom loads tuning guidance from:

```text
docs/tuning.md                 # repo-wide, if present and git-tracked
<skill-package>/tuning.md      # optional package-local guidance
```

Skill and tuning package filenames are matched case-insensitively:

```text
skill.md, SKILL.md, Skill.md

tuning.md, TUNING.md, Tuning.md
```

Generated files use lowercase `skill.md` and `tuning.md`. Multiple case variants
of either basename in one directory are hard errors.

Package `tuning.md` must be git-tracked and loads only when the owning package
skill is applicable and in the tune target set. Every case in a package
`tuning.md` must include the owning `skill:<name>` target. Loose single-file
skills do not have adjacent tuning docs in v1.

Markdown prose is optimizer context for `fast`, `run`, and `full` tuning. Fenced
`loom-case` blocks are removed from prose context and parsed as structured data.
Declared case summaries may be shown to the candidate generator because they are
tracked regression tests, not hidden validation evidence.

## `loom-case` Blocks

A declared regression case is a fenced block whose info string is exactly
`loom-case`. The body is TOML only.

````markdown
```loom-case
id = "review-misses-style-rule"
checker = "behavior.review.finding-recall"
targets = ["skill:repo-review", "phase:review"]
# role defaults to "regression"

[input]
patch = "tuning/cases/review-misses-style-rule.diff"

[[expected.findings]]
contains = ["docs/style-rules.md", "style rule"]
```
````

Top-level fields:

| Field | Required | Meaning |
|-------|----------|---------|
| `id` | yes | Stable case id, globally unique across loaded tuning docs. |
| `checker` | yes | Built-in behavioral checker id. |
| `targets` | yes | Non-empty array of explicit concrete tune targets. |
| `role` | no | Defaults to `regression`; this is the only v1 role. |
| `input` | checker-specific | Input schema owned by the checker implementation. |
| `expected` | checker-specific | Expected-result schema owned by the checker implementation. |

Unknown fields, unknown tables, TOML parse failures, wrong types, duplicate case
ids, unknown targets, retired checkers, and invalid checker-specific schemas are
hard errors. There is no top-level schema/version field in v1.

### Case Ids

`TuningCaseId` format:

```text
1-96 chars
lowercase ASCII kebab-case
[a-z0-9-] only
must start with [a-z]
no leading/trailing hyphen
no consecutive hyphens
globally unique across all loaded tuning docs
```

No dots, slashes, colons, or path semantics are allowed. Source file and line are
recorded separately for diagnostics.

### Checker Ids

`CheckerId` format:

```text
<kind>.<domain>.<name>
```

V1 rules:

- Exactly three dotted segments.
- `kind` is `preflight` or `behavior`.
- Domains are closed: `skill`, `template`, `review`, `todo`, `loop`, `inbox`,
  `tune`, `agent`, `gate`.
- Segment names are lowercase ASCII with digits and hyphens, no leading/trailing
  or consecutive hyphens.
- `loom-case.checker` must name a `behavior.*` checker. `preflight.*` validators
  run automatically and are not valid declared cases.
- The full id must be registered in Loom's internal machine-readable checker
  registry and have active status.

Checker ids are compatibility surface. Released ids are stable. Retired ids stay
registered with replacement/migration guidance and are hard errors in cases.

### Targets

`targets` is always a non-empty array. There is no `target = "..."` shorthand.
V1 allows only explicit concrete targets:

```text
skill:<skill-name>
phase:<phase-name>
partial:<partial-name>
```

Wildcards (`skill:*`, `phase:*`, `partial:*`) and `all` are invalid in
`loom-case` blocks.

Repo-wide `docs/tuning.md` may target any known skill, phase, or partial. Unknown
targets are hard errors. Known but inactive targets are valid and skipped for a
particular run. A case is selected when its targets are known/applicable and at
least one target intersects the current tune target set.

Package `tuning.md` cases must include the owning `skill:<name>` target. They may
also include additional targets such as `phase:review`.

### Paths

Relative paths inside a case resolve relative to the markdown file containing the
block. `..` is allowed only when the normalized/canonical result remains inside
the repo root.

Every referenced path must:

- stay inside the repo root;
- not resolve under `.loom/`;
- exist and be readable;
- be git-tracked when it is a file;
- contain only git-tracked files used by the checker when it is a directory;
- not escape through symlinks.

Absolute paths are invalid in consumer tuning docs.

## Checker Registry

The checker registry is internal and machine-readable. `loom-skills` does not
expose it. In the target v1 layout, the authoritative registry is typed Rust
metadata in the internal `loom-tune` crate; docs describe the shape, but
generated/snapshot metadata from the crate is the source of truth. Conceptual
metadata for each checker includes:

```toml
[[checker]]
id = "behavior.review.finding-recall"
title = "Review finding recall"
summary = "Runs review on a known diff and scores expected findings."
status = "active"
target_kinds = ["skill", "phase"]
levels = ["run", "full"]
cost = "agent-replay"
mandatory = false
case_roles = ["regression"]
implementation = "review_finding_recall"
soft_regression_epsilon = 0.01
input_schema = "review_finding_recall_input_v1"
expected_schema = "review_finding_recall_expected_v1"
```

The planner reads the registry to validate cases, select checkers, enforce
levels/caps, disable policy, and print `loom tune checker` output. Mandatory
preflight validators cannot be disabled. A `loom-case` naming a disabled
behavioral checker is a hard error. Consumer repos can write `loom-case` blocks,
but cannot define checker implementations or metadata in v1.

`loom tune checker` prints a human table by default with id, status, target
kinds, levels, cost, mandatory/disableable policy, and one-line summary. It does
not expose internal implementation keys unless a future global debug/JSON output
contract requires it.

### Preflight Validators

Preflight validators are legality checks that run automatically by applicability.
They are not valid `loom-case.checker` values. V1 starts with coarse stable ids:

| Checker | Purpose |
|---------|---------|
| `preflight.skill.registry` | Skill parse/frontmatter/name/duplicate/override registry legality. |
| `preflight.skill.materialization` | Safe materialization paths and backend disclosure/registration inputs. |
| `preflight.skill.protocol-boundary` | Skill content cannot weaken compiled phase protocol, terminal markers, gate rules, or safety contracts. |
| `preflight.template.compile` | Candidate phase/partial templates compile against typed Askama contexts. |
| `preflight.template.conformance` | Include graph, marker ownership, options/findings wire-format, and surface-reference walkers pass. |
| `preflight.tune.case-validation` | Loaded tuning docs and `loom-case` blocks parse, validate, and reference known targets legally. |

## Behavioral Checker Set

Initial v1 behavioral checker families use strict checker-specific TOML structs.
Schemas are defined one checker at a time in code and kept minimal; v1 does not
allow repo-authored checker implementations, fixture scripts, or arbitrary
command DSLs.

### `behavior.review.finding-recall`

Runs the review phase against a known patch and scores whether expected
`LOOM_FINDING` predicates are present.

Common case fields:

```toml
[input]
patch = "tuning/cases/review.diff"

[[expected.findings]]
contains = ["missing test"]
file = "src/lib.rs"

[expected]
max_extra_findings = 5
```

Hard pass requires parseable review output, a valid review terminal marker, and
all expected findings matched. Soft score is the fraction of expected finding
predicates satisfied, with penalties for malformed output or excessive extras.

### `behavior.todo.decomposition`

Runs todo decomposition for a known request and scores parseable, scoped
`LOOM_TODO` output.

```toml
[input]
prompt = "tuning/cases/cross-spec-change.md"

[expected]
min_items = 2
max_items = 5
required_specs = ["skills", "harness"]
forbidden_specs = ["llm"]
```

Hard pass requires exactly one parseable `LOOM_TODO`, no generic success marker,
valid spec labels, required spec coverage, forbidden spec absence, and item count
inside bounds.

### `behavior.loop.verify-after-edit`

Runs a loop fixture and verifies the agent actually ran a relevant verifier after
the final relevant edit.

```toml
[input]
fixture = "tuning/cases/rust-failing-test"
task = "Fix the failing unit test by correcting the implementation."

[expected]
edited_paths = ["src/lib.rs"]
verify_commands = ["cargo test"]
marker = "LOOM_COMPLETE"
```

The checker inspects command/edit traces, not final-answer claims.

### `behavior.loop.scope-discipline`

Runs a trap fixture and scores that the requested task was solved without
opportunistic unrelated edits.

```toml
[input]
fixture = "tuning/cases/unrelated-cleanup-trap"
task = "Fix the parser panic."

[expected]
allowed_edit_paths = ["src/parser.rs"]
forbidden_edit_paths = ["src/format.rs", "docs/**"]
max_changed_files = 1
```

### `behavior.inbox.resolution-path`

Runs an inbox fixture and scores that chat, not removed host-side mutation
commands, resolves the human-decision item.

```toml
[input]
fixture = "tuning/cases/inbox-clarify"
user_response = "Choose the smaller scoped fix."

[expected]
forbidden_commands = [
  "loom inbox reply",
  "loom inbox pick",
  "loom inbox resolve",
  "loom inbox apply",
]
allowed_terminal_markers = ["LOOM_COMPLETE", "LOOM_APPLY"]
must_update_beads = true
must_not_push = true
```

### `behavior.tune.apply-handoff`

Runs an accepted tune-proposal fixture and scores a valid `LOOM_APPLY` handoff to
the trusted driver without chat-side push or `.loom/integration` mutation.

```toml
[input]
fixture = "tuning/cases/accepted-tune-proposal"
user_response = "Accept this proposal."

[expected]
apply_proposals = ["lm-fixture-1"]
must_emit_apply = true
must_not_push = true
must_not_dirty_integration = true
```

### `behavior.agent.context-before-edit`

Runs a fixture and verifies required context files were read before the first
relevant edit.

```toml
[input]
fixture = "tuning/cases/style-rule-edit"
task = "Update the code to follow the repository style rule for error handling."

[expected]
must_read_before_edit = [
  "docs/style-rules.md",
  "src/lib.rs",
]
edited_paths = ["src/lib.rs"]
```

If a backend cannot expose read/edit ordering, this checker is not applicable for
that backend and must be reported unavailable rather than faked.

### Fixture Layout

Behavioral cases that need a workspace use tracked, self-contained fixture
directories:

```text
fixture/
  repo/          # files copied into the isolated checker checkout
  state.toml     # optional bead/inbox/tune setup state
  input.md       # optional user/task text
```

Checker implementations own execution. Fixture files are evidence inputs, not
programs to run.

## Levels, Planning, and Acceptance

Levels are explicit proposal subcommands:

```text
fast  preflight + tuning doc/case validation only; no behavioral rollouts
run   bounded behavioral validation
full  all applicable declared regressions, then broader mined selection evidence
```

`loom tune ... --dry-run` prints loaded tuning docs, evidence roots, seed, case
pool, selected/skipped cases, and the frozen checker plan, then creates nothing.
`--seed <n>` pins deterministic sampling. Without `--seed`, Loom generates and
records a seed.

Planner determinism inputs:

- tune target set;
- requested level;
- checker registry;
- config caps;
- loaded tuning docs/cases;
- evidence snapshot;
- seed.

`run` treats `[tune.checks].max_behavior_cases` as a hard cap. Declared
regression cases have priority but may be sampled/skipped. `full` runs all
applicable declared regression cases before sampling mined cases. Skipped
declared regressions are reported with guidance to use `full` or increase caps.
Only selected cases can block a proposal.

Per selected case, Loom records one outcome category:

```text
improved
regressed
persistent-fail
stable-success
```

Default regression epsilon is `0.01` for soft-score comparisons. A candidate
regresses a case when `candidate.hard < current.hard`, or when hard is equal and
`candidate.soft < current.soft - epsilon`. Improvement mirrors that relation.
V1 aggregates use equal case weights.

Acceptance policy:

- Preflight failure blocks staging.
- Regression on a selected declared regression case blocks the tune bead.
- Worse aggregate score on selected mined selection evidence blocks the tune
  bead.
- Mixed mined evidence with no aggregate regression remains pending but is
  prominently flagged.
- Human review through `loom inbox` is still required for every adoption.

## Evidence Mining and Splits

V1 mines Loom-owned evidence first: JSONL events, gate/review outputs, bead
state, git diffs, criterion evidence, workspace-contained transcripts, and loaded
tuning docs. External transcripts require explicit `[tune.evidence].external_roots`
configuration and are printed before harvesting. Evidence is redacted before it is
persisted in proposal artifacts.

Mined evidence has two stable splits in v1:

```text
train
selection
```

`[tune.evidence].selection_fraction` defaults to `0.34` and must satisfy
`0.0 < selection_fraction < 1.0`.

Split membership uses SHA-256 over
`repo_or_workspace_salt || evidence_item_id`, maps the digest to `[0,1)`, and
assigns the item to `selection` when the value is less than
`[tune.evidence].selection_fraction`; all other items are `train`. The salt is an
opaque stable repository/workspace identity owned by the mining algorithm.
Reports record only the salt id; local manifests may record workspace/cache paths
separately for resume/debug, but never as salt material. Split membership does
not depend on the tune-run seed. Reports/manifests also record the split
algorithm version and selection fraction. Train evidence may be shown to
candidate generation. Selection evidence is withheld and used for checking/gating.
V1 has no mined `test` split.

Declared `loom-case` cases are tracked regression cases. They are not secret
selection evidence.

## Tune Proposal Records

The tune bead is the canonical durable record. Local `.loom/tune/<id>/`
artifacts (`repo/`, `manifest.json`, `evidence.md`, logs, and evidence cache) are
resume/debug material. Bead metadata records `loom.tune.id`, state, targets,
level, seed, base commit, proposal branch/head, plan hash, case counts, outcome
counts, and any apply-failure payload. If local artifacts are missing but the
proposal branch/head still exists, Loom may regenerate them; if the branch or
commits are missing/unreachable, the item remains in the inbox as a blocked tune
item rather than being skipped.

Accepted tune proposals are applied as an all-or-nothing batch by the trusted
driver, not by chat. `cherry_pick_conflict`, `verify_failed`, `review_failed`, or
`push_failed` aborts the batch, pushes nothing, and attaches shared diagnostics
to every proposal in the batch.

## CLI Summary

`loom tune` with no subcommand prints help and creates no proposal. Listing
commands are read-only:

```sh
loom tune skill
loom tune phase
loom tune partial
loom tune checker
loom tune all
```

Proposal creation requires an explicit level:

```sh
loom tune skill fast|run|full [<skill-name>...]
loom tune phase fast|run|full [<phase-name>...]
loom tune partial fast|run|full [<partial-name>...]
loom tune all fast|run|full
```

No plural aliases and no `template` umbrella command exist in v1.
