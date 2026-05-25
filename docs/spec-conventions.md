# Spec Conventions

What a spec is, what it isn't, and how to author one.

A spec is a **forward-facing contract** about what a component does
and why. It is the *intent floor* of the system — no downstream
process (`loom todo`, the agent, `loom gate verify`, `loom gate
review`, the push gate) can recover more about intent than the spec
contains. The spec is what every other artefact in the loop is held
against.

Specs are read frequently and edited rarely. When in doubt, write
less; put the rest in implementation notes (`loom note set`) or
commit messages.

## In scope

A spec defines:

1. **Purpose.** What problem this component solves and why. One
   paragraph; if it takes more, the problem may be the spec's
   scope — consider splitting.
2. **External-observable contracts.** Things outside the system can
   see and depend on:
   - CLI surface (commands, flags, exit codes, marker outputs)
   - Wire formats (JSON shapes, event schemas, RPC protocols)
   - Database schema (table/column shapes that any tool with read
     access can interpret)
   - Public Rust crate API (types, traits, public re-exports)
3. **Architecture.** Load-bearing structural commitments and their
   rationale. This includes **language-conditional architecture** —
   choices shaped by the language but still load-bearing within it:
   - Rust crate layout (workspace member crates, their roles)
   - Inter-crate dependency direction
   - Module boundaries that are part of a public API
4. **Architecture-bearing types.** Types whose *shape* enforces an
   architectural claim — when the type's structure is how an
   invariant is made unrepresentable, the type is part of the
   contract. Examples:
   - **Newtype identifiers at parse boundaries** (`BeadId`,
     `SpecLabel`, etc.). The newtype shape encodes "raw strings are
     parsed into validated identifiers at the boundary; downstream
     code receives the typed form, never `String`".
   - **Typestate state machines** (`AgentSession<Idle>` /
     `AgentSession<Active>`). The two states plus the operations
     available in each ARE the protocol-correctness invariant.
   - **Parse-stamp split types** (e.g., `ParsedAgentEvent` →
     `AgentEvent`). The two-type split encodes that an unstamped
     event cannot reach a consumer.

   The discriminator: does the type's **shape** make a spec claim
   structurally unrepresentable? If yes, the type belongs in spec.
   Types that are pure convenience (helper structs, internal
   control-flow enums) do not.
5. **Invariants.** Properties that must never be violated, in code
   or in sibling specs. Five categories — architectural decisions,
   data-structure choices, explicit constraints, non-functional
   requirements, out-of-scope items.
6. **Non-functional contracts.** Performance bounds, security
   posture, portability requirements, isolation guarantees.
7. **Cross-spec relationships.** Where this spec defers to a sibling
   and where it owns a concern siblings reference.
8. **Verifier bindings.** Each success criterion carries one
   annotation (`[check]`, `[test]`, `[system]`, or `[judge]`)
   pointing at the verifier that proves the claim. See *Trust tiers*
   below.

## Out of scope

A spec does NOT contain:

1. **Implementation status.** No `[x]` / `[ ]` checkboxes, no "TODO",
   no "in progress", no progress percentages. Status is **computed**
   by running the verifier, not stored in spec markdown. Past passes
   do not grant immunity from re-evaluation; the current code-spec
   pair is what counts.
