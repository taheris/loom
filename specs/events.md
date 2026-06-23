# Events and Rendering

Defines the typed event stream, human renderer, persisted JSONL event logs,
and diagnostic tracing boundary.

## Problem Statement

Loom drives long-running host and agent work that operators need to watch
without reading raw JSON or Rust tracing noise. The durable primitive is the
`AgentEvent` stream: agents and the host driver emit events, renderers turn
those events into human timelines, disk logs persist them for replay, and
machine consumers parse the same JSONL stream.

## Architecture

Events are the source of truth. A live `loom loop` session, a saved
`.loom/logs/*.jsonl` file, a `loom logs` replay, and any external consumer
observe the same `AgentEvent` values in the same order. Logs are persisted
copies of the event stream, not a separate state model.

### Crate Boundaries

`loom-events` is the public contract crate. It owns `AgentEvent`, the common
envelope, identifier newtypes, `DriverKind`, the `Session` interoperability
trait, `EventSink`, and `SessionCommand`. It is a leaf crate so frontends,
SSE bridges, `llm` consumers, and log analyzers can depend on the event
contract without pulling in the Loom driver.

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
scope. The `LogSink` tee is the standard fan-out: one `emit(&AgentEvent)`
writes the JSONL event and drives the selected renderer, when a renderer is
attached.

## Event Schema

`AgentEvent` is a flat tagged JSON object. Each line in a persisted log is one
complete JSON object terminated by `\n`. Consumers dispatch on the top-level
`kind` field; there is no nested event envelope.

### Common Envelope

Every event carries these top-level fields in addition to its payload:

| Field | Type | Purpose |
|-------|------|---------|
| `kind` | `string` | Snake-case event discriminator. |
| `bead_id` | `string` | Per-bead routing key. |
| `molecule_id` | `string?` | Optional work-epic grouping key. |
| `iteration` | `u32` | Work-epic iteration counter. |
| `source` | `"agent" \| "driver"` | Distinguishes backend activity from host-driver activity. |
| `ts_ms` | `i64` | Unix milliseconds UTC. |
| `seq` | `u64` | Monotonic per bead spawn; SSE resume key is `<bead_id>:<seq>`. |

### Variant Set

The event stream contains these canonical variant groups:

- **Lifecycle** — `agent_start`, `agent_end`, `turn_start`, `turn_end`,
  `session_complete`
- **Streaming** — `text_delta`, `text_end`, `thinking_delta`,
  `thinking_end`, `toolcall_delta`
- **Tools** — `tool_call`, `tool_result`, `tool_progress`
- **Operational** — `compaction_start`, `compaction_end`, `auto_retry`,
  `error`
- **Driver catch-all** — `driver_event`

Payload fields are also top-level fields on the same flat JSON object. `?`
means nullable or omitted according to the serde shape.

| Event | Payload fields | Meaning |
|-------|----------------|---------|
| `agent_start` | `schema_version`, `title`, `profile`, `spec_label`, `started_at_ms`, `parent_tool_call_id?` | First event in a session; carries renderer/replay metadata. |
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
- **Identifier newtypes.** `BeadId`, `MoleculeId`, `ToolCallId`,
  `SpecLabel`, `ProfileName`, and related ids parse at the boundary; downstream
  code receives typed ids rather than raw strings.
- **Parser-to-stamper split.** Backend parsers produce `ParsedAgentEvent`
  payloads. The session layer is the only constructor of full `AgentEvent`
  values because it owns bead identity, source, timestamp, and sequence.
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
`signature_verification_failed`, `worktree_cleanup_ok`, and `tree_not_clean`.
Gate lifecycle values carried by the same field are `gate_run_start`,
`gate_run_scope`, `gate_run_lane`, `gate_run_end`, and `gate_run_skipped`;
[gate.md](gate.md) owns their GateRun semantics. Unknown future strings must
round-trip as `DriverKind::Other` and render generically.

## Rendering UX

`loom loop` is the primary live view. The default TTY renderer is a Pi-style
transcript: assistant text reads as prose instead of repeated log labels,
thinking content remains in the event stream but its prose is hidden by
default, and tool calls render as visually distinct stateful blocks keyed by
`ToolCallId` (`id` on the wire). Pretty mode may use color, background, and
glyph styling; the spec does not mandate box-drawing characters or an exact
glyph layout.

### Modes

| Mode | Selected when | Output shape |
|------|---------------|--------------|
| `Pretty` | TTY, no `--plain` / `--json` / `--raw`, and no `NO_COLOR` | Pi-style human transcript with color/background/glyph styling, stateful tool blocks, compact default detail, and OSC 8 links where supported. |
| `Plain` | Non-TTY, `NO_COLOR`, or `--plain` | Same event content as Pretty, ASCII/no color/no OSC 8/no decorative line art. |
| `Json` | `--json` | One parsed event rendered as JSON per emission; no terminal chrome. |
| `Raw` | `--raw` | The compact JSONL event bytes; no parsing or formatting. |

