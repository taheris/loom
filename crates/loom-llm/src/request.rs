//! `CompletionRequest` — the typed builder consumers compose to call
//! `LlmClient::complete*`. Messages are typed; cache control attaches
//! per-content-block.

use crate::cache::CacheControl;
use crate::client::ToolUseRequest;
use crate::model_id::ModelId;
use crate::tool::ToolDef;

/// One message in a completion request. Constructed via the builder
/// helpers on [`CompletionRequest`]; consumers compose blocks rather
/// than handing in raw JSON.
#[derive(Debug, Clone)]
pub struct Message {
    /// Speaker role.
    pub role: Role,
    /// Free-form text content. Empty when the message carries only
    /// `tool_calls` / `tool_result` payload.
    pub content: String,
    /// Cache-control marker for this content block. Providers that do
    /// not support typed per-block cache markers no-op the marker
    /// without error.
    pub cache: CacheControl,
    /// Tool calls the assistant emitted on this turn (only populated on
    /// `Role::Assistant` messages produced by the loop after a
    /// tool-calling completion).
    pub tool_calls: Vec<ToolUseRequest>,
    /// Provider-stable identifier of the originating tool call (only
    /// populated on `Role::Tool` messages — the result the loop carries
    /// back to the model).
    pub tool_call_id: Option<String>,
    /// True when this tool-result message reports an error from the
    /// tool handler. Providers that distinguish error tool-results
    /// surface this; others ignore it.
    pub tool_is_error: bool,
}

/// Role on a [`Message`]. The system role is carried via
/// [`CompletionRequest::system`] rather than as a `Role` variant so that
/// the system prefix is structurally distinct from the user/assistant
/// turn sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Tool,
}

/// Typed builder for a single completion. Model is required at
/// construction — `CompletionRequest::new(model)` is the only entry
/// point, so the type system forbids constructing a request without
/// naming the model.
///
/// Omitting the `ModelId` is a compile error:
///
/// ```compile_fail
/// use loom_llm::CompletionRequest;
/// // No `ModelId` argument -> does not compile.
/// let _req = CompletionRequest::new();
/// ```
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    /// Model the underlying provider should route to.
    pub model: ModelId,
    /// Optional system instruction prefix.
    pub system: Option<String>,
    /// Ordered user/assistant/tool turns.
    pub messages: Vec<Message>,
    /// Optional `max_tokens` cap surfaced through the provider.
    pub max_tokens: Option<u32>,
    /// Tool definitions the model may invoke. Empty when no tools are
    /// attached.
    pub tools: Vec<ToolDef>,
}

impl CompletionRequest {
    /// Construct a new request. `ModelId` is positional so the type
    /// system requires a model on every call site.
    pub fn new(model: ModelId) -> Self {
        Self {
            model,
            system: None,
            messages: Vec::new(),
            max_tokens: None,
            tools: Vec::new(),
        }
    }

    /// Set the system instruction prefix. Overwrites any prior value.
    pub fn system(mut self, prefix: impl Into<String>) -> Self {
        self.system = Some(prefix.into());
        self
    }

    /// Append a user turn with no cache marker.
    pub fn user(mut self, content: impl Into<String>) -> Self {
        self.messages.push(Message::user(content));
        self
    }

    /// Append a user turn with a per-block cache marker.
    pub fn user_cached(mut self, content: impl Into<String>, cache: CacheControl) -> Self {
        self.messages.push(Message::user_cached(content, cache));
        self
    }

    /// Append an assistant turn with no cache marker.
    pub fn assistant(mut self, content: impl Into<String>) -> Self {
        self.messages.push(Message::assistant(content));
        self
    }

    /// Append an assistant turn with a per-block cache marker.
    pub fn assistant_cached(mut self, content: impl Into<String>, cache: CacheControl) -> Self {
        self.messages
            .push(Message::assistant_cached(content, cache));
        self
    }

    /// Append a pre-built message. Used by the conversation loop to
    /// reflect assistant tool-use turns and tool-result turns back into
    /// the next request.
    pub fn message(mut self, message: Message) -> Self {
        self.messages.push(message);
        self
    }

    /// Replace the tool set the model can invoke on this call.
    pub fn tools(mut self, tools: Vec<ToolDef>) -> Self {
        self.tools = tools;
        self
    }

    /// Cap the provider's response length.
    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }
}

impl Message {
    /// Construct a plain user turn.
    pub fn user(content: impl Into<String>) -> Self {
        Self::text(Role::User, content, CacheControl::None)
    }

    /// Construct a user turn with a per-block cache marker.
    pub fn user_cached(content: impl Into<String>, cache: CacheControl) -> Self {
        Self::text(Role::User, content, cache)
    }

    /// Construct a plain assistant turn.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::text(Role::Assistant, content, CacheControl::None)
    }

    /// Construct an assistant turn with a per-block cache marker.
    pub fn assistant_cached(content: impl Into<String>, cache: CacheControl) -> Self {
        Self::text(Role::Assistant, content, cache)
    }

    /// Construct an assistant turn that carries tool calls. `content`
    /// may be empty when the model emitted only tool-use blocks.
    pub fn assistant_tool_use(content: impl Into<String>, tool_calls: Vec<ToolUseRequest>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            cache: CacheControl::None,
            tool_calls,
            tool_call_id: None,
            tool_is_error: false,
        }
    }

    /// Construct a tool-result turn the loop forwards back to the model
    /// after dispatching an assistant tool call.
    pub fn tool_result(
        call_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            cache: CacheControl::None,
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            tool_is_error: is_error,
        }
    }

    fn text(role: Role, content: impl Into<String>, cache: CacheControl) -> Self {
        Self {
            role,
            content: content.into(),
            cache,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_is_error: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheTtl;

    /// `CompletionRequest::new` requires a `ModelId` positionally — the
    /// only public constructor takes one, the `model` field is
    /// non-optional, and there is no `Default` impl. The compile-fail
    /// doctest on the type proves the negative path; this runtime test
    /// pins the positive path (constructed value carries the chosen
    /// model and is reachable from the builder chain).
    #[test]
    fn completion_request_requires_model_at_construction() {
        let req = CompletionRequest::new(ModelId::ClaudeSonnet46)
            .system("prefix")
            .user("question")
            .user_cached("doc", CacheControl::Ephemeral(CacheTtl::Hours1))
            .max_tokens(2048);
        assert_eq!(req.model, ModelId::ClaudeSonnet46);
        assert_eq!(req.system.as_deref(), Some("prefix"));
        assert_eq!(req.max_tokens, Some(2048));
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.messages[0].content, "question");
        assert!(matches!(req.messages[0].cache, CacheControl::None));
        assert_eq!(req.messages[1].role, Role::User);
        assert!(matches!(
            req.messages[1].cache,
            CacheControl::Ephemeral(CacheTtl::Hours1),
        ));
    }

    /// The builder also chains assistant turns (cached and uncached) —
    /// the surface is symmetric across roles so multi-turn replays can
    /// be reconstructed without dropping into raw `Message` literals.
    #[test]
    fn completion_request_builder_chains_all_roles() {
        let req = CompletionRequest::new(ModelId::Other("gpt-5-preview".to_string()))
            .assistant("previous reply")
            .assistant_cached("cached reply", CacheControl::Ephemeral(CacheTtl::Minutes5));
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, Role::Assistant);
        assert!(matches!(req.messages[0].cache, CacheControl::None));
        assert_eq!(req.messages[1].role, Role::Assistant);
        assert!(matches!(
            req.messages[1].cache,
            CacheControl::Ephemeral(CacheTtl::Minutes5),
        ));
    }
}
