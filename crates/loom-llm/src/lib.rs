//! `llm` — typed multi-provider LLM primitives, `Conversation`
//! with a built-in tool-use loop, and the two agent-loop observers
//! (`DoomLoopObserver`, `DuplicateResultObserver`) Loom's binary and
//! external Rust consumers share.
//!
//! # Public Contract
//!
//! `llm` is one of five public-contract crates in the loom
//! workspace (alongside `loom-events`, `loom-protocol`, `loom-templates`,
//! and `loom-skills`). External Rust consumers depend on this crate
//! directly for typed LLM access
//! without taking on Loom's CLI / workflow / beads surface. The
//! consumer-facing surface (re-exported below) is the only stable API:
//! additive type / variant changes are minor version bumps; removing
//! or renaming public types, methods, or `ModelId` variants is a major
//! bump.
//!
//! `llm` is a typed wrapper, not a thin re-export — every public
//! type is defined inside this crate so swapping the underlying
//! multi-provider implementation is an internal change rather than a
//! breaking change for every consumer.

pub mod api_key;
pub mod cache;
pub mod client;
pub mod conversation;
pub mod model_id;
pub mod observer;
pub mod request;
pub mod tool;
pub mod usage;

pub use api_key::{ApiKey, ApiKeyError};
pub use cache::{CacheControl, CacheTtl};
#[cfg(feature = "openai-compat")]
pub use client::OpenAiCompatClient;
pub use client::{
    AnthropicClient, BoxFuture, CompletionResponse, DEFAULT_RETRY_AFTER, GeminiClient,
    LlmCapability, LlmClient, LlmClientExt, LlmError, OpenAiClient, ParseToolCallIdError,
    RetryAdvice, ToolCallId, ToolUseRequest, parse_retry_after,
};
pub use conversation::{ContextBudget, Conversation, ConversationError, LoopOutcome};
pub use model_id::{AnthropicModel, GeminiModel, ModelId, OpenAiModel, SchemaKind};
pub use observer::{
    DoomLoopConfig, DoomLoopObserver, DuplicateDetection, DuplicateResultConfig,
    DuplicateResultObserver,
};
pub use request::{BinaryContent, CompletionRequest, Message, MessageContent, MimeType, Role};
pub use tool::{Tool, ToolDef, ToolOutput};
pub use usage::TokenUsage;