`--plain`, `--json`, and `--raw` are `loom loop` renderer flags in v1. `loom
logs` exposes the replay-specific flag set below; it supports `--raw`, chooses
Pretty/Plain by TTY and `NO_COLOR`, and uses `-v` for verbose replay.

`-v` / `--verbose` means more human-rendered detail, not raw diagnostics:
thinking prose, expanded tool arguments and bodies within safety caps,
successful command output that default mode may collapse, and fuller driver
payload summaries. `loom loop --trace` is the separate diagnostic flag for raw
Rust tracing.

### Human Timeline

- Assistant `text_delta` content is visible by default and coalesced into
  readable prose rather than token-count bookkeeping or repeated `assistant`
  labels.
- `thinking_delta` prose is persisted but hidden in default rendering. Live
  mode may show a transient thinking/activity indicator while no other output
  is available, but replay does not emit permanent "thinking hidden" spam.
  `-v` / `--verbose` renders thinking prose in a distinct thinking/comment
  style.
- `tool_call`, `tool_progress`, and `tool_result` for the same `ToolCallId`
  (`id` on the wire) render as one stable block. Running tools use the pending
  style; successful tools use the success style; failed tools use the error
  style. Bash exit status drives success/error coloring.
- Default tool blocks are useful but bounded: they show the tool name and key
  target, state, failed output prominently, useful successful-output excerpts,
  compact edit/write diffs or summaries, and a truncation hint that points to
  `-v`. Exact line and byte caps are implementation-defined.
- Driver events are interleaved chronologically with agent events, but default
  rendering is sparse: only meaningful workflow state such as stalls, retries,
  gate status, container failures, push blocks, merge conflicts, and useful log
  breadcrumbs appears. `-v` renders fuller driver payload summaries. Token
  usage and cost appear in final/session summaries by default; detailed token
  usage rows are verbose-only.
- Under `--parallel N > 1`, each rendered line carries a bead prefix or compact
  bead marker, and bead start/end summaries print atomically. In-place spinners
  and mutable live tool blocks are disabled because multiple carriage-return
  regions do not compose.
- Ctrl-C/SIGINT collapses any in-place region and emits a clean interrupted
  closing row.

### Diagnostic Tracing Boundary

Rust `tracing` is diagnostic output, not the product UI. Normal `loom loop`
TTY output contains only the rendered event timeline. Agent-event bookkeeping
such as `message_delta (5 chars)`, `thinking_delta`, `toolcall_delta`,
`tool_call`, `tool_result`, and `turn_end` is trace-level diagnostics when it
is emitted at all. Stall watchdogs remain warning-level events because work is
continuing after an abnormal silence window; the renderer presents them as
warning rows rather than repeated timestamped terminal spam. `loom loop
--trace` mirrors raw tracing diagnostics to stderr for debugging Loom itself.
No separate durable trace or driver-log file is required by this spec;
operationally relevant driver activity belongs in structured `driver_event`
records in `.loom/logs`.

## Persisted Logs and Replay

Loom writes the full raw JSONL event stream for every bead spawn regardless of
terminal mode:

```text
.loom/logs/<spec-label>/<bead-id>-<utc-timestamp>.jsonl
```

Parallel batches never interleave multiple beads into one file. Each emitted
event is flushed so `tail -f`, file-watcher bridges, and CI ingestion observe
events at emit time.

Gate invocations outside an agent session write separate JSONL event logs under
the gate log root and reference those paths from the parent event stream rather
than interleaving concurrent gate events into a bead log. This spec owns the log
surface and breadcrumbs; [gate.md](gate.md) owns `GateRun` evidence validity and
how incomplete gate logs affect gate decisions.

`loom logs` locates a persisted event log and feeds it through the same
renderer pipeline used by `loom loop`.

| Flag | Behavior |
|------|----------|
| default | Render the most recent log and exit at EOF. |
| `-f` / `--follow` | Continue rendering as the selected log grows; do not switch to newer log files or later iterations. |
| `-b` / `--bead <id>` | Select the latest log for a bead when the default latest-log selection is not enough. |
| `-v` / `--verbose` | Use verbose human rendering. |
| `--raw` | Emit raw JSONL bytes; composes with `--follow`. |
| `--path` | Print the selected log path and exit. |

Bare `loom logs` against an empty log root exits 0 with a concise message.
`loom logs --path` against an empty root exits non-zero so shell substitution
fails loudly. Bare `loom logs` never auto-follows a running bead; live tailing
requires `--follow`, and follow stays on the selected file.

