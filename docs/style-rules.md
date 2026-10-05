# Style Rules

Rules the Judge enforces mechanically. Every rejection must cite a rule by ID.
Rules not listed here cannot be used to reject — flag unlisted concerns for the
Mayor via `bd human` instead.

## Shell (SH-)

- **SH-1** — _Every script must start with `set -euo pipefail`._
- **SH-2** — _All variable expansions must be quoted._ `"$var"`, not `$var`
  (exception: intentional word-splitting with a comment explaining why).
- **SH-3** — _Use `[[ ]]` for conditionals, not `[ ]`._
- **SH-4** — _Use `$(command)` for substitution, not backticks._
- **SH-5** — _Functions use `local` for all variables except intentional
  exports._
- **SH-6** — _Never silently suppress errors._ `2>/dev/null || true` is banned
  without a comment explaining which specific error is expected and why it's
  safe to ignore. Prefer precondition checks (`if [ -f ... ]; then`) over
  try-and-ignore. If an operation is genuinely best-effort (e.g. notifications),
  comment `# best-effort: <why>` on the same line. Treat `|| true` like an empty
  `catch {}` — it must justify what it's catching.

## Nix (NX-)

- **NX-1** — _Use `inherit` to pull names from enclosing scope; do not repeat
  `x = x`._ For `lib.*` specifically: if a Nix file calls `lib.elem`,
  `lib.optional`, or any `lib.X` more than once, lift the names with
  `inherit (lib) elem optional X;` at the top of the enclosing `let` block (or
  via destructuring on the module's argset) and use them bare. Single-use
  accessors can stay as `lib.X` inline. Repeated `lib.` prefixes are noise; the
  `inherit` form puts the imported surface up top where it's easy to audit.
- **NX-2** — _Files must pass `nix fmt` (nixfmt-rfc-style) with no diff._
- **NX-3** — _Keep derivations pure._ No `builtins.fetchurl` without a hash, no
  `builtins.currentSystem`.
- **NX-4** — _Attrset arguments use `{ a, b, ... }:` destructuring, not
  `args: args.a`._

## Documentation (DOC-)

- **DOC-1** — _New specs go in `specs/` and must be added to the spec index in
  `docs/README.md` (the session-start pin)._
- **DOC-2** — _Architecture references point to `docs/architecture.md`, not
  `specs/`._
- **DOC-3** — _Terminology references point to `docs/README.md`._
- **DOC-4** — _Spec contract changes (additions, rewrites, reconciliations)
  require human authorization — inline for conversational drivers,
  `LOOM_CLARIFY` for in-loom agents._ Retargets, typos, and renames are routine.

## Git (GIT-)

- **GIT-1** — _Commit messages are imperative mood, max 72 chars for the subject
  line._
- **GIT-2** — _No secrets, credentials, or API keys in committed files._
- **GIT-3** — _Hidden specs (e.g. files in `.git/info/exclude`'d paths) must
  never be copied into `specs/`._

## Testing (TST-)

- **TST-1** — _Tests must execute the code under test, not grep for strings in
  source._
- **TST-2** — _Mock external dependencies (podman, network), not internal
  logic._
- **TST-3** — _Each test function tests one behavior and has a descriptive
  name._
- **TST-4** — _Any PR that updates an `insta` snapshot under
  `*/tests/snapshots/` must include a `snapshot updated because: <reason>` line
  in the PR description._ Snapshots cover contract surfaces (Askama templates,
  CLI `--help`); silent diffs there are accidental drift, not a deliberate
  change.
- **TST-5** — _Every spec contract maps to verified acceptance._ Every
  behavioral contract section and every behavioral lifecycle / decision /
  contract table row maps to annotated criteria in the same package's
  `tests.md`, per
  [Spec conventions — Contract coverage](spec-conventions.md#contract-coverage).
  Bindings stay beside those criteria; `spec.md` does not duplicate annotations.
  Before the package cutover, use the same owner's inline Success Criteria and
  local section links under the conventions' bootstrap authoring rule. An
  unrelated annotation or equal row/annotation counts do not establish coverage.
  `loom gate verify` checks annotation integrity under Gate's resolution and
  pending rules; planning and `loom gate review` assess whether the mapped
  criteria cover the behavior. Findings include:
  - **Annotations:** missing, multiple, malformed, or unresolved bindings under
    the applicable pending policy.
  - **Tables:** behavioral rows not covered by mapped acceptance criteria.
  - **Normative prose:** behavioral claims absent from the mapped criteria, even
    when another claim in the section has a verifier.
- **TST-6** — _Orphan walk-tier verifiers are flagged._ A consumer's
  `[check]`-tier walk binary (e.g. one that takes named walks as positional
  args) must accept only walks referenced by some spec annotation. Walks
  declared in the binary but never annotated are caught by `loom gate verify`.
  Delete them, or wire a spec annotation that uses them.
- **TST-7** — _Removals demonstrate their replacements work._ When a change set
  deletes a test, code path, schema element, or file paired with a "replaces" /
  "replaced by" / "supersedes" claim (commit message, code comment, or PR
  description), the replacement's verifier must land in the same change set.
  `loom gate check` scans staged diffs and fails pre-commit on:
  - test-function or verifier-annotation deletions paired with replacement-claim
    text where the change set adds no corresponding new test or annotation
  - schema migrations containing `ALTER TABLE … DROP COLUMN` or equivalent
    removal patterns where the change set still leaves source references to the
    removed element
- **TST-8** — _Test infrastructure is itself tested._ Stubs (stub agents,
  fixture helpers, `wrix spawn` fakes, JSONL fakes) must carry at least one
  conformance test asserting the stub's observable contract matches the real
  implementation. `loom gate` audits (verifier, review, integrity-gate walks)
  must each carry a self-test with synthetic fixtures that exhibit the violation
  and assert the audit fires. Without these, bugs in the test infrastructure or
  audit layer silently invalidate every downstream test. `loom gate check`
  enforces this.
- **TST-9** — _Cross-spec terms are consistent._ When a CLI flag, bead label,
  event variant, schema element, environment variable, or other named term
  appears in multiple specs in `specs/`, the meaning, type, and behaviour must
  match. `loom gate check` walks shared terms across `specs/` and flags
  definitions that diverge.

## Rust (RS-)

### Workspace conventions (RS-1..RS-3)

- **RS-1** — _Edition 2024 + `resolver = "3"` at the workspace root._ Every
  crate inherits via `edition.workspace = true`, `version.workspace = true`,
  etc. No per-crate pins.
- **RS-2** — _Workspace-pinned dependencies._ Every third-party crate is pinned
  exactly once under `[workspace.dependencies]`. Member crates consume with
  `foo = { workspace = true }`.
- **RS-3** — _All lint configuration lives at the workspace root._ Two files own
  the surface.

  `Cargo.toml` → `[workspace.lints.rust]` + `[workspace.lints.clippy]`. Enable
  clippy groups (`all`, `pedantic`, `nursery`) at `warn` with `priority = -1`,
  then layer targeted overrides:
  - Opt in to restriction lints that enforce the production bans (`unwrap_used`,
    `expect_used`, `panic`, `todo`, `unimplemented`, `unreachable`, `dbg_macro`,
    `print_stdout`, `print_stderr`).
  - Set `unsafe_code = "forbid"` under `[workspace.lints.rust]`.
  - `= "allow"` pedantic/nursery lints that don't earn their keep (`use_self`,
    `must_use_candidate`, `option_if_let_else`, `too_many_lines`,
    `unused_async_trait_impl`; async trait signatures require async
    implementations even when a method needs no await).

  Every member declares `[lints] workspace = true`. No crate-root
  `#![warn(...)]` / `#![deny(...)]`. CI runs `cargo clippy -- -D warnings`.

  `clippy.toml` (workspace root) sets the test exemptions clippy supports
  natively:

  ```toml
  allow-expect-in-tests = true
  allow-panic-in-tests  = true
  allow-unwrap-in-tests = true
  allow-print-in-tests  = true
  allow-dbg-in-tests    = true
  ```

  Restriction lints without an `allow-*-in-tests` flag (`clippy::todo`,
  `clippy::unimplemented`, `clippy::unreachable`) stay warned everywhere — tests
  have no legitimate use for them.

  Per-site `#[expect(...)]` / `#[allow(...)]` overrides in production are a
  high-friction escape hatch. Each requires a substantive `reason = "..."`
  naming the specific invariant being violated and why this site cannot meet it.
  Generic reasons (`"for now"`, `"TODO"`, `"tests use panicking helpers"`) are
  rejected at review. Audit: `rg '#\[(expect|allow)' -trust`.

### Type discipline (RS-4..RS-8)

- **RS-4** — _Per-module error enums via `thiserror` + `displaydoc`._ Messages
  go in doc comments — never `#[error("...")]` attributes. A `#[from]` variant's
  `Display` must NOT interpolate the source (no `{0}`); it carries this layer's
  context only. The chain renders at the print site: `{err:#}` for CLI sinks
  (anyhow's chain-walk); `?err` or `format!("{err:#}")` for `tracing` fields —
  **never `%err`**, which uses `Display` and drops the cause.
- **RS-5** — _Nested directory module structure._ No central `types.rs` or
  `error.rs`. Types and errors live in the module that owns them. `lib.rs` has
  `pub mod` declarations only.
- **RS-6** — _Parse, Don't Validate._ Raw bytes and strings are parsed into
  typed, constrained representations at the boundary; downstream code never
  touches raw input.
- **RS-7** — _Newtypes for identifiers._ No bare `String` for IDs, codes, or
  domain numbers. Each newtype validates at construction; invalid input becomes
  a typed parse error, never a wrapped invalid value.
- **RS-8** — _Never `derive(From)` / `derive(Into)` on newtypes._ It bypasses
  validation. Use `#[from]` only on error enum variants.

### Banned in production (RS-9..RS-14)

- **RS-9** — _No panicking macros in production code._ `unwrap()`, `expect()`,
  `panic!()`, `todo!()`, `unimplemented!()`, and **`unreachable!()`** are banned
  in non-test code. Return an error variant instead —
  `Error::Unimplemented { ... }` for stubs, `Error::Bug { ... }` for "can't
  happen" branches. "Can't reach here" is a proof obligation for the type
  system, not a runtime macro. Enforced mechanically via the RS-3 clippy
  restriction lints.
- **RS-10** — _`#[allow(dead_code)]` is banned._ Use
  `#[expect(dead_code, reason = "...")]` so the compiler tells you when the
  expectation is unfulfilled.
- **RS-11** — _No silent error swallows._ `Result::ok()`, `let _ = result`, and
  `.unwrap_or_default()` on `Result` discard error information and let
  downstream code mistake failure for an empty value. Errors must be either
  handled with user-visible feedback or propagated with `?` after mapping to a
  typed variant. Exceptions: provably infallible sinks (`write!` to `String`,
  `writeln!` to `Vec<u8>`) with an inline comment naming why the sink can't
  fail.

  _Trap to watch:_ `mutex.lock().ok()?` turns "another thread panicked holding
  this mutex" into "no value" — map to a `LockPoisoned` variant and `?` instead.

- **RS-12** — _No placeholder / sentinel values in production data types._ Every
  instance of a production type must be valid on its own; "must be overwritten
  before use" is a type-system failure, not a runtime contract. If a value has
  two lifecycle stages (parser-emitted vs. session-stamped, partial vs.
  complete, etc.), give each stage its own type and let the constructor that
  combines them be the only path to the downstream type (see RS-13 for the
  test-only escape hatch). The compiler then makes "unstamped value reaches a
  consumer" unrepresentable. Applies to newtypes _and_ structs _and_ enums —
  anywhere production code can hold the value.
- **RS-13** — _`Default` only when the zero-value is safe to use._ If a value
  must be overwritten before use, it's not a default. The named-constructor
  escape hatch (`placeholder()`, `pending()`, `for_test()`) is for
  `#[cfg(test)]` constructors only — production code does not get a "poison
  instance with an honest name" loophole; it must split the type per RS-12.
  `Type::default()` must mean "the caller accepts the default", not "I owe a
  real value later".
- **RS-14** — _No test-fixture shape in production trait impls._ Test-only
  constructors live behind `#[cfg(test)]` (or in `tests/common/`); production
  types do not carry shape that exists only to satisfy test fixtures.

### Logging (RS-15)

- **RS-15** — _Use `tracing` with structured fields._ Log level signals whether
  processing continued:
  - `error!` — operation failed and did not continue
  - `warn!` — something went wrong but processing continued
  - `info!` — normal operational events (startup, request received)
  - `debug!` / `trace!` — development diagnostics
  - `error!(target: "loom::bug", ...)` — invariant violation that should never
    happen

  Every log carries structured fields identifying what was being processed
  (`%bead_id`, `%spec_label`, etc.). Variable _names_ may be logged;
  environment-variable values, tokens, and API keys are never logged — wrap any
  secret-bearing value in a `Redacted(&str)` whose `Debug` impl prints
  `[REDACTED]`.

### Naming (RS-16)

- **RS-16** — _Avoid stutter naming._ Don't repeat a module's name in its type
  or function names: prefer `event::Envelope` over `event::EventEnvelope`,
  `state::Db` over `state::StateDb`. Clippy's `module_name_repetitions`
  (pedantic, via RS-3) catches accidental cases. Per-site
  `#[expect(clippy::module_name_repetitions, reason = "...")]` is allowed when
  the longer name reads better at the call site (e.g. re-exported at crate root
  where the module prefix isn't visible), but the `reason` must name that
  call-site context.

### Closed sets (RS-17)

- **RS-17** — _Enums for closed sets, not strings._ A parameter or field whose
  valid values are a fixed set owned by this codebase (note `kind`,
  `driver_kind`, render mode, log level, etc.) must be an enum, never `&str` /
  `String`. The obverse of RS-7: identifiers are open sets parsed at the
  boundary; closed sets are enumerated by the type system so the compiler
  catches typos and refactors are mechanical via exhaustive `match`. For
  wire-additive cases where unknown variants must round-trip (forward compat
  across versions), the enum carries an `Other(String)` (or `#[serde(other)]`)
  fallback arm — the Rust API stays closed at the call site, the wire stays
  open.

### Module naming (RS-18)

- **RS-18** — _Crate and module names use the singular noun._ A module is one
  unit of organization, named for what it organizes; the count of items it
  contains doesn't determine the name. Prefer `gate` over `loom-gates`, `walk/`
  over `walks/`, `judge/` over `judges/`. Mass nouns (`metadata`, `data`) and
  abbreviations (`io`, `os`) are exempt — they aren't pluralizable.

### Simplification mandate (RS-19)

- **RS-19** — _Favor removing code over adding it._ When implementing a change,
  look for surrounding code that's now unused, redundant abstractions that no
  longer earn their place, or duplicated logic that can collapse. A successful
  change often has a net-negative LOC delta. Code volume tracks one-to-one with
  cognitive load and the surface area for whole-class bugs (gate skips, silent
  drift, unrepresented state). At review time, ask "what got removed?" alongside
  "what got added?"; in spec interviews, prefer the smaller mechanism over the
  more general one; in code, prefer deleting an abstraction over keeping it "for
  future use."

## Comments (COM-)

Cross-language: applies to shell, nix, rust, and any other source we keep here.

- **COM-1** — _Inline comments inside function bodies are a code smell._
  Restructure first: a clearer name, a smaller function, a typed boundary, an
  enum variant — these survive renames and refactors. Comments rot. When an
  inline comment is genuinely the right answer (non-obvious invariant,
  workaround for a specific bug, citation of a spec/RFC/issue), keep it to one
  line. Multi-line prose blocks explaining what code does are banned.
- **COM-2** — _Doc comments describe contract, not implementation._ Module-,
  type-, function-, and public-API doc comments are fine when they describe what
  callers can assume and what invariants hold. They are _not_ the place to
  explain what the code does — that's the code's job. Keep them concise; no
  multi-paragraph prose. Doc-comment messages on `thiserror` error variants
  follow RS-4 (`Display` text in doc comments, not `#[error("...")]`).
- **COM-3** — _No volatile coordinates in comments._ A comment must not
  reference content that moves independently of the comment. Banned: line
  numbers in any file (`see foo.rs:123-145`), call-site lists
  (`used by X, Y, Z`), commit SHAs, bead/issue IDs in code, decision-log refs
  (`per D2`), "added for the X flow". These rot silently because nothing breaks
  when the target moves. Acceptable references: a stable section name when
  load-bearing, an RFC/spec citation that names the document not its layout
  (`per RFC 7230 §3.2`, `per specs/events.md Event Schema`). Best is no
  cross-reference at all — describe what the code itself asserts.

## CLI (CLI-)

Cross-language: applies to any user-facing CLI we ship.

- **CLI-1** — _Command help text is one short sentence describing current
  behavior._ The single-line help string visible in `<tool> --help` and
  `<tool> help <cmd>` lists is for orienting users, not documenting internals.
  Banned: implementation details (storage backends, frameworks, file formats),
  migration history ("replacement for the deprecated …"), decision references
  ("per D2"), bead/issue IDs, and any other COM-3 volatile coordinate. One
  sentence, end with a period. Long-form context belongs in the spec.
