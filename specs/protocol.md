# Agent-output protocol

Defines one typed message family, decoder, and phase-admission contract for
agent output, with mechanically checked prompt agreement.

## Problem Statement

Independent marker scanners and handwritten instructions can disagree about what
agents should emit and what Loom accepts. A common protocol removes those
mechanical differences without confusing syntactic decoding with contextual
admission, human decisions, or trusted workflow effects.

## Architecture

`loom-protocol::output::Message` is the canonical public Rust enum for
agent-output messages. Marker spelling, message role, payload arity, decoding,
and phase projections come from this contract, not separately maintained
terminal enums, token-string registries, or per-phase scanners. Typed domain
payloads remain owned by their domains; they are not generic JSON blobs.

The pipeline is:

```text
agent-origin text
  -> shared framing and typed Message decoding
  -> phase admission
  -> domain/context resolution
  -> trusted workflow effects
```

Decoding performs no Beads writes and grants no execution or publication
permission. Findings still resolves `RawFinding` into immutable `Finding`; Todo
validates its fixed preflight roster and persists accepted bindings; Loop
reconciles worker completion and scheduling state; Inbox preserves human-owned
resolution and separately admits tune application.

This owner adds no crate or workflow CLI command. [Harness](harness.md) owns the
leaf-crate dependency floor and wire-versioning policy.
[Templates](templates.md) owns prompt composition and delivery.
[Findings](findings.md), [Todo](todo.md), [Loop](loop.md), [Inbox](inbox.md),
and [Plan](plan.md) own phase behavior.

