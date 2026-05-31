# Judge Rubric — Fix-up batch acceptance is "agent processed the batch"

This rubric pins the contract referenced by `specs/gate.md`:

> A fix-up batch carrying multiple findings exposes worker
> discretion to fix all and close, fix a subset and split the
> remainder into sibling fix-up beads under the molecule epic via
> `bd create --parent=<molecule-epic-id>` for deferred work, or
> emit `LOOM_CLARIFY` for no-progress cases; the bead's acceptance
> criterion is "agent processed the batch", not "every finding
> individually resolved".

It also concretizes the *Worker discretion on a minted fix-up
batch* section of `specs/gate.md § Per-batch processing`. The
contract is **prompt-level**: when the driver dispatches a worker
against a bead carrying `loom:fixup:<fp>` and a description that
enumerates multiple findings, the rendered worker prompt must
direct the worker at all three legitimate acceptance shapes — so a
worker that closes the batch after fixing only a subset (without
splitting the remainder) understands they have violated the
contract, and a worker that cannot make progress knows to emit
`LOOM_CLARIFY` rather than close the batch silently.

The driver does not gate on which acceptance shape the worker
chose; the system self-corrects via re-audit per
`specs/gate.md § Per-batch processing` (any unresolved finding
re-emerges in the next mint run's batch under a new fingerprint).
What this rubric pins is that the worker has been *told* the
shapes — silent closure of an unaddressed batch is a contract
violation the prompt must prevent up front.

## Source under judgement

`crates/loom-templates/templates/loop.md`

(Plus any partial transitively included by `loop.md` that the
fix-up-batch guidance is factored into; the judge follows the
include graph and treats the rendered prompt as the surface under
evaluation.)

## Criterion

The `loop.md` template (directly or via an included partial) MUST
direct a worker dispatched against a fix-up batch — a bead
carrying the `loom:fixup:<fp>` label and a description enumerating
multiple findings — that **all three** of the following acceptance
shapes are legitimate, and that closing the batch without taking
one of them is not:

1. **Fix every finding and close the batch.** The prompt must
   name this as one acceptable shape — the worker resolves the
   full enumerated finding set in one diff and runs `bd close`
   on the batch.

2. **Fix a subset and split the remainder into sibling fix-up
   beads under the molecule epic.** The prompt must name this as
   acceptable AND name the exact bd invocation shape:
   `bd create --parent=<molecule-epic-id>` (parent is the
   **molecule epic**, not the batch bead itself, per
   `specs/gate.md § Per-batch processing — Worker discretion`).
   "Create a follow-up bead" without naming the molecule epic as
   the parent does not satisfy this — the parenting shape is
   load-bearing because the molecule lifecycle expects fix-ups
   bonded as direct epic children.

3. **Emit `LOOM_CLARIFY` when no progress is possible.** The
   prompt must name this as the legitimate exit when neither (1)
   nor (2) is achievable — routed via the standard per-bead
   clarify path (Options block persisted to bead state per the
   *Options Format Contract* in `specs/gate.md`).

In addition, the prompt MUST frame the acceptance criterion
correctly: closing the batch is the agent's signal that they
**processed** the batch (chose one of shapes 1–3), not that every
finding was individually resolved. Phrasings such as *"the bead's
acceptance criterion is 'agent processed the batch', not 'every
finding individually resolved'"*, or an equivalent statement that
makes the distinction explicit, satisfy this — a prompt that
simply lists the three shapes without naming the underlying
acceptance contract leaves the worker free to interpret closure as
"all findings fixed" and treat a partial fix as a contract
violation.

## Verdict

- **Pass** iff all four conditions above hold in the rendered
  template: shapes (1), (2) with the correct `--parent=<molecule-
  epic-id>` framing, (3), and the "agent processed the batch"
  framing.
- **Fail** otherwise, naming the missing piece:
  - *"fix-all shape absent"* — condition 1 not met;
  - *"split-subset shape absent"* — condition 2 not met (the
    shape is mentioned but the bd invocation / parent target is
    wrong or missing);
  - *"clarify shape absent"* — condition 3 not met;
  - *"acceptance framing absent"* — the worker is given the
    shapes but not told that "agent processed the batch" is the
    acceptance criterion, so closure semantics remain ambiguous.
  Multiple failures may apply; name each.

## Non-goals

- The judge does not check that the driver enforces a particular
  acceptance shape — it does not (the contract is prompt-level
  and the system self-corrects via re-audit).
- The judge does not check that the worker actually chose one of
  the three shapes on any particular bead — runtime worker
  behavior is observed by the gate's audit pipeline, not this
  rubric. This rubric pins only the prompt-level contract.
- The judge does not require a specific section heading — the
  instruction may live under *Instructions*, *Discovered Work*, a
  dedicated *Fix-up batches* section, or any partial included by
  `loop.md`, as long as the rendered prompt makes the four
  required points before the progress-marker partial include.
- The judge does not pin guidance for single-finding fix-up beads
  — the discretion contract applies to batches carrying
  **multiple** findings; a single-finding batch has no remainder
  to split and the three shapes collapse to "fix and close" or
  "clarify".
