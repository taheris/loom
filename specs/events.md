# Events and Rendering

Defines the typed event stream, command-wide live/replay renderer, persisted
JSONL event logs, and diagnostic tracing boundary.

## Problem Statement

Loom drives long-running host and agent work that operators need to watch
without reading raw JSON or Rust tracing noise, and operators need to see the
text Loom sends to LLM-backed agents rather than infer it from final summaries.
The durable primitive is the `AgentEvent` stream: agents and the host driver
emit events, renderers turn those events into human timelines, disk logs persist
them for replay, and machine consumers parse the same JSONL stream.

## Architecture

Events are the source of truth. A live `loom loop` session, a saved
`.loom/logs/*.jsonl` file, a `loom logs` replay, and any external consumer
observe the same `AgentEvent` values in the same order. Logs are persisted
copies of the event stream, not a separate state model.

### Crate Boundaries

`loom-events` is the public contract crate. It owns `AgentEvent`, the common
envelope, identifier newtypes, `DriverKind`, the `Session` interoperability
trait, `EventSink`, and `SessionCommand`. Its leaf dependency constraint is
owned by [harness.md § Dependency Graph](harness.md#dependency-graph).

`loom-render` owns human and machine renderers plus the `LogSink` event sink.
It depends on `loom-events` for the public event contract and may use
rendering/support crates, but it must not depend on `loom-driver` or
`loom-workflow`. Those crates produce or route events; they do not own the event
schema or renderer contract.

### Single Renderer Pipeline

Formatted live output, formatted `loom logs` replay, and `--json` event output
share one event-consuming pipeline. `Raw` is the only pass-through branch: it
copies compact JSONL event bytes without parsing or formatting. A live-only
formatter, a replay-only formatter, or a second ad-hoc log view is out of
scope.

An **LLM-bearing command** is a non-interactive command path that spawns an
agent backend, Direct conversation, LLM rubric, or tuning evaluator whose
conversation is driven by Loom rather than an inherited interactive UI. Every
LLM-bearing command attaches this pipeline live: `loom loop`, `loom todo`,
LLM-spawning gate inspection commands, `loom gate audit`, `loom gate mint
--tree`, and tune runs that spawn LLMs. The command may add command-specific
summaries before or after the stream, but it must not buffer the agent
transcript internally while stdout appears idle. Interactive `loom plan` and
`loom inbox chat` use their inherited stdio/native UI surfaces instead of this
live renderer.

The `LogSink` tee is the standard fan-out: one `emit(&AgentEvent)` writes the
JSONL event and drives the selected renderer, when a renderer is attached.

## Event Schema

`AgentEvent` is a flat tagged JSON object. Each line in a persisted log is one
complete JSON object terminated by `\n`. Consumers dispatch on the top-level
`kind` field; there is no nested event envelope.

### Common Envelope

Every event carries these top-level fields in addition to its payload. `?` means
nullable or omitted according to the serde shape.

| Field | Type | Purpose |
|-------|------|---------|
| `kind` | `string` | Snake-case event discriminator. |
| `session_id` | `string` | Stable event-session routing key; SSE resume key is `<session_id>:<seq>`. |
| `bead_id` | `string?` | Beads item id when the session is tied to a task or epic; absent for standalone phase logs. |
| `molecule_id` | `string?` | Optional work-epic grouping key. |
| `iteration` | `u32?` | Work-epic iteration counter when the session is tied to a work epic. |
| `source` | `"agent" \| "driver"` | Distinguishes backend activity from host-driver activity. |
| `ts_ms` | `i64` | Unix milliseconds UTC. |
| `seq` | `u64` | Monotonic per event session. |

### Variant Set

The event stream contains these canonical variant groups:

- **Lifecycle** — `agent_start`, `agent_end`, `turn_start`, `turn_end`,
  `session_complete`
- **Inputs** — `agent_input`
- **Streaming** — `text_delta`, `text_end`, `thinking_delta`,
  `thinking_end`, `toolcall_delta`
- **Tools** — `tool_call`, `tool_result`, `tool_progress`
- **Operational** — `compaction_start`, `compaction_end`, `auto_retry`,
  `error`
- **Driver catch-all** — `driver_event`

Payload fields are also top-level fields on the same flat JSON object. Payload
`?` markers also mean nullable or omitted according to the serde shape.

| Event | Payload fields | Meaning |
|-------|----------------|---------|
| `agent_start` | `schema_version`, `title`, `profile`, `spec_label`, `started_at_ms`, `parent_tool_call_id?` | First event in a session; carries renderer/replay metadata. |
| `agent_input` | `input_kind`, `text`, `redactions?` | Loom-authored text sent into the backend session, emitted before the send. |
| `agent_end` | — | Agent session lifecycle bookend paired with `agent_start`. |
| `turn_start` | — | A multi-turn session opened a new turn. |
| `text_delta` | `text` | Streaming assistant prose fragment. |
| `text_end` | — | Closes a `text_delta` stream. |
| `thinking_delta` | `text` | Streaming thinking fragment when the backend exposes it. |
| `thinking_end` | — | Closes a `thinking_delta` stream. |
| `toolcall_delta` | `id`, `delta` | Streaming tool-call argument fragment. |
| `tool_call` | `id`, `tool`, `params`, `parent_tool_call_id?` | Agent invoked a tool. |
| `tool_result` | `id`, `output`, `is_error` | Tool execution completed. |
| `tool_progress` | `id`, `text` | In-flight progress update from a long-running tool. |
| `turn_end` | — | One turn finished; the session may have more turns. |
| `session_complete` | `exit_code`, `cost_usd?` | Process exit or final result observed. |
| `compaction_start` | `reason` | Context compaction began. |
| `compaction_end` | `aborted` | Context compaction finished or was abandoned. |
| `auto_retry` | `attempt`, `max_attempts`, `delay_ms`, `error_message` | Backend signaled an auto-retry attempt. |
| `error` | `message` | Agent reported a mid-stream error. |
| `driver_event` | `driver_kind`, `summary`, `payload` | Host-driver activity carried in the event stream. |

`agent_input.input_kind` is `initial_prompt`, `follow_up`, `steer`, or
`repin`. `agent_input` events use `source: "driver"` because the host emits the
exact text it is about to send to the backend. Secret redaction, when required
by security policy, happens before event emission; `redactions` records explicit
redaction markers or classes rather than silently omitting the input.

`compaction_start.reason` is `context_limit`, `user_requested`, or `unknown`.
Backend-specific compaction reasons map into that closed set; unknown native
reasons map to `unknown`.

The spec owns the architectural shape: flat tagged event, common envelope,
stable variant and payload field names, and source separation.

### Schema Versioning

`agent_start.schema_version` names the event-log schema version for that
session. Adding new fields, variants, or `driver_kind` values is additive when
old consumers can ignore or generically render the unknown data. Renaming,
removing, or repurposing existing fields is a breaking schema change and
requires a version bump. Consumers that cannot support a schema version fail
loudly rather than silently mis-rendering a log.

### Architecture-Bearing Types

- **Session lifecycle.** The backend-neutral session contract is prompt /
  steer / cancel / mode commands producing an `AgentEvent` stream. Internal
  backends may keep typestate for subprocess handshakes, but consumers see the
  typed session surface.
- **Identifier newtypes.** `SessionId`, `BeadId`, `MoleculeId`, `ToolCallId`,
  `SpecLabel`, `ProfileName`, and related ids parse at the boundary; downstream
  code receives typed ids rather than raw strings.
- **Parser-to-stamper split.** Backend parsers produce `ParsedAgentEvent`
  payloads. Driver-origin event producers and backend parsers both hand
  payloads to the event-stamping layer; that layer is the only constructor of
  full `AgentEvent` values because it owns bead identity, source, timestamp,
  and sequence.
- **InputKind.** `agent_input.input_kind` is a closed enum for Loom-authored
  session inputs. It makes prompt / follow-up / steer / re-pin distinctions
  explicit without encoding them as untyped driver payload strings.
- **DriverKind.** `driver_event.driver_kind` is a forward-compatible wire
  string. Producers use a typed enum with an `Other(String)` fallback so known
  kinds cannot be misspelled and unknown future kinds still deserialize.
- **EventSink.** `EventSink::emit(&AgentEvent)` is synchronous, takes a shared
  event reference, and composes with `.tee(other)`. `react()` is pull-based and
  is invoked only after non-streaming events; returned `Abort` commands are
  terminal before any later command in the same batch.

### Driver Events

Driver events describe host-side work: bead dispatch, container spawn/OOM,
verdict routing, retry dispatch, gate runs, push-gate walks/refusals/clean
passes, bd state transitions, observer signals, stall watchdogs, token usage,
and Direct output offload. They use `source: "driver"` and render as driver
rows in the same chronological timeline as agent output.

Known `driver_kind` wire values include `verdict_gate`, `retry_dispatch`,
`push_gate_walk`, `push_gate_refuse`, `push_gate_clean`, `container_spawn`,
`container_oom`, `infra_failure`, `stall_watchdog`, `token_usage`, `offload`,
`duplicate_tool_result`, `doom_loop_tripped`, `epic_auto_closed`,
`bead_branch_pushed`, `merge_ok`, `merge_conflict`, `integration_conflict`,
`signature_verification_failed`, `worktree_cleanup_ok`, `tree_not_clean`,
`workspace_recovery`, `marker_routed`, `clarify_downgraded`, and
`bd_state_transition`.
Gate lifecycle values carried by the same field are `gate_run_start`,
`gate_run_scope`, `gate_run_lane`, `gate_run_end`, and `gate_run_skipped`;
[gate.md](gate.md) owns their GateRun semantics. Unknown future strings must
round-trip as `DriverKind::Other` and render generically.

## Rendering UX

The live renderer is the normal operator view for every non-interactive
LLM-bearing command. `loom loop` remains the primary live view, but `loom todo`,
LLM-spawning gate commands, `loom gate mint --tree`, and tune runs use the same
transcript surface. The default TTY renderer is a Pi-style transcript:
Loom-authored inputs, assistant text, thinking/reasoning text when exposed,
tool calls/results, and driver progress read as a chronological conversation
instead of repeated log labels. Pretty mode may use color, background, and glyph
styling; the spec does not mandate box-drawing characters or an exact glyph
layout.

### Modes

| Mode | Selected when | Output shape |
|------|---------------|--------------|
| `Pretty` | TTY, no `--plain` / `--json` / `--raw`, and no `NO_COLOR` | Faithful human transcript with color/background/glyph styling, stateful tool blocks, LLM-visible text shown by default, and OSC 8 links where supported. |
| `Plain` | Non-TTY, `NO_COLOR`, or `--plain` | Same event content as Pretty, ASCII/no color/no OSC 8/no decorative line art. |
| `Json` | `--json` | One parsed event rendered as JSON per emission; no terminal chrome. |
| `Raw` | `--raw` | The compact JSONL event bytes; no parsing or formatting. |

`--plain`, `--json`, and `--raw` are `loom loop` renderer flags in v1. `loom
logs` exposes the replay-specific flag set below; it supports `--raw`, chooses
Pretty/Plain by TTY and `NO_COLOR`, and uses `-v` for diagnostic replay.

`-v` / `--verbose` is not required to see the real transcript. Normal
Pretty/Plain output shows text Loom sends to an LLM-backed agent and text the
backend exposes back to Loom. Verbose adds diagnostic event metadata — event
kind/sequence, ids, timestamps, backend-safe parameter JSON, and fuller driver
payload summaries — while preserving the same transcript visibility and tool
body caps. `loom loop --trace` is the separate diagnostic flag for raw Rust
tracing to stderr.

### Human Timeline

- `agent_input` content is visible by default. Initial prompts, follow-up
  prompts, steering messages, and compaction re-pins render with explicit input
  boundaries before the backend can produce output from them.
- Assistant `text_delta` content is visible by default and coalesced into
  readable prose rather than token-count bookkeeping or repeated `assistant`
  labels.
- `thinking_delta` prose is persisted and rendered by default in a distinct
  thinking/comment style when the backend exposes it. The renderer may still
  use transient activity indicators while no text is available, but replay does
  not emit permanent "thinking hidden" spam.
- `tool_call`, `tool_progress`, and `tool_result` for the same `ToolCallId`
  (`id` on the wire) render as one stable block. Running tools use the pending
  style; successful tools use the success style; failed tools use the error
  style. Bash exit status drives success/error coloring.
- Tool body rendering follows the LLM-visibility boundary. Where Loom controls
  the inline tool-output budget (Direct), the human renderer uses the same
  byte cap as `[direct].max_inline_bytes` (default 16 KiB) and no additional
  line cap. Text below the cap was sent inline and is shown; text above the cap
  is shown only as the inline head plus an explicit offload/truncation recovery
  path. For Pi and Claude, Loom cannot prove the backend's internal context
  boundary, so any renderer safety cap is labelled display-only and points to
  `loom logs --raw` for the observed event bytes.
- Cumulative tool-output snapshots are coalesced. If a backend reports progress
  as repeated full-output snapshots, the renderer prints only newly observed
  suffix content or updates the existing block; it must not print the same
  compiler/Nix output section repeatedly.
- Driver events are interleaved chronologically with agent events. Meaningful
  workflow state is visible by default across commands: log paths, prompt/send
  boundaries, stalls, retries, gate lanes, reviewer/rubric start and finish,
  minting outcomes, container failures, push blocks, merge conflicts, and useful
  breadcrumbs. Verbose renders fuller driver payload summaries. Token usage and
  cost appear in final/session summaries by default; detailed token usage rows
  are verbose-only.
- Under `--parallel N > 1`, each rendered line carries a bead prefix or compact
  bead marker, and bead start/end summaries print atomically. In-place spinners
  and mutable live tool blocks are disabled because multiple carriage-return
  regions do not compose.
- Ctrl-C/SIGINT collapses any in-place region and emits a clean interrupted
  closing row.

### Diagnostic Tracing Boundary

Rust `tracing` is diagnostic output, not the product UI. Normal and verbose
TTY output contain the rendered event timeline; verbose adds event metadata but
still does not dump raw Rust tracing. Rust tracing records about event
bookkeeping — for example token-count messages, raw event-name debug lines, or
parser counters — are trace-level diagnostics when they are emitted at all; the
corresponding `AgentEvent` payloads remain renderer input. Stall watchdogs
remain warning-level events because work is continuing after an abnormal silence
window; the renderer presents them as warning rows rather than repeated
timestamped terminal spam. `loom loop --trace` mirrors raw tracing diagnostics
to stderr for debugging Loom itself. No separate durable trace or driver-log
file is required by this spec; operationally relevant driver activity belongs
in structured `driver_event` records in `.loom/logs`.

## Persisted Logs and Replay

Loom writes the full raw JSONL event stream for every agent-bearing phase and
every gate run that does work, regardless of terminal mode. Bead `loom loop`
sessions use:

```text
.loom/logs/<spec-label>/<bead-id>-<utc-timestamp>.jsonl
```

Non-bead LLM sessions such as `loom todo`, gate review/rubric runs, and tune
runs use a phase log root under `.loom/logs/<phase>/`. Parallel batches never
interleave multiple beads or phase sessions into one file. Each emitted event is
flushed so `tail -f`, file-watcher bridges, and CI ingestion observe events at
emit time.

Gate invocations outside an agent session write separate JSONL event logs under
the gate log root and reference those paths from the parent event stream rather
than interleaving concurrent gate events into a bead log. This spec owns the log
surface and breadcrumbs; [gate.md](gate.md) owns `GateRun` evidence validity and
how incomplete gate logs affect gate decisions.

`loom logs` locates a persisted event log and feeds it through the same
renderer pipeline used by live commands.

| Flag | Behavior |
|------|----------|
| default | Render the most recent log across bead and phase log roots, then exit at EOF. |
| `-f` / `--follow` | Continue rendering as the selected log grows; do not switch to newer log files or later iterations. |
| `-b` / `--bead <id>` | Select the latest log for a bead when the default latest-log selection is not enough. |
| `-v` / `--verbose` | Use diagnostic human rendering with event metadata. |
| `--raw` | Emit raw JSONL bytes; composes with `--follow`. |
| `--path` | Print the selected log path and exit. |

Bare `loom logs` against an empty log root exits 0 with a concise message.
`loom logs --path` against an empty root exits non-zero so shell substitution
fails loudly. Bare `loom logs` never auto-follows a running bead or phase
session; live tailing requires `--follow`, and follow stays on the selected
file.

Logs older than `[logs] retention_days` are swept on `loom loop` startup;
`0` disables sweeping. Retention failures are best-effort diagnostics and do
not abort the run.

## Success Criteria

- `AgentEvent` serialization carries the common flat envelope fields on every variant, including `session_id` plus optional work-routing fields such as `bead_id` and `iteration`
  [test](common_envelope_fields_present_on_every_variant)
- Event JSON is flat tagged by `kind`, with no nested event envelope
  [check](cargo test -p loom-events --lib flat_variant_shape_has_no_nested_envelopes)
- `AgentEvent` payload fields match this spec's variant payload table, including `agent_input`
  [test](agent_event_payload_fields_match_spec)
- `agent_input` events are emitted before initial prompt, follow-up, steer, and re-pin text is sent to the backend, carrying the full rendered text after required redaction
  [test](agent_input_events_precede_backend_send)
- Agent input rendering and persisted logs apply required secret redaction with explicit transcript markers rather than silent omission
  [test](agent_input_redaction_is_explicit)
- Unknown `driver_kind` wire values deserialize as forward-compatible driver events
  [test](driver_event_accepts_unknown_driver_kind)
- Core driver event kinds deserialize as `source: "driver"` events, including
      the loop dirty-work preservation event `workspace_recovery`
  [test](driver_kinds_present_for_spec_emission_sites)
- Gate lifecycle driver event kinds serialize through `driver_event.driver_kind` rather than new top-level `AgentEvent` variants
  [test](driver_kind_typed_enum_carries_gate_lifecycle_values)
- `EventSink` and `SessionCommand` are defined in `loom-events` with sync `emit`, default `react`, `Steer`, and `Abort`
  [check](cargo run -p loom-walk -- event_sink_in_loom_events)
- `EventSink` composition preserves registration order for `react()`
  [test](tee_chain_preserves_registration_order_for_react)
- The driver invokes `react()` after non-streaming events and not after streaming deltas
  [test](react_invoked_after_non_streaming_events_only)
- `LogSink::emit` writes the JSONL log and drives the renderer from the same event call
  [check](cargo run -p loom-walk -- single_event_channel)
- Formatted `loom logs` replay and live rendering for every non-interactive LLM-bearing command share the same renderer pipeline
  [test](live_llm_commands_use_shared_renderer_pipeline)
- Normal Pretty/Plain output renders transcript text without requiring `--verbose`: agent inputs, assistant text, exposed thinking, and inline tool-result text up to the Loom-controlled LLM inline budget
  [test](normal_rendering_shows_llm_visible_transcript)
- Verbose Pretty/Plain output adds diagnostic event metadata without changing transcript visibility or tool-body caps
  [test](verbose_adds_event_metadata_without_changing_transcript)
- Pretty/Plain mode renders each tool call as one pending/success/error block keyed by `ToolCallId` (`id` on the wire)
  [test](pretty_tool_block_updates_from_pending_to_success_or_error)
- Tool body rendering uses a byte-only cap aligned with Direct `[direct].max_inline_bytes` when Loom controls the LLM inline budget, and applies no additional line cap
  [test](tool_body_rendering_uses_byte_only_inline_budget)
- Cumulative tool progress/result snapshots are coalesced so previously rendered output is not printed again
  [test](cumulative_tool_output_snapshots_render_only_new_content)
- Driver events render interleaved with agent events in chronological order
  [test](driver_events_render_interleaved_with_agent_events)
- `loom todo` renders live agent progress through the shared event renderer before final validation/summary output
  [test](todo_agent_events_render_live_progress)
- `loom gate mint --tree` streams verifier, rubric, and minting progress through event logs and the shared live renderer while it walks
  [test](gate_mint_tree_streams_progress_events)
- Parallel Pretty/Plain rendering prefixes each line with the bead id and disables in-place spinners
  [test](parallel_rendering_prefixes_lines_and_disables_spinners)
- Cancellation finalizes any in-place running row before the closing output
  [test](run_finish_finalizes_dangling_running_indicator)
- Agent-event bookkeeping tracing is trace-level diagnostics, not default terminal `INFO` output
  [test](agent_event_bookkeeping_uses_trace_level)
- Stall watchdogs produce warning-severity driver rows without repeated timestamped terminal spam
  [test](stall_watchdog_renders_coalesced_warning_row)
- `loom loop --trace` mirrors raw Rust tracing diagnostics to stderr without changing normal or verbose event rendering
  [test](loop_trace_flag_mirrors_tracing_to_stderr)
- Renderer mode selection supports Pretty, Plain, Json, and Raw
  [test](renderer_modes_present)
- Plain mode is selected for non-TTY stdout, `NO_COLOR`, or `--plain`
  [test](plain_selected_on_non_tty)
- Json mode emits parsed event JSON without ANSI terminal decoration
  [test](json_mode_pretty_prints)
- Raw mode passes compact JSONL event bytes through unformatted
  [test](raw_mode_passthrough)
- Every bead spawn writes a raw JSONL log under `.loom/logs/<spec-label>/`
  [test](run_writes_per_bead_jsonl_log)
- Every non-bead LLM session writes a raw JSONL log under its `.loom/logs/<phase>/` root
  [test](non_bead_agent_sessions_write_phase_jsonl_logs)
- Non-bead phase event logs carry a `session_id` routing key without requiring a synthetic `bead_id`
  [test](non_bead_event_logs_use_session_id_routing_key)
- Log writes flush every event so followers observe events at emit time
  [test](log_sink_per_event_flush)
- Parallel bead spawns write independent log files with no cross-bead interleaving
  [test](parallel_logs_are_per_bead)
- `loom logs` exposes default, follow, bead selection, verbose, raw, and path-only surfaces
  [test](loom_logs_help_snapshot)
- `loom logs --follow` keeps rendered replay open past EOF until the selected log grows or the user interrupts
  [test](follow_blocks_past_eof_until_budget_expires)
- `loom logs --raw` copies persisted JSONL bytes verbatim, and `--follow --raw` waits past EOF
  [test](follow_raw_blocks_past_eof_until_budget_expires)
- Bare `loom logs` handles an empty log root as a normal zero-log state
  [test](empty_root_returns_no_logs)
- Log retention deletes files older than `[logs] retention_days` and preserves recent files
  [test](log_retention_sweep)
- `[logs] retention_days = 0` disables retention sweeping
  [test](log_retention_disabled)
- Retention sweep failures do not abort `loom loop`
  [test](log_retention_failure_tolerance)
- Gate invocations outside an agent session write separate JSONL logs and parent streams reference the gate log path
  [test](gate_invocations_write_separate_jsonl_logs_with_parent_breadcrumb)
- Incomplete gate event logs are marked incomplete rather than rendered as successful completed gate runs
  [test](incomplete_gate_event_log_is_not_successful)
- `loom-render` depends on `loom-events` and does not depend on `loom-driver` or `loom-workflow`
  [check](cargo run -p loom-walk -- loom_render_deps)

## Requirements

### Functional

1. **Event normalization.** Every backend adapter emits canonical
   `AgentEvent` values before workflow code, renderers, or observers consume
   the stream. Backend-specific wire shapes are owned by [agent.md](agent.md).
2. **LLM input visibility.** Loom emits `agent_input` before sending
   Loom-authored text to an LLM-backed backend: initial prompts, follow-up
   prompts, steering messages, and compaction re-pins. Normal human rendering
   shows that text without requiring `--verbose`.
3. **Live rendering.** Every non-interactive LLM-bearing command renders the
   event stream as a human timeline by default and keeps raw Rust tracing out of
   that timeline unless `loom loop --trace` is passed.
4. **Replay rendering.** Formatted `loom logs` renders persisted event streams
   through the same renderer pipeline as live commands.
5. **Machine output.** `loom loop --json`, `loom loop --raw`, and
   `loom logs --raw` are machine surfaces and contain event data without
   Pretty/Plain terminal chrome.
6. **Driver observability.** Host-driver activity that affects workflow
   progress is emitted as `driver_event` and rendered in chronological order
   with agent activity.
7. **Tool-output fidelity.** Where Loom controls the backend's inline tool
   budget, tool output sent inline to the model is shown up to the same byte
   budget; output not sent inline is represented by an explicit
   offload/truncation boundary and recovery path. Opaque backend display caps
   are labelled as display-only.
8. **Duplicate suppression.** Repeated cumulative tool-output snapshots update
   the existing tool block or append only new suffix content; replay does not
   multiply already-seen output.
9. **Diagnostic boundary.** Rust tracing levels follow
   [RS-15](../docs/style-rules.md#logging-rs-15): event bookkeeping is trace
   diagnostics; abnormal-but-continuing silence windows are warnings.

### Non-Functional

1. **Low-latency persistence.** Per-event flush keeps file followers close to
   live event time.
2. **Operator fidelity.** Normal output is the trustworthy transcript surface;
   `--verbose` adds diagnostic metadata but is not required to discover what
   Loom sent to the LLM.
3. **Terminal portability.** Pretty features degrade to Plain semantics when
   color, OSC 8, or in-place terminal controls are unavailable; Plain avoids
   decorative line art.
4. **Security redaction.** Secret-bearing values must not be rendered or
   persisted by this event surface; required redaction is explicit in the
   transcript rather than silent omission.
5. **Forward compatibility.** New driver event kinds are additive on the wire;
   consumers render unknown kinds generically.
6. **Small public dependency floor.** External consumers can depend on
   `loom-events` without pulling in driver, workflow, git, database, or async
   runtime dependencies.

## Out of Scope

- Loom does not ship an SSE server; external pipeline runners may build one
  by tailing JSONL logs and using `session_id:seq` as the resume key.
- Bare `loom logs` does not auto-follow running logs, and `--follow` does not
  auto-switch to newer log files or later loop iterations.
- Raw Rust tracing is not part of the persisted event log schema.
- Separate durable trace files or driver-log files are out of scope.
- A second renderer or replay-only formatter is out of scope.
- Interactive `loom plan` and `loom inbox chat` native UI rendering is out of
  scope for this event renderer; this spec only covers event logs those
  sessions independently choose to write.
- Interactive TUI widgets, sidebars, or picker-style controls are out of
  scope for the event renderer; the contract is a terminal timeline.
