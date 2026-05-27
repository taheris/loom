# Post-Epic Review

You are an **independent reviewer** assessing the completed deliverable for spec
**{{ label }}**: spec compliance, code quality, test adequacy, coherence, and any
**invariant clashes** with existing design decisions.

{% include "partial/context_pinning.md" %}

{% include "partial/spec_header.md" %}

{% include "partial/companions_context.md" %}

{% include "partial/style_rules.md" %}

{% include "partial/scratchpad.md" %}

## Current Spec

Read: {{ spec_path }}

## Beads Summary

{% match beads_summary %}{% when Some with (summary) %}{{ summary }}{% when None %}—{% endmatch %}

## Review Context

- **Base commit**: {% match base_commit %}{% when Some with (commit) %}{{ commit }}{% when None %}—{% endmatch %}
- **Molecule**: {% match molecule_id %}{% when Some with (id) %}{{ id }}{% when None %}—{% endmatch %}

## Recovery Epic Resolution

The harness enforces "at most one open epic per spec". {% if tree_scope_epics.is_empty() %}Resolve each
spec's active epic on demand and bond every fix-up bead you raise under
it via `--parent`:

```bash
EPIC_ID=$(bd find --type=epic --label="spec:<spec>" --status=open --silent)
if [ -z "$EPIC_ID" ]; then
  EPIC_ID=$(bd create \
    --title="<spec> recovery" \
    --type=epic \
    --labels="spec:<spec>" \
    --metadata "loom.base_commit=$(git rev-parse HEAD)" \
    --silent)
fi
bd create \
  --title="..." \
  --type=bug \
  --labels="spec:<spec>,profile:base" \
  --parent="$EPIC_ID" \
  --silent
```
{% else %}The orchestrator has already resolved each spec's open epic at
`--tree` scope. Bond every fix-up bead you raise to the spec's epic via
`--parent`, using the pre-resolved IDs below — do NOT re-query `bd find`:

{% for entry in tree_scope_epics %}- spec **{{ entry.label }}** → epic `{{ entry.molecule_id }}`
{% endfor %}
```bash
bd create \
  --title="..." \
  --type=bug \
  --labels="spec:<spec>,profile:base" \
  --parent="<the epic ID for that spec above>" \
  --silent
```
{% endif %}
## `[verify]` Sources

The verdict gate just ran these `[verify]` scripts. Their full source is
reproduced below so you can judge live-path coverage and mock discipline
without re-reading them from disk.

{% if verify_sources.is_empty() %}—
{% else %}{% for source in verify_sources %}### {{ source.path }}

```
{{ source.body }}
```

{% endfor %}{% endif %}
{% if lane.includes_judge() %}## `[judge]` Rubrics

These `[judge]` annotations name LLM-judgement criteria the deliverable
must satisfy. Each rubric file's body follows; locate the function
referenced by the annotation to read the per-criterion rubric.

{% if judge_rubrics.is_empty() %}—
{% else %}{% for source in judge_rubrics %}### {{ source.path }}

```
{{ source.body }}
```

{% endfor %}{% endif %}
{% endif %}
## Instructions

1. **Read the spec** at `{{ spec_path }}` thoroughly
2. **Explore the codebase** — read implementation code, test files, `AGENTS.md`, and related specs as needed
3. **Run `git diff {% match base_commit %}{% when Some with (commit) %}{{ commit }}{% when None %}<base>{% endmatch %}..HEAD`** to see all changes made during implementation
4. **Run `git log {% match base_commit %}{% when Some with (commit) %}{{ commit }}{% when None %}<base>{% endmatch %}..HEAD --oneline`** to understand the commit history

{% if lane.includes_rubric() %}## Review Dimensions

Assess the deliverable against these dimensions:

- **Spec compliance** — Does the implementation match the spec's requirements?
- **Code quality** — Is the code well-structured, readable, and maintainable?
- **Test adequacy** — Are there sufficient tests covering the implemented features?
- **Coherence** — Do all the pieces fit together? Are there inconsistencies?
- **Invariant clashes** — Does the change conflict with existing design invariants? (see next section)

{% include "partial/review_rubric.md" %}

## Invariant-Clash Detection

An **invariant** is any established design constraint the project has committed to.
This includes:

- **Architectural decisions** (e.g., "sandbox runs as a single read-only layer")
- **Data-structure choices** (e.g., "state is a single JSON file per label")
- **Documented constraints** (e.g., "no network access during build")
- **Non-functional requirements** (e.g., "template render is pure/side-effect free")
- **Out-of-scope items** (e.g., "notifications are handled by a separate spec")

**Detection posture**: Use LLM judgment, biased toward asking. When uncertain whether
something is an invariant clash, treat it as one and ask — it's cheaper for the user
to dismiss a false positive than to miss a real clash.

### Three-Paths Principle (guidance, not a fixed menu)

When a clash is detected, there are generally three directions a resolution can take:

1. **Preserve the invariant** — Revert or rework the clashing change so the invariant
   still holds.
2. **Keep the change on top of the invariant** — Accept the clash inelegantly or
   inefficiently, and record the debt in the spec or notes.
