# Loom Skills

Dynamic agent skill documents, built-in skill bundles, SkillOpt-style tuning,
and the human review surface for adopting tuned skill/template proposals.

## Problem Statement

Loom needs reusable agent strategy without turning every consumer-specific
preference into a compiled workflow template. Templates define phase protocol,
safety rules, required markers, and gate rubrics. Skills define dynamic,
reusable strategy that consumers can author, version, register with agent
backends, and tune from observed outcomes.

The SkillOpt discipline applies to both skills and templates: harvest evidence,
mine checkable tasks, replay/evaluate, reflect, make bounded edits, gate against
held-out evidence, and stage proposals for human review. The adoption paths
differ: skills are dynamic guidance; workflow templates are protocol-bearing
source and cannot become runtime overrides.

## Architecture

### Templates vs. Skills

Templates are static, compiled workflow protocol. They define the phase
objective, required terminal markers, permitted state mutations, and gate
rubrics. A skill must not redefine those protocol rules or override the compiled
prompt's safety contract; it can only add strategy guidance that helps an agent
satisfy the phase.

Skills are dynamic Markdown artifacts. They are discovered, registered, and
progressively disclosed at runtime. A skill can describe a workflow, heuristics,
examples, scripts, references, or project conventions. Skills may be tuned and
adopted without recompiling Loom.

Template tuning is supported only as source-change proposal work. `loom tune`
may propose edits to phase templates or partials in an isolated proposal
worktree. Those candidates must pass template validation before they enter the
human review queue. They are never hot-loaded as runtime overrides of Loom's
compiled workflow templates.

### Public Crate and Type Pipeline

`loom-skills` is a public-contract crate. It owns the skill artifact model and
registry surface that downstream consumers can reuse. The SkillOpt-style tuning
engine is internal in v1 and belongs to the internal `loom-tune` crate in the
target v1 layout; it can become a separate public surface only after the
evidence, task, replay, gate, and proposal schemas stabilize.

The public crate follows parse-don't-validate. Raw strings and paths become typed
stage values at the boundary, and downstream APIs accept only the stage they need:

```text
RawSkillPath
  -> SkillDocument          # Markdown/frontmatter parsed
  -> NamedSkill             # typed name + description present
  -> SkillSet               # discovered/configured candidates loaded
  -> SkillRegistry          # duplicates and overrides resolved
  -> ApplicableRegistry     # phase/profile filters applied
  -> MaterializedRegistry   # built-ins copied to readable session paths
  -> RegisteredSkills       # native registration / prompt disclosure ready
```

Examples of public types:

- `SkillName`, `SkillDescription`, `ProfileName`, and phase/filter newtypes.
- `SkillFrontmatter` for Agent Skills metadata Loom interprets.
- `SkillDocument`, `NamedSkill`, `SkillSource`, `SkillRegistry`,
  `ApplicableRegistry`, and `MaterializedRegistry` stage types.
- Discovery from workspace files, configured paths, built-in bundles, and
  override roots.
- Typed diagnostics and parse/resolution errors.

A function that needs materialized paths accepts `MaterializedRegistry`, not raw
files. A backend registration function cannot accept unparsed Markdown or an
unresolved collection.

### Skill Artifact Model

Loom follows the Agent Skills directory-package convention by default: one skill
is a directory containing a `skill.md` package document. Package document
matching is case-insensitive (`skill.md`, `SKILL.md`, `Skill.md`, etc.), but
Loom-generated files use lowercase `skill.md`. A directory containing multiple
case variants of `skill.md` is invalid because it is ambiguous on case-sensitive
filesystems and broken on case-insensitive filesystems.

The containing directory is the skill's base directory; relative references and
helper files resolve from there. A package skill may also contain an optional
`tuning.md` document, matched case-insensitively and generated in lowercase. The
same duplicate-case rule applies to `tuning.md` inside a package. Package
`tuning.md` is loaded only for applicable/tuned package skills and is
specified in [docs/tuning.md](../docs/tuning.md). Loose single-file skills do not
have adjacent tuning documents in v1.

