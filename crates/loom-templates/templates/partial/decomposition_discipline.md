## Decomposition Discipline

Every bead you author in this session must correspond to **evidence-confirmed missing work**, not to a fixed task axis or a Success Criteria bullet considered in the abstract. The driver has already computed the exact changed-spec roster and created or reused the `loom:todo` work epic; decompose that roster exactly.

### Audit before fan-out

Before authoring any non-audit bead, you MUST:

1. **Consult the `criterion_status` surface** rendered above for each criterion in each changed spec. Each row exposes the criterion text, annotation target, typed evidence state (`Current`, `Missing`, or `StaleAnnotation`), any current verifier result (`Pass | Fail | Skipped`), the commit it was recorded against, and how many commits have landed on HEAD since. A row whose evidence is `Current`, whose result is `Pass`, and whose commits-since count is low is positive evidence of coverage — do **not** author a bead for it.

2. **Inspect representative implementations and verifier functions** for any row that is suspicious: missing evidence, stale annotation evidence, failed or skipped current evidence, a large commits-since value, or a verifier-target name whose body may not actually exercise the live system. A directory listing proves the file exists; it does not prove the file contains the named target, and it does not prove the named target exercises the contract the criterion asserts.

The discipline is template-agnostic: it applies to whatever workspace layout the consumer has. "Representative implementations" means the production code path the criterion's verifier is supposed to exercise, under whatever organisation that consumer's repository uses. It does **not** require blanket full-file reads across every changed spec; use the injected diffs and status rows first, then read targeted sections or verifier bodies only where the evidence is ambiguous.

### What the audit governs

The `criterion_status` audit governs **how** the injected changed-spec work decomposes into beads — split, merge, dependency-order — **not whether** driver-provided planning inputs get authored at all. Implementation notes and changed-spec diffs describe the next batch of planned work; a green row is evidence about prior coverage, not proof that a new planned change is already shipped.

The audit's role is to discipline the *shape* of the decomposition: which notes collapse into a single bead, which split across several, how the resulting beads order against each other, and whether any proposed bead is redundant because representative source inspection confirms the contract is already covered.

### Acceptable session outcomes

A successful decomposition session emits `LOOM_TODO: <json>` with a required final work-epic title and every changed spec represented exactly once:

- **Decomposed.** Use the `Decomposed { beads }` outcome for a spec where you created one or more beads under the injected work epic. For each non-audit bead, cite the evidence that drove its creation in the bead description: the `criterion_status` row you consulted, the file you read that surfaced the gap, and/or the verifier-source observation that showed the target was dishonest.
- **NoWork.** Use the `NoWork { reason }` outcome only after auditing the changed spec and confirming no implementation change is needed. The reason must name the evidence that made no work safe.

Omitted specs, duplicate specs, pending outcomes, and generic `LOOM_COMPLETE` / `LOOM_NOOP` markers are not success states for todo.

### Clarify or block on the work epic

When coverage cannot be determined by inspection — the spec is ambiguous, verifier targets conflict, cursor/index state is inconsistent, or your judgement of the status cache's trustworthiness is contestable — emit `LOOM_CLARIFY`. Persist both the question and the canonical `## Options — …` block to the **driver-created work epic** notes per the *Options Format Contract* in `specs/gate.md` before emitting the marker. The verdict gate applies `loom:clarify` to the work epic; the human resolves via `loom inbox`, and a subsequent `loom todo` invocation reuses the pending work epic.

When you have no candidate resolutions to enumerate, emit `LOOM_BLOCKED`; the work epic remains the session-stable carrier for the blocked decomposition batch.

### Enumerate-everything fan-outs are structurally invalid

A fixed decomposition axis applied across the board irrespective of evidence — for example, "setup, implementation, tests, documentation" mechanically expanded onto every criterion — is precisely the failure mode this discipline targets. The combined effect of the typed `criterion_status` surface and the exact-roster `LOOM_TODO` validator makes such fan-outs structurally unviable. `loom gate review`'s judge-tier walk catches any decomposition that bypasses the evidence surface to re-introduce enumerate-everything beads.
