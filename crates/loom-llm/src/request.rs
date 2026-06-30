//! `CompletionRequest` — the typed builder consumers compose to call
//! `LlmClient::complete*`. Messages are typed; cache control attaches
//! per-content-block.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use bytes::Bytes;
use displaydoc::Display;
use thiserror::Error;

use crate::cache::CacheControl;
use crate::client::{ParseToolCallIdError, ToolCallId, ToolUseRequest};
use crate::model_id::ModelId;
use crate::tool::ToolDef;

/// Validated MIME type used by binary request parts.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MimeType(Cow<'static, str>);

impl MimeType {
    /// PDF document MIME type.
    pub const APPLICATION_PDF: Self = Self(Cow::Borrowed("application/pdf"));
    /// PNG image MIME type.
    pub const IMAGE_PNG: Self = Self(Cow::Borrowed("image/png"));
    /// JPEG image MIME type.
    pub const IMAGE_JPEG: Self = Self(Cow::Borrowed("image/jpeg"));
    /// WebP image MIME type.
    pub const IMAGE_WEBP: Self = Self(Cow::Borrowed("image/webp"));

    /// Parse and validate a MIME type string.
    pub fn parse(raw: impl Into<String>) -> Result<Self, MimeTypeParseError> {
        let raw = raw.into();
        validate_mime_type(&raw)?;
        Ok(Self(Cow::Owned(raw.to_ascii_lowercase())))
    }

    /// Borrow the normalized MIME type string.
    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

impl fmt::Debug for MimeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("MimeType").field(&self.as_str()).finish()
    }
}

impl fmt::Display for MimeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MimeType {
    type Err = MimeTypeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for MimeType {
    type Error = MimeTypeParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for MimeType {
    type Error = MimeTypeParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

/// invalid MIME type: {value}
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub struct MimeTypeParseError {
    value: String,
}

impl MimeTypeParseError {
    fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }
}

/// Binary request content owned by `loom-llm`.
#[derive(Clone, PartialEq, Eq)]
pub struct BinaryContent {
    /// Validated MIME type for the payload.
    pub mime_type: MimeType,
    /// Raw payload bytes. Provider clients encode at the transport boundary.
    pub bytes: Bytes,
    /// Optional caller-supplied name or filename metadata.
    pub name: Option<String>,
}

impl BinaryContent {
    /// Construct an unnamed binary content part.
    pub fn new(mime_type: MimeType, bytes: impl Into<Bytes>) -> Self {
        Self {
            mime_type,
            bytes: bytes.into(),
            name: None,
        }
    }

    /// Construct a named binary content part.
    pub fn named(mime_type: MimeType, bytes: impl Into<Bytes>, name: impl Into<String>) -> Self {
        Self {
            mime_type,
            bytes: bytes.into(),
            name: Some(name.into()),
        }
    }
}

impl fmt::Debug for BinaryContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BinaryContent")
            .field("mime_type", &self.mime_type)
            .field("name", &self.name)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

/// One ordered content part in a message.
#[derive(Clone, PartialEq, Eq)]
pub enum MessageContent {
    /// Text prompt part.
    Text(String),
    /// Binary payload part.
    Binary(BinaryContent),
}

impl fmt::Debug for MessageContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageContent::Text(text) => f
                .debug_struct("Text")
                .field("char_len", &text.chars().count())
                .finish(),
            MessageContent::Binary(binary) => fmt::Debug::fmt(binary, f),
        }
    }
}

/// One message in a completion request. Constructed via the builder
/// helpers on [`CompletionRequest`]; consumers compose blocks rather
/// than handing in raw JSON.
#[derive(Clone)]
pub struct Message {
    /// Speaker role.
    pub role: Role,
    /// Ordered text and binary content parts.
    pub content: Vec<MessageContent>,
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
    pub tool_call_id: Option<ToolCallId>,
    /// True when this tool-result message reports an error from the
    /// tool handler. Providers that distinguish error tool-results
    /// surface this; others ignore it.
    pub tool_is_error: bool,
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Message")
            .field("role", &self.role)
            .field("content", &self.content)
            .field("cache", &self.cache)
            .field("tool_call_count", &self.tool_calls.len())
            .field("tool_call_id", &self.tool_call_id)
            .field("tool_is_error", &self.tool_is_error)
            .finish()
    }
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
#[derive(Clone)]
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