Skill Markdown may carry YAML frontmatter. Every registered skill requires
frontmatter `name` and `description`. Loom does not infer either value from a
filename or heading, because `name` is durable identity and `description` is the
routing signal used by progressive disclosure.

```yaml
---
name: rust-review
description: Use when reviewing Rust implementation changes for Loom style and verifier honesty.
metadata:
  loom:
    phases: ["loop", "review"]
    profiles: ["rust"]
---
```

`SkillName` follows Agent Skills / Pi compatibility:

- 1-64 characters.
- Lowercase ASCII letters, digits, and hyphens only.
- No leading or trailing hyphen.
- No consecutive hyphens.

`SkillDescription` is limited to 1024 characters.

Unknown frontmatter fields remain valid. Loom reads only the fields it owns and
preserves compatibility with other agents.

### Discovery and Diagnostics

Auto-discovery walks git-tracked files under the workspace and discovers only
standard package files whose basename matches `skill.md` case-insensitively.
Loom does not auto-discover `*_skill.md`, arbitrary loose Markdown files, or a
separate loose skill root in v1. Multiple case variants of `skill.md` in the
same directory are a hard error.

Auto-discovered invalid skill candidates are warnings and are skipped. Explicitly
configured invalid paths are errors. Duplicate skill names are errors. Invalid
built-in skills are fatal release-contract errors.

Configured skill paths are explicit rather than glob-based:

```toml
[skills]
paths = [
  "docs/review-skill.md",
  "team/agent-skills/",
]
```

A configured file path loads exactly that file as a loose-file skill regardless
of basename. A configured directory path is a loose skill collection: Loom
recursively loads Markdown files under it, each file as one skill. Explicit
configured paths may point at untracked local files; this is the user's opt-in.
Standard package skills found through auto-discovery are deduplicated by
canonical path if a configured directory also contains them.

There is no `loom skills init` command in v1. Diagnostics tell the user or agent
which required fields are missing or malformed.

### Built-in Skills and Overrides

Loom ships built-in skills as Agent Skills packages. Built-ins are bundled with
the Loom release and are read-only from a consumer workspace. Built-in skill
names use the `loom-` prefix to reduce collisions with consumer-defined skills.

Built-ins are profile-scoped:

- `base` built-in skills register for every profile.
- `rust` built-in skills register when the resolved profile is `rust`.
- Future profile bundles (`python`, etc.) follow the same rule.

The v1 built-in catalog is intentionally moderate: broad enough to cover Loom's
actual workflow, but small enough to tune and check with behavioral evidence.
Built-in source packages use lowercase `skill.md`; backend adapters may transform
materialized content into a runtime-specific registration format when a tested
native registrar requires it.

| Bundle | Skill | Primary phases | Purpose |
|--------|-------|----------------|---------|
| `base` | `loom-context-before-edit` | `loop`, `review`, `tune` | Read relevant specs, style rules, and source before editing; keep context available for reports/review. |
| `base` | `loom-workspace-discipline` | `loop`, `inbox`, `tune` | Respect operator checkout, bead clone, tune proposal checkout, and integration checkout boundaries. |
| `base` | `loom-scope-discipline` | `todo`, `loop`, `review`, `tune` | Avoid unrelated edits, protect user changes, and keep diffs reviewable. |
| `base` | `loom-todo-decomposition` | `todo` | Produce small, testable, dependency-aware work beads from specs/issues. |
| `base` | `loom-verify-after-edit` | `loop`, `tune` | Run relevant verification after edits and report skipped/failed checks honestly. |
| `base` | `loom-review-finding-recall` | `review`, `gate` | Systematically check diffs against spec/style/test expectations and avoid dropping findings. |
| `base` | `loom-inbox-resolution` | `inbox` | Resolve clarify/blocked/infra/tune items through chat rather than host-side mutation menus. |
| `base` | `loom-tune-proposal-handoff` | `inbox`, `tune` | Treat tune proposals as review artifacts; authorize apply via `LOOM_APPLY`, never chat-side push. |
| `base` | `loom-final-reporting` | `loop`, `gate`, `inbox` | End with concise changed-files, verifier, risk, and status summaries. |
| `rust` | `loom-rust-change-planning` | `todo`, `loop` | Plan Rust changes around module/API boundaries, ownership, and tests. |
| `rust` | `loom-rust-verification` | `loop`, `gate`, `tune` | Prefer `nix fmt`, `cargo build`, `cargo nextest run`, and `nix flake check` as appropriate. |
| `rust` | `loom-rust-review` | `review`, `gate` | Review Rust diffs for correctness, error handling, async/process behavior, and public API drift. |
| `rust` | `loom-rust-style-rules` | `loop`, `review` | Apply repo `docs/style-rules.md`, rustfmt, clippy, naming, and layout expectations. |