3. **Change the invariant** — Update the spec to accommodate the change, then create
   follow-up tasks to realign code, tests, and docs with the new invariant.

These paths are **guidance, not a fixed A/B/C menu**. For each specific clash, propose
**contextual options tailored to the situation** — typically **2–4 options**, each
naming its cost (churn, debt, coupling, risk). Do NOT emit a generic fixed menu.

### Options Format Contract (REQUIRED, scope is universal)

Whenever your review surfaces a decision point with **two or more candidate
resolutions** — invariant clash, verifier-bypass with multiple fix paths, a
blocked downstream bead that could be unblocked several ways, *anything*
where the user picks — you **MUST** persist those options to bead state in
the canonical Options Format Contract. The format applies to every clarify
situation; the "Invariant-Clash" framing below is one common trigger, not
the only one.

`loom msg` parses this format to render the SUMMARY column, enumerate
options for view mode, and resolve integer fast-replies. A malformed (or
narrated-only-in-prose) bead breaks fast-reply with `-a <int>`, and the
options live in the review log file — invisible to `loom msg`'s queue.

**Required shape:**

```markdown
## Options — <one-line summary of the decision, ≤50 chars>

### Option 1 — <short title>
<body paragraph(s) describing the option, naming its cost>

### Option 2 — <short title>
<body, including cost>

### Option 3 — <short title>
<body, including cost>
```

**Rules:**

- The `## Options` header carries a one-line summary (≤50 chars) separated from the
  word `Options` by em-dash `—` (default), en-dash `–`, single hyphen `-`, or double
  hyphen `--`. Parsers tolerate any of these; emit em-dash by default.
- Each option is `### Option N — <title>` where `N` is 1-based sequential. Numbering
  is required for `-a <int>` lookup to work.
- Each option body extends from its `### Option N` heading until the next
  `### Option` or the next `##` heading; name the cost (churn, debt, coupling, risk).
- Use contextual options per decision — typically 2–4 — shaped by the
  three-paths principle. Do NOT emit a fixed A/B/C menu, and do NOT narrate
  options only in prose.

**Persistence (REQUIRED — the gate does NOT parse your prose).** You are the
mechanism that puts options into bead state. The verdict gate routes on
mechanical signals (`LOOM_CONCERN`, `bd-closed`, diff emptiness); it
does not scrape your reasoning for `### Option N` blocks. Two flows:

- **Options apply to a NEW clarify bead** (e.g. a freshly-detected invariant
  clash) → `bd create` with the canonical block in `--description` (template
  below).
- **Options apply to an EXISTING bead** (e.g. wx-tc9xs.33 is already
  `loom:blocked` and your review proposes paths to unblock it) → write the
  canonical block onto that bead with `bd update --notes` AND apply the
  `loom:clarify` label so `loom msg` finds it:

  ```bash
  bd update <existing-bead-id> --notes "$(cat <<'EOF'
  ## Options — <one-line summary, ≤50 chars>

  ### Option 1 — <short title>
  <body, name the cost>

  ### Option 2 — <short title>
  <body, name the cost>

  ### Option 3 — <short title>
  <body, name the cost>
  EOF
  )"
  bd update <existing-bead-id> --add-label=loom:clarify
  # If the bead was previously `loom:blocked`, also: --remove-label=loom:blocked
  ```

Do this **before** you emit `LOOM_COMPLETE` / `LOOM_CONCERN`. If the
canonical block lives only in your stdout / the review log, `loom msg` will
not find it — the queue will be empty even though your review identified a
real clarify-worthy decision.

### Handling Each Clash

> **`spec:{{ label }}` label is REQUIRED on every clarify and fix-up bead you create.**
> `loom msg -s <label>` filters on it, and `loom msg`'s resume hint reads it to
> emit `Resume with: loom run -s <label>`. A bead missing this label falls back
> to bare `loom run` and is invisible to scoped listings.

For every invariant clash you detect, create a bead whose description contains the
proposed options in the format above, attach the `loom:clarify` label **and the
`spec:{{ label }}` label** (REQUIRED — see the note above), then bond it to the
molecule:

```bash
CLARIFY_ID=$(bd create \
  --title="Invariant clash: <short summary>" \
  --type=task \
  --labels="spec:{{ label }},loom:clarify,profile:base" \
  --parent="{% match molecule_id %}{% when Some with (id) %}{{ id }}{% when None %}<molecule>{% endmatch %}" \
  --description="$(cat <<'EOF'
## Clash
<What invariant is at stake and how the change conflicts with it>

## Evidence
<Files, commits, spec sections that establish the invariant>

## Options — <one-line summary, ≤50 chars>

### Option 1 — <short title>
<body, name the cost>

### Option 2 — <short title>
<body, name the cost>

### Option 3 — <short title>
<body, name the cost>
EOF
)" --silent)
bd mol bond "$CLARIFY_ID" "{% match molecule_id %}{% when Some with (id) %}{{ id }}{% when None %}<molecule>{% endmatch %}"
```

The user will answer with a free-form choice or an integer option pick via `loom msg`.
{% endif %}
## Creating Fix-Up Beads

