# Loom Agent

Agent backend abstraction, three backend implementations (pi-mono,
Claude Code, and Direct), container communication, and per-runtime
layers for the agent images.

## Problem Statement

Single-runtime designs that bind the workflow to one agent binary
create vendor lock-in. Users with a Claude Max subscription need the
`claude` binary; users who want LLM-agnostic switching need an
alternative. Pi-mono provides 20+ LLM provider backends and a JSONL
RPC mode that enables programmatic control — but it requires a
different communication protocol than Claude Code's stream-json
output mode. A third backend, Direct, composes
[llm.md](llm.md)'s `Conversation` with Loom's six
sandbox-aware tools so phases that need typed multi-provider LLM
access (e.g. cost-sensitive structured-output `gate review` runs)
can opt in without driving a subprocess agent.

As of April 2026, Anthropic no longer allows third-party applications to consume
Claude Pro/Max subscription quota. This means pi-mono cannot use a Max
subscription even when backed by Claude — validating the need for a dedicated
Claude Code backend that runs the `claude` binary directly.

This spec defines the agent abstraction that lets Loom drive any of
the three runtimes through a common interface, and the infrastructure
changes (runtime layer, entrypoint) that make each backend available
inside wrix containers. The Loom platform (crate structure,
templates, workflow) is defined in [harness.md](harness.md).

## Architecture

Throughout this section, **"driver"** refers to loom's backend-side code that
drives the agent process over JSONL — distinct from the agent (`pi`,
`claude`, or `loom-direct-runner`) running inside the container.

### Dispatch: ZST Backends + Per-Phase Selection

Backends are zero-sized types — `PiBackend`, `ClaudeBackend`, and
`DirectBackend`. All runtime state lives in the session and
`SpawnConfig`; the backend type parameter alone carries dispatch.
No instances, no constructor.

The backend is resolved **per phase** from config, not once at startup.
Each workflow command (plan, todo, loop, gate, msg) independently selects
its backend + model. The binary crate exposes a single `dispatch`
function that matches on the per-phase choice and forwards to a generic
helper parameterized by backend type. The workflow engine receives that
helper as a parameter and never touches concrete backend types — static
dispatch is preserved inside each match arm.

**Per-phase config example:** `loom todo` uses a cheap model via pi,
`loom gate review` uses direct (typed structured output + cost
tracking), and the rest defaults to claude:

```toml
[phase.default]
agent.backend = "claude"

[phase.todo]
agent.backend = "pi"
agent.provider = "deepseek"
agent.model_id = "deepseek-v3"

[phase.gate.review]
agent.backend = "direct"
agent.model_id = "claude-sonnet-4-6"
```

Phases without explicit config inherit `[phase.default]`. The pi
backend calls `set_model` after spawn if the phase config specifies a
provider/model; the direct backend reads `agent.model_id` directly
into its `Conversation`'s `ModelId`.