Behavioral checker/tuning support is prioritized in this order:
`loom-context-before-edit`, `loom-scope-discipline`, `loom-verify-after-edit`,
`loom-inbox-resolution`, then `loom-tune-proposal-handoff`. Other built-ins may
ship before they have dedicated behavioral regressions.

At session start, built-in skills selected for the resolved profile are
materialized under the per-session scratch directory using the package shape:

```text
.loom/scratch/<key>/skills/<skill-name>/skill.md
```

The registry records source/provenance separately; source is not encoded in the
materialized path.

Consumers tune built-ins by fork-on-tune. A consumer workspace cannot mutate the
embedded built-in. Tuning a built-in outside the Loom repository creates or
updates a tracked override under:

```text
.loom-override/skills/<skill-name>.md
.loom-override/skills/<directory>/skill.md
```

Both loose Markdown files and recursive package skills whose document basename
matches `skill.md` case-insensitively are auto-discovered under
`.loom-override/skills/`. Frontmatter `name` is the actual
identity; Loom does not require the name to match the parent directory or file
stem. An override is valid only if its `name` matches a known Loom built-in. The
override root overrides built-ins only, not repo/configured skills. Duplicate
overrides for the same built-in fail fast.

A repo/configured skill outside `.loom-override/skills/` with the same `name` as
a built-in is a duplicate-name error. A repo/configured skill with the same
`name` as another repo/configured skill is also a duplicate-name error.

Tune-generated overrides may include optional audit metadata, but metadata is
not required to declare override intent:

```yaml
metadata:
  loom:
    source_hash: "<bundled-skill-hash>"
    source_version: "<loom-version>"
```

When tuning the Loom repository itself, built-in skill tuning edits the source
built-in skill files in the proposal worktree instead of creating an override.

### Phase and Profile Filters

Skills are applicable to all phases and profiles by default. A skill may narrow
itself through `metadata.loom.phases` and/or `metadata.loom.profiles`:

```yaml
metadata:
  loom:
    phases: ["plan", "loop"]
    profiles: ["rust"]
```

Built-in skills use the same filter model internally. A phase running under
`profile = "rust"` receives applicable `base` built-ins, applicable `rust`
built-ins, repo/configured skills whose filters match, and overrides whose
filters match.

### Registration and Progressive Disclosure

Loom builds one effective skill registry per agent-bearing session. The compiled
phase prompt receives a compact skill index. Full skill bodies are not pinned
into the prompt by default.

Skill disclosure derives from the resolved per-phase backend (`agent.backend`)
and `[skills]` policy. There is no separate skills "mode" flag that duplicates
the backend choice.

```toml
[skills]
registration = "auto"  # auto | prompt
show_paths = "needed"  # needed | always
```

`registration = "auto"` is the default. If the resolved backend supports native
skill registration, Loom registers the effective registry natively. A backend is
native-capable only when Loom ships a concrete, tested registrar for that
backend/runtime version; Loom does not infer support from the product name.
Native registration failure is fatal once a backend declares native capability.
If the backend has no Loom-implemented registrar, Loom uses prompt disclosure.

`registration = "prompt"` disables native skill registration globally and uses
prompt disclosure even for native-capable backends.

