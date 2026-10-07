# Agent skills

Discovers and resolves capability artifacts into a filtered, progressively
disclosed skill registry.

## Problem Statement

Agents need relevant capabilities without loading every skill or resolving
conflicting definitions themselves. The registry selects, validates, and
progressively discloses skills for the current profile and phase.

## Architecture

Discovery and override resolution produce a typed registry for materialization
and backend disclosure. Related owners: [templates](templates.md),
[agent](agent.md), [tuning](tuning.md).

Acceptance: [criteria and verifier bindings](#success-criteria).

### Templates vs. Skills

[Acceptance](#success-criteria).

Phase owners define workflow protocol; [Templates](templates.md) composes and
delivers that policy. A skill must not redefine those protocol rules or override
the prompt's safety contract; it can only add strategy guidance that helps an
agent satisfy the phase.

Skills are dynamic Markdown artifacts. They are discovered, registered, and
progressively disclosed at runtime. A skill can describe a workflow, heuristics,
examples, scripts, references, or project conventions. Skills may be tuned and
adopted without recompiling Loom.

Template tuning follows the
[Templates — Out of Scope](templates.md#out-of-scope) boundary.
[Tuning](tuning.md#tune-proposal-worktrees-and-beads) owns proposal isolation
and candidate validation before human review.

### Public Crate and Type Pipeline

[Acceptance](#success-criteria).

`loom-skill` is a public-contract crate. It owns the skill artifact model and
registry surface that downstream consumers can reuse. The SkillOpt-style tuning
engine is internal in v1 and belongs to the internal `loom-tune` crate in the
target v1 layout; it can become a separate public surface only after the
evidence, task, replay, gate, and proposal schemas stabilize.

The public crate follows parse-don't-validate. Raw strings and paths become
typed stage values at the boundary, and downstream APIs accept only the stage
they need:

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

[Acceptance](#success-criteria).

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
`tuning.md` is loaded only for applicable/tuned package skills and is specified
in [docs/tuning.md](../docs/tuning.md). Loose single-file skills do not have
adjacent tuning documents in v1.

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

[Acceptance](#success-criteria).

Auto-discovery walks git-tracked files under the workspace and discovers only
standard package files whose basename matches `skill.md` case-insensitively.
Loom does not auto-discover `*_skill.md`, arbitrary loose Markdown files, or a
separate loose skill root in v1. Multiple case variants of `skill.md` in the
same directory are a hard error.

Auto-discovered invalid skill candidates are warnings and are skipped.
Explicitly configured invalid paths are errors. Duplicate skill names are
errors. Invalid built-in skills are fatal release-contract errors.

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

[Acceptance](#success-criteria).

Loom ships built-in skills as Agent Skills packages. Built-ins are bundled with
the Loom release and are read-only from a consumer workspace. Built-in skill
names use the `loom-` prefix to reduce collisions with consumer-defined skills.

Built-ins are profile-scoped:

- `base` built-in skills register for every profile.
- `rust` built-in skills register when the resolved profile is `rust`.
- Future profile bundles (`python`, etc.) follow the same rule.

The v1 built-in catalog is intentionally moderate: broad enough to cover Loom's
actual workflow, but small enough to tune and check with behavioral evidence.
Built-in source packages use lowercase `skill.md`; backend adapters may
transform materialized content into a runtime-specific registration format when
a tested native registrar requires it.

| Bundle | Skill                        | Primary phases                   | Purpose                                                                                                 |
| ------ | ---------------------------- | -------------------------------- | ------------------------------------------------------------------------------------------------------- |
| `base` | `loom-context-before-edit`   | `loop`, `review`, `tune`         | Read relevant specs, style rules, and source before editing; keep context available for reports/review. |
| `base` | `loom-workspace-discipline`  | `loop`, `inbox`, `tune`          | Respect operator checkout, bead clone, tune proposal checkout, and integration checkout boundaries.     |
| `base` | `loom-scope-discipline`      | `todo`, `loop`, `review`, `tune` | Avoid unrelated edits, protect user changes, and keep diffs reviewable.                                 |
| `base` | `loom-todo-decomposition`    | `todo`                           | Produce small, testable, dependency-aware work beads from specs/issues.                                 |
| `base` | `loom-verify-after-edit`     | `loop`, `tune`                   | Run relevant verification after edits and report skipped/failed checks honestly.                        |
| `base` | `loom-review-finding-recall` | `review`, `gate`                 | Systematically check diffs against spec/style/test expectations and avoid dropping findings.            |
| `base` | `loom-inbox-resolution`      | `inbox`                          | Resolve clarify/blocked/infra/tune items through chat rather than host-side mutation menus.             |
| `base` | `loom-tune-proposal-handoff` | `inbox`, `tune`                  | Treat tune proposals as review artifacts; authorize apply via `LOOM_APPLY`, never chat-side push.       |
| `base` | `loom-final-reporting`       | `loop`, `gate`, `inbox`          | End with concise changed-files, verifier, risk, and status summaries.                                   |
| `rust` | `loom-rust-change-planning`  | `todo`, `loop`                   | Plan Rust changes around module/API boundaries, ownership, and tests.                                   |
| `rust` | `loom-rust-verification`     | `loop`, `gate`, `tune`           | Prefer `nix fmt`, `cargo build`, `cargo nextest run`, and `nix flake check` as appropriate.             |
| `rust` | `loom-rust-review`           | `review`, `gate`                 | Review Rust diffs for correctness, error handling, async/process behavior, and public API drift.        |
| `rust` | `loom-rust-style-rules`      | `loop`, `review`                 | Apply repo `docs/style-rules.md`, rustfmt, clippy, naming, and layout expectations.                     |

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
`.loom-override/skills/`. Frontmatter `name` is the actual identity; Loom does
not require the name to match the parent directory or file stem. An override is
valid only if its `name` matches a known Loom built-in. The override root
overrides built-ins only, not repo/configured skills. Duplicate overrides for
the same built-in fail fast.

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

[Acceptance](#success-criteria).

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

[Acceptance](#success-criteria).

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

## Success Criteria

### Acceptance

- Skill parsing follows parse-don't-validate staging: raw Markdown cannot be
  registered until it has become a `NamedSkill`, unresolved collections cannot
  be registered, and backend registration accepts only materialized/applicable
  registry types [test](skill_registry_typestate_prevents_misuse)

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
  built-in [test](builtin_skill_profile_selection_and_override_policy)

- Optional `metadata.loom.phases` / `metadata.loom.profiles` filters default to
  all phases/profiles and narrow registration only when present
  [test](skill_frontmatter_phase_profile_filters)

- `registration = "auto"` natively registers skills for native-capable backends
  and fails on registration failure; `registration = "prompt"` disables native
  registration globally [test](skill_registration_policy_auto_and_prompt)

- The skill-index prompt partial renders name/description only for native mode,
  adds paths for prompt-disclosure mode, and adds paths to native mode only when
  `show_paths = "always"` [test](skill_prompt_index_disclosure_modes)

- Skills remain additive strategy guidance and cannot override compiled phase
  protocol, terminal markers, state-mutation authority, or gate discipline
  [judge](../tests/judges/loom.sh#skills_template_boundary_review)

## Requirements

### Functional

1. **Public skill registry.** Loom exposes skill parsing, discovery, resolution,
   filtering, and materialization through a public `loom-skill` crate. Consumers
   can use the same registry model outside the Loom binary.

2. **Standard package discovery.** Auto-discovery walks git-tracked workspace
   files for package documents whose basename matches `skill.md`
   case-insensitively at any depth. Each containing directory is one skill
   package; generated package documents use lowercase `skill.md`, and multiple
   case variants in one directory are hard errors. Package-local `tuning.md`
   documents follow the same case-insensitive/lowercase-generated duplicate
   policy.
3. **Explicit loose skill paths.** `[skills].paths` lists non-standard files or
   directories. Files load as single loose-file skills; directories recurse
   through Markdown files and load each as one loose-file skill. No wildcard or
   glob syntax exists in v1.
4. **Frontmatter identity.** Registered skills require `name` and `description`.
   Loom never infers either field. Duplicate `name` values fail fast except for
   valid built-in overrides from `.loom-override/skills/`.
5. **Built-in bundles.** Loom ships the accepted v1 built-in skill catalog by
   profile. `base` built-ins are always eligible; profile-specific built-ins are
   eligible only when the resolved profile matches. Built-in names use the
   `loom-` prefix.
6. **Built-in override root.** Consumers override tuned built-ins by committing
   loose Markdown files or packages under `.loom-override/skills/`. The
   frontmatter `name` must match a known built-in. Overrides never shadow
   repo/configured skills.
7. **Progressive disclosure.** Phase prompts include only a compact skill index;
   agents load full skill bodies on demand. Backends with native skill support
   receive native registration in `registration = "auto"`; prompt disclosure is
   used for Direct/no-native backends or `registration = "prompt"`.

### Non-Functional

1. **Prompt budget.** Skills use progressive disclosure; full bodies are read on
   demand, not pinned into every phase prompt.
2. **Portability.** Repo skills and built-in overrides use Agent
   Skills-compatible names/frontmatter. Directory packages remain available for
   assets and helper files; loose files are allowed only where explicitly
   configured or under the override root.
3. **SemVer.** Removing or renaming public `loom-skill` types or fields is a
   major version change; adding new optional metadata or diagnostics is minor.

## Out of Scope

- Auto-discovery of `*_skill.md` or arbitrary Markdown files outside configured
  paths and `.loom-override/skills/`.
- Workflow-template override policy is owned by
  [Templates — Out of Scope](templates.md#out-of-scope).

- A `loom skills init` scaffolding command.
- Public tuning-engine APIs in v1.
