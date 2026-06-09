# Loom-LLM

Typed multi-provider LLM primitives, multimodal request content,
Conversation with built-in tool-use loop, and agent-loop observers
for both Loom's binary and external Rust consumers.

## Problem Statement

Loom's Direct backend needs typed multi-provider LLM access with
per-call model selection within a schema, typed multimodal content
parts, typed prompt-cache markers, structured-output
deserialization, and typed transport-failure classification so
consumers can drive their own retry policies. The same primitives
are useful to external Rust crates (e.g. RAG pipelines,
domain-specific review tools, on-prem
deployments backed by customer-hosted models reached via an
OpenAI-compatible endpoint) that want typed LLM calls without
taking on Loom's CLI / workflow / beads surface.

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
  typed `LlmError` classification (see [LlmError](#llmerror))
- Carries bus-factor mitigation for the underlying crate — a
  minimal provider client (Anthropic Messages, at minimum) can
  be vendored as a contingency seed inside `llm` without
  changing the public surface

### `LlmClient` Trait

Object-safe trait; per-schema Client types implement it. The
trait carries the schema-kind discriminator so the
Client–`ModelId` compatibility check is structural rather than
stringly-typed:

```rust
pub trait LlmClient: Send + Sync {
    fn schema(&self) -> SchemaKind;
    fn supports(&self, model: &ModelId) -> bool {
        model.schema() == self.schema()
    }
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError>;
    async fn complete_structured<T>(&self, req: CompletionRequest) -> Result<T, LlmError>
        where T: DeserializeOwned + JsonSchema;
}
```

Model is required positional on the request (`CompletionRequest::new(ModelId)`);
the type system forbids constructing a request without naming the
model. The Client's `schema()` is fixed at construction (each
Client type maps 1:1 to a `SchemaKind`); per-call selection
varies the model *within* that schema. Calling `complete` with a
`ModelId` whose schema does not match `self.schema()` returns
`LlmError::IncompatibleModel { model, expected: SchemaKind }` —
no network call is made. Consumers can pre-validate
allowed-model sets via `supports(&ModelId)` without issuing a
request.

The trait is object-safe so `Arc<dyn LlmClient>` works for
runtime polymorphism — per-tenant Client caches, mock impls in
tests, and external-crate `LlmClient` impls compose through the
same dyn surface.

### `SchemaKind`

```rust
#[non_exhaustive]
pub enum SchemaKind {
    Anthropic,
    OpenAi,
    Gemini,
    OpenAiCompat,
}
```

`SchemaKind` is the wire-format discriminator: each variant
names one HTTP message-shape family. Variants map 1:1 to
`ModelId` outer variants and to per-schema Client types — see
[Client Types](#client-types). `#[non_exhaustive]` so future
adapters add variants additively (`Bedrock`, `AzureOpenAi`,
`Vertex` — see [Out of Scope](#out-of-scope)) without breaking
matchers.

### `ModelId`

Hybrid nested enum: outer variant discriminates by `SchemaKind`,
inner enum names known models within that schema with an
`Other(String)` fallback for forward-compat. `OpenAiCompat` is
flat-`String` because customer-hosted models have no
loom-knowable name set.

```rust
#[non_exhaustive]
pub enum ModelId {
    Anthropic(AnthropicModel),
    OpenAi(OpenAiModel),
    Gemini(GeminiModel),
    #[cfg(feature = "openai-compat")]
    OpenAiCompat(String),
}

pub enum AnthropicModel {
    ClaudeOpus48,
    ClaudeSonnet46,
    ClaudeHaiku45,
    // … other known Anthropic models
    Other(String),
}
// OpenAiModel, GeminiModel: same shape — known variants + Other(String).
// Inner enums are NOT #[non_exhaustive]: the `Other(String)` fallback
// already absorbs unknown names, and exhaustive matching from outside
// the crate (model-picker UIs, etc.) is a supported pattern.

impl ModelId {
    pub fn schema(&self) -> SchemaKind {
        match self {
            ModelId::Anthropic(_) => SchemaKind::Anthropic,
            ModelId::OpenAi(_)    => SchemaKind::OpenAi,
            ModelId::Gemini(_)    => SchemaKind::Gemini,
            #[cfg(feature = "openai-compat")]
            ModelId::OpenAiCompat(_) => SchemaKind::OpenAiCompat,
        }
    }
}
```

Adding a known model is a minor version bump (new inner-enum
variant). Adding a new schema is a minor bump (new `SchemaKind`
variant + new `ModelId` outer variant + new Client type — all
additive under `#[non_exhaustive]`).

### `CompletionRequest`

Builder shape; messages typed; cache control typed per content
part. Existing text-only builders stay source-compatible and
append text content parts:

```rust
let req = CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
    .system("Short instruction prefix")
    .user_cached("Long context document…", CacheControl::Ephemeral(CacheTtl::Hours1))
    .user("Question that varies per call")
    .max_tokens(2048);
```

Messages are ordered lists of typed content parts rather than raw
provider JSON:

```rust
pub enum MessageContent {
    Text(String),
    Binary(BinaryContent),
}

pub struct BinaryContent {
    pub mime_type: MimeType,
    pub bytes: bytes::Bytes,
    pub name: Option<String>,
}
```

Binary payloads are bytes in the public request model. Provider
clients base64-encode at the transport boundary when the native
wire format requires it; consumers do not pass provider-specific
inline-data, document-block, image-block, or file-part structs.
`name` is optional caller metadata used as a provider filename
when the native wire shape has one; if a provider requires a
filename and `name` is absent, the client synthesizes a safe generic
filename from the MIME type. `BinaryContent`'s `Debug` output
redacts bytes and prints only MIME type, optional name, and byte
length.

`MimeType` is a validated public newtype with associated constants
for the built-in set (`APPLICATION_PDF`, `IMAGE_PNG`, `IMAGE_JPEG`,
`IMAGE_WEBP`) plus a fallible parser for additional syntactically
valid MIME types. No public binary-content API accepts a bare
unvalidated string for MIME type.

Convenience builders preserve the simple text API while adding
multipart ergonomics. Role-specific binary builders append to the
most recent message for that role when one exists; otherwise they
start a new message for that role:

```rust
let req = CompletionRequest::new(ModelId::Gemini(GeminiModel::Gemini15Pro))
    .system("Answer from the uploaded document")
    .user("Summarize this PDF")
    .user_binary(MimeType::APPLICATION_PDF, pdf_bytes)
    .user_binary_named(MimeType::IMAGE_PNG, image_bytes, "diagram.png");
```

Each `Message::*` and `Message::*_cached` constructor produces a
typed content part; consumers compose parts via the builder rather
than handing in JSON objects.

### Client Types

One Client type per `SchemaKind`. Each implements `LlmClient`
and exposes a `pub const SCHEMA: SchemaKind`. The genai-backed
Clients share `genai::Client` internally; **`genai` does not
appear in any public signature** — see [Wrapper Thickness](#wrapper-thickness).

```rust
pub struct AnthropicClient    { /* genai::Client inside */ }
pub struct OpenAiClient       { /* genai::Client inside */ }
pub struct GeminiClient       { /* genai::Client inside */ }
pub struct OpenAiCompatClient { /* reqwest + Url + Option<ApiKey> */ }

impl AnthropicClient {
    pub const SCHEMA: SchemaKind = SchemaKind::Anthropic;
    pub fn new(api_key: ApiKey) -> Self;
}
impl OpenAiClient {
    pub const SCHEMA: SchemaKind = SchemaKind::OpenAi;
    pub fn new(api_key: ApiKey) -> Self;
}
impl GeminiClient {
    pub const SCHEMA: SchemaKind = SchemaKind::Gemini;
    pub fn new(api_key: ApiKey) -> Self;
}
impl OpenAiCompatClient {
    pub const SCHEMA: SchemaKind = SchemaKind::OpenAiCompat;
    pub fn new(base_url: Url, api_key: Option<ApiKey>) -> Self;
}
```

Per-tenant deployment context (credentials, endpoint URL) lives
on the Client; per-call selection lives on `ModelId`. A
`HashMap<OrgId, Arc<dyn LlmClient>>` cache is the canonical
pattern. `ApiKey` is a newtype that rejects empty strings at
construction; `base_url` is `url::Url`, requiring consumers to
parse strings into URLs at the boundary before constructing the
Client. Invalid configuration fails at the boundary rather than
at first request.

Each Client supports attaching an `EventSink` chain (`EventSink`
is defined in `loom-events`; see
[loom-harness — EventSink and SessionCommand](harness.md#eventsink-and-sessioncommand))
via a `.with_event_sink(impl EventSink)` builder method called
after `::new`. The chain receives `DriverKind::TokenUsage` events
and agent-loop observer commands during `complete*` calls — see
[Conversation and the Built-in Tool-Use Loop](#conversation-and-the-built-in-tool-use-loop).

`OpenAiCompatClient` is gated behind the `openai-compat` Cargo
feature (default-off — see [Feature Flags](#feature-flags));
the three genai-backed Clients are unconditional.

### `CacheControl`

```rust
pub enum CacheControl {
    None,
    Ephemeral(CacheTtl),
}
pub enum CacheTtl { Minutes5, Hours1, Hours24 }
```

Per-content-part granularity via `Message::*_cached(content,
CacheControl)`. The TTL set matches Anthropic's prompt-cache
breakpoint API. Providers that do not support typed per-part
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
providers swaps both the Client type and the `ModelId` variant,
with the call shape unchanged. Multimodal content parts are
preserved for structured-output requests; adding binary parts does
not force consumers onto a separate structured-output API. Failure
modes surface as typed `LlmError` variants — `MalformedJson` for
non-JSON or parse-failure responses, `SchemaViolation` for
parsed-but-invalid responses (see [LlmError](#llmerror)).

### Multimodal Provider Mapping

Provider clients translate `MessageContent::Binary` to native
provider wire shapes:

- **Gemini:** serializes each binary part as
  `{ inline_data: { mime_type, data: <base64> } }` in the same
  ordered message content as adjacent text parts.
- **Anthropic:** serializes supported document and image MIME types
  to native content blocks (`document` for `application/pdf`,
  `image` for supported image types) with
  `source: { type: "base64", media_type, data }`. Unsupported MIME
  types fail before network I/O with `LlmError::UnsupportedCapability`.
- **OpenAI:** official `OpenAiClient` serializes PDFs/files through
  OpenAI Responses `input_file` content with `filename` and
  `file_data: "data:<mime>;base64,<payload>"`; supported images use
  native image content (`input_image` or Chat-Completions
  `image_url`) with data URLs. The public API does not expose which
  OpenAI endpoint is used for a given request.
- **OpenAI-compatible:** no portable multimodal contract is promised.
  Binary parts fail before network I/O with
  `LlmError::UnsupportedCapability`; text-only Chat-Completions
  compatibility remains unchanged.

Invalid binary request shapes that are provider-independent (for
example, an empty binary payload) fail before network I/O with
`LlmError::IncompatibleRequest`.

### `TokenUsage`

Every `CompletionResponse` carries raw token counts; pricing is
the consumer's concern (per-tenant contracts, regional rates,
and custom-hosted models all make a loom-shipped pricing table
either incomplete or wrong):

```rust
pub struct TokenUsage {
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
    pub cache_write: u32,
}
```

The same surface drives SaaS billing pipelines via a
`DriverKind::TokenUsage` `AgentEvent` emitted on every `complete*`
call. Consumers maintain their own `ModelId → cost` mapping (see
[Out of Scope](#out-of-scope)) and compute cost from these
counts.

### `LlmError`

Typed transport-failure classification so consumers can drive
retry policy without parsing message strings:

```rust
#[non_exhaustive]
pub enum LlmError {
    // Transport / network
    Transport(String),                            // DNS, connect, TLS, mid-stream
    Timeout,                                      // deadline exceeded

    // HTTP-level (classified)
    RateLimited { retry_after: Duration },        // 429 + Retry-After
    AuthFailed { reason: String },                // 401 / 403
    ProviderHttp { status: u16, body: String },   // other non-success

    // Response-content
    MalformedJson(String),                        // expected JSON, got non-JSON / parse failure
    SchemaViolation(String),                      // parsed JSON failed schema validation

    // Client-side
    IncompatibleModel { model: ModelId, expected: SchemaKind },
    UnsupportedCapability { provider: SchemaKind, capability: LlmCapability },
    IncompatibleRequest { reason: String },

    // Fallback
    Provider { message: String },                 // genuinely unclassified
}

pub enum LlmCapability {
    MultimodalBinary { mime_type: MimeType },
}

pub enum RetryAdvice {
    Retryable,
    RetryAfter(Duration),
    NonRetryable,
}

impl LlmError {
    pub fn retry_advice(&self) -> RetryAdvice;
}
```

Classification is canonical and lives in `loom-llm`:

| Variant | `retry_advice` |
|---------|----------------|
| `Transport`, `Timeout` | `Retryable` |
| `RateLimited { retry_after }` | `RetryAfter(retry_after)` |
| `MalformedJson`, `SchemaViolation` | `Retryable` |
| `ProviderHttp { status, .. }` | `Retryable` iff `status >= 500`, else `NonRetryable` |
| `AuthFailed`, `IncompatibleModel`, `UnsupportedCapability`, `IncompatibleRequest`, `Provider` | `NonRetryable` |

`loom-llm` does not retry. The method returns *advice*; the
consumer composes its own backoff, jitter, and budget policy.
Upstream error → `LlmError` mapping is exhaustive for each
Client family: the three genai-backed Clients classify every
`genai::Error` variant; `OpenAiCompatClient` classifies every
`reqwest::Error` shape plus every parsed HTTP-response status.
`Provider { message }` is the documented fallback for cases
that do not map cleanly.

`#[non_exhaustive]` so future variants (new HTTP-status carve-outs,
provider-specific error families, new capability classes) land
additively without breaking consumer matchers.

### `Conversation` and the Built-in Tool-Use Loop

For multi-turn work with tool calls, consumers register tool
handlers via a `Tool` trait and call `run`:

```rust
let mut conv = Conversation::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
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
chain attached to the driving `LlmClient`. The chain is attached
per Client via `.with_event_sink(impl EventSink)` — see
[Client Types](#client-types). `Conversation` does not expose a
separate streaming entry point. Consumers that want a
`Stream<Item = AgentEvent>` wrap a short mpsc-backed `EventSink`
impl — paid by the one consumer that needs it, not by every
consumer up-front.

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

## Feature Flags

Optional adapters are in-crate Cargo features, default-off. The
three genai-backed Clients (`AnthropicClient`, `OpenAiClient`,
`GeminiClient`) are unconditional core.

| Feature | Adds | Default |
|---------|------|---------|
| `openai-compat` | `OpenAiCompatClient`, `ModelId::OpenAiCompat`, `SchemaKind::OpenAiCompat` | off |

Future regulated-provider adapters (`bedrock`, `azure`, `vertex`)
slot into the same pattern: one Cargo feature gates one Client
type plus its `ModelId` / `SchemaKind` additions. Heavy SDK
dependencies (AWS, Azure, GCP) stay out of the default build;
consumers opt in explicitly. CI exercises both `--all-features`
and `--no-default-features` to catch feature-gating regressions.

## Success Criteria

### Public surface

- `llm` exposes object-safe `LlmClient` trait with `schema(&self) -> SchemaKind`, `supports(&self, &ModelId) -> bool` (default impl), `complete(req)`, and `complete_structured::<T>(req)`; no `embed` in v1
  [check](cargo run -p loom-walk -- loom_llm_public_surface)
- `LlmClient` is object-safe — `Arc<dyn LlmClient>` compiles and dispatches `complete` / `complete_structured` correctly
  [test](llm_client_trait_is_object_safe)
- `CompletionRequest::new(ModelId)` requires model as positional argument; constructing a request without a model is a compile error
  [test](completion_request_requires_model_at_construction)
- `ModelId` is a hybrid nested enum: outer `Anthropic | OpenAi | Gemini | OpenAiCompat` carries inner per-family enum (with `Other(String)` fallback) for the three genai-backed schemas, and `String` for `OpenAiCompat`
  [test](modelid_outer_variants_match_schema_kind_one_to_one)
- `SchemaKind` is `#[non_exhaustive]` with one variant per `ModelId` outer variant; `ModelId::schema(&self) -> SchemaKind` returns the matching tag for every variant
  [test](modelid_schema_method_returns_matching_schema_kind)
- `complete_structured::<T>` hides provider mechanism: same call shape works for Anthropic (synthetic forced-tool), OpenAI (`response_format`), Gemini (`response_schema`); returned `T: DeserializeOwned + JsonSchema` is deserialized regardless of provider
  [test](complete_structured_returns_typed_t_across_providers)
- `CompletionResponse` carries `usage: TokenUsage { input, output, cache_read, cache_write }` (raw token counts only — no `cost_cents`) on every successful call
  [test](completion_response_carries_token_usage_without_cost)
- `complete*` calls emit `DriverKind::TokenUsage` event into the active `EventSink` chain (so SaaS billing pipelines tail the same AgentEvent stream and compute cost from token counts using their own rate tables)
  [test](complete_emits_token_usage_driver_event)

### Multimodal content

- Existing text-only builders (`system`, `user`, cached variants) remain source-compatible and continue to create text-only requests without forcing callers to construct `MessageContent` manually
  [test?](completion_request_text_only_api_remains_compatible)
- `CompletionRequest` can carry ordered multipart user content containing both text and a PDF binary part with MIME type `application/pdf`
  [test?](completion_request_accepts_text_and_pdf_binary_parts)
- Role-specific binary builders append to the most recent message for that role when present, preserving text-plus-binary ordering inside one multipart message
  [test?](binary_builders_append_to_existing_role_message)
- Binary content APIs require validated `MimeType` values; no public binary-content constructor or builder accepts a bare unvalidated MIME-type string
  [check?](cargo run -p loom-walk -- loom_llm_mime_type_no_raw_strings)
- `MimeType` exposes built-in constants for supported common types and its parser accepts only syntactically valid MIME strings
  [test?](mime_type_parser_accepts_valid_and_rejects_invalid)
- Public multimodal types (`MessageContent`, `BinaryContent`, `MimeType`) are defined in `llm`; no public multimodal signature references Gemini, Anthropic, OpenAI, or genai wire structs
  [check?](cargo run -p loom-walk -- loom_llm_multimodal_no_provider_wire_types)
- `BinaryContent` debug formatting redacts payload bytes/base64 while still exposing MIME type, optional name, and byte length
  [test?](binary_content_debug_redacts_payload)
- Default logging does not emit binary bytes, base64 payloads, prompt bodies, or response bodies; MIME type and byte length are safe to log
  [judge](../tests/judges/loom.sh#judge_llm_multimodal_logging_redaction)
- `GeminiClient` serializes PDF binary parts to Gemini `inline_data` with `mime_type: "application/pdf"` and base64 `data`
  [test?](gemini_multimodal_serializes_pdf_inline_data)
- `AnthropicClient` serializes PDF binary parts to native Anthropic `document` content blocks with `source: { type: "base64", media_type: "application/pdf", data }`, not prompt text
  [test?](anthropic_multimodal_serializes_pdf_document_block)
- `AnthropicClient` serializes supported image binary parts to native Anthropic `image` content blocks with `source: { type: "base64", media_type, data }`, not prompt text
  [test?](anthropic_multimodal_serializes_image_block)
- `OpenAiClient` serializes PDF binary parts to OpenAI Responses `input_file` content with `filename` and `file_data` data URI, not prompt text
  [test?](openai_multimodal_serializes_pdf_input_file)
- `OpenAiClient` serializes supported image binary parts to OpenAI-native image content with a data URL, not prompt text
  [test?](openai_multimodal_serializes_image_data_url)
- Providers that require a filename synthesize a safe generic filename from `MimeType` when `BinaryContent::name` is absent
  [test?](provider_filename_synthesized_when_binary_name_absent)
- `OpenAiCompatClient` rejects binary parts with `LlmError::UnsupportedCapability` before network I/O while preserving text-only Chat-Completions compatibility
  [test?](openai_compat_multimodal_returns_unsupported_without_network)
- Unsupported multimodal MIME/provider combinations return typed `LlmError::UnsupportedCapability` and never panic
  [test?](unsupported_multimodal_request_returns_typed_error_not_panic)
- Empty binary payloads return `LlmError::IncompatibleRequest` before network I/O
  [test?](empty_binary_payload_returns_incompatible_request)
- `complete_structured::<T>` accepts requests containing multimodal content parts using the same call shape as text-only structured output
  [test?](complete_structured_accepts_multimodal_messages)

### Client types

- Four Client types ship in `loom-llm` mapping 1:1 to `SchemaKind`: `AnthropicClient`, `OpenAiClient`, `GeminiClient` (unconditional), `OpenAiCompatClient` (under `openai-compat` feature)
  [check](cargo run -p loom-walk -- loom_llm_client_types_per_schema_kind)
- Each Client exposes `pub const SCHEMA: SchemaKind` matching `LlmClient::schema(&self)` at runtime
  [test](client_const_schema_matches_runtime_schema)
- `AnthropicClient::new`, `OpenAiClient::new`, `GeminiClient::new` take `ApiKey` (newtype rejecting empty strings); `OpenAiCompatClient::new` takes `url::Url` + `Option<ApiKey>`; no constructor accepts `String` for credentials or base URL
  [check](cargo run -p loom-walk -- loom_llm_client_constructors_use_newtypes)
- `ApiKey::new` returns `Err` on empty input; construction-time validation prevents downstream call-time failures
  [test](api_key_newtype_rejects_empty)
- Each Client exposes a `.with_event_sink(impl EventSink)` builder method that returns `Self`; the attached sink chain receives `DriverKind::TokenUsage` events during `complete*` calls
  [test](client_with_event_sink_attaches_chain_and_receives_usage_events)
- Calling `LlmClient::complete` with a `ModelId` whose `schema()` does not match `self.schema()` returns `LlmError::IncompatibleModel { model, expected }` synchronously without issuing a network call
  [test](incompatible_modelid_returns_typed_error_without_network)
- `LlmClient::supports(&model)` returns `model.schema() == self.schema()` for every variant pair
  [test](supports_matches_schema_equality)

### OpenAI-compatible adapter

- `OpenAiCompatClient::new(base_url, api_key)` builds a Client routed at `base_url`; calls send OpenAI Chat-Completions-shaped JSON over HTTP
  [test](openai_compat_client_sends_chat_completions_shape_to_configured_url)
- `OpenAiCompatClient` accepts `ModelId::OpenAiCompat(_)` only; rejects every other `ModelId` variant with `IncompatibleModel`
  [test](openai_compat_client_rejects_non_compat_modelids)
- Adapter compiles and passes its tests under `--features openai-compat`
  [system](cargo test -p loom-llm --features openai-compat)
- Default build (`--no-default-features`) compiles cleanly; the openai-compat adapter, `ModelId::OpenAiCompat`, and `SchemaKind::OpenAiCompat` are all gated behind `#[cfg(feature = "openai-compat")]`
  [system](cargo check -p loom-llm --no-default-features)
- Wiremock contract test exercises a 200 happy path, 401, 429 + Retry-After, 500, and a malformed-JSON response against `OpenAiCompatClient`; each maps to the expected `LlmError` variant and `retry_advice`
  [test](openai_compat_wiremock_contract_covers_status_classes)

### `LlmError`

- `LlmError` is `#[non_exhaustive]` and carries the documented variants: `Transport`, `Timeout`, `RateLimited`, `AuthFailed`, `ProviderHttp`, `MalformedJson`, `SchemaViolation`, `IncompatibleModel`, `UnsupportedCapability`, `IncompatibleRequest`, `Provider`
  [check?](cargo run -p loom-walk -- loom_llm_error_variant_set_multimodal)
- `LlmError::retry_advice(&self)` returns the classification documented in [LlmError](#llmerror) for every variant, including `UnsupportedCapability` and `IncompatibleRequest` as `NonRetryable`
  [test?](llm_error_retry_advice_includes_multimodal_client_errors)
- `RateLimited { retry_after }` is populated from the `Retry-After` HTTP header (seconds or HTTP-date) when the provider returns 429; missing header falls back to a documented default
  [test](rate_limited_parses_retry_after_header)
- `ProviderHttp { status, body }` carries the raw status and response body for unclassified non-success responses; `retry_advice` returns `Retryable` for `status >= 500`, `NonRetryable` otherwise
  [test](provider_http_retry_advice_threshold_at_500)
- Upstream error → `LlmError` mapping is exhaustive for each Client family. For the genai-backed Clients (`AnthropicClient`, `OpenAiClient`, `GeminiClient`), every variant of `genai::Error` maps to a non-`Provider` `LlmError` variant where the upstream carries enough information to classify. For `OpenAiCompatClient`, every `reqwest::Error` shape (DNS, connect, TLS, timeout, body) and every parsed HTTP-response status maps to the documented `LlmError` variant per the classification table above. `Provider { message }` is the fallback only for explicitly-unclassifiable cases.
  [judge](../tests/judges/loom.sh#judge_llm_error_mapping_honesty)

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

- `llm` is a typed wrapper, not a thin re-export: the public surface (`LlmClient`, `CompletionRequest`, `Message`, `ModelId`, `SchemaKind`, `CacheControl`, `Tool`, `Conversation`, `LlmError`, `RetryAdvice`, and the per-schema Client types) is defined in `llm`, not re-exported from the underlying multi-provider crate
  [check](cargo run -p loom-walk -- loom_llm_no_underlying_crate_reexports)
- No Client constructor or public method signature references `genai::Client`, `genai::Error`, or any other `genai` type — `genai` remains an internal implementation dependency
  [check](cargo run -p loom-walk -- loom_llm_no_public_genai_types)

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

1. **Typed multi-provider LLM access.** Object-safe `LlmClient`
   trait exposes `schema(&self) -> SchemaKind`, `supports(&self,
   &ModelId) -> bool` (default impl checks
   `model.schema() == self.schema()`), `complete(req)`, and
   `complete_structured::<T>(req)`. Per-call model selection via
   required positional `ModelId` on the request. Schema is fixed
   at Client construction; per-call selection varies the model
   within that schema. Requests carry typed text and binary
   content parts without exposing provider-specific structs. No
   `embed` in v1.
2. **Typed `CacheControl`.** `Ephemeral(CacheTtl)` with
   `CacheTtl::{Minutes5, Hours1, Hours24}` matching Anthropic's
   prompt-cache breakpoint API. Per-content-part granularity.
   Other providers no-op the marker.
3. **Provider-mechanism-hidden structured output.**
   `complete_structured::<T: DeserializeOwned + JsonSchema>(req)`
   is one method; internally picks the right underlying mechanism
   per provider (synthetic forced-tool / `response_format` /
   `response_schema`) and deserializes into `T`. Multimodal
   content parts remain compatible with this call shape.
   Schema-violation failures surface as
   `LlmError::SchemaViolation`; malformed JSON as
   `LlmError::MalformedJson`.
4. **`TokenUsage` on every response — raw counts only.**
   `CompletionResponse.usage` carries
   `{ input, output, cache_read, cache_write }`. No `cost_cents`;
   pricing lives in consumer-owned `ModelId → cost` mappings (see
   [Out of Scope](#out-of-scope)). Same surface emits as
   `DriverKind::TokenUsage` `AgentEvent` into the active sink
   chain for SaaS billing pipelines.
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
    (`LlmClient`, `CompletionRequest`, `Message`,
    `MessageContent`, `BinaryContent`, `MimeType`, `ModelId`,
    `SchemaKind`, `CacheControl`, `Tool`, `Conversation`,
    `LlmError`, `RetryAdvice`, per-schema Client types) is
    defined in `llm`. The underlying multi-provider crate is an
    internal-implementation dependency, swappable without
    consumer breaking changes. No public Client constructor or
    method signature mentions `genai::Client` or other `genai`
    types.
11. **`SchemaKind` discrimination.** `#[non_exhaustive]` enum
    with one variant per supported wire-format family
    (`Anthropic`, `OpenAi`, `Gemini`, `OpenAiCompat`). Maps 1:1
    to `ModelId` outer variants and to per-schema Client types.
    `ModelId::schema(&self) -> SchemaKind` returns the matching
    tag.
12. **Per-schema Client types.** One Client type per
    `SchemaKind`: `AnthropicClient`, `OpenAiClient`,
    `GeminiClient` (unconditional core);
    `OpenAiCompatClient` (under `openai-compat` Cargo feature).
    Each implements `LlmClient` and exposes `pub const SCHEMA:
    SchemaKind`. Construction takes provider-native credentials
    parsed into typed forms at the boundary (`ApiKey` newtype,
    `url::Url`); no construction accepts `String` for a
    credential or base URL. Each Client supports
    `.with_event_sink(impl EventSink)` (from `loom-events`) as a
    builder method called after `::new`; the attached chain
    receives `DriverKind::TokenUsage` events and observer
    `SessionCommand`s during `complete*` calls.
13. **OpenAI-compatible adapter.** `OpenAiCompatClient` routes
    OpenAI Chat-Completions-shaped JSON to a configured
    `base_url`, with an optional `ApiKey`. Targets local
    runners (vLLM, llama.cpp, LM Studio, Ollama via its `/v1`
    endpoint), API-shaped proxies (LiteLLM), and
    commercial OpenAI-compatible providers. No portable
    multimodal contract is promised for this adapter: binary
    parts return `LlmError::UnsupportedCapability` before
    network I/O. No retries, rate limiting, or fallback — that
    is the consumer's job.
14. **Typed multimodal request content.** `CompletionRequest`
    messages are ordered lists of `MessageContent::Text` and
    `MessageContent::Binary` parts. Binary parts carry validated
    `MimeType`, bytes, and optional name/filename metadata.
    Existing `.system("...")`, `.user("...")`, and cached text
    builders remain source-compatible. New binary builders append
    binary parts without requiring consumers to construct provider
    JSON.
15. **Native multimodal provider support.** `GeminiClient`
    serializes binary parts as `inline_data`; `AnthropicClient`
    serializes supported PDFs/images as native document/image
    blocks with base64 source objects; official `OpenAiClient`
    serializes PDFs/files as Responses `input_file` parts and
    images as native image content with data URLs. Unsupported
    MIME/provider combinations fail before network I/O with typed
    errors and never degrade into base64 prompt text.
16. **Typed `LlmError` variants.** `#[non_exhaustive]` enum
    distinguishing transport, timeout, rate-limit (with
    `Retry-After`), auth, classified-HTTP, malformed-JSON,
    schema-violation, incompatible-model, unsupported-capability,
    incompatible-request, and a `Provider` fallback.
    `LlmError::retry_advice(&self) -> RetryAdvice` encodes the
    canonical retry-class table (5xx retryable, 4xx
    non-retryable except 429-with-delay, transport/timeout
    retryable, auth/incompatible-model/unsupported-capability/
    incompatible-request non-retryable). `loom-llm` classifies;
    consumers compose their own backoff/budget on top. Upstream
    error → `LlmError` mapping is exhaustive for each Client
    family (`genai::Error` for the genai-backed Clients,
    `reqwest::Error` plus parsed HTTP status for
    `OpenAiCompatClient`).
17. **`IncompatibleModel` check is synchronous and
    network-free.** `LlmClient::complete` returns
    `LlmError::IncompatibleModel { model, expected }` without
    issuing a network call when `model.schema() != self.schema()`.
    Consumers can pre-validate allowed-model sets via
    `LlmClient::supports(&ModelId)`.
18. **Feature-gated adapters.** Optional adapters are in-crate
    Cargo features, default-off. Today: `openai-compat`. Future
    regulated providers (`bedrock`, `azure`, `vertex`) slot into
    the same pattern without further architectural changes. CI
    exercises `--all-features` and `--no-default-features` at
    minimum.

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
3. **Sensitive-data logging.** Default logs and debug output do
   not include prompt bodies, response bodies, binary bytes, or
   base64-encoded payloads. MIME type, optional name, and byte
   length are safe diagnostics.
4. **Style.** Follows the team's
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
- **OpenAI-compatible multimodal portability.** The
  `OpenAiCompatClient` contract remains text-only
  Chat-Completions-shaped JSON. Local runners and proxy servers
  diverge on binary/file support, so multimodal compatibility for
  that adapter is out of scope until a concrete server contract is
  specified.
- **Regulated-provider adapters (Bedrock, Azure OpenAI, Vertex
  AI).** Deferred follow-up. Each slots into the in-crate
  feature-flag pattern established by `openai-compat`: one Cargo
  feature gates one Client type plus its `SchemaKind` and
  `ModelId` variants. Heavy SDK dependencies stay opt-in. The
  pattern is established by this spec; the adapters themselves
  land in future spec updates when a concrete integration
  timeline appears. Until then, the GovCloud / AAD / GCP
  service-account credential models are not designed.
- **Per-model pricing / context-window / capability metadata
  in `loom-llm`.** Pricing is contract-variable (regional rates,
  enterprise contracts, customer-hosted models with private
  rates) and turns `loom-llm` into a billing-table maintenance
  dependency. Consumers maintain their own `ModelId → cost`
  mappings, looking up whatever metadata (context window,
  capability flags) they need from their own catalogue. A
  shared catalogue may emerge later as a separate crate
  (`loom-llm-models` or similar); it does not live in core.
- **Built-in retry / backoff / rate-limit budget.** `loom-llm`
  classifies failures via `LlmError::retry_advice` and stops
  there. The consumer composes its own retry policy (backoff,
  jitter, attempt budget, circuit-breaking) on top. Building
  retry into `loom-llm` would force one policy on every
  consumer.
- **`genai::Client` as public construction input.** No
  `from_genai`-style constructor on any Client. The
  Wrapper Thickness invariant requires `genai` to remain an
  internal-implementation dependency so future swaps or
  vendoring do not break consumers. Per-tenant credential
  injection happens through native-credential constructors
  (`AnthropicClient::new(ApiKey)`, etc.); custom HTTP knobs
  (proxies, middleware, transport config) are added through
  explicit per-Client builder methods when a real consumer
  needs them, not by leaking the underlying client.