impl fmt::Debug for CompletionRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompletionRequest")
            .field("model", &self.model)
            .field(
                "system_char_len",
                &self.system.as_ref().map(|s| s.chars().count()),
            )
            .field("messages", &self.messages)
            .field("max_tokens", &self.max_tokens)
            .field("tool_count", &self.tools.len())
            .finish()
    }
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

    /// Append binary content to the most recent user turn, or create one.
    pub fn user_binary(mut self, mime_type: MimeType, bytes: impl Into<Bytes>) -> Self {
        self.append_binary(Role::User, BinaryContent::new(mime_type, bytes));
        self
    }

    /// Append named binary content to the most recent user turn, or create one.
    pub fn user_binary_named(
        mut self,
        mime_type: MimeType,
        bytes: impl Into<Bytes>,
        name: impl Into<String>,
    ) -> Self {
        self.append_binary(Role::User, BinaryContent::named(mime_type, bytes, name));
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

    /// Append binary content to the most recent assistant turn, or create one.
    pub fn assistant_binary(mut self, mime_type: MimeType, bytes: impl Into<Bytes>) -> Self {
        self.append_binary(Role::Assistant, BinaryContent::new(mime_type, bytes));
        self
    }

    /// Append named binary content to the most recent assistant turn, or create one.
    pub fn assistant_binary_named(
        mut self,
        mime_type: MimeType,
        bytes: impl Into<Bytes>,
        name: impl Into<String>,
    ) -> Self {
        self.append_binary(
            Role::Assistant,
            BinaryContent::named(mime_type, bytes, name),
        );
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

    fn append_binary(&mut self, role: Role, binary: BinaryContent) {
        if let Some(message) = self.messages.iter_mut().rev().find(|m| m.role == role) {
            message.content.push(MessageContent::Binary(binary));
        } else {
            self.messages.push(Message::binary(role, binary));
        }
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

    /// Construct an unnamed user binary turn.
    pub fn user_binary(mime_type: MimeType, bytes: impl Into<Bytes>) -> Self {
        Self::binary(Role::User, BinaryContent::new(mime_type, bytes))
    }

    /// Construct a named user binary turn.
    pub fn user_binary_named(
        mime_type: MimeType,
        bytes: impl Into<Bytes>,
        name: impl Into<String>,
    ) -> Self {
        Self::binary(Role::User, BinaryContent::named(mime_type, bytes, name))
    }

    /// Construct a plain assistant turn.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::text(Role::Assistant, content, CacheControl::None)
    }

    /// Construct an assistant turn with a per-block cache marker.
    pub fn assistant_cached(content: impl Into<String>, cache: CacheControl) -> Self {
        Self::text(Role::Assistant, content, cache)
    }

    /// Construct an unnamed assistant binary turn.
    pub fn assistant_binary(mime_type: MimeType, bytes: impl Into<Bytes>) -> Self {
        Self::binary(Role::Assistant, BinaryContent::new(mime_type, bytes))
    }

    /// Construct a named assistant binary turn.
    pub fn assistant_binary_named(
        mime_type: MimeType,
        bytes: impl Into<Bytes>,
        name: impl Into<String>,
    ) -> Self {
        Self::binary(
            Role::Assistant,
            BinaryContent::named(mime_type, bytes, name),
        )
    }

    /// Construct an assistant turn that carries tool calls. `content`
    /// may be empty when the model emitted only tool-use blocks.
    pub fn assistant_tool_use(content: impl Into<String>, tool_calls: Vec<ToolUseRequest>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![MessageContent::Text(content.into())],
            cache: CacheControl::None,
            tool_calls,
            tool_call_id: None,
            tool_is_error: false,
        }
    }

    /// Construct a tool-result turn the loop forwards back to the model
    /// after dispatching an assistant tool call.
    pub fn tool_result(call_id: ToolCallId, content: impl Into<String>, is_error: bool) -> Self {
        Self {
            role: Role::Tool,
            content: vec![MessageContent::Text(content.into())],
            cache: CacheControl::None,
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id),
            tool_is_error: is_error,
        }
    }

    /// Parse a raw tool-call id and construct a tool-result turn.
    pub fn try_tool_result(
        call_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Result<Self, ParseToolCallIdError> {
        Ok(Self::tool_result(
            ToolCallId::parse(call_id)?,
            content,
            is_error,
        ))
    }

    /// Concatenate text parts in order, omitting binary parts.
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|part| match part {
                MessageContent::Text(text) => Some(text.as_str()),
                MessageContent::Binary(_) => None,
            })
            .collect()
    }

    fn text(role: Role, content: impl Into<String>, cache: CacheControl) -> Self {
        Self {
            role,
            content: vec![MessageContent::Text(content.into())],
            cache,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_is_error: false,
        }
    }

    fn binary(role: Role, binary: BinaryContent) -> Self {
        Self {
            role,
            content: vec![MessageContent::Binary(binary)],
            cache: CacheControl::None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_is_error: false,
        }
    }
}

fn validate_mime_type(raw: &str) -> Result<(), MimeTypeParseError> {
    let Some((type_part, subtype_part)) = raw.split_once('/') else {
        return Err(MimeTypeParseError::new(raw));
    };
    if type_part.is_empty()
        || subtype_part.is_empty()
        || subtype_part.contains('/')
        || !type_part.bytes().all(is_mime_token_byte)
        || !subtype_part.bytes().all(is_mime_token_byte)
    {
        return Err(MimeTypeParseError::new(raw));
    }
    Ok(())
}

