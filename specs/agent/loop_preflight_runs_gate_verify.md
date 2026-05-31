# Judge Rubric — Worker preflight runs `loom gate verify --diff HEAD`

This rubric pins the contract referenced by `specs/gate.md`:

> The bead-container worker runs `loom gate verify --diff HEAD`
> before emitting `LOOM_COMPLETE` and resolves findings in-session;
> contract is prompt-level (rendered in `loop.md`), not driver-gated.

It is the middle layer of the three-layer belt-and-braces audit
described in `specs/gate.md § Per-diff stage checks`:

- bead-container pre-commit hook (per `specs/pre-commit.md`) catches
  findings at commit time;
- this preflight catches findings at marker-emit;
- the driver-side push-gate audit catches findings at push (and is
  the authoritative gate).

The contract is prompt-level only — the driver does not separately
gate on the preflight's presence. The failure mode of an agent
skipping the preflight is caught by the push-gate audit anyway.

## Source under judgement

`crates/loom-templates/templates/loop.md`

## Criterion

The `loop.md` template MUST contain an instruction directing the
worker to:

1. **Run the preflight command.** A clear, unambiguous sentence or
   bullet naming the exact command `loom gate verify --diff HEAD`
   and placing it before `LOOM_COMPLETE` emission. Aliases,
   paraphrases, or scope-flag substitutions (e.g. `--bead <id>`,
   `--tree`, `loom gate audit ...`) do not satisfy this — the
   command surface the agent runs is the literal string
   `loom gate verify --diff HEAD`.

2. **Resolve findings in-session.** The same instruction (or an
   adjacent sentence in the same bullet/section) must emphasise
   that any findings the preflight surfaces are resolved in the
   current session — *not* deferred to a follow-up bead and *not*
   ignored before emitting `LOOM_COMPLETE`. Phrasings such as
   "resolve any findings in-session", "fix findings before
   emitting `LOOM_COMPLETE`", or equivalent imperatives count;
   silently mentioning the command without the resolution
   discipline does not.

## Verdict

- **Pass** iff both conditions above hold in the rendered template.
- **Fail** otherwise, naming the missing piece — either *"command
  surface absent"* (condition 1 not met) or *"in-session resolution
  not emphasised"* (condition 2 not met), or both.

## Non-goals

- The judge does not check that the driver enforces the preflight
  ran (it does not — see *Per-diff stage checks* in
  `specs/gate.md`).
- The judge does not check that the preflight appears in any other
  template; `loop.md` is the bead-container worker prompt and the
  only template this contract pins.
- The judge does not require a specific section heading — the
  instruction may live under *Quality Gates*, *Instructions*,
  *Land the Plane*, or any equivalent location, as long as it
  appears before the progress-marker partial include.