Logs older than `[logs] retention_days` are swept on `loom loop` startup;
`0` disables sweeping. Retention failures are best-effort diagnostics and do
not abort the run.

## Success Criteria

- `AgentEvent` serialization carries the common flat envelope fields on every variant
  [test](common_envelope_fields_present_on_every_variant)
- Event JSON is flat tagged by `kind`, with no nested event envelope
  [check](cargo test -p loom-events --lib flat_variant_shape_has_no_nested_envelopes)
- `AgentEvent` payload fields match this spec's variant payload table
  [test](agent_event_payload_fields_match_spec)
- Unknown `driver_kind` wire values deserialize as forward-compatible driver events
  [test](driver_event_accepts_unknown_driver_kind)
- Core driver event kinds deserialize as `source: "driver"` events
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
- Formatted `loom logs` replay and live `loom loop` rendering share the same renderer pipeline
  [test](replay_renders_via_shared_renderer)
- Pretty mode streams assistant text as prose, hides thinking prose by default, and renders thinking prose under `-v`
  [test](pretty_hides_thinking_by_default_verbose_shows_it)
- Pretty mode renders each tool call as one pending/success/error block keyed by `ToolCallId` (`id` on the wire)
  [test](pretty_tool_block_updates_from_pending_to_success_or_error)
- Default tool rendering is useful but bounded, while verbose rendering expands details within safety caps
  [test](default_tool_rendering_is_useful_and_bounded_verbose_expands)
- Driver events render interleaved with agent events in chronological order
  [test](driver_events_render_interleaved_with_agent_events)
- Parallel Pretty/Plain rendering prefixes each line with the bead id and disables in-place spinners
  [test](parallel_rendering_prefixes_lines_and_disables_spinners)
- Cancellation finalizes any in-place running row before the closing output
  [test](run_finish_finalizes_dangling_running_indicator)
- Agent-event bookkeeping tracing is trace-level diagnostics, not default terminal `INFO` output
  [test](agent_event_bookkeeping_uses_trace_level)
- Stall watchdogs produce warning-severity driver rows without repeated timestamped terminal spam
  [test](stall_watchdog_renders_coalesced_warning_row)
- `loom loop --trace` mirrors raw Rust tracing diagnostics to stderr without changing event rendering
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
- `loom-events` remains a leaf public-contract crate
  [check](cargo run -p loom-walk -- loom_events_is_leaf)
- `loom-render` depends on `loom-events` and does not depend on `loom-driver` or `loom-workflow`
  [check](cargo run -p loom-walk -- loom_render_deps)

## Requirements

### Functional

1. **Event normalization.** Every backend adapter emits canonical
   `AgentEvent` values before workflow code, renderers, or observers consume
   the stream. Backend-specific wire shapes are owned by [agent.md](agent.md).
2. **Live rendering.** `loom loop` renders the event stream as a human
   timeline by default and keeps raw Rust tracing out of that timeline unless
   `loom loop --trace` is passed.
3. **Replay rendering.** Formatted `loom logs` renders persisted event streams
   through the same renderer pipeline as live `loom loop`.
4. **Machine output.** `loom loop --json`, `loom loop --raw`, and
   `loom logs --raw` are machine surfaces and contain event data without
   Pretty/Plain terminal chrome.
5. **Driver observability.** Host-driver activity that affects workflow
   progress is emitted as `driver_event` and rendered in chronological order
   with agent activity.
6. **Diagnostic boundary.** Rust tracing levels follow
   [RS-15](../docs/style-rules.md#logging-rs-15): event bookkeeping is trace
   diagnostics; abnormal-but-continuing silence windows are warnings.

### Non-Functional

1. **Low-latency persistence.** Per-event flush keeps file followers close to
   live event time.
2. **Terminal portability.** Pretty features degrade to Plain semantics when
   color, OSC 8, or in-place terminal controls are unavailable; Plain avoids
   decorative line art.
3. **Forward compatibility.** New driver event kinds are additive on the wire;
   consumers render unknown kinds generically.
4. **Small public dependency floor.** External consumers can depend on
   `loom-events` without pulling in driver, workflow, git, database, or async
   runtime dependencies.

## Out of Scope

- Loom does not ship an SSE server; external pipeline runners may build one
  by tailing JSONL logs and using `bead_id:seq` as the resume key.
- Bare `loom logs` does not auto-follow running logs, and `--follow` does not
  auto-switch to newer log files or later loop iterations.
- Raw Rust tracing is not part of the persisted event log schema.
- Separate durable trace files or driver-log files are out of scope.
- A second renderer or replay-only formatter is out of scope.
- Interactive TUI widgets, sidebars, or picker-style controls are out of
  scope for the event renderer; the contract is a terminal timeline.
