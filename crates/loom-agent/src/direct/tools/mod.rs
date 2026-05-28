//! Sandbox-aware tool implementations registered with the in-process
//! `Conversation` by `loom-direct-runner`.
//!
//! Six net-new tools — [`Read`], [`Write`], [`Edit`], [`Bash`], [`Grep`],
//! [`Glob`] — each implementing the
//! [`Tool`](loom_llm::Tool) trait and executing against the workspace
//! bind-mount inside the container. See `specs/agent.md`
//! § Direct Backend — *The six tools*.

pub mod bash;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod read;
pub mod write;

pub use bash::Bash;
pub use edit::Edit;
pub use glob::Glob;
pub use grep::Grep;
pub use read::Read;
pub use write::Write;

use loom_llm::LlmError;
use schemars::{JsonSchema, SchemaGenerator};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Generate a JSON-Schema value for the tool's argument struct. Each
/// tool's [`Tool::input_schema`](loom_llm::Tool::input_schema) calls
/// this with its own `Args` type so the model sees a typed surface.
fn schema_for<T: JsonSchema>() -> Value {
    SchemaGenerator::default()
        .into_root_schema_for::<T>()
        .to_value()
}

/// Decode the model-supplied `args` payload into the tool's typed
/// argument struct. Returns [`LlmError::MalformedJson`] on a shape
/// mismatch so the caller surfaces a typed protocol error rather than
/// a tool-result.
fn parse_args<T: DeserializeOwned>(args: Value) -> Result<T, LlmError> {
    serde_json::from_value(args).map_err(|err| LlmError::MalformedJson(err.to_string()))
}