`show_paths = "needed"` is the default. Paths appear in the prompt only when the
prompt is the loading mechanism. `show_paths = "always"` includes paths even
after native registration succeeds, for debugging and audit.

Prompt contents depend on disclosure mode:

- Native-registered mode lists `name` and `description`. It instructs the agent
  to use its native skill mechanism when a skill is relevant. Paths are omitted
  unless `show_paths = "always"`.
- Prompt-disclosure mode lists `name`, `description`, and readable `path`. It
  instructs the agent to read the listed path when a skill is relevant.

Source (`builtin`, `override`, `repo`, `configured`), source hashes,
phase/profile filters, native registration status, and override provenance are
recorded in logs/manifests, not in the normal skill index.

### SkillOpt-Style Tuning Loop

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

`docs/tuning.md` is Loom's repo-wide tuning document. In the Loom repository it
also documents the tuning system itself; the normative owner remains this spec
(`spec:skills`). Consumer repositories may commit their own `docs/tuning.md` as
repo-wide tuning guidance.

Package-form skills may also include an optional git-tracked `tuning.md` next to
`skill.md`. Both basenames are matched case-insensitively and generated in
lowercase; duplicate case variants of either basename in one package directory
are hard errors. A package `tuning.md` is loaded only when the owning package
skill is applicable and in the tune target set. Every case in a package
`tuning.md` must
include the owning `skill:<name>` target. Loose single-file skills have no
adjacent tuning document in v1.

Tuning markdown prose is optimizer context for all levels (`fast`, `run`,
`full`). Fenced `loom-case` blocks are removed from prose context and parsed as
strict TOML. `loom-case` syntax, path rules, target selectors, and case id rules
are specified in [docs/tuning.md](../docs/tuning.md). Loaded cases are parsed and
validated at every level, including `fast` and `--dry-run`.

### Checker Portfolio

Tune validation uses an internal machine-readable checker registry rather than
ad-hoc checks invented after a candidate diff exists. The registry is not part of
the public `loom-skills` crate. In v1 the authoritative registry is typed Rust
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
programs to run.

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

Only optional checkers can be disabled. Disabling a mandatory preflight validator
is a configuration error. A loaded `loom-case` that names a disabled behavioral
checker is also a hard error; Loom does not silently skip explicit regressions.
`selection_fraction` defaults to `0.34` and must satisfy
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
sampling within stable pools, not mined train/selection split membership. `loom
tune ... --dry-run` prints loaded tuning docs, evidence roots, seed, candidate
case pool, selected/skipped cases, and the frozen checker plan, then exits before
candidate generation.

If targets are invalid during preflight, `loom tune` fails without creating a
bead. If targets are valid but planning/generation later determines the scope is
too broad, incompatible, or cannot fit configured budgets, the run creates a
blocked tune bead that explains the problem and suggests narrower commands. V1
has no `max_targets` / `max_files` knobs; checker budgets and coherence determine
refusal.

### Evidence Roots and Splits

By default, tuning sees only the current workspace (`/workspace` in Loom-managed
containers). V1 mines Loom-owned evidence first: JSONL events under `.loom/logs/`,
gate/review outputs, bead state, git diffs, criterion evidence, review findings,
workspace-contained agent transcripts, and loaded tuning docs. Evidence is
redacted before persistence in proposal artifacts.

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
checking/gating. There is no mined `test` split in v1. Declared `loom-case` cases
are tracked regression cases, not hidden selection evidence.

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
`candidate.soft < current.soft - epsilon`; improvement mirrors that relation.
V1 aggregate scores use equal case weights.

### Tune Proposal Worktrees and Beads

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
target kind/names/files, git base/branch/head/commit ids, tune level/seed/
plan hash/plan/results/caps, and local paths. The bead and proposal branch are
canonical; if manifest and bead disagree, the tune item blocks for review.
`loom inbox view -p <id>` must still work from the tune bead body and local
proposal repo when `evidence.md` is absent. If `.loom/tune/<id>/` is missing or
corrupt but bead metadata and the proposal branch/head still exist, Loom may
regenerate local manifest/evidence artifacts on demand. If the proposal branch or
identified commits are missing/unreachable, the tune item remains `kind = tune`
but moves to blocked state for chat review with repair/drop options. Corrupt tune
items are never silently skipped.

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

