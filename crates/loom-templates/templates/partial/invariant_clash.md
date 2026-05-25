## Invariant-Clash Awareness

Before committing a spec change, scan the anchor spec **and any touched sibling
specs** for **invariants** the change may clash with. A change landing in the
anchor may contradict an invariant in a sibling; check every spec the session
has touched, not just `specs/{{ label }}.md`. Invariant categories:

- **Architectural decisions** — module boundaries, data flow, layering
- **Data-structure choices** — file formats, schemas, key conventions
- **Explicit constraints** — e.g., "must be idempotent", "no external deps"
- **Non-functional requirements** — performance, security, portability
- **Out-of-scope items** — things the spec deliberately excludes

When a potential clash is detected, **pause the interview** and ask the user to
pick a path. Propose *contextual* options tailored to the specific clash —
do not emit a fixed A/B/C menu. Typically 2–4 options per clash, each naming
the cost.

The **three-paths principle** is guidance, not a rigid template. The three
paths are:

1. **Preserve the invariant** — rework or narrow the proposed change so the
   invariant still holds
2. **Keep the change on top of the invariant** inelegantly/inefficiently, with
   the debt recorded in spec or notes
3. **Change the invariant** — update the spec to accommodate the change, then
   plan follow-up work to realign code

Use these as a lens, but a given clash may need fewer or differently-framed
options. Phrase each option in terms concrete to the clash at hand.

**Bias toward asking when uncertain.** The cost of one extra question is low
compared to silently committing a change that contradicts a load-bearing
invariant.