`[phase.plan]` and `[phase.msg]` are also valid per-phase keys, but
the interactive phases (`loom plan`, `loom msg --chat`) bypass this
dispatch entirely — see [Interactive Shell-Out](#interactive-shell-out)
below.

Mock backends slot into the same dispatch — they are ZSTs too, so a
test-time entry parameterized with `MockBackend` works without any
production code change.

### Interactive Shell-Out

`loom plan` and `loom msg --chat` bypass the agent-backend abstraction
and shell out to an interactive REPL with inherited stdio. Both invoke
`wrix run <workspace> <agent command> ... <prompt>` and let the spawned
process attach directly to the controlling terminal; the driver-side
stream-json parser does not run on this path. The command is selected
from the resolved chat-capable phase backend:
`claude --dangerously-skip-permissions` for Claude and `pi` for Pi.
Direct has no interactive REPL command; selecting
`agent.backend = "direct"` for `plan` or chat `msg` is a configuration
error before any Wrix child process is spawned.

Per-phase config still resolves: each phase's `profile` key flows
through `LoomConfig::agent_for(Phase)` exactly like the non-interactive
phases. The resolved profile/runtime pair is looked up in the
profile-image manifest and the resulting `ImageEntry` is exported to
`wrix run` via the `WRIX_DEFAULT_IMAGE_REF` /
`WRIX_DEFAULT_IMAGE_SOURCE` env vars
documented in [harness.md — Profile-Image
Manifest](harness.md#profile-image-manifest). Loom also sets the
backend-derived `WRIX_AGENT` on the `wrix run` child process so launcher
host-side setup matches the selected runtime. The env-var hand-off is
the sole image-selection contract on this path: `wrix run` has no
`--profile` parser, and any extra tokens between the workspace
positional and the agent command would be forwarded into the container as
the command vector (the entrypoint would exec them literally and exit 127).

`wrix run` reads those env vars (when no `--spawn-config` is supplied)
to pick the same profile/runtime image the non-interactive `wrix spawn`
path selects — the two paths must select the same image for the same
profile name and backend runtime.

### Agent Backend Trait

The agent-backend abstraction is deliberately minimal: it exposes a
single asynchronous `spawn` operation that consumes a `SpawnConfig` and
yields an idle session. Process lifecycle is its only concern. Session
interaction (prompt, steer, abort, event streaming) lives on the session
type, not the backend trait — the backend's job is to spawn a session;
the session's job is to drive the conversation.

All three backends support steering: pi via the native `steer`
command, claude via `--input-format stream-json --output-format
stream-json` (sends a stream-json user message on stdin during the
session), and direct via `loom-direct-runner` injecting a steer
message into the in-progress `Conversation`'s next turn. There is
no capability gate — steering works for every backend. If a future
backend cannot support steering, a capability constant can be
reintroduced.

Backends carry no per-instance state — the type parameter conveys all
information. The implementation uses native `async fn` in traits
(edition 2024) with static dispatch, avoiding the `async-trait` crate.

### Session Lifecycle Contract

The public agent-driver contract is behavioural: workflow code selects a
backend for a phase, spawns a session handle, sends prompt / steer /
cancel / mode commands through that handle, and consumes the resulting
`AgentEvent` stream. The spec does not require type erasure, dynamic
dispatch, or any specific Rust carrier type for that handle. The stable
compatibility point is the command/event lifecycle, plus the shared
`Session` interoperability surface in `loom-events` for consumers that
need a backend-neutral trait.

Backends surface asynchronous outcomes through the event stream:
failures become `AgentEvent::Error` or a non-zero `exit_code` on
`SessionComplete`; command submission remains a session operation rather
than a separate workflow-side transport protocol. `SessionMode` is a
closed-set mode selection surface that can grow additively, preserving
RS-17 while allowing future modes.

### Typestate (host-side session lifecycle mechanic)

Host-side backends that drive a JSONL subprocess use a typestate
`AgentSession<Idle|Active>` to enforce protocol-correctness invariants:
a prompt cannot be sent before the session is ready, and the same active
session cannot be re-prompted before its current run completes. Invalid
transitions are compile errors inside the backend implementation. The
typestate does not leak through the public `Session` interoperability
trait.

State-machine rules for host-side subprocess sessions:

- **Idle session** must be prompted before events can be read. The
  prompt operation consumes the idle session and yields an active one.
- **Active session** exposes `next_event`, `steer`, and `abort`. It
  cannot be prompted again — only completed or aborted.
- **Aborting** returns to idle: if the backend has a wire abort command
  (pi), the parser encodes it and the session is reusable; if not
  (claude), the typestate still returns to idle but the underlying
  process is left to backend-level shutdown (SIGTERM/SIGKILL via the
  watchdog), so a follow-up prompt fails with a process-exit error.

The session type and the parser abstraction both live in `loom-driver` —
not in `loom-agent` — because the agent-backend trait returns a session,
and the inverse dependency would be a cycle.

The Direct host backend participates in the same session lifecycle: it
launches `loom-direct-runner` as a JSONL subprocess and returns a
host-side session handle. The part that lacks Pi / Claude handshake
typestate is the in-container Direct conversation loop, where
`loom-llm::Conversation` manages multi-turn state internally. This split
keeps workflow code backend-neutral while avoiding unnecessary typestate
inside the Direct runner's tool loop.

A single inbound protocol line can yield multiple events. The session
buffers excess events and returns one per call to `next_event`. A
state-agnostic accessor lets backends borrow the underlying child
process without surrendering session ownership; this is the hook the
claude backend's shutdown watchdog uses to drive the SIGTERM/SIGKILL
escalation described in requirement #4.

**Line parsing.** Each backend provides a parser that owns **both
directions of the wire** — decoding inbound JSONL lines into events, and
encoding outbound commands (initial prompt, steer, abort) for stdin. The
parser is held internally by the session via dynamic dispatch so the
session itself stays a single concrete type, free of backend generics.
Static dispatch on the outer agent-backend layer plus dynamic dispatch
on the inner parser layer is the deliberate split — the per-line vtable
call is negligible next to the IO cost of reading from a subprocess
pipe.

The parser's decoded output carries two fields: a list of events to
yield, and an optional response string the session should write back to
stdin before yielding those events. The response slot handles protocol
control flow such as claude's `control_request` auto-approve: the
parser populates the field; the session does the IO. The list of events
is a list (not a single event) because some inbound messages map to
multiple events — claude's `result/success` produces `TurnEnd` +
`SessionComplete`; `result/error` produces `Error` + `SessionComplete`.
Pi's `turn_end` and `agent_end` are separate inbound events that each
map to one outbound event.

### AgentEvent

The session emits a stream of typed events. Event names are part of the
wire format: they are serialized as snake_case (`text_delta`,
`tool_call`, …) when the terminal renderer and on-disk JSONL log share
the tee-style sink (see [loom-harness — Loop UX &
Logging](harness.md#loop-ux--logging)). Log readers consume those
names directly. Every variant carries a flat envelope (`bead_id`,
`molecule_id?`, `iteration`, `source`, `ts_ms`, `seq`) in addition to
the per-variant payload listed below.

| Event | Payload | Meaning |
|-------|---------|---------|
| `agent_start` | `schema_version`, `title`, `profile`, `spec_label`, `started_at_ms`, `parent_tool_call_id?` | First event in any session; carries per-spawn metadata for the renderer/log replayer. |
| `agent_end` | — | Agent session ended; lifecycle bookend paired with `agent_start`. |
| `turn_start` | — | Multi-turn session opened a new turn; paired with `turn_end`. |
| `text_delta` | `text` | Streaming text fragment from the agent. |
| `text_end` | — | Closes a `text_delta` stream. |
| `thinking_delta` | `text` | Streaming reasoning fragment (when the backend exposes it). |
| `thinking_end` | — | Closes a `thinking_delta` stream. |
| `toolcall_delta` | `id`, `delta` | Streaming tool-call argument fragment. |
| `tool_call` | `id`, `tool`, `params`, `parent_tool_call_id?` | Agent invoked a tool. |
| `tool_result` | `id`, `output`, `is_error` | Tool execution completed. |
| `tool_progress` | `id`, `text` | In-flight progress update from a long-running tool. |
| `turn_end` | — | One turn finished; the session may have more turns. |
| `session_complete` | `exit_code`, `cost_usd?` | Process exiting or final result received. |
| `compaction_start` | `reason` | Context compaction beginning. |
| `compaction_end` | `aborted` | Compaction finished. |
| `auto_retry` | `attempt`, `max_attempts`, `delay_ms`, `error_message` | Backend signaled an auto-retry attempt. |
| `error` | `message` | Agent reported an error mid-stream (does not necessarily end the session). |
| `driver_event` | `driver_kind`, `summary`, `payload` | Driver-side event (verdict gate, push gate, container lifecycle, token usage, observer signal, …). `driver_kind` is forward-compatible — unknown wire strings land in `DriverKind::Other`. |

`reason` is one of `context_limit`, `user_requested`, or `unknown`. Pi's
`threshold` and `overflow` both map to `context_limit`; `manual` maps to
`user_requested`.

The session's terminal outcome — exit code and optional reported cost —
flows out via the `session_complete` event; nothing further is needed
from the workflow engine to learn how a session ended.

### ProtocolError

Operations against an active session can fail with one of a small,
closed set of error categories:

- **Invalid JSON** on a protocol line — the inbound line did not parse.
- **Unknown message type** — JSON parsed, but the discriminator is one
  the backend does not recognize.
- **IO error** — the underlying stdin/stdout channel returned a system
  IO failure.
- **Process exit** — the agent process terminated; the captured exit
  code is reported.
- **Unexpected EOF** — the event stream ended without a terminating
  `session_complete`.
- **Line too long** — an inbound JSONL line exceeded the framing budget
  (10 MB; see [JSONL Framing](#jsonl-framing)).
- **Unsupported** — the backend cannot perform the requested operation
  (e.g., a future backend without steering).
- **Handshake timeout** — a named backend handshake stage did not
  complete within its budget; the variant carries the stage name and
  the elapsed duration.
- **Lock poisoned** — a parser-internal mutex was poisoned by a
  panicking thread.

These are the only protocol-level error classes. Backend-specific
failures (model errors, container teardown failures, etc.) surface
through the same channel and are mapped onto these categories at the
parser boundary.

### SpawnConfig

The harness writes a `SpawnConfig` to a JSON file at dispatch time, and
`wrix spawn --spawn-config <file>` reads it back. This is the single
serialization boundary between loom and the wrapper — preferred over a
fat argv interface. The wrapper's JSON shape is the stable contract;
loom and `wrix spawn` ship from the same flake and stay in lockstep.

Required fields:

- `image_ref` — podman image reference (e.g. `localhost/wrix-rust-pi:<hash>`).
- `image_source` — Nix store path the launcher uses to materialize that ref
  when needed.
- `image_digest_path` — optional Nix store path containing the image content
  digest. Modern wrix launchers use it to skip reloading identical image
  content that is already present under any tag.
- `workspace` — host path bind-mounted into the container at
  `/workspace`.
- `env` — explicit env allowlist (table below); the host environment is
  never inherited wholesale.
- `mounts` — typed list of per-spawn bind mounts beyond `workspace`,
  additive to the resolved profile's `mounts`. Each entry carries
  `host_path`, `container_path`, and `read_only`. Loom uses this to
  project the `wrix-beads` dolt socket into every bead container at
  `/workspace/.wrix/dolt.sock` (replacing the host-side hardlink
  shim) and the shared sccache directory at the configured cache
  path; see [harness.md § Bead Dispatch](harness.md#bead-dispatch).
  Single-file mounts (sockets) and directory mounts both pass through
  virtiofs on Linux. On Darwin the wrix sandbox classifier accepts
  directories and regular files but rejects Unix-socket `host_path`
  entries at launch — VirtioFS does not pass socket operations across
  the VM boundary — so dolt-over-socket on Darwin needs a TCP-routed
  alternative or a platform gate skipping the socket entry.
- `initial_prompt` — prompt rendered from the phase template.
- `agent_args` — extra argv to pass to the agent binary.
- `scratch_dir` — per-key scratch directory the agent backend reads on
  compaction events; see [harness.md § Compaction
  Recovery](harness.md#compaction-recovery).

Additionally, `output_limits` (optional, Direct-only) carries
`max_inline_bytes` — the inline-output cap above which content-returning
Direct tools offload to the scratch offload directory. See [Direct Output
Bounding](#direct-output-bounding).

`image_ref`, `image_source`, and optional `image_digest_path` come from the profile-image manifest at
dispatch time — see [harness.md — Profile-Image
Manifest](harness.md#profile-image-manifest).

`SpawnConfig` also carries a host-only `launcher_env` map that is
`#[serde(skip)]`-excluded from the JSON file. Backends apply it to the
`wrix spawn` `Command` before exec; it is for launcher inputs wrix must
see before container startup, such as `WRIX_AGENT` and host key paths.

The env allowlist is constructed by the workflow engine from
known-needed variables. `WRIX_AGENT` is special: Loom derives it from the
resolved closed-set backend runtime (`pi`, `claude`, or `direct`) and
writes the same value to `SpawnConfig.env` and `SpawnConfig.launcher_env`.
The parent shell's `WRIX_AGENT` is never the source of truth for agent
selection.

| Variable | When | Purpose |
|----------|------|---------|
| `WRIX_AGENT` | always | Agent selection in entrypoint; same backend-derived value as launcher env |
| `CLAUDE_CODE_OAUTH_TOKEN` | claude backend | Claude authentication |
| `ANTHROPIC_API_KEY` | pi or direct backend (Anthropic models) | LLM API key |
| `TERM` | always | Terminal capability |
| `BEADS_DOLT_SERVER_SOCKET`, `BEADS_DOLT_AUTO_START` | always | Beads dolt-socket path (set to the bind-mount); auto-start disabled (host owns the server) |
| `LOOM_INSIDE` | always | Set to `1`; trips the nested-loom guard if the agent invokes `loom` inside the container — see [harness.md — Nested-Loom Guard](harness.md#nested-loom-guard) |

Provider-specific API keys for the pi and direct backends (OpenAI,
Google, DeepSeek, etc.) are added only when the configured model
requires them. Variable names are logged at `info!` level during
spawn; secret values are never logged. The closed-set `WRIX_AGENT`
value is non-secret and is logged as spawn diagnostics.

### Host-to-Container Communication

```
loom (host)                                            container
    │                                                       │
    ├─ serialize SpawnConfig → /tmp/loom-<id>.json          │
    ├─ set launcher env: WRIX_AGENT=<runtime>, keys…        │
    ├─ wrix spawn --spawn-config <file> --stdio        │
    │   └─ exec podman run [no TTY, stdio piped] ─►  entrypoint.sh
    │                                                       │
    │                                                  agent (pi --mode rpc / claude / loom-direct-runner)
    │   stdin ──────────────────────────────────────►  agent
    │   stdout ◄──────────────────────────────────────  agent
    │                                                       │
    ├─ writes JSONL to stdin ────────────────────────►  agent processes command
    │   (all three backends)                                │
    │                                                       │
    ├─ reads JSONL from stdout ◄─────────────────────  agent streams events
    │   (all three backends)                                │
    │                                                       │
    └─ on exit: container teardown via wrix              │
```

The wrapper hides container construction (mounts, env allowlist, krun
runtime, network filter, deploy key, beads dolt socket) so loom owns only
JSONL framing and the typed `SpawnConfig` it serializes. All three backends use
bidirectional JSONL over stdin/stdout. The Claude backend uses
`--input-format stream-json --output-format stream-json` for full
bidirectional support.

### JSONL Framing

JSON Lines (JSONL, also known as NDJSON): each line is one complete JSON object,
terminated by `\n` (0x0A). Both pi RPC and Claude stream-json use this framing.

Parsing rules:

- Split on `\n` (0x0A). Trailing `\r` is stripped.
- U+2028 and U+2029 are NOT line terminators — they pass through as JSON content.
- Empty lines (blank between objects) are silently skipped.
- Each non-empty line is independently parsed as JSON.
- A line that fails JSON parsing is an "invalid JSON" protocol error.

A per-line byte budget of **10 MB** prevents a malicious or
malfunctioning agent from exhausting host memory by sending a single
line without a `\n` terminator. 10 MB is well above any legitimate JSONL
message (the largest are tool results with file contents). The limit is
checked after the read completes — the reader will still buffer the full
line, but the error fires before the line is parsed or accumulated
further.

### Pi-Mono RPC Protocol

Pi's `--mode rpc` uses JSONL over stdin/stdout. The protocol has no version
negotiation or handshake. After launching `wrix spawn`, the Pi backend waits
for wrix's container-start stderr marker before starting the Pi RPC probe
budget; image materialization and container staging are launcher startup, not
agent-protocol silence. It then sends a `get_state` probe and verifies the
response has the documented state-object shape (`isStreaming`,
`isCompacting`, `messageCount`, and `pendingMessageCount` are required). If
the response shape is unexpected, the backend fails fast with a clear
version-mismatch error before any workflow begins. After the probe succeeds,
normal command flow starts.

Pi 0.73's `get_commands` does **not** enumerate built-in RPC verbs; it lists
slash commands, prompt templates, and skills that can be invoked through
`prompt` by prefixing `/`. Loom therefore does not use `get_commands` for
startup capability validation. Built-in command failures are still enforced at
send time: configured `set_model` is a hard-fail handshake step, while
`set_thinking_level` remains best-effort.

Messages are classified by **two-phase deserialization**: peek at the
`type` and `id` fields to determine the message category, then
deserialize the full payload into the correct type.

The classifier rules:

- A `type` of `"response"` → a response message (may carry an `id`; prompt
  acknowledgements in current Pi omit it).
- A `type` of `"extension_ui_request"` → a UI extension request.
- Any other line lacking an `id` → an event (events carry their own
  `type` values like `"message_update"` and never have an `id`).
- Any other line with an `id` but an unrecognized `type` → an
  unknown-message-type protocol error.

**Why two-phase?** Pi messages don't follow a clean tagged union:
correlated responses have `type: "response"` plus an `id`, prompt
acknowledgements are `type: "response"` without an `id`, events carry their
own `type` values without an `id`, and extension UI requests have a distinct
`type`. The discriminant is `type` for the known message names, with
id-absence as the fallback for events — a two-field dispatch that serde's
built-in tag/content support can't express. The envelope parse is cheap
(unknown fields are skipped); the second parse deserializes into the exact
target type.

**Response envelope.** Every response carries `command`, `success`, optional
`id`, optional `data` (success payload), and optional `error` (failure
message). The `command` field echoes back the command name. The `success`
boolean discriminates between a successful result (payload in `data`) and a
failure (message in `error`); the driver checks `success` before accessing
`data`. Startup handshake commands (`get_state`, `set_model`,
`set_thinking_level`) require correlated `id` responses in the backend wait
loop. Mid-session command acknowledgements without `id` are valid and are
parsed then ignored.

**Commands (driver → pi, via stdin):**

All commands are JSONL objects with a `type` field. Every command supports an
optional `id: String` field for request-response correlation — if provided, the
response echoes it back.

| Command | Fields | Purpose |
|---------|--------|---------|
| `prompt` | `message`, `images?`, `streamingBehavior?` | Send prompt. `streamingBehavior`: `"steer"` or `"followUp"` — controls queuing of messages sent during streaming |
| `steer` | `message`, `images?` | Mid-session course correction (queued during streaming) |
| `follow_up` | `message`, `images?` | Follow-up after turn completion |
| `abort` | — | Terminate current operation |
| `set_model` | `provider`, `modelId` | Switch LLM provider and model (two separate fields) |
| `set_thinking_level` | `level` | Adjust reasoning: `off`, `minimal`, `low`, `medium`, `high`, `xhigh` |
| `new_session` | `parentSession?` | Start fresh session (optional parent for forking) |
| `compact` | `customInstructions?` | Trigger manual compaction |
| `set_auto_compaction` | `enabled` | Toggle automatic compaction |
| `get_state` | — | Startup liveness/protocol-shape probe; returns current session state |
| `get_commands` | — | List slash commands, prompt templates, and skills (not startup validation) |

Loom uses the commands above as needed by backend features. Pi supports
additional commands that Loom does not use in v1: `get_messages`,
`get_session_stats`, `cycle_model`,
`get_available_models`, `cycle_thinking_level`, `set_steering_mode`,
`set_follow_up_mode`, `set_auto_retry`, `abort_retry`, `bash`, `abort_bash`,
`export_html`, `switch_session`, `fork`, `clone`, `get_fork_messages`,
`get_last_assistant_text`, `set_session_name`.

**Events (pi → driver, via stdout):**

Pi events have a `type` field and no `id`. The `message_update` event contains
a nested `assistantMessageEvent` with its own delta types — the parser must
dispatch on both levels.

| Event | Key Fields | Maps To |
|-------|------------|---------|
| `message_update` | `assistantMessageEvent` | see delta mapping below |
| `tool_execution_start` | `toolCallId`, `toolName`, `args` | `AgentEvent::ToolCall` |
| `tool_execution_end` | `toolCallId`, `toolName`, `result`, `isError` | `AgentEvent::ToolResult` |
| `tool_execution_update` | `toolCallId`, `partialResult` | logged at `trace!`, skipped |
| `turn_start` | — | logged at `trace!`, skipped |
| `turn_end` | `message`, `toolResults` | `AgentEvent::TurnEnd` |
| `agent_start` | — | logged at `trace!`, skipped |
| `agent_end` | `messages` | `AgentEvent::SessionComplete` (`exit_code: 0`, synthesized) — see note below |
| `compaction_start` | `reason` | `AgentEvent::CompactionStart` |
| `compaction_end` | `aborted`, `reason`, `result?`, `willRetry`, `errorMessage?` | `AgentEvent::CompactionEnd` |
| `queue_update` | `steering`, `followUp` | logged at `trace!`, skipped |
| `auto_retry_start` | `attempt`, `maxAttempts`, `delayMs`, `errorMessage` | logged at `debug!`, skipped |
| `auto_retry_end` | `success`, `attempt`, `finalError` | logged at `debug!`, skipped |
| `extension_error` | `extensionPath`, `event`, `error` | logged at `debug!`, skipped |

**Compaction reasons:** Pi uses `"threshold"` (approaching limit) and
`"overflow"` (already exceeded) — both map to `CompactionReason::ContextLimit`.
`"manual"` (user-triggered) maps to `CompactionReason::UserRequested`. These
are the only reasons emitted by pi as of v0.72.

**`agent_end` semantics:** In pi, `agent_end` signals "this prompt cycle is
done" — the process keeps accepting commands. Loom's per-bead-container
model maps this to `SessionComplete` because each container handles exactly
one prompt; after `agent_end`, loom tears down the container rather than
sending another command. The mapping assumes one prompt per container; pi's
`agent_end` carries no exit code, so loom synthesizes `0`.

**`message_update` delta mapping:**

The `assistantMessageEvent` sub-object carries a delta `type` field. Most
deltas are observability-only — tool lifecycle and turn boundaries are handled
by the top-level `tool_execution_*` and `turn_end` events.

| Delta Type | Maps To |
|------------|---------|
| `text_delta` | `AgentEvent::TextDelta` (extract `delta`; legacy `text` accepted) |
| `text_end` | `AgentEvent::TextEnd` |
| `thinking_delta` | `AgentEvent::ThinkingDelta` (extract `delta`; legacy `text` accepted) |
| `thinking_end` | `AgentEvent::ThinkingEnd` |
| `toolcall_delta` with `toolCallId` | `AgentEvent::ToolcallDelta` |
| `error` | `AgentEvent::Error` (reasons: `"aborted"`, `"error"`) |
| `start`, `text_start`, `thinking_start` | logged at `trace!`, skipped |
| `toolcall_start`, `toolcall_delta` without `toolCallId`, `toolcall_end` | logged at `trace!`, skipped; executable tool lifecycle is handled by top-level `tool_execution_*` events |
| `done` | logged at `trace!`, skipped (reasons: `"stop"`, `"length"`, `"toolUse"`) |

**Extension UI passthrough:** Pi emits `extension_ui_request` messages for
extension-defined UI. Loom logs these at `debug!` level — no pi extensions
are loaded in the wrix sandbox, so this should not arise in practice.
However, the timeout on these requests is set by the *extension*, not
enforced by pi: if an extension does not specify `timeout?` and the host
does not respond, the extension's promise hangs forever and may stall the
agent. As a defensive fallback, when loom observes an
`extension_ui_request` whose `method` requires a response
(`select`/`confirm`/`input`/`editor`), it replies with
`{"type":"extension_ui_response","id":"<request_id>","cancelled":true}`.
Methods that don't need a response (`notify`/`setStatus`/`setWidget`/
`setTitle`/`set_editor_text`) are logged and ignored. The auto-cancel reply
is built inside `PiParser::parse_line`, which populates `ParsedLine::response`
with the encoded `extension_ui_response` line so the runner just writes it
back to the agent's stdin — no policy lives in the workflow layer.

**Stdout discipline:** Pi v0.72+ guards its protocol stdout so
extensions, libraries, and OSC escape sequences cannot corrupt the
protocol stream. The JSONL parser's malformed-line handling (log
warning, skip line) is retained as defensive coding against any
future stdout corruption regression.

### Claude Stream-JSON Protocol

Claude Code's `--output-format stream-json` emits JSONL events.
Combined with `--input-format stream-json`, communication is bidirectional.

**Events (claude → driver, via stdout).** Unlike pi, claude messages
follow a clean tagged union: every message has a `type` field that
uniquely identifies the variant, so a single-pass deserialization with
the `type` field as discriminator suffices (no two-phase classifier
needed). The wire types and their payload shapes:

| `type` | Payload |
|--------|---------|
| `system` | `subtype`, optional `session_id` |
| `assistant` | message content (text or tool_use) |
| `user` | message content (tool_result) |
| `result` | `subtype`, optional `result`, optional `total_cost_usd`, optional `duration_ms`, optional `num_turns`, optional `is_error` |
| `control_request` | `id`, `tool`, `input` |

Any `type` value the parser does not recognize is logged at `debug!` and
skipped — claude can introduce new event types without breaking the
session.

**Event mapping:**

| Claude Event | Maps To |
|-------------|---------|
| `system` (subtype `init`) | session metadata — extract `session_id` |
| `assistant` (tool_use content) | `AgentEvent::ToolCall` |
| `assistant` (text content) | `AgentEvent::TextDelta` |
| `user` (tool_result content) | `AgentEvent::ToolResult` |
| `result` (subtype `success`) | `AgentEvent::TurnEnd` then `AgentEvent::SessionComplete` |
| `result` (subtype `error`) | `AgentEvent::Error` then `AgentEvent::SessionComplete` |
| `control_request` | log at `info!`, auto-approve via `control_response` on stdin |
| `Unknown` | logged at `debug!`, skipped |

**Permission prompt tool:** With `--permission-prompt-tool stdio`, Claude emits
`control_request` events for tool permissions and expects `control_response` on
stdin. Loom auto-approves tool calls because the container is sandboxed, but
logs every approval at `info!` level with the tool name and a truncated input
summary (first 200 chars). This provides an audit trail and makes unexpected
tool types visible in logs.

```json
{"type": "control_response", "id": "<request_id>", "approved": true}
```

**Deny-list.** A configurable `denied_tools` list under `[security]` in
`loom.toml` (e.g. `denied_tools = ["WebFetch"]`) rejects specific tool
names with `approved: false`. Empty by default — the container sandbox is
the trust boundary and logging is the primary mitigation. The slot exists
today so a deny rule can be added without a loom release if Claude Code
ships a tool type that reaches outside the container boundary.

### Compaction Handling

The harness creates a per-session scratch directory containing the
rendered prompt and a live scratchpad — see [harness.md § Compaction
Recovery](harness.md#compaction-recovery) for the file layout and
lifecycle. This section describes only how each backend delivers the
recovery content to the agent.

**Delivery is asymmetric across backends.** Claude stream-json does
not expose compaction events — Anthropic compacts internally with no
protocol notification, so claude uses its own `SessionStart` hook
system. Pi exposes compaction events natively in JSONL, so its
backend reacts to them with a `steer`-based re-pin. Direct owns the
conversation transcript itself via `loom-llm` and never sees an
external compaction event at all. The asymmetry is fundamental to
how each underlying agent (or LLM) manages context, not a Loom
design choice.

**Claude backend:**
- Before spawn, the harness writes `repin.sh` and a `claude-settings.json`
  fragment registering it under `SessionStart[matcher: compact]` into the
  container's runtime directory.
- Claude Code's hook system runs `repin.sh` on each compaction; the
  script emits a JSON envelope assembled from the scratch directory's
  `prompt.txt` and `scratch.md`. The driver is not involved at compaction
  time.

**Pi backend:**
- Knows the per-key scratch directory path from the harness's
  `SpawnConfig`.
- When a `compaction_start` event arrives in the JSONL stream, reads
  `prompt.txt` + `scratch.md` from the scratch directory and sends the
  concatenated content via a `steer` command on stdin.
- **Steer timing.** A `steer` command queues; pi delivers it after the
  current assistant turn finishes its tool calls, before the next LLM
  call — it does not inject content during compaction itself. The re-pin
  therefore reaches the agent on the *next* turn after compaction
  completes, which is the desired effect (post-compact context
  restoration).
- **Overflow auto-retry.** When `compaction_start.reason == "overflow"`
  and compaction succeeds, pi automatically retries the prompt
  (`compaction_end.willRetry == true`). A steer queued during this window
  interleaves with the auto-retry: it lands on the turn following the
  retry's first response, not before. This is acceptable — the retry
  plus re-pin combined still restore working context — but documented so
  the behavior is not surprising in logs.
- The subsequent `compaction_end` event confirms whether compaction
  succeeded (`aborted: false`) or was abandoned. If pi retries compaction
  (a fresh `compaction_start` arrives), the driver re-reads the scratch
  directory and re-pins again — the scratchpad may have grown between
  compactions.

**Direct backend:**
- Compaction is not a provider-driven event in Direct — `loom-llm`
  owns the conversation transcript itself, so there is no
  external compaction notification to react to.
- `loom-direct-runner` is responsible for its own context-budget
  management (truncation, summarization) when the conversation
  approaches model limits. The re-pin mechanism doesn't apply;
  the runner already has direct access to the rendered prompt and
  scratchpad from the start of the session.
- Context-management strategy for Direct is implementation work
  for the runner itself, not a spec-level protocol — defer until
  a Direct-driven phase actually hits context overflow in practice.

### Direct Backend

The Direct backend (`loom-agent::direct`) is the third backend
implementation, alongside Pi and Claude. Where Pi and Claude drive
subprocess agents whose tools live inside their own binaries,
Direct **composes `loom-llm::Conversation` with Loom's six
sandbox-aware tools** to assemble an agent in-process — but the
in-process is **inside the container**, not on the host. A small
`loom-direct-runner` binary ships with the direct runtime layer
and serves as the container entrypoint; it constructs the
`Conversation`, registers the six tools, runs the loop, and emits
the same `AgentEvent` JSONL stream over stdout that Pi and Claude
emit. The trust boundary (loom on host = trusted; agent in
container = sandboxed) is preserved.

Selection works identically to the other backends: per-phase
config picks it, the dispatch function selects the impl.

```toml
[phase.gate.review]
agent.backend = "direct"
agent.model_id = "claude-sonnet-4-6"
```

**The six tools.** Direct registers six sandbox-aware tools with
the Conversation: `Read`, `Write`, `Edit`, `Bash`, `Grep`, `Glob`.
These are **net-new implementations in `loom-agent::direct`** — not
shared with Claude Code (whose tools live in a closed-source
binary; no code to share). Each tool reads workspace bind-mount
paths and executes inside the container's sandbox, matching how
the subprocess backends' built-in tools behave.

**Per-call provider and caching.** Direct exposes the typed
`CacheControl` surface from `loom-llm` for prompts that want
explicit cache breakpoints; the agent's system prompt and any
long static context can be marked cached. Token usage flows back
through the standard `DriverKind::TokenUsage` event.

**Observability and safety nets.** Because Direct composes
`Conversation`, both `DoomLoopObserver` and
`DuplicateResultObserver` are active in Direct sessions by
default — without the driver doing anything special. Loom's
binary-level event chain (LogSink + driver-emitting events) sits
on top, composed via `EventSink::tee`.

**Library use vs CLI use of `loom-llm`.** The above describes
Loom's CLI use of Direct backend. External Rust consumers that
depend on `loom-llm` directly (without `loom-agent`) make their
own sandboxing decisions — `loom-llm` is just a library with no
opinion about how its tool handlers execute. The
`loom-direct-runner` binary's sandboxing is a Loom-CLI concern,
not a `loom-llm` concern.

**Dependencies.** `loom-agent::direct` depends on `loom-llm`
(internal-to-workspace dependency); both crates respect their
respective public-contract surfaces. Adding a new sandbox-aware
tool to Direct is a `loom-agent` change, independent of the
`loom-llm` surface.

### Direct Output Bounding

Direct is the only backend whose tool implementations Loom owns: a
tool's output flows `loom-agent::direct::tools` → `ToolOutput` → the
`Conversation` transcript `loom-llm` manages on Loom's behalf, so a size
cap can be applied **at the source**, before the bytes reach the
agent's context. The Pi and Claude backends produce tool output inside
their own subprocess agents and own their own transcripts — Loom only
observes their `tool_result` events *after* the content has already
entered context — so output bounding is **Direct-only by structure**,
not by policy (see *Out of Scope*).

**Single cap, offload-preferred.** Each content-returning Direct tool —
`Read`, `Bash`, `Grep`, `Glob` — bounds the bytes it places inline at
`max_inline_bytes`. `Write` and `Edit` return short status strings and
are not capped. The cap is measured against the **raw UTF-8 byte length
of the content string** the tool would emit (per stream for `Bash`),
*before* JSON serialization — not the escaped/serialized size of
`ToolOutput.content`. At or below the cap the tool returns its payload
verbatim. Above it the tool writes the **full** payload to a
session-scoped offload file and returns a structured reference in place
of the content:

```json
{ "offloaded": true,
  "path": "/workspace/.loom/scratch/<key>/offload/<hash>.txt",
  "total_bytes": 1234567,
  "total_lines": 9001,
  "head_lines": 412,
  "head": "first 412 whole lines …\n[truncated: showing 412 of 9001 lines; full output at <path>; Read with offset 413 to continue]" }
```

`head` is the longest whole-line prefix of the payload whose UTF-8 byte
length stays within `max_inline_bytes` (a single line longer than the cap
is cut at a UTF-8 character boundary, with `head_lines: 0`). Cutting on a
line boundary lets the agent **resume cleanly**: `head_lines` is the line
count of the head, so the agent recovers the tail by issuing `Read`
against `path` with `offset = head_lines + 1`. All Direct tools emit
valid UTF-8 (`Bash` via lossy conversion), so the offloaded file always
round-trips back through `Read`. The head's marker is a bracketed
truncation notice in the spirit of Grep's existing `[truncated at N
matches]`. If the offload write itself fails, the tool degrades to a
plain inline truncation: the `head` followed by a **path-less** marker
(`[truncated: showing N of M lines]`) and no offload reference — the same
shape Grep emits at its match cap.

`max_inline_bytes` bounds the `head` payload, not the whole reference:
the `{ offloaded, path, total_bytes, total_lines, head_lines }` envelope
around `head` adds a small bounded overhead, so a reference puts slightly
more than `max_inline_bytes` inline by that fixed wrapper cost. The cap
is a budget for content, not a hard ceiling on the envelope.

`Bash` output is structured (`{exit_code, stdout, stderr}`); the cap
applies to `stdout` and `stderr` **independently**, and `exit_code` is
always kept inline. A stream under the cap stays verbatim; only an
oversized stream is replaced with a reference. `Grep` applies its
existing 1000-match cap **first**; the byte cap then gates that
already-capped output, offloading only if the capped match set still
exceeds `max_inline_bytes` (e.g. long individual match lines).

**Content-addressed naming.** An offload file is named by a hash of its
payload — `<hash>.txt` under the offload directory. Naming is therefore a
pure function of the content: deterministic for fixed input (no counter,
no wall-clock, no randomness in the path) and collision-free across
distinct content. Two results with identical content dedupe to one file;
two with distinct content get distinct paths. Writes are atomic — write
to a temp file, then rename into place — so two identical-content writes
that race converge safely; this matters only if the Conversation loop
ever dispatches tool calls concurrently, which today it does not (it
dispatches strictly sequentially).

**Offload location & lifecycle.** Offload files live under `offload/`
inside the existing per-session scratch directory
(`.loom/scratch/<key>/offload/`, see [harness.md § Compaction
Recovery](harness.md#compaction-recovery)). Reusing the scratch
directory inherits its lifecycle for free: the path is already
agent-visible and `Read`-able; `<key>` is the per-session / per-bead
concurrency unit (no cross-session collision); the driver removes and
recreates the whole `<key>` tree at **session start**, so a crashed
prior session leaves no carry-over (the lazily-created `offload/` begins
empty each session); and the driver's session-end teardown removes the
tree again (no stranded files). The `git clean` reset of the bead
worktree does *not* reach the scratch tree — it is a sibling of the bead
clone, not inside it — so the start-of-session remove-and-recreate, not
`git clean`, is what guarantees a clean slate. The subdirectory is
created lazily by the runner on the first offload.

**Session-context handle.** Cap-and-offload needs per-session state (the
offload directory) the previously stateless tools did not carry, so the
six tools stop being zero-sized types and instead hold a cheap clone of a
`ToolContext` constructed once per session and passed at `six_tools(ctx)`
construction. `ToolContext` v1 carries only the offload sink: the offload
directory plus the `cap_or_offload` helper that returns a payload
verbatim under the cap or writes-and-references it above. The handle is
shaped so a future delegate / sub-agent tool could carry an `LlmClient` +
`ModelId` through the same mechanism **without** changing `six_tools`'s
signature or the `loom-llm::Tool` trait — it absorbs new per-session
capabilities additively. No delegation is built here.

**Offload observability.** Every offload emits a `driver_event` whose
`driver_kind` marks a tool-output offload, carrying the tool name and the
offloaded byte count — the sibling signal to `DriverKind::TokenUsage`. It
is the only way to see how often the cap actually bites, and the evidence
that informs whether the deferred delegation work (see *Out of Scope*) is
worth doing.

**Configuration.** `max_inline_bytes` is set by a top-level `[direct]`
block in `loom.toml` (default 16384), symmetric with the `[claude]`
block, and flows into the Direct runner via a `SpawnConfig.output_limits`
field.

### Two-Axis Composition

Container images compose from two independent axes:

| Axis | Options | Determines |
|------|---------|------------|
| **Workspace profile** | base, rust, python | Toolchain packages (cargo, python, etc.) |
| **Agent runtime** | claude, pi, direct | Agent binary that runs inside the container |

Selected profile × selected runtime → one composed image. The image
name may include both axes (for example, `wrix-rust-pi`), but bead
labels stay profile-only (`profile:rust`, not `profile:rust-pi`) and
backend selection stays in `agent.backend`. The claude runtime layer is
empty (claude is already in the base image today); the pi runtime layer
adds Node.js and the pi binary; the direct runtime layer adds the
statically-linked `loom-direct-runner` binary (which carries `loom-llm`
and the six sandbox-aware tool impls).

### Entrypoint Agent Selection

The container entrypoint branches on `WRIX_AGENT`:

- `claude` (default): existing Claude config merging, hooks,
  launching the claude binary.
- `pi`: skips Claude-specific config merging and the Claude
  permission flag; starts pi in RPC mode listening on
  stdin/stdout.
- `direct`: skips Claude-specific config and exec's
  `loom-direct-runner` listening on stdin/stdout.

All three branches preserve shared setup: git SSH, beads-dolt
connection, network filtering, session audit logging.

Loom sets `WRIX_AGENT` on the `wrix spawn` child process from the
resolved backend runtime; it does not rely on the operator's shell
already exporting it. Spawn diagnostics include `agent_backend`,
`wrix_agent_env`, and `image_ref`. Loom does not infer correctness by
parsing the image-ref string; it logs the ref for diagnosis. If the
selected image entry carries typed runtime metadata and it conflicts
with the resolved backend, Loom errors before spawn rather than letting
the entrypoint run the wrong runtime.

## Success Criteria

### Agent trait

- `Session` interoperability trait defined in `loom-events` with `prompt`, `steer`, `cancel`, `set_mode` methods
  [check](grep -q 'pub trait Session' crates/loom-events/src/lib.rs)
- `AgentBackend` trait defined in loom-driver with associated `spawn`; no `SUPPORTS_STEERING` constant (all three backends steer)
  [check](grep -q 'pub trait AgentBackend' crates/loom-driver/src/agent/backend.rs)
- `run_agent` compiles with `PiBackend`, `ClaudeBackend`, and `DirectBackend` as concrete types
  [test?](all_backends_dispatch_through_run_agent)
- `AgentEvent` enum covers: AgentStart, AgentEnd, TurnStart, TextDelta, TextEnd, ThinkingDelta, ThinkingEnd, ToolcallDelta, ToolCall, ToolResult, ToolProgress, TurnEnd, SessionComplete, CompactionStart, CompactionEnd, AutoRetry, Error, DriverEvent
  [check](cargo test -p loom-events --lib every_spec_variant_present)
- `SpawnConfig` struct captures image_ref, image_source, optional image_digest_path, workspace, env, initial_prompt, agent_args, scratch_dir
  [check](cargo test -p loom-driver --lib spawn_config_with_image_digest_path_round_trips)
- `SpawnConfig.launcher_env` exists as host-only state and is skipped from spawn-config JSON serialization
  [test](launcher_env_is_never_serialized)
- Typestate `AgentSession<Idle>` / `AgentSession<Active>` exists as an internal host-side lifecycle mechanic for JSONL subprocess sessions. It does not leak through the `Session` interoperability trait; Direct's in-container conversation loop carries no Pi / Claude handshake typestate.
  [check](grep -q 'pub struct Idle' crates/loom-driver/src/agent/session.rs)
- `Session` trait surface does not reference `AgentSession`, `Idle`, or `Active` types (typestate is private to subprocess backends)
  [check](cargo run -p loom-walk -- session_trait_does_not_expose_typestate)
- `ProtocolError` variants cover InvalidJson, UnknownMessageType, Io, ProcessExit, UnexpectedEof, LineTooLong, Unsupported, HandshakeTimeout, LockPoisoned
  [check](grep -q 'pub enum ProtocolError' crates/loom-driver/src/agent/error.rs)

### Pi backend

- Pi backend waits for the wrix container-start marker before starting the RPC probe budget
  [test](loom_todo_pi_hang_probe_surfaces_handshake_timeout)
- Pi backend sends `get_state` after startup and proceeds when the response shape is valid
  [test](startup_probe_succeeds_when_get_state_shape_is_valid)
- Pi backend fails fast if the startup `get_state` response shape is invalid
  [test](startup_probe_fails_fast_when_get_state_shape_is_invalid)
- Pi backend parses JSONL events via two-phase deserialization
  [test](full_response_classifies_and_re_deserializes)
- Pi backend sends JSONL commands to pi's stdin
  [test](driver_sends_prompt_as_jsonl_line)
- Pi backend supports steering (steer returns Ok and reaches the agent on the next turn)
  [test](driver_steers_mid_session_and_mock_observes_payload)
- Pi backend maps all pi event types to AgentEvent variants
  [test](message_update_text_delta_yields_message_delta)
- Pi backend detects CompactionStart event, reads `prompt.txt` + `scratch.md` from the per-key scratch directory, and sends the concatenated content via steer
  [test](driver_repins_on_compaction_start_via_steer)
- Pi backend handles malformed JSONL gracefully (logs warning, continues)
  [test](malformed_json_returns_invalid_json_error)
- Pi backend logs extension_ui_request at debug level without responding
  [test](extension_ui_notify_leaves_response_none)

### Claude backend

- Claude backend parses stream-json JSONL events from claude's stdout
  [test](parses_assistant_text_and_tool_use)
- Claude backend uses `#[serde(tag = "type")]` for tagged enum deserialization
  [check](grep -q 'serde(tag = "type")' crates/loom-agent/src/claude/messages.rs)
- Claude backend maps claude event types to AgentEvent variants
  [test](result_success_yields_turn_end_then_session_complete)
- Claude backend captures cost_usd from result events
  [test](result_event_captures_cost_usd)
- Claude backend handles unknown event types via `#[serde(other)]`
  [test](unknown_message_type_returns_empty_events)
- Claude backend's `repin.sh` is registered under `SessionStart[matcher: compact]` before spawn, and the script emits a JSON envelope containing the scratch directory's `prompt.txt` + `scratch.md` when fired
  [test](claude_settings_registers_repin_under_session_start_compact)
- Claude backend auto-approves permission requests via control_response
  [test](control_request_autoapproves_when_denylist_empty)
- Claude backend supports steering — sends a stream-json user message via stdin during the session and verifies the agent receives it
  [test](steering_message_reaches_mock_and_emits_followup_turn)
- Claude backend shutdown watchdog: on `result` event, loom closes stdin; if claude does not exit within grace period, sends SIGTERM then SIGKILL
  [test](shutdown_watchdog_escalates_to_sigkill_when_child_ignores_stdin_close)

### Direct backend

- Direct backend's `Session` impl spawns a container via `wrix spawn` with the `direct` runtime layer; the container's entrypoint exec's `loom-direct-runner`
  [test](direct_session_spawn_invokes_wrix_spawn_with_direct_runtime)
- `loom-direct-runner` constructs a `loom-llm::Conversation`, registers the six sandbox-aware tools, runs the loop, and emits `AgentEvent` JSONL to stdout — the same common event shape as the subprocess backends
  [test?](direct_runner_emits_agent_event_jsonl_compatible_with_common_agent_events)
- Direct registers exactly six tools by name: `Read`, `Write`, `Edit`, `Bash`, `Grep`, `Glob`
  [test](direct_runner_registers_canonical_six_tools)
- Each Direct tool's impl lives in `loom-agent::direct::tools` — net-new code, not re-exported from any other crate
  [check](cargo run -p loom-walk -- direct_tools_net_new)
- Direct tools execute against the container's bind-mounted workspace; absolute paths under `/workspace/...` resolve inside the container
  [test](direct_tools_read_against_container_workspace_mount)
- `DoomLoopObserver` and `DuplicateResultObserver` are composed into the Conversation's sink by default in `loom-direct-runner`
  [test](direct_runner_composes_default_observers)
- Direct backend respects per-phase `agent.model_id` config; resolves through `ModelId::from_str` (with `Other` fallback for unknown models)
  [test](direct_model_id_respects_phase_config)
- Per-call `CacheControl::Ephemeral(CacheTtl)` markers in the runner's prompt construction flow through to provider requests (Anthropic confirmed via mock; OpenAI/Gemini no-op)
  [test](direct_cache_control_propagates_to_anthropic_request)
- `DriverKind::TokenUsage` event emits on every completion within Direct sessions
  [test](direct_emits_token_usage_per_completion)

### Direct output bounding

- `Read` whose returned content exceeds `max_inline_bytes` returns an `{ offloaded, path, total_bytes, total_lines, head_lines, head }` reference whose `head` is a whole-line prefix within the cap; the full payload is written to the offload file
  [test](read_over_cap_offloads_full_payload_and_returns_head_reference)
- The cap is measured on the raw UTF-8 byte length of the tool's content string (per stream for `Bash`), not the JSON-serialized size of `ToolOutput.content`
  [test](cap_measured_on_raw_utf8_byte_length_not_serialized)
- `Bash` caps `stdout` and `stderr` independently and always keeps `exit_code` inline; an oversized stream is offloaded while an under-cap stream stays verbatim
  [test?](bash_caps_streams_independently_keeps_exit_code_inline)
- `Grep` and `Glob` string output exceeding `max_inline_bytes` is offloaded and replaced with a reference
  [test?](grep_and_glob_offload_string_output_over_cap)
- The agent recovers the tail by issuing `Read` against the offload `path` with `offset = head_lines + 1`, reconstructing the full original content (offload round-trips)
  [test](offloaded_file_round_trips_through_read_via_head_lines_offset)
- Two results with distinct content offload to distinct paths; the file name is a deterministic content hash for fixed input
  [test](distinct_content_offloads_to_distinct_deterministic_paths)
- When the offload write fails, the tool degrades to an inline truncation (head + marker, no `path`) rather than erroring
  [test](offload_write_failure_degrades_to_inline_truncation)
- Every offload emits a `driver_event` (offload `driver_kind`) carrying the tool name and offloaded byte count
  [test](offload_emits_driver_event_with_tool_and_byte_count)
- `[direct].max_inline_bytes` resolves from `loom.toml` into `SpawnConfig.output_limits`, defaulting to 16384 when absent
  [test](direct_max_inline_bytes_resolves_from_config_default_16384)
- `ToolContext` is shaped so a future delegate tool can carry an `LlmClient` + `ModelId` through it without changing `six_tools`'s signature or the `loom-llm::Tool` trait
  [judge](../tests/judges/loom.sh#judge_tool_context_shape)

### Backend selection

- Per-phase config resolves correct backend (`[phase.todo].agent.backend` overrides `[phase.default].agent.backend`)
  [test](agent_for_per_phase_resolves_override_and_default)
- `--agent` CLI flag accepts `pi`, `claude`, and `direct` and overrides all phase config for the invocation
  [test](loom_accepts_agent_backend_values)
- Default (no phase config, no flag) selects claude
  [test](agent_for_default_is_claude_when_config_empty)
- Invalid backend name produces clear error
  [test](agent_for_unknown_backend_in_default_returns_error)
- Pi backend calls `set_model` after spawn when phase config specifies provider/model
  [test](set_model_from_phase_config_reaches_mock_pi)
- Pi backend hard-fails the handshake when pi rejects configured `set_model`
  [test](set_model_rejection_from_pi_hard_fails_handshake)
- Pi backend sends best-effort `set_thinking_level` when phase config sets it
  [test](set_thinking_level_from_phase_config_reaches_mock_pi)
- Pi backend skips `set_thinking_level` entirely when phase config leaves it unset
  [test](set_thinking_level_skipped_when_config_none)
- Pi backend tolerates pi rejection of `set_thinking_level` without aborting the handshake
  [test](set_thinking_level_tolerates_pi_rejection)
- Backend runtime names map to the `WRIX_AGENT` child-env values exactly: Pi → `pi`, Claude → `claude`, Direct → `direct`
  [test?](agent_runtime_name_maps_to_wrix_agent_values)

### Interactive shell-out

- `loom plan` exports `WRIX_DEFAULT_IMAGE_REF` / `WRIX_DEFAULT_IMAGE_SOURCE` and backend-derived `WRIX_AGENT` to `wrix run` (no `--profile` argv flag — `wrix run` has no parser for it), with the profile/runtime image resolved through `LoomConfig::agent_for(Phase::Plan)`
  [test?](plan_runner_passes_resolved_profile_runtime_to_wrix_run)
- `loom msg --chat` exports `WRIX_DEFAULT_IMAGE_REF` / `WRIX_DEFAULT_IMAGE_SOURCE` and backend-derived `WRIX_AGENT` to `wrix run` (no `--profile` argv flag), with the profile/runtime image resolved through `LoomConfig::agent_for(Phase::Msg)`
  [test?](msg_chat_passes_resolved_profile_runtime_to_wrix_run)
- `[phase.default].profile` alone (no per-phase override, no CLI override) reaches `Phase::Plan` via the env-var hand-off
  [test](plan_phase_default_profile_alone_picks_manifest_entry)
- Direct backend selection for `loom plan` or `loom msg --chat` fails before spawning Wrix because Direct has no interactive REPL command
  [test?](interactive_shell_out_rejects_direct_backend)

### Container integration

- Loom spawns containers via `wrix spawn --spawn-config <file>
      --stdio` with the correct profile/runtime image, never via `podman run` directly
  [test](wrix_spawn_invocation_records_correct_argv)
- Every `wrix spawn` child process receives `WRIX_AGENT` from the resolved backend runtime, independent of whether the parent shell has `WRIX_AGENT` set
  [test](wrix_spawn_child_env_sets_backend_derived_wrix_agent)
- Spawn diagnostics log `agent_backend`, `wrix_agent_env`, and `image_ref`; typed image-runtime metadata mismatches fail before spawn
  [test](wrix_spawn_logs_backend_runtime_and_image_ref)
- Container receives agent stdin/stdout via pipe
  [test](child_stdin_is_a_pipe_not_a_tty)
- Entrypoint starts pi in RPC mode when `WRIX_AGENT=pi`
  [check](bash -c "grep -q 'pi --mode rpc' $(nix build --no-link --print-out-paths .#wrixSrc 2>/dev/null)/lib/sandbox/linux/entrypoint.sh")
- Entrypoint starts claude normally when `WRIX_AGENT=claude`
  [check](bash -c "grep -q 'dangerously-skip-permissions' $(nix build --no-link --print-out-paths .#wrixSrc 2>/dev/null)/lib/sandbox/linux/entrypoint.sh")
- Entrypoint starts the Direct runner when `WRIX_AGENT=direct`
  [check](bash -c "grep -q 'loom-direct-runner' $(nix build --no-link --print-out-paths .#wrixSrc 2>/dev/null)/lib/sandbox/linux/entrypoint.sh")
- Entrypoint preserves git SSH, beads, network filtering for all runtime branches
  [check](bash -c "grep -q '/git-ssh-setup.sh' $(nix build --no-link --print-out-paths .#wrixSrc 2>/dev/null)/lib/sandbox/linux/entrypoint.sh")

### Agent runtime layer

- The smoke/default Loom sandbox image builds a concrete profile/runtime
  image with the Pi agent runtime selected explicitly (`agent = "pi"`),
  matching wrix's one-agent-per-image format.
  [system](nix build .#sandbox)
- The selected Pi agent binary launches inside that built sandbox image and
  responds to `--version`; failures identify when the selected runtime is
  missing or broken.
  [system](nix run .#test-sandbox)

## Requirements

### Functional

1. **Host-side execution** — Loom runs on the host, not inside containers. It
   spawns per-bead containers by invoking `wrix spawn --spawn-config
   <file> --stdio` (a thin wrix subcommand that owns container construction)
   and communicates with the agent process inside via stdin/stdout pipes.
   Loom never calls `podman run` directly; see
   [harness.md — Process Architecture](harness.md#process-architecture).
2. **Agent backend trait** — an async Rust trait (`AgentBackend`) abstracting
   agent lifecycle: spawn a session. Used via type parameter
   (`<B: AgentBackend>`) — the concrete backend is known at each call site.
3. **Pi backend** — speaks pi-mono's JSONL RPC protocol over stdin/stdout.
   Commands:
   - `prompt` — send initial or follow-up prompts
   - `steer` — mid-session course correction
   - `abort` — terminate current operation
   - `set_thinking_level` — adjust reasoning effort (best-effort: sent only
     when the phase config requests it, and silently skipped if pi rejects it)
   - `set_model` — switch LLM provider/model mid-session

   Plus streaming event parsing for message deltas, tool calls, tool results,
   completion, compaction, and errors.
4. **Claude backend** — launches
   `claude --print --input-format stream-json --output-format stream-json`,
   parses JSONL events from stdout, and writes user messages (initial
   prompt, steering) as stream-json on stdin. `--permission-prompt-tool
   stdio` enables the `control_request` / `control_response` flow. The
   `--print` flag keeps the session non-interactive (runs to completion,
   exits) while `--input-format stream-json` enables mid-session steering
   via additional user messages. On observing a `result` event, loom closes
   its end of stdin, waits `[claude] post_result_grace_secs` (default 5s)
   for natural exit, then escalates SIGTERM → SIGKILL.
5. **Direct backend** — composes `loom-llm::Conversation` with Loom's six
   sandbox-aware tools (`Read`, `Write`, `Edit`, `Bash`, `Grep`, `Glob`).
   The actual agent loop runs inside a per-bead container via the
   `loom-direct-runner` entrypoint binary that ships in the `direct`
   runtime layer — preserving the trust boundary (loom on host = trusted;
   agent in container = sandboxed) identically to Pi and Claude. Direct's
   tools are net-new implementations in `loom-agent::direct`, not shared
   with Claude Code (closed-source) or with consumer-supplied tools (which
   consumers register via `Conversation::register` in their own apps when
   using `loom-llm` as a library). All `loom-llm` features — typed
   `CacheControl`, structured output via `T: DeserializeOwned + JsonSchema`,
   per-call `ModelId`, `DoomLoopObserver` + `DuplicateResultObserver`
   composed by default, `DriverKind::TokenUsage` events — are available
   in Direct sessions.
6. **Per-phase backend selection** — each workflow phase (plan, todo, loop,
   gate, msg) independently resolves its backend and model from config.
   `[phase.default].agent.backend` sets the fallback (`claude`). Per-phase
   overrides (e.g. `[phase.todo]`) carry `agent.backend` plus optional
   `agent.provider` and `agent.model_id`. Valid `agent.backend` values are
   `claude`, `pi`, and `direct`. `--agent` CLI flag overrides all phase
   config for the current invocation.
7. **Interactive shell-out profile contract** — `loom plan` and `loom msg
   --chat` bypass the agent-backend abstraction (so stdio can attach as a
   REPL) and invoke `wrix run <workspace> <agent command> ... <prompt>`
   directly. The profile and chat-capable backend resolved by
   `LoomConfig::agent_for(Phase)` select the image and command together:
   Claude uses `claude --dangerously-skip-permissions`, while Pi uses `pi`.
   Direct is rejected for these interactive phases before Wrix spawn/run.
   The matching profile/runtime `ImageEntry` is exported to `wrix run`
   via the `WRIX_DEFAULT_IMAGE_REF` / `WRIX_DEFAULT_IMAGE_SOURCE` env vars,
   and the backend-derived `WRIX_AGENT` is set on the child process. The
   env-var hand-off is the sole image-selection contract on this path;
   `wrix run` has no `--profile` parser, and extra argv tokens between the
   workspace positional and agent command would be forwarded into the
   container as the command vector (exit 127).
8. **Agent runtime layer** — the image builder composes two orthogonal axes:
   *workspace profile* (base, rust, python) and *agent runtime* (claude, pi,
   direct). The bundled profile manifest contains concrete entries for the
   configured profile/runtime pairs. The selected runtime layer is added to
   the selected workspace profile: Pi contributes Node.js + the pi binary,
   Direct contributes `loom-direct-runner`, and Claude uses the Claude-capable
   base layer. The variants are distinct image entries rather than multiple
   agent binaries selected from one undifferentiated image.
9. **Entrypoint agent selection** — `entrypoint.sh` checks `WRIX_AGENT` and:
   - `claude` (default): existing behavior (Claude config merging, hooks,
     `claude --dangerously-skip-permissions`)
   - `pi`: skips Claude-specific config, starts `pi --mode rpc` listening on
     stdin/stdout
   - `direct`: skips Claude-specific config, exec's `loom-direct-runner`
     listening on stdin/stdout
10. **Event normalization** — all three backends emit a common `AgentEvent` enum so
    the workflow engine does not need backend-specific event handling.
11. **JSONL framing** — all three backends' wire protocols use JSON Lines
    (one complete JSON object per line, separated by `\n`). The JSONL
    reader splits on `\n` only, not Unicode line separators (U+2028,
    U+2029). Each line is independently parseable.
12. **Direct output bounding** — content-returning Direct tools (`Read`,
    `Bash`, `Grep`, `Glob`) cap the bytes they place inline at
    `max_inline_bytes` (the `[direct]` block, default 16384). Above the
    cap the tool writes the full payload to a content-addressed file under
    the per-session scratch offload directory and returns a
    `{ offloaded, path, total_bytes, total_lines, head_lines, head }`
    reference whose `head` is a whole-line prefix (so the agent resumes via
    `Read` at `offset = head_lines + 1`); an offload-write failure degrades
    to an inline truncation marker. The cap is measured on the raw UTF-8
    byte length of the content string (per stream for `Bash`, which keeps
    `exit_code` inline and caps `stdout`/`stderr` independently); offload
    writes are atomic (temp-then-rename). `Write` and `Edit` are not
    capped. Every offload emits a `driver_event` recording the tool and
    byte count. The per-session `ToolContext` handle carries the offload
    sink and is shaped to absorb a future delegate tool's `LlmClient` +
    `ModelId` without altering `six_tools` or the `Tool` trait. See
    [Direct Output Bounding](#direct-output-bounding).

### Non-Functional

1. **No podman socket mounting** — `wrix spawn` invokes podman on the
   host; the agent runs inside the resulting container with no access to the
   podman socket. No nested container support needed.
2. **Graceful degradation** — if a backend-specific feature is unavailable
   (e.g. pi providers that don't support `set_thinking_level`, or pi
   builds where manual `compact` is disabled), the driver continues
   without it. No hard failures for missing optional capabilities.
3. **Parse, Don't Validate** — raw protocol bytes are parsed into typed domain
   representations at the JSONL boundary. All code downstream of the parser
   works with already-validated types. No re-parsing, no stringly-typed event
   matching.
4. **Static dispatch** — `AgentBackend` uses an explicit type parameter
   (`<B: AgentBackend>`), not a trait object. Backends are zero-sized types
   with associated functions (no `&self`). A `dispatch` function in the
   binary crate matches on `AgentRuntime` per phase and calls
   `run_agent::<ConcreteType>`. No `async-trait` needed — `async fn` in
   traits is stable and works directly with static dispatch.

## Out of Scope

- **Pi-mono extensions** — Loom controls pi via RPC, not via pi's extension
  system. No TypeScript extensions are written or loaded.
- **Pi-mono web-ui** — terminal-only integration.
- **Pi-mono forking or vendoring** — consumed as an npm package bundled by
  Nix. No source-level fork.
- **macOS (Darwin) support for pi runtime layer** — initially Linux
  containers only. Darwin support is a follow-up.
- **Tool-set sharing with Claude Code** — Claude Code is a closed-source
  binary; its built-in tool implementations are not available to share.
  Loom's six sandbox-aware tools in `loom-agent::direct` are net-new
  Rust implementations.
- **Sharing Direct's tools with consumer-driven `loom-llm` use** —
  consumers depending on `loom-llm` directly register their own custom
  tools via `Conversation::register`. The six sandbox-aware tools live in
  `loom-agent::direct` (internal); their sandboxing model assumes the
  `loom-direct-runner` container context. Consumers building their own
  Rust apps on `loom-llm` make their own sandboxing decisions per
  [llm.md — Two Consumer Paths](llm.md#two-consumer-paths).
- **Transcript-rewriting dedup in pi/Claude backends** — pi-mono and
  Claude Code own their own transcripts; Loom does not intercept and
  rewrite them. The `DuplicateResultObserver` (see [llm.md —
  Agent-Loop Observers](llm.md#agent-loop-observers)) emits
  observability events about duplicates but never rewrites. Future
  transcript-rewriting work, if any, would be Direct-backend-only.
- **Multiple simultaneous backends** — one backend per phase invocation.
  No mixing of backends within a single phase; parallel sessions all use
  the backend resolved for that phase.
- **Claude Code RPC mode** — if Anthropic ships an RPC mode for Claude Code,
  the Claude backend can be upgraded. Not in scope today.
- **Output bounding for the Pi and Claude backends** — those agents own
  their tool implementations and transcripts; Loom only sees their
  `tool_result` events after content has entered context, so it bounds tool
  output only for Direct, where it owns the tools. See [Direct Output
  Bounding](#direct-output-bounding).
- **Sub-agent / delegation tool** — `ToolContext` is shaped so a future
  delegate tool could carry an `LlmClient` + `ModelId`, but no delegation
  tool is built in this work.
