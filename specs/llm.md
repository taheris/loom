# Loom-LLM

Typed multi-provider LLM primitives, Conversation with built-in
tool-use loop, and agent-loop observers for both Loom's binary and
external Rust consumers.

## Problem Statement

Loom's Direct backend needs typed multi-provider LLM access with
per-call model selection, typed prompt-cache markers, and
structured-output deserialization. The same primitives are useful
to external Rust crates (e.g. RAG pipelines, domain-specific
review tools) that want typed LLM calls without taking on Loom's
CLI / workflow / beads surface.

`llm` is the public-contract crate exposing those primitives.
Its detailed wrapping rationale lives in [Wrapper Thickness](#wrapper-thickness);
the short version: a typed wrapper over a multi-provider LLM crate
gives us a stable consumer-facing surface, room for enrichment
(token-usage events, observer composition), and a single-crate
swap path if the underlying crate becomes unmaintained.

[harness.md](harness.md) owns the broader platform (crate
graph, process architecture, configuration); this spec owns the
loom-llm public surface and the agent-loop observers it hosts.
[agent.md](agent.md) owns the Direct backend that wraps
loom-llm internally to satisfy the `Session` trait.

## Architecture

### Two Consumer Paths

Two consumers depend on `llm`:

1. **Internal:** `loom-agent::direct` wraps `Conversation` with
   Loom's six sandbox-aware tools to satisfy the `Session` trait
   for the Direct backend. See
   [agent.md — Direct Backend](agent.md#direct-backend).
2. **External:** Rust crates outside the loom workspace
   (e.g. RAG pipelines, domain-specific review tools) depend on
   `llm` for typed multi-provider LLM calls without taking
   on Loom's CLI / workflow / beads surface.

The same `Conversation` runs on both paths; observers, cache
control, structured output, and token-usage events fire identically
regardless of which consumer is driving.

### Wrapper Thickness

`llm` is a **typed wrapper**, not a thin re-export. The
wrapper:

- Insulates consumers from the underlying multi-provider LLM
  crate's API churn — a future swap is a single-crate internal
  change rather than a breaking change for every consumer
- Enables enrichment at the boundary: token-usage `AgentEvent`
  emission on every completion, default observer composition,
  consistent error types
- Carries bus-factor mitigation for the underlying crate — a
  minimal provider client (Anthropic Messages, at minimum) can
  be vendored as a contingency seed inside `llm` without
  changing the public surface

### `LlmClient` Trait

Per-call model selection (no fixed-model client construction):

```rust
pub trait LlmClient: Send + Sync {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse>;
    async fn complete_structured<T>(&self, req: CompletionRequest) -> Result<T>
        where T: DeserializeOwned + JsonSchema;
}
```

The same client instance accepts a different model on every call.
Model is a required positional argument on the request, not a
client-construction parameter — the type system forbids
constructing a request without naming the model.

### `CompletionRequest`

Builder shape; messages typed; cache control typed per content
block:

```rust
let req = CompletionRequest::new(ModelId::ClaudeSonnet46)
    .system("Short instruction prefix")
    .user_cached("Long context document…", CacheControl::Ephemeral(CacheTtl::Hours1))
    .user("Question that varies per call")
    .max_tokens(2048);
```

Each `Message::*` and `Message::*_cached` constructor produces a
typed content block; consumers compose blocks via the builder
rather than handing in JSON objects.

### `ModelId`

Typed enum with `Other(String)` fallback — same pattern as
`DriverKind` in [loom-events](harness.md#event-schema). Known
providers and models are variants; unknown / fine-tuned / custom
models pass through `Other`. Provider routing inferred from
variant; `Other` routes via prefix match on the string. Adding a
known model is a minor version bump.

### `CacheControl`

```rust
pub enum CacheControl {
    None,
    Ephemeral(CacheTtl),
}
pub enum CacheTtl { Minutes5, Hours1, Hours24 }
```

Per-content-block granularity via `Message::*_cached(content,
CacheControl)`. The TTL set matches Anthropic's prompt-cache
breakpoint API. Providers that do not support typed per-block
cache markers (e.g. OpenAI today) no-op the marker without error.

### Structured Output

`complete_structured::<T>(req)` is **one method** that hides the
provider-specific structured-output mechanism. Internally
`llm` picks the right path per provider — synthetic
forced-tool for Anthropic, `response_format` for OpenAI,
`response_schema` for Gemini — and deserializes into `T`. The
bound `T: DeserializeOwned + JsonSchema` means the type carries
its own schema via `schemars`. Consumers never write
provider-specific code or see the mechanism difference; switching
providers is a `ModelId` variant change.

### `TokenUsage`

Every `CompletionResponse` carries:

```rust
pub struct TokenUsage {
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_write: u32,
    pub cost_cents: u32,
}
```

The same surface drives SaaS billing pipelines via a
`DriverKind::TokenUsage` `AgentEvent` emitted on every `complete*`
call. Consumers see cache hits directly and can make cost-aware
decisions.

### `Conversation` and the Built-in Tool-Use Loop

For multi-turn work with tool calls, consumers register tool
handlers via a `Tool` trait and call `run`:

```rust
let mut conv = Conversation::new(ModelId::ClaudeSonnet46)
    .system("...")
    .register(MyCustomTool::new())
    .max_iterations(50)
    .on_iteration_exhausted(LoopOutcome::Error);

conv.user("Do the thing.");
let resp = conv.run(&client).await?;
```

The loop iterates `complete → tool_calls? → dispatch handlers →
tool_results → complete` until the agent stops calling tools or
the budget is exhausted. Behaviour on exhaustion is
consumer-selectable via `LoopOutcome` (`Error`, `ReturnLast`, or
a custom variant). Cancellation via standard tokio primitives;
per-iteration timeout is configurable.

Live event observation during the loop is via the `EventSink`
chain attached to the driving `LlmClient` (see
[TokenUsage criterion](#public-surface) and `Client::with_event_sink`);
`Conversation` does not expose a separate streaming entry point.
Consumers that want a `Stream<Item = AgentEvent>` wrap a short
mpsc-backed `EventSink` impl — paid by the one consumer that needs
it, not by every consumer up-front.

### `Tool` Trait

The handler abstraction:

```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    async fn invoke(&self, args: serde_json::Value) -> Result<ToolOutput>;
}
```

Trait shape is designed so an impl is reasonably convertible to
other ecosystem agent-loop crates' tool shapes (e.g. the
`agent-client-protocol` crate's tool surface, or Rust agent-runtime
crates that grow market traction). This forward-compatibility
constraint keeps the option open to re-host `Conversation` on a
different agent-loop crate later without taking the dep now.

### Agent-Loop Observers

Two observers ship in `llm`. Both implement the `EventSink`
trait defined in `loom-events` (see
[loom-harness — EventSink and SessionCommand](harness.md#eventsink-and-sessioncommand))
and are composed into `Conversation`'s default sink chain.
Consumers driving via `Conversation::run` get the safety nets out
of the box; Loom's binary composes the same observers when driving
Pi / Claude / Direct backends. Observer state resets on
`CompactionEnd` (not on `TurnEnd` — agent doom loops routinely
span turns; compaction is the actual context reset).

#### `DoomLoopObserver`

Detects when the agent calls the same tool with the same params
*and* the same result repeated — a known agent failure mode where
the LLM is stuck retrying an action that isn't moving the world.

- **Key:** `(CallKey, ResultHash)` where `CallKey = (tool_name,
  canonical_params)` (canonical JSON per RFC 8785 JCS, normalized
  numbers) and `ResultHash` is BLAKE3-16 of the canonical result
  payload (shared with `DuplicateResultObserver`'s hashing).
- **Detection:** 3 of the last 5 entries are identical pairs.
  Strict-consecutive is too narrow (misses oscillation patterns
  like `ABABA` that *are* real loops); whole-window-of-32 is too
  loose. The 3-of-5 sliding-window catches both `AAA` and
  oscillation while keeping the false-positive surface manageable.
- **Two-stage response:**
  - **Stage 1** — `SessionCommand::Steer` with a nudge that names
    the tool, states that result and params have been identical,
    declares the remaining budget before abort, and invites the
    agent to reconsider or escalate to `LOOM_BLOCKED`.
  - **Stage 2** — after stage 1, if **N more** identical pairs
    occur for the same CallKey (configurable
    `stage_2_after_stage_1`, default 3), emit
    `SessionCommand::Abort("doom-loop: <tool>")`. The driver
    classifies this through the verdict gate as recovery cause
    `observer-abort` (see [loom-harness — Verdict Gate](harness.md#verdict-gate)).
  - The gap between stage 1 and stage 2 is the *structural
    escape hatch*: legitimate polling that needed nudging stops
    naturally, or the agent escalates manually; only persistent
    repetition after explicit feedback aborts.
- **Emits** `DriverKind::DoomLoopTripped { stage: 1|2, tool,
  params, call_id }` for observability — both stages surface,
  enabling downstream analysis of nudge effectiveness.

#### `DuplicateResultObserver`

Pure observability. Detects any tool result whose payload
duplicates an earlier result in the same session, regardless of
which tool produced it (e.g. agent reads file A and file B and
gets bytewise-identical content; agent re-fetches the same record).
Surfaces wasted-token signal for SaaS billing pipelines and local
diagnostics.

- **Key:** BLAKE3-16 of canonical result payload (shared
  infrastructure with `DoomLoopObserver`).
- **Map:** `HashMap<ResultHash, FirstCallId>` — first seen wins.
- **Threshold:** skip results below `min_bytes` (default 256 B)
  so short outputs like `"ok"` don't dominate.
- **`react()` returns empty `Vec`** (default) — never sends
  commands; observability only. Transcript rewriting is closed
  for pi/Claude (those backends own their transcripts);
  rewriting inside Direct is deferred follow-up work.
- **Emits** `DriverKind::DuplicateToolResult { original_call_id,
  repeated_call_id, bytes_wasted }`.

Both observers are **enabled by default** — safety nets for known
agent failure modes, not experimental features. Users opt out via
`[agent.doom_loop] enabled = false` / `[agent.duplicate_result]
enabled = false` in Loom's CLI config (see [Configuration](#configuration)).
Consumer-driven `Conversation` runs disable per-Conversation via
the builder.

## Configuration

CLI-side configuration of the observers lives under the `[agent.*]`
blocks of `LoomConfig` (see
[harness.md — Configuration](harness.md#configuration)
for the surrounding config schema). External consumers driving
`Conversation` directly configure via the builder; the same
defaults apply.

```toml
[agent.doom_loop]
enabled = true
window = 5
threshold = 3
stage_2_after_stage_1 = 3

[agent.duplicate_result]
enabled = true
min_bytes = 256
```

## Success Criteria

### Public surface

- `llm` exposes `LlmClient` trait with `complete(req)` and `complete_structured::<T>(req)` (no `embed` in v1)
  [check](cargo run -p loom-walk -- loom_llm_public_surface)
- `CompletionRequest::new(ModelId)` requires model as positional argument; constructing a request without a model is a compile error
  [test](completion_request_requires_model_at_construction)
- `ModelId` is a typed enum with variants for known models plus `Other(String)` fallback; provider routing inferred from variant or `Other` prefix
  [test](modelid_other_fallback_routes_provider_by_prefix)
- `complete_structured::<T>` hides provider mechanism: same call shape works for Anthropic (synthetic forced-tool), OpenAI (`response_format`), Gemini (`response_schema`); returned `T: DeserializeOwned + JsonSchema` is deserialized regardless of provider
  [test](complete_structured_returns_typed_t_across_providers)
- `CompletionResponse` carries `usage: TokenUsage { input, output, cache_read, cache_write, cost_cents }` on every successful call
  [test](completion_response_carries_usage_with_cache_fields)
- `complete*` calls emit `DriverKind::TokenUsage` event into the active `EventSink` chain (so SaaS billing pipelines tail the same AgentEvent stream)
  [test](complete_emits_token_usage_driver_event)

### Cache control

- `CacheControl::Ephemeral(CacheTtl)` typed with three `CacheTtl` variants: `Minutes5`, `Hours1`, `Hours24` (matches Anthropic-supported set)
  [test](cache_control_ttl_set_matches_anthropic_supported)
- Cache markers apply per-content-block via `Message::*_cached(...)`; consumers control where the cache breakpoint lands
  [test](message_text_cached_marks_per_block_in_anthropic_request)
- Providers that do not support cache markers (e.g. OpenAI today) no-op the marker without error
  [test](cache_marker_no_ops_on_openai_provider)

### Conversation + tool-use loop

- `Conversation::new(ModelId)` returns a builder accepting `system`, tool registration via `register(impl Tool)`, `max_iterations`, `on_iteration_exhausted(LoopOutcome)`
  [test](conversation_builder_accepts_documented_knobs)
- `Tool` trait has `name`, `description`, `input_schema`, async `invoke(args) -> Result<ToolOutput>` — no closure-only registration
  [check](grep -q 'pub trait Tool' crates/loom-llm/src/tool.rs)
- `Conversation::run(&client)` runs the tool-use loop to completion and returns the final `CompletionResponse`; live event observation during the loop happens via the `EventSink` chain attached to the driving `LlmClient` (see `complete_emits_token_usage_driver_event`), not via a separate streaming entry point
  [test](conversation_run_completes_loop_and_returns_final_response)
- Loop respects `max_iterations`; on exhaustion behaves per `on_iteration_exhausted` (default `LoopOutcome::Error`)
  [test](conversation_loop_respects_max_iterations)
- Loop respects tokio cancellation: dropping the future cancels the in-flight LLM call and tool invocation
  [test](conversation_loop_cancellation_aborts_in_flight_work)
- `Tool` trait shape is convertible to ecosystem agent-loop tool shapes (Anthropic tool-schema JSON; forward-compat smoke test against an external tool trait — re-evaluated each loom release)
  [judge](../tests/judges/loom.sh#judge_tool_trait_ecosystem_compat)

### Wrapper boundary

- `llm` is a typed wrapper, not a thin re-export: the public surface (`LlmClient`, `CompletionRequest`, `Message`, `ModelId`, `CacheControl`, `Tool`, `Conversation`) is defined in `llm`, not re-exported from the underlying multi-provider crate
  [check](cargo run -p loom-walk -- loom_llm_no_underlying_crate_reexports)

### Agent-loop observers

**DoomLoopObserver**

- Detector keys on `(CallKey, ResultHash)` where `CallKey = (tool_name, canonical_params)` via RFC 8785 JCS and `ResultHash = BLAKE3-16(canonical_result)`
  [test](doom_loop_key_uses_canonical_call_args_and_result_hash)
- Stage 1 fires when 3 of the last 5 entries in the per-CallKey window are identical pairs
  [test](doom_loop_stage_1_fires_at_3_of_5_identical)
- Stage 1 emits `SessionCommand::Steer` with a message naming the tool, the explicit budget before abort, and an invitation to reconsider or escalate to `LOOM_BLOCKED`
  [test](doom_loop_stage_1_steer_names_tool_budget_and_escalation_path)
- Stage 1 also emits `DriverKind::DoomLoopTripped { stage: 1, tool, params, call_id }` for observability
  [test](doom_loop_stage_1_emits_driver_event)
- Stage 2 fires only after `stage_2_after_stage_1` additional identical pairs for the same CallKey (default 3); emits `SessionCommand::Abort` with `"doom-loop: <tool>"` reason
  [test](doom_loop_stage_2_requires_configurable_extra_pairs_after_stage_1)
- Stage 2 also emits `DriverKind::DoomLoopTripped { stage: 2, ... }`
  [test](doom_loop_stage_2_emits_driver_event)
- Observer state (window + stage state) resets on `CompactionEnd`; does NOT reset on `TurnEnd`
  [test](doom_loop_resets_on_compaction_end_not_turn_end)
- Enabled by default; `[agent.doom_loop] enabled = false` disables; `Conversation` builder exposes the same knob for consumer override
  [test](doom_loop_config_disable_path)

**DuplicateResultObserver**

- Pure observability: `react()` returns empty `Vec` on every call (no `SessionCommand`s ever emitted)
  [test](duplicate_result_react_always_returns_empty)
- Detector keys on `ResultHash` alone (BLAKE3-16 of canonical result payload); first-seen call ID wins, subsequent matches emit duplicate events
  [test](duplicate_result_first_seen_wins_subsequent_emit)
- Skip results below `[agent.duplicate_result] min_bytes` (default 256 B); shorter results don't populate the map
  [test](duplicate_result_ignores_payloads_below_min_bytes)
- Emits `DriverKind::DuplicateToolResult { original_call_id, repeated_call_id, bytes_wasted }`; `bytes_wasted` equals canonical-payload byte length of the duplicate
  [test](duplicate_result_event_payload_carries_bytes_wasted)
- Observer state resets on `CompactionEnd`
  [test](duplicate_result_resets_on_compaction_end)
- Enabled by default; configurable via `Conversation` builder or `[agent.duplicate_result]`
  [test](duplicate_result_config_disable_path)

**Shared infrastructure**

- Both observers consume the same result-canonicalization + BLAKE3-16 hashing pipeline (single `ResultHasher` utility in `llm`); per-result canonicalization happens once
  [check](cargo run -p loom-walk -- result_hasher_single_call_site)
- Both observers ship in `llm`'s `observer` module so consumers driving via `Conversation::run` get them by default; Loom's binary composes the same observers when driving Pi / Claude / Direct
  [check](cargo run -p loom-walk -- observers_in_loom_llm)

## Requirements

### Functional

1. **Typed multi-provider LLM access.** `LlmClient` trait exposes
   `complete(req)` and `complete_structured::<T>(req)`. Per-call
   model selection via required positional `ModelId` on the
   request. Provider routing inferred from `ModelId` variant or
   `Other(String)` prefix. No `embed` in v1.
2. **Typed `CacheControl`.** `Ephemeral(CacheTtl)` with
   `CacheTtl::{Minutes5, Hours1, Hours24}` matching Anthropic's
   prompt-cache breakpoint API. Per-content-block granularity.
   Other providers no-op the marker.
3. **Provider-mechanism-hidden structured output.**
   `complete_structured::<T: DeserializeOwned + JsonSchema>(req)`
   is one method; internally picks the right underlying mechanism
   per provider (synthetic forced-tool / `response_format` /
   `response_schema`) and deserializes into `T`.
4. **`TokenUsage` on every response.** `CompletionResponse.usage`
   carries `{ input, output, cache_read, cache_write, cost_cents }`.
   Same surface emits as `DriverKind::TokenUsage` `AgentEvent`
   into the active sink chain for SaaS billing pipelines.
5. **`Conversation` with built-in tool-use loop.** Consumers
   register tools via the `Tool` trait, configure
   `max_iterations` budget and `on_iteration_exhausted` behavior,
   then call `run(&client)`. Loop iterates
   `complete → tool_calls? → dispatch → tool_results → complete`
   until the agent stops calling tools or the budget is
   exhausted. Cancellation via standard tokio primitives. Live
   event observation during the loop is via the `EventSink` chain
   attached to the driving `LlmClient` (Requirement 4) — no
   separate streaming entry point.
6. **`Tool` trait designed for ecosystem convertibility.** Shape
   permits reasonable conversion to other Rust agent-loop crates'
   tool shapes (`agent-client-protocol`, rig, etc.) so re-hosting
   `Conversation` on a different crate later is feasible without
   breaking consumers.
7. **`DoomLoopObserver`** — per [Agent-Loop Observers](#agent-loop-observers).
   `(CallKey, ResultHash)` keying; 3-of-5 sliding-window
   detection; two-stage Steer → Abort response with configurable
   gap; resets on `CompactionEnd`. Emits
   `DriverKind::DoomLoopTripped` for observability. Stage 2's
   abort classifies as recovery cause `observer-abort` in the
   verdict gate.
8. **`DuplicateResultObserver`** — pure observability. BLAKE3-16
   keying; `min_bytes` threshold; emits
   `DriverKind::DuplicateToolResult` with `bytes_wasted` payload.
   `react()` always returns empty `Vec`; never sends commands.
9. **Observer composition.** Both observers ship by default in
   `Conversation`'s sink chain. Users opt out via Loom's CLI
   config (`[agent.doom_loop]` / `[agent.duplicate_result]`) or
   per-`Conversation` via the builder.
10. **Wrapper, not re-export.** Public surface
    (`LlmClient`, `CompletionRequest`, `Message`, `ModelId`,
    `CacheControl`, `Tool`, `Conversation`) is defined in
    `llm`. The underlying multi-provider crate is an
    internal-implementation dependency, swappable without
    consumer breaking changes.

### Non-Functional

1. **Public-contract crate.** `llm` is one of three
   public-contract crates in the loom workspace (alongside
   `loom-events` and `templates`). External Rust consumers
   depend on it directly. Stability rules: additive type / variant
   changes are minor bumps; removing or renaming public types,
   methods, or `ModelId` variants is a major bump.
2. **Dep-graph leaf.** `llm` depends on `loom-events` only
   among internal crates. No `loom-driver`, `agent`, or
   `loom-workflow` imports.
3. **Style.** Follows the team's
   [`docs/style-rules.md`](../docs/style-rules.md).

## Out of Scope

- **Embedding API.** No `LlmClient::embed` in v1. When it lands,
  the API will need explicit provider-per-call routing (different
  from completion's `ModelId`-inferred routing) because Anthropic
  doesn't expose a first-class embedding endpoint — design that
  shape when a concrete consumer-integration story requires it.
- **RAG / memory injection at the loom-llm layer.** RAG is the
  consumer's responsibility — consumers construct their own
  prompts (potentially with RAG chunks baked in) and call
  `LlmClient::complete*` / `Conversation::run`. `llm` exposes
  no retriever-hook surface.
- **Transcript-rewriting dedup.** `DuplicateResultObserver` is
  observability-only. Pi-mono and Claude Code own their own
  transcripts; rewriting them is architecturally closed.
  Rewriting inside the Direct backend's transcript is deferred
  follow-up work.
- **Provider-tuning escape hatches.** v1 hides the
  structured-output mechanism, cache-control mapping, and other
  per-provider knobs behind the typed surface. If a concrete
  consumer needs provider-specific tuning, add an escape hatch
  later — not by default.
- **Inheriting an ecosystem agent-loop crate's `Agent` /
  `Conversation` type wholesale.** `llm` carries its own
  `Conversation` to preserve observer composition, typed
  `CacheControl`, and per-call `ModelId` ergonomics. Re-hosting on
  a different agent-loop crate is a tracked option, not a default.