Acceptance: [canonical contract](#canonical-contract) and
[consumer conformance](#consumer-conformance).

## Message contract

[Acceptance](#canonical-contract).

Two independent dimensions describe each variant:

- **Role:** a nonterminal record or a terminal outcome proposal.
- **Payload arity:** a unit variant or a data-carrying variant.

Terminality does not require a JSON object. A unit variant has one bare
spelling; a data-carrying variant has one marker-plus-JSON spelling. Neither
accepts the other encoding or an arbitrary optional payload.

| Rust variant          | Marker          | Role     | Payload                                                                      |
| --------------------- | --------------- | -------- | ---------------------------------------------------------------------------- |
| `Finding(RawFinding)` | `LOOM_FINDING`  | Record   | Findings-owned typed JSON object                                             |
| `Clarify`             | `LOOM_CLARIFY`  | Record   | `{"decisions":["<bead-id>",...]}`; nonempty checked `BeadId` references      |
| `Complete`            | `LOOM_COMPLETE` | Terminal | Unit                                                                         |
| `Noop`                | `LOOM_NOOP`     | Terminal | Unit                                                                         |
| `Waiting`             | `LOOM_WAITING`  | Terminal | Unit; prerequisites are durable Beads state                                  |
| `Concern`             | `LOOM_CONCERN`  | Terminal | `{"summary":"<nonblank text>"}`                                              |
| `Retry`               | `LOOM_RETRY`    | Terminal | `{"reason":"<nonblank text>"}`                                               |
| `Blocked`             | `LOOM_BLOCKED`  | Terminal | `{"reason":"<nonblank text>"}`                                               |
| `Todo(TodoSuccess)`   | `LOOM_TODO`     | Terminal | Todo-owned typed JSON object                                                 |
| `Apply`               | `LOOM_APPLY`    | Terminal | `{"proposals":["<bead-id>",...]}`; nonempty checked tune-proposal references |

Checked construction and deserialization preserve required nonblank text,
nonempty reference collections, and valid identifiers. JSON syntax alone does
not resolve a reference, validate an Options brief, establish task acceptance,
or authorize an apply batch. Domain schemas retain their own additional
constraints.

## Framing

[Acceptance](#framing-and-errors).

A live message starts in column one of an agent-origin text line. Unit messages
occupy that line, allowing trailing whitespace. Data messages use the exact
case-sensitive marker followed by `:` and one JSON object; whitespace after the
colon and ordinary JSON whitespace inside the object are allowed. After the
object's closing `}`, the rest of that physical line must be whitespace only; a
second object, marker, fence, or prose suffix is an error. Commentary may resume
on a later line after a nonterminal record, never after a terminal.

The decoder distinguishes live messages from ordinary commentary, inline marker
mentions, Markdown examples, and payload content. Code-fenced examples,
prose-prefixed or decorated markers, and marker-looking text inside a JSON
string are not live messages. Tool output, prompt echoes, driver status records,
and event-log wrappers are not promoted into agent-origin messages.

Outside those excluded contexts, column-one `LOOM_`-prefixed marker tokens
reserve the protocol namespace. An unknown marker is a typed protocol error, not
ordinary commentary, even when a valid terminal follows. A misspelled record
cannot silently disappear into an otherwise clean result.

Compact JSON is recommended, not required. Valid pretty-printed objects span
physical lines but remain one logical message. Embedded string newlines, quotes,
and control characters require JSON escaping. The decoder does not repair raw
string newlines, control characters, trailing fences, or other malformed JSON.

A session has exactly one admitted terminal, as its final logical message, with
no trailing non-whitespace text. Nonterminal records may precede it. An earlier
terminal, duplicate terminal, record after terminal, missing terminal, wrong
payload arity, or malformed recognized message is a typed error, not success.
Interactive phases emit their terminal only on the final assistant turn;
ordinary conversational turns are not individually required to terminate.

Malformed output retains original text and source spans, all independently
decoded valid records, and any independently established terminal/context.
Recovery cannot invent a boundary inside an unterminated string or promote an
ambiguous suffix into a valid terminal. Partial context is diagnostic, not a
successful admitted session. Existing phase-specific bounded recovery or
interactive diagnostics consume it; the common decoder introduces no new retry
policy.

## Phase admission

[Acceptance](#phase-admission-1).

Each phase uses a projection of the same enum and rejects otherwise well-formed
messages that do not belong to that phase. The table defines protocol admission,
not permission to perform the outcome's trusted effects.

| Phase  | Allowed records | Allowed terminals                                 |
| ------ | --------------- | ------------------------------------------------- |
| Plan   | None            | `Complete`                                        |
| Todo   | `Clarify`       | `Todo`, `Waiting`, `Retry`, `Blocked`             |
| Loop   | `Clarify`       | `Complete`, `Noop`, `Waiting`, `Retry`, `Blocked` |
| Review | `Finding`       | `Complete`, `Concern`, `Retry`, `Blocked`         |
| Inbox  | None            | `Complete`, `Apply`                               |

Review keeps explicit concern signaling and Findings' stream/terminal pairing.
Reviewers remain inspection-only: decision-worthy review concerns use a
clarify-route finding, not direct decision-bead creation or `Clarify` records.

Todo/Loop `Clarify` records report decisions independently of the emitter's
terminal outcome. Multiple records aggregate by exact `BeadId` set union,
including repeated references within a record. A repeated ID identifies one
decision, not another queue item. Every reference remains subject to contextual
admission; nonexistent, malformed, ambiguous, or out-of-scope references are
errors, never silently discarded. Reporting all currently discovered unresolved
decisions is an agent obligation; the decoder does not prove that the agent has
discovered every possible ambiguity.

Parentage records discovery provenance. Actual dependency edges determine which
work waits. Reporting a decision does not inherently block its emitter, and
resolving a decision does not prove implementation complete. The scheduling and
resolution contracts live in
[Loop](loop.md#decision-batches-and-attributed-waits) and
[Inbox](inbox.md#decision-beads-and-resolution).

## Success Criteria

### Canonical contract

- One public `loom-protocol::output::Message` enum represents every variant,
  role, and payload in the message table with checked identifiers, nonempty
  references, and required nonblank text; arbitrary JSON blobs and invalid
  construction cannot substitute for those typed payloads.
  [test](canonical_agent_output_message_contract_is_constructible)

- Independent literal wire fixtures pin every marker's unit/data shape and
  decoded variant. Missing or extra payloads and invalid required fields fail;
  serialization round trips alone do not define the accepted language.
  [test](canonical_agent_output_wire_fixtures_pin_variant_shapes)

### Framing and errors

- Shared decoding recognizes live column-one messages, not inline mentions,
  decorated markers, code-fenced examples, or marker-looking payload content.
  Unknown live `LOOM_`-prefixed markers fail even when followed by a valid
  terminal; malformed recognized JSON fails without newline/control-character
  repair. Non-whitespace suffixes on the object's closing line fail rather than
  being reclassified as commentary or another message.
  [test](shared_decoder_enforces_root_line_framing_and_strict_json)

- Valid compact and pretty-printed JSON decode to the same message, including
  correctly escaped multiline evidence; terminal placement uses the complete
  logical object rather than its final physical line.
  [test](shared_decoder_treats_multiline_payload_as_one_logical_message)

- Admitted sessions have exactly one final logical terminal. Missing, duplicate,
  earlier, or trailing-text terminals and records after terminal fail without
  manufacturing success from a suffix.
  [test](shared_decoder_enforces_terminal_cardinality_and_position)

- Errors retain raw text, source spans, valid decoded context, and an
  independently established terminal when available. Unterminated payloads
  cannot turn ambiguous interior text into a live message or passing result.
  [test](shared_decoder_retains_valid_context_with_raw_errors)

### Phase admission

- Every cell of the phase-admission table is exercised against the canonical
  decoder: permitted records and terminals are admitted, wrong-phase messages
  are rejected, and decoding alone performs no workflow mutation.
  [test](canonical_phase_admission_rejects_wrong_message_roles)

- Multiple `Clarify` records aggregate the exact decision-ID union without
  duplicate queue identity or silent loss of invalid references; reporting is
  independent of the emitter's final outcome.
  [test](clarify_records_aggregate_exact_decision_ids)

### Consumer conformance

<!-- prettier-ignore -->
- Production agent-marker consumers and domain parser entry points use the
  shared decoder and canonical message vocabulary, with no independent legacy
  terminal enum, substring scanner, reason/question prose scraper, or JSON
  repair path. [check?](cargo run -p loom-walk -- agent_output_single_decoder)

- Production phase entry points exercise the shared decode/admission path with
  controlled external agent output, retain context on malformed messages, and
  do not interpret prompt/tool/driver text as emitted agent messages.
  [test?](production_phase_consumers_enforce_shared_agent_output_contract)

## Out of Scope

- Backward-compatible acceptance of bare data-bearing self-reports, terminal
  `Clarify`, per-phase framing quirks, or malformed-JSON repair. The unified
  contract is a breaking wire change under Harness's versioning policy.
- A universal envelope marker, generic capability registry, tools/MCP protocol,
  or separate state channel. Typed payloads and existing workflow authorities
  remain distinct.
- Unifying backend RPC, `AgentEvent`, driver-authored finding-status records,
  deterministic verifier verdicts, or publication receipts with agent messages.
  These have different producers and trust boundaries.
- Changing native interactive launch/transport contracts merely to capture text;
  wherever marker output is consumed, the common contract applies.
- A Markdown attachment/reference protocol, duplicate evidence narration, or new
  rendering UI. Existing persistence and Inbox presentation consume decoded
  content; ordinary commentary remains allowed before the terminal.
- Proving semantic findings, adequate task decomposition, or correct human
  choices from parser/prompt agreement. Those remain contextual and semantic
  review obligations, not consequences of valid JSON.