`loom tune` with no subcommand prints command help and exits without tuning.
Listing commands are read-only and do not create beads:

| Command | Meaning |
|---------|---------|
| `loom tune skill` | List tuneable skills. |
| `loom tune phase` | List tuneable phase templates. |
| `loom tune partial` | List tuneable partials. |
| `loom tune checker` | List registered tuning checkers with id, status, target kinds, levels, cost, mandatory/disableable policy, and summaries. |
| `loom tune all` | List all tuneable surfaces and counts. |

Proposal creation requires an explicit level:

| Command | Meaning |
|---------|---------|
| `loom tune skill fast|run|full [<skill-name>...]` | Tune all applicable skills when no names are supplied, or the named skills when supplied. |
| `loom tune phase fast|run|full [<phase-name>...]` | Tune all phase templates when no names are supplied, or named phase templates such as `plan`, `todo`, `loop`, `review`, `inbox`. |
| `loom tune partial fast|run|full [<partial-name>...]` | Tune all partials when no names are supplied, or named partials such as `review_rubric`. |
| `loom tune all fast|run|full` | Tune skills, phase templates, and partials in one proposal. Target names are not accepted after `all`. |

There are no plural aliases (`skills`, `phases`, `partials`) and no `template`
umbrella command in v1. Template target names use phase names and partial
filenames without `.md`. The former `msg` phase is renamed to `inbox`.

Each proposal-creating invocation creates one proposal bead. Mixed surfaces are
allowed only through `loom tune all fast|run|full` in v1. Proposal branches may
contain one or more commits; one commit total is the default expectation unless
the tuning agent has a strong reason to split. A proposal command with no target
names tunes every target on that surface; if the requested scope is too broad for
the checker budget or cannot form one coherent proposal, Loom blocks the tune
bead with split guidance rather than silently creating multiple beads.

Common tune flags for proposal-creating commands:

| Flag | Meaning |
|------|---------|
| `--dry-run` | Print loaded tuning docs, evidence roots, seed, case pool, selected/skipped cases, and frozen checker plan; create no candidate. Invalid on list commands. |
| `--seed <n>` | Use a deterministic checker-plan seed; generated and recorded when absent. |

### Human Review Through Inbox

