//! `Tool` — the handler abstraction for `Conversation`'s built-in
//! tool-use loop. The trait shape is designed for reasonable conversion
//! to other ecosystem agent-loop crates' tool surfaces.

use std::pin::Pin;

use serde_json::Value;

use crate::client::LlmError;

/// Boxed future returned from [`Tool::invoke`]. Boxed so `dyn Tool` is
/// dyn-compatible — concrete handlers compose into the loop's
/// `Vec<Box<dyn Tool>>` registry without per-type monomorphisation.
pub type InvokeFuture<'a> = Pin<Box<dyn Future<Output = Result<ToolOutput, LlmError>> + Send + 'a>>;

/// The result a `Tool` returns from `invoke`. Carries a canonical JSON
/// payload the loop forwards to the agent as a `tool_result` block; the
/// payload is also the input to result-hashing observers
/// (`DoomLoopObserver`, `DuplicateResultObserver`).
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// Canonical result payload. Observers hash the canonical-JSON form;
    /// the loop forwards it to the agent unmodified.
    pub content: Value,
    /// When true, the loop reports this result to the agent as an
    /// error tool-result; the agent typically retries or steers away.
    pub is_error: bool,
}

/// Static definition of a tool attached to a [`crate::request::CompletionRequest`].
/// Carries the same `(name, description, input_schema)` triple a [`Tool`]
/// exposes, decoupled from the handler so the surface that flows to the
/// provider is plain data.
#[derive(Debug, Clone)]
pub struct ToolDef {
    /// Stable tool name advertised to the model.
    pub name: String,
    /// Human-readable description the model consults to decide whether
    /// to call this tool.
    pub description: String,
    /// JSON-Schema describing the tool's accepted arguments.
    pub input_schema: Value,
}

impl ToolDef {
    /// Lift a [`Tool`] handler to its plain-data definition.
    pub fn from_tool(tool: &dyn Tool) -> Self {
        Self {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            input_schema: tool.input_schema(),
        }
    }
}

/// Handler trait every consumer-registered tool implements. The shape
/// (name + description + JSON-Schema input + async invoke) is
/// reasonably convertible to other Rust agent-loop crates' tool shapes
/// — keeps the option of re-hosting `Conversation` on a different
/// agent-loop crate later without breaking consumers.
pub trait Tool: Send + Sync {
    /// Stable tool name advertised to the model. Matches the JSON-Schema
    /// tool identifier the model echoes in `tool_use` blocks.
    fn name(&self) -> &str;

    /// Human-readable description the model consults to decide whether
    /// to call this tool.
    fn description(&self) -> &str;

    /// JSON-Schema describing the tool's accepted arguments.
    fn input_schema(&self) -> Value;

    /// Run the tool against the model-supplied arguments and return a
    /// canonical result payload (or an error). Returns a boxed future
    /// so the trait is dyn-compatible — the loop holds handlers as
    /// `Vec<Box<dyn Tool>>`.
    fn invoke<'a>(&'a self, args: Value) -> InvokeFuture<'a>;
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    /// Sample echo tool that doubles as a forward-compat surrogate for
    /// the documented ecosystem trait shapes (`agent-client-protocol`,
    /// rig). It implements the full `Tool` surface — name, description,
    /// JSON-Schema input, async invoke returning a canonical
    /// `ToolOutput` — so the round-trip and dyn-compat tests below
    /// exercise the same shape an ecosystem-bridge adapter would walk.
    struct SampleEchoTool;

    impl Tool for SampleEchoTool {
        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "Echo the given text payload back to the caller."
        }

        fn input_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"],
                "additionalProperties": false,
            })
        }

        fn invoke<'a>(&'a self, args: Value) -> InvokeFuture<'a> {
            Box::pin(async move {
                let text = args
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or(LlmError::Canonicalize)?
                    .to_string();
                Ok(ToolOutput {
                    content: json!({ "echo": text }),
                    is_error: false,
                })
            })
        }
    }

    /// Build the Anthropic Messages-API tool-definition JSON from the
    /// trait surface alone. Anthropic's documented tool shape is
    /// `{ name, description, input_schema }` — exactly the three
    /// non-invoke methods on [`Tool`]. The helper takes `&dyn Tool` so
    /// the trait's dyn-compatibility is part of the contract under
    /// test.
    fn anthropic_tool_definition(tool: &dyn Tool) -> Value {
        json!({
            "name": tool.name(),
            "description": tool.description(),
            "input_schema": tool.input_schema(),
        })
    }

    /// Forward-compat smoke test: a sample `Tool` impl generates the
    /// Anthropic tool-schema JSON, the JSON round-trips through
    /// serde_json without loss, and the recovered fields match the
    /// trait surface. Pins three things at once:
    ///
    /// 1. The trait's read-side surface (`name`, `description`,
    ///    `input_schema`) is sufficient to satisfy the Anthropic
    ///    Messages-API tool definition shape — keeps the option open
    ///    to re-host `Conversation` on a different agent-loop crate
    ///    later without breaking consumers.
    /// 2. The schema payload is canonical JSON: serialising and
    ///    re-parsing produces a structurally identical value (no lossy
    ///    coercions snuck in via the trait's `Value` return type).
    /// 3. `&dyn Tool` is the right boundary — the helper takes a trait
    ///    object, mirroring how the conversation loop stores handlers
    ///    as `Vec<Box<dyn Tool>>`.
    #[test]
    fn tool_trait_generates_anthropic_schema_that_round_trips() {
        let tool = SampleEchoTool;
        let schema = anthropic_tool_definition(&tool);

        let wire = serde_json::to_string(&schema).expect("schema serialises");
        let parsed: Value = serde_json::from_str(&wire).expect("schema round-trips");
        assert_eq!(parsed, schema);

        assert_eq!(parsed["name"], json!("echo"));
        assert_eq!(
            parsed["description"],
            json!("Echo the given text payload back to the caller.")
        );
        assert_eq!(parsed["input_schema"]["type"], json!("object"));
        assert_eq!(parsed["input_schema"]["required"], json!(["text"]));
        assert_eq!(
            parsed["input_schema"]["properties"]["text"]["type"],
            json!("string")
        );
    }

    /// `Tool` is dyn-compatible — concrete handlers compose into the
    /// conversation loop's `Vec<Box<dyn Tool>>` registry without
    /// per-type monomorphisation. The async `invoke` returns the
    /// [`InvokeFuture`] alias (boxed, `Send`, lifetime-tied to `&self`)
    /// so the trait object stays object-safe; this test pins both the
    /// boxing path and that an awaited invocation produces the
    /// canonical `ToolOutput` shape the loop expects.
    #[test]
    fn tool_trait_is_dyn_compatible_and_invoke_resolves() {
        let handlers: Vec<Box<dyn Tool>> = vec![Box::new(SampleEchoTool)];
        let handler = handlers.first().expect("one handler registered");

        let output =
            tokio_test::block_on(handler.invoke(json!({ "text": "hi" }))).expect("invoke succeeds");
        assert!(!output.is_error);
        assert_eq!(output.content, json!({ "echo": "hi" }));
    }
}