fn is_mime_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheTtl;
    use crate::model_id::{AnthropicModel, OpenAiModel};

    #[test]
    fn completion_request_requires_model_at_construction() {
        let req = CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .system("prefix")
            .user("question")
            .user_cached("doc", CacheControl::Ephemeral(CacheTtl::Hours1))
            .max_tokens(2048);
        assert_eq!(
            req.model,
            ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
        );
        assert_eq!(req.system.as_deref(), Some("prefix"));
        assert_eq!(req.max_tokens, Some(2048));
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(
            req.messages[0].content,
            vec![MessageContent::Text("question".into())]
        );
        assert!(matches!(req.messages[0].cache, CacheControl::None));
        assert_eq!(req.messages[1].role, Role::User);
        assert!(matches!(
            req.messages[1].cache,
            CacheControl::Ephemeral(CacheTtl::Hours1),
        ));
    }

    #[test]
    fn completion_request_builder_chains_all_roles() {
        let req = CompletionRequest::new(ModelId::OpenAi(OpenAiModel::Other(
            "gpt-5-preview".to_string(),
        )))
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

    #[test]
    fn completion_request_text_only_api_remains_compatible() {
        let msg = Message::assistant("reply");
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content, vec![MessageContent::Text("reply".into())]);

        let cached = Message::user_cached("doc", CacheControl::Ephemeral(CacheTtl::Hours24));
        assert_eq!(cached.content, vec![MessageContent::Text("doc".into())]);
        assert!(matches!(
            cached.cache,
            CacheControl::Ephemeral(CacheTtl::Hours24),
        ));
    }

    #[test]
    fn completion_request_accepts_text_and_pdf_binary_parts() {
        let req = CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .user("Summarize this PDF")
            .user_binary(MimeType::APPLICATION_PDF, vec![1_u8, 2, 3]);

        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.messages[0].content.len(), 2);
        assert_eq!(
            req.messages[0].content[0],
            MessageContent::Text("Summarize this PDF".into()),
        );
        match &req.messages[0].content[1] {
            MessageContent::Binary(binary) => {
                assert_eq!(binary.mime_type, MimeType::APPLICATION_PDF);
                assert_eq!(binary.bytes.as_ref(), &[1_u8, 2, 3]);
                assert_eq!(binary.name, None);
            }
            other => panic!("expected binary part, got {other:?}"),
        }
    }

    #[test]
    fn binary_builders_append_to_existing_role_message() {
        let req = CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .user("first")
            .assistant("middle")
            .user_binary_named(MimeType::IMAGE_PNG, vec![9_u8], "diagram.png")
            .assistant_binary(MimeType::IMAGE_WEBP, vec![8_u8]);

        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, Role::User);
        assert_eq!(req.messages[0].content.len(), 2);
        assert_eq!(req.messages[1].role, Role::Assistant);
        assert_eq!(req.messages[1].content.len(), 2);
        match &req.messages[0].content[1] {
            MessageContent::Binary(binary) => {
                assert_eq!(binary.name.as_deref(), Some("diagram.png"));
            }
            other => panic!("expected named binary part, got {other:?}"),
        }
    }

    #[test]
    fn binary_builder_creates_role_message_when_absent() {
        let req = CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46))
            .assistant_binary_named(MimeType::IMAGE_JPEG, vec![1_u8], "photo.jpg");

        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, Role::Assistant);
        match &req.messages[0].content[0] {
            MessageContent::Binary(binary) => {
                assert_eq!(binary.mime_type, MimeType::IMAGE_JPEG);
                assert_eq!(binary.name.as_deref(), Some("photo.jpg"));
            }
            other => panic!("expected binary part, got {other:?}"),
        }
    }

    #[test]
    fn mime_type_parser_accepts_valid_and_rejects_invalid() {
        for valid in [
            "application/pdf",
            "image/png",
            "image/jpeg",
            "image/webp",
            "text/plain",
            "application/vnd.example+json",
        ] {
            let parsed: MimeType = valid.parse().expect("valid MIME type parses");
            assert_eq!(parsed.as_str(), valid);
        }

        for invalid in [
            "",
            "text",
            "/plain",
            "text/",
            "text/plain/json",
            "text /plain",
            "text/plain; charset=utf-8",
        ] {
            assert!(MimeType::parse(invalid).is_err(), "{invalid:?} rejects");
        }
    }

    #[test]
    fn binary_content_debug_redacts_payload() {
        let binary = BinaryContent::named(
            MimeType::APPLICATION_PDF,
            b"secret pdf bytes".to_vec(),
            "doc.pdf",
        );
        let rendered = format!("{binary:?}");
        assert!(rendered.contains("application/pdf"));
        assert!(rendered.contains("doc.pdf"));
        assert!(rendered.contains("byte_len"));
        assert!(rendered.contains("16"));
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("pdf bytes"));
    }
}