The inbox command modes, addressing, filters, queue ordering, interactive
resolution authority, terminal markers, and trusted apply batch are defined
once in [harness.md § Inbox Modes](harness.md#inbox-modes). This spec does not
restate that shared workflow contract.

Tuning contributes tune-kind proposal records to that authoritative inbox flow.
Each record and local envelope must satisfy
[Tune Proposal Worktrees and Beads](#tune-proposal-worktrees-and-beads), so the
inbox can review the candidate and hand any authorized adoption to the trusted
driver without making tuning a second resolution authority.

## Success Criteria

- Skill parsing follows parse-don't-validate staging: raw Markdown cannot be
  registered until it has become a `NamedSkill`, unresolved collections cannot be
  registered, and backend registration accepts only materialized/applicable
  registry types
  [test](skill_registry_typestate_prevents_misuse)
- Skill discovery finds git-tracked package documents whose basename matches
  `skill.md` case-insensitively, explicit loose-file skills, recursive
  configured-directory Markdown skills, and `.loom-override/skills/`
  loose/package overrides while rejecting duplicate names except valid built-in
  overrides and rejecting duplicate `skill.md` / `tuning.md` basename case
  variants in one package directory
  [test](skill_registry_discovery_and_duplicate_policy)
- Missing or malformed frontmatter is a warning+skip for auto-discovered repo
  skills, an error for explicit configured paths or override candidates, and a
  fatal release-contract error for built-ins
  [test](skill_frontmatter_diagnostics_by_source)
- The v1 built-in catalog contains the accepted `base` and `rust` `loom-*`
  skills, is selected per profile, materialized under
  `.loom/scratch/<key>/skills/<name>/skill.md`, and can be shadowed only by
  `.loom-override/skills/` entries whose frontmatter `name` matches a known
  built-in
  [test](builtin_skill_profile_selection_and_override_policy)
- Optional `metadata.loom.phases` / `metadata.loom.profiles` filters default to
  all phases/profiles and narrow registration only when present
  [test](skill_frontmatter_phase_profile_filters)
- `registration = "auto"` natively registers skills for native-capable backends
  and fails on registration failure; `registration = "prompt"` disables native
  registration globally
  [test](skill_registration_policy_auto_and_prompt)
- The skill-index prompt partial renders name/description only for native mode,
  adds paths for prompt-disclosure mode, and adds paths to native mode only when
  `show_paths = "always"`
  [test](skill_prompt_index_disclosure_modes)
- `loom tune` with no subcommand prints help; `loom tune skill`, `phase`,
  `partial`, `checker`, and `all` list surfaces/checkers; proposal creation
  requires explicit `fast`, `run`, or `full` after `skill`/`phase`/`partial`/`all`;
  `--dry-run` and `--seed` apply only to proposal-creating commands
  [test](loom_tune_cli_surface)
- Each tuning invocation creates one tune bead plus one isolated
  `.loom/tune/<bead-id>/` envelope with `repo/`, `manifest.json`,
  `evidence.md`, candidate commit(s), and no changes to the invoking checkout
  [test](loom_tune_subcommands_create_isolated_proposals)
- Tune checker planning freezes a deterministic registered-checker plan from the
  internal typed `loom-tune` registry before candidate generation, records the
  level/seed/case-pool/selected/skipped plan/results in the bead/manifest, and
  rejects post-candidate checker changes as validation evidence
  [test](tune_checker_plan_freeze_contract)
- Skill tuning reads workspace evidence by default, reads explicit
  `[tune.evidence].external_roots` only when configured, loads `docs/tuning.md`
  plus applicable package `tuning.md` files, validates `loom-case` blocks, prints
  evidence roots before harvesting, and gates candidate edits with selected
  behavioral cases before inbox exposure
  [test](skill_tune_evidence_roots_and_gate)
- Phase and partial tuning validates candidate templates in the proposal worktree
  by compiling Askama templates, rendering representative snapshots, and running
  template conformance walkers before inbox exposure
  [test](template_tune_candidate_validation)
- Pending and blocked tune proposal records enter the authoritative
  [harness.md § Inbox Modes](harness.md#inbox-modes) flow as tune-kind items,
  and authorized adoption is performed only by that flow's trusted apply
  handoff
  [test](inbox_apply_marker_triggers_single_driver_handoff)
- Skills remain additive strategy guidance and cannot override compiled phase
  protocol, terminal markers, state-mutation authority, or gate discipline
  [judge](../tests/judges/loom.sh#skills_template_boundary_review)

## Requirements

### Functional

1. **Public skill registry.** Loom exposes skill parsing, discovery,
   resolution, filtering, and materialization through a public `loom-skills`
   crate. Consumers can use the same registry model outside the Loom binary.
2. **Internal tuning engine.** The SkillOpt-style tuning engine remains internal
   in v1, with registry/case/evidence/scoring/metadata types housed in the
   internal `loom-tune` crate. Public tuning APIs are out of scope until the
   evidence, task, replay, gate, and proposal types stabilize.
3. **Standard package discovery.** Auto-discovery walks git-tracked workspace
   files for package documents whose basename matches `skill.md`
   case-insensitively at any depth. Each containing directory is one skill
   package; generated package documents use lowercase `skill.md`, and multiple
   case variants in one directory are hard errors. Package-local `tuning.md`
   documents follow the same case-insensitive/lowercase-generated duplicate
   policy.
4. **Explicit loose skill paths.** `[skills].paths` lists non-standard files or
   directories. Files load as single loose-file skills; directories recurse
   through Markdown files and load each as one loose-file skill. No wildcard or
   glob syntax exists in v1.
5. **Frontmatter identity.** Registered skills require `name` and `description`.
   Loom never infers either field. Duplicate `name` values fail fast except for
   valid built-in overrides from `.loom-override/skills/`.
6. **Built-in bundles.** Loom ships the accepted v1 built-in skill catalog by
   profile. `base` built-ins are always eligible; profile-specific built-ins are
   eligible only when the resolved profile matches. Built-in names use the
   `loom-` prefix.
7. **Built-in override root.** Consumers override tuned built-ins by committing
   loose Markdown files or packages under `.loom-override/skills/`. The
   frontmatter `name` must match a known built-in. Overrides never shadow
   repo/configured skills.
8. **Progressive disclosure.** Phase prompts include only a compact skill index;
   agents load full skill bodies on demand. Backends with native skill support
   receive native registration in `registration = "auto"`; prompt disclosure is
   used for Direct/no-native backends or `registration = "prompt"`.
9. **Manual tuning.** `loom tune` with no subcommand prints help and never starts
   tuning. `loom tune skill` / `phase` / `partial` / `checker` / `all` are
   listing commands. Tuning starts only when `fast`, `run`, or `full` follows
   `skill`, `phase`, `partial`, or `all`. Omitted names tune every target on
   that surface.
10. **Checker portfolio.** Tuning uses Loom-registered internal checkers in
    `fast`, `run`, or `full` levels. Repo config may set budgets and disable
    optional checker ids, but mandatory preflight validators remain enabled; v1
    has no arbitrary tune-specific checker commands and no public checker
    registry API.
11. **Proposal bead and isolation.** Tuning creates one tune bead and one local
    `.loom/tune/<bead-id>/` envelope per invocation. Proposal commits live in
    `repo/` on branch `loom/tune/<bead-id>` and never modify the invoking
    checkout or push automatically.
12. **Template validation.** Phase and partial template proposals must validate
    in their proposal worktree before entering `loom inbox` as pending.
13. **Inbox ownership.** Tuning emits tune-kind proposal records; the command
    surface and resolution authority are owned exclusively by
    [harness.md § Inbox Modes](harness.md#inbox-modes).
14. **Tune apply handoff.** Tune adoption follows the trusted apply contract in
    [harness.md § Inbox Modes](harness.md#inbox-modes); tuning does not define a
    second apply path.
15. **Workspace-first evidence.** Tuning evidence defaults to the workspace;
    external transcript roots require `[tune.evidence].external_roots` and are
    printed before use. Mined evidence is stably split into `train` and
    `selection` using `[tune.evidence].selection_fraction`.

### Non-Functional

1. **Privacy.** Loom never implicitly reads home-directory transcript stores.
   Evidence roots outside the workspace are explicit configuration.
2. **Safety.** Skill tuning cannot weaken compiled phase protocol. Template
   tuning cannot bypass source review and validation by becoming a runtime
   override. Native registration failure is fatal when native registration was
   selected.
3. **Prompt budget.** Skills use progressive disclosure; full bodies are read on
   demand, not pinned into every phase prompt.
4. **Portability.** Repo skills and built-in overrides use Agent Skills-compatible
   names/frontmatter. Directory packages remain available for assets and helper
   files; loose files are allowed only where explicitly configured or under the
   override root.
5. **SemVer.** Removing or renaming public `loom-skills` types or fields is a
   major version change; adding new optional metadata or diagnostics is minor.

## Out of Scope

- Automatic background tuning, scheduled tuning, auto-adoption, and `auto_adopt`
  config.
- Implicit harvesting of `~/.claude`, `~/.codex`, or any other path outside the
  workspace.
- Auto-discovery of `*_skill.md` or arbitrary Markdown files outside configured
  paths and `.loom-override/skills/`.
- Runtime replacement of Loom's compiled workflow templates.
- A standalone `loom inbox apply` command.
- A `loom skills init` scaffolding command.
- Public tuning-engine APIs in v1.