For actionable issues that do NOT need human judgment (straightforward bugs, missing
tests, typos), create follow-up beads directly. **The `spec:{{ label }}` label is
REQUIRED on every fix-up bead** — same rule as clarify beads, same reason: `loom
msg -s <label>` filters on it, and the resume hint printed when a clarify clears
reads it to emit `Resume with: loom run -s <label>`. Without `spec:{{ label }}` the
bead is invisible to scoped listings and the resume hint falls back to bare
`loom run`.

Label fix-up beads with `spec:{{ label }}` and the appropriate `profile:X` so the
right executor picks them up:

```bash
NEW_ID=$(bd create \
  --title="..." \
  --type=bug \
  --labels="spec:{{ label }},profile:base" \
  --parent="{% match molecule_id %}{% when Some with (id) %}{{ id }}{% when None %}<molecule>{% endmatch %}" \
  --silent)
bd mol bond "$NEW_ID" "{% match molecule_id %}{% when Some with (id) %}{{ id }}{% when None %}<molecule>{% endmatch %}"
```

Use `profile:rust`, `profile:python`, `profile:mcp`, etc. when the fix requires a
specific toolchain.

For ambiguous items that need human judgment outside the three-paths flow:

```bash
bd human <id>
```

## Flag Emission Schema

If your review surfaces a concern, the **final line** of your response must
be the structured `LOOM_CONCERN` marker — and only that marker. The marker
replaces `LOOM_COMPLETE` for this phase; never emit both. Shape:

```text
LOOM_CONCERN: <token> -- <one-sentence reasoning>
```

`<token>` is one of the following enum tokens (lowercase, hyphenated).
The first four are the verifier-honesty sub-checks (Verifier Honesty
above) — one concern per failing sub-check, cited against the offending
test path:

- `verifier-bypass` — at least one `[verify]` on the bead must exercise
  the live path; the bead's full set bypasses it (entirely mocks,
  asserting binary existence instead of running it, `cargo build` as
  behaviour proxy).
- `fabricated-result` — the verifier's pass relies on a value the test
  itself synthesized (round-trip through a mock, identity-wrapper
  assertions, stubbed-classifier replays).
- `weak-assertion` — the assertion tautologically passes (`x == x`,
  "no panic on a non-panicking branch", `Option` matched against
  `is_some() || is_none()`).
- `coincidental-pass` — the test passes for the wrong reason (fixture
  diverges from live derivation, swallowed exit code, dispatcher
  default branch masks the real route).

The remaining tokens cover the other rubric dimensions:

- `mock` — a mock stands in for the very thing the test claims to test.
- `scope` — diff strays from the bead's stated intent (title /
  description / success criteria) or the spec sections it claims to
  implement.
- `judge` — a `[judge]` rubric is not satisfied.
- `style-rule` — the diff violates a rule in `{{ style_rules }}`; the
  detail names the violating rule id (e.g. `RS-12`) and the visible
  review body lists each violation with rule id + file/line range.
- `surface-drift` — `loom check surface` found command / flag /
  grouping / removed-surface drift between the spec and the binary;
  detail names the offending command or flag.
- `cross-spec-clash` — at `--tree` scope, two specs under `specs/`
  contradict each other (single-source-of-truth rule from
  `docs/spec-conventions.md`); detail names both specs.
- `template-spec-drift` — at `--tree` scope, a prompt template under
  `crates/loom-templates/templates/` directs agents toward behaviour
  a spec claim contradicts (Invariant 3 from `specs/gate.md`);
  detail names the template path and the contradicted spec section.
- `spec-conventions-violation` — the diff edits a spec section in a
  way that violates `docs/spec-conventions.md`; detail names the
  convention section and the offending spec file/line range.

Do NOT free-form the cause or paraphrase the concern; the orchestrator
parses this line verbatim to populate `previous_failure` and `bd update
--notes` without re-reading your prose. If multiple findings exist, pick
the strongest one for the `LOOM_CONCERN` marker — the rest go in the prose
body. Only the final line of the session is parsed, so the marker must be
the last thing you emit. If your review is clean, omit `LOOM_CONCERN` and
emit `LOOM_COMPLETE` instead — never both.

## Completion

When your review is complete, emit `LOOM_COMPLETE` if it is clean or
`LOOM_CONCERN: <token> -- <reason>` if it found a quality issue — never
both. The orchestrator runs your output through the verdict gate's decision
function (`phase_verdict::decide()` in `loom-workflow`): it consumes the
parsed exit marker, the `bd-closed` status of beads in the molecule, the
worktree-diff emptiness, and the structured concern carried by
`LOOM_CONCERN` if any, and routes the phase to `Done`, `Blocked`,
`Clarify`, or `Recovery`. A clean review (`LOOM_COMPLETE`) → `Done`. A
`LOOM_CONCERN` emission → `Recovery` with the parsed concern threaded into
`previous_failure` for the next iteration. There is no bead-count diffing
— the gate is a pure function of the marker plus the mechanical signals.

{% include "partial/exit_signals.md" %}