2. **Internal implementation organization** with no architectural
   role:
   - File paths inside a crate (`src/foo/bar.rs`, line numbers)
   - Helper structs, internal control-flow enums, private utility
     functions
   - Module layout within a crate

   (Types whose *shape* enforces an architectural claim — newtype
   IDs, typestate state machines, parse-stamp splits — ARE spec
   content; see *In scope #4*.)
3. **Specific tool configuration** when the *rule* lives elsewhere:
   - Specific dependency version pins (live in `Cargo.toml`)
   - Specific clippy lints (the rule lives in `docs/style-rules.md`)
   - Specific pre-commit hook commands (live in
     `.pre-commit-config.yaml`)
4. **Historical narrative.** No decision archaeology, no "previous
   approach was X but we switched to Y", no change logs. Commit
   messages and git history carry that. The spec is a snapshot of
   *current intent*, not a history of how we got here.
5. **Implementation hints.** Transient per-session guidance (file
   paths to touch, hidden constraints, recovery shortcuts) belongs
   in `loom note set <label> --kind implementation`, not in the spec
   body. The hints are consumed and deleted; the spec is durable.
6. **Affected-file lists that enumerate what THIS edit touches.**
   The spec is not a PR description. An "Affected Files" section is
   acceptable only when it enumerates what files the spec *owns* as
   source of truth (e.g., "this spec owns `.pre-commit-config.yaml`
   and the `tests/integration/` tree"). Listing files an in-flight
   change happens to modify is TODO-list shape, not spec shape.

## Trust tiers

Every functional claim in a spec MUST name how it's verified.
Reliability is a property of *gate composition*, not of individual
claims; binding each claim to a tier is how the gate composes
honestly.

Three cost-and-confidence tiers, partitioned by what decides whether
the claim holds (code, LLM, or human):

| Tier | What it proves | Annotation forms |
|------|----------------|------------------|
| **Deterministic** | Structural correctness (the code does X for input Y) | `[check]`, `[test]`, `[system]` |
| **Stochastic** | Semantic correctness (the implementation matches the *intent*, the test is honest, the style rule holds) | `[judge]` |
| **Oracle (human)** | Ground truth on intent (the criterion captures what was actually wanted) | Implicit — the human accepts a `loom plan` interview |

### Deterministic annotations

The three deterministic annotations partition by *what activity
verifies the claim*:

| Annotation | What the verifier does | Target shape |
|------------|------------------------|--------------|
| **`[check]`** | Static analysis of source (presence, absence, structural property across files) | `[check](command)` — a shell command that runs a walk / lint / AST analysis. Each annotation invokes its own process. |
| **`[test]`** | Runs the code in isolation and asserts behaviour | `[test](path)` — a language-native test path (e.g. `crate::module::test_name` for Rust, `tests/test_foo.py::test_bar` for Python). The gate batches all `[test]` targets in a single `loom gate test` invocation into one runner subprocess. |
| **`[system]`** | Runs the assembled system (containers, packaging, end-to-end) | `[system](command)` — a shell command that exercises the full system. Each annotation invokes its own process. |

### Stochastic annotation

`[judge]` invokes an LLM to evaluate a claim that requires semantic
judgement — code-quality dimensions structural analysis cannot
capture (error-message clarity, doc-comment usefulness, API
ergonomics, naming consistency).

| Annotation | Target shape |
|------------|--------------|
| **`[judge]`** | `[judge](path)` — a file path or criterion id whose content is the rubric the LLM evaluates against. The gate batches `[judge]` invocations to use API-level concurrency. |

### Annotation flags

A criterion without an annotation is a flag at `loom gate verify`
(no resolvable verifier). A criterion whose annotation points at a
missing or stubbed verifier is a flag at the same audit. A
criterion whose annotation is satisfied by a unit-test pass but
production diverges from that unit is a flag at `loom gate review`'s
verifier-honesty walk.

### Deterministic ceiling

Structural correctness ≠ semantic correctness. A deterministic
annotation passing proves the verifier passes; it does not prove
the system does the right thing. Anything that requires judgement
(mock discipline, scope appropriateness, style-rule conformance for
prose rules, conformance trace through current code) is
`[judge]`-tier, not deterministic. Choosing the wrong tier is itself
a flag.

### No tier-skipping

A claim whose verification depends on running the production code
path is not satisfied by a unit test that runs the underlying
function in isolation. A function can be unit-tested and correct
while production calls a different function entirely; the unit
test's pass status says nothing about whether production satisfies
the claim. The criterion's verifier must exercise the **live path**
— same binary, same argv shape, same env as the real invocation.

## Section structure

Standard top-level sections, in this order. Spec authors omit
sections that don't apply rather than padding them with "N/A".

```
# <Spec Title>

<One-sentence summary of what this spec defines.>

## Problem Statement
<One paragraph. The problem this component solves and the scope
boundaries.>

## Architecture
<Load-bearing structural commitments and rationale. For Rust
projects: crate layout, dep graph, public surface per crate.>

<Spec-specific sections as needed — e.g., Wire Format, State
Machine, Event Schema, Algorithm, Glossary, Configuration. The
spec's topic dictates what belongs here. No section in this slot
is required; include only those that earn their place.>

## Success Criteria
<Plain bullets — NO `[ ]` / `[x]`. Each bullet is a checkable
property with a `[check]`, `[test]`, `[system]`, or `[judge]`
annotation. The criteria ARE the contract surface — what the gate
actually checks. Place them up front so a reader sees the testable
claims before the prose that elaborates them.>

## Requirements

### Functional
<Numbered FRs. Each describes a behavior or contract in prose,
elaborating on a criterion above.>

### Non-Functional
<Numbered NFRs. Each describes a quality attribute.>

## Out of Scope
<Explicit non-goals. Bulleted.>
```

Sections NOT in the standard set (omit unless genuinely needed):

- `## Affected Files` — usually a TODO list in disguise; omit. If a
  spec genuinely owns a set of files as source-of-truth, name them
  in *Architecture* or a dedicated *Source-of-truth Files* section,
  with the framing "this spec owns X" not "this change modifies X".
- `## Implementation Notes` — never. Notes belong in
  `loom note set`, not in spec body.
- `## Decisions Log` — never. Commit messages and PR descriptions
  carry decision history. The spec is the current contract.
- `## Changelog` / `## History` — never. See *Out of scope #4*.

## Length guidance

A spec should be the smallest document that fully states the
contract. As a soft target: spec body (everything before *Success
Criteria*) under 500 lines for most components, under 1000 lines for
the most complex (e.g., a multi-crate workspace).

If a spec is sprawling past this, the cause is usually one of:

- Implementation detail leaked into the body (re-audit against
  *Out of scope*).
- Historical narrative accumulated (delete; commit messages own it).
- Multiple concerns merged into one spec (split into sibling specs;
  cross-reference).

## Rust-specific guidance

For Rust workspaces, the following are spec content:

- **Crate enumeration.** The list of workspace member crates and
  what each one is for. One short paragraph per crate is enough.
- **Crate roles.** Which crate is the public contract (e.g.,
  `loom-events`); which crates are internal runtime.
- **Inter-crate dependency direction.** What each crate may
  import; what it must not. The dep graph is architecture, not
  organization.
- **Public type-contract shape.** For each crate that carries a
  public contract: the *shape* of its public types (e.g., "flat
  tagged enum"), the *variant set* (one-line meaning per variant),
  the *envelope* (shared fields across variants), and the
  *evolution policy* (semver, schema-version).

The following are NOT spec content (live in code, generated docs,
or `docs/style-rules.md`):

- **Per-field details inside a variant.** A variant's full field
  list, types, and serde attributes are in the Rust source; the
  crate's API docs are the place to find them. The spec names the
  variant and any fields the contract depends on (e.g., the
  envelope's `kind` discriminator). Fields that are pure payload
  shape live in code; fields whose type encodes architecture (e.g.,
  a newtype-wrapped field at a parse boundary) are spec per
  *In scope #4*.
- **Workspace `Cargo.toml` enumerations.** Specific dependency
  versions, feature flags, lint denials — all implementation. The
  *pattern* (workspace-deps + workspace-lints, RS-3) lives in
  `docs/style-rules.md`; the *contents* live in `Cargo.toml` and
  `clippy.toml`.
- **Internal module paths.** Whether a crate organizes its code
  as `src/foo.rs` or `src/foo/mod.rs` + submodules is implementer's
  choice.

## Single source of truth

Each fact belongs to exactly one spec. Cross-reference; do not
duplicate.

- If a fact appears in two specs identically, one of them is wrong
  (drift incoming).
- If two specs need the same fact stated differently, one of them
  is paraphrasing — replace the paraphrase with a cross-reference.
- If two specs disagree on a fact, the contradiction is a flag
  raised by `loom gate review`'s cross-spec walk.

## Migration

This convention applies immediately to:

- New spec sections (added in a new `loom plan -n` or `-u` session)
- Edits to existing spec sections (changes made in `loom plan -u`
  must produce convention-compliant text)

Existing spec content that pre-dates this convention may be
non-compliant. The compliance audit is incremental — each `loom
plan -u` session is expected to bring touched sections into
compliance; sections not touched in a given session may remain
non-compliant until a future session migrates them. A dedicated
compliance-migration epic per spec tracks the remaining work
explicitly.

## How the gate enforces this

The loom gates carry the enforcement:

- `loom gate verify` enumerates every criterion annotation and
  reports pass/fail by running the verifier. Status is live.
  Tier-specific subcommands (`loom gate check`, `loom gate test`,
  `loom gate system`) run one tier in isolation.
- `loom gate review` walks the conformance / style / test-quality
  rubric, citing every applicable rule from `docs/style-rules.md`
  and every applicable convention from this document.
- Plain `loom gate` reads cached results from the last verifier run
  (sqlite-backed) and prints a fast status report — no verifiers
  run, no network, no spawn cost.
- `loom plan` pins this document so every planning session has the
  convention in context.

A spec violation discovered by any of these gates is a flag,
treated like any other style-rule violation.
