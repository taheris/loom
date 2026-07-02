//! `LlmClient` — the public-contract trait every backend implements
//! plus the typed [`LlmError`] surface and [`RetryAdvice`]
//! classification consumers compose retry policies on top of.
//!
//! Per-call model selection (no fixed-model client construction): the
//! same client instance accepts a different model on every request, with
//! `ModelId` carried as a required positional field of
//! [`crate::request::CompletionRequest`]. Schema routing is structural —
//! each outer [`crate::model_id::ModelId`] variant maps 1:1 to a
//! [`crate::model_id::SchemaKind`] via `ModelId::schema(&self)`.

mod multi_provider;
#[cfg(feature = "openai-compat")]
mod openai_compat;

pub use multi_provider::{AnthropicClient, GeminiClient, OpenAiClient};
#[cfg(feature = "openai-compat")]
pub use openai_compat::OpenAiCompatClient;

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use displaydoc::Display;
use schemars::{JsonSchema, SchemaGenerator};
use serde::de::DeserializeOwned;
use thiserror::Error;

use loom_events::{AgentEvent, DriverEventPayload};

use crate::model_id::{ModelId, SchemaKind};
use crate::request::{CompletionRequest, MimeType};
use crate::usage::TokenUsage;

/// Boxed future the object-safe trait methods return. Using a boxed
/// future (rather than `impl Future`) keeps the trait dyn-compatible so
/// `Arc<dyn LlmClient>` works for runtime polymorphism — per-tenant
/// Client caches, mock impls, and external-crate impls compose through
/// the same dyn surface.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Successful completion outcome. Every call carries token usage so
/// consumers see cache hits and cost directly; the same `TokenUsage` is
/// fanned out as a `DriverKind::TokenUsage` `AgentEvent` for SaaS billing
/// pipelines tailing the live event stream.
#[derive(Clone)]
pub struct CompletionResponse {
    /// Final assistant text. Tool-use loops yield this from the last
    /// non-tool-calling turn.
    pub text: String,
    /// Per-call token usage including cache fields.
    pub usage: TokenUsage,
    /// Tool calls the model emitted on this turn. Empty when the model
    /// produced text only. [`crate::Conversation`]'s loop iterates while
    /// this is non-empty.
    pub tool_calls: Vec<ToolUseRequest>,
}

impl fmt::Debug for CompletionResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompletionResponse")
            .field("text_char_len", &self.text.chars().count())
            .field("usage", &self.usage)
            .field("tool_call_count", &self.tool_calls.len())
            .finish()
    }
}

/// Provider-stable identifier for a model-issued tool call.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ToolCallId(String);

impl ToolCallId {
    /// Parse and validate a provider tool-call identifier.
    pub fn parse(raw: impl Into<String>) -> Result<Self, ParseToolCallIdError> {
        let raw = raw.into();
        validate_tool_call_id(&raw)?;
        Ok(Self(raw))
    }

    /// Borrow the validated identifier string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ToolCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ToolCallId").field(&self.as_str()).finish()
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ToolCallId {
    type Err = ParseToolCallIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for ToolCallId {
    type Error = ParseToolCallIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for ToolCallId {
    type Error = ParseToolCallIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

/// invalid tool call id: {value}
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub struct ParseToolCallIdError {
    value: String,
}

impl ParseToolCallIdError {
    fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }
}

fn validate_tool_call_id(raw: &str) -> Result<(), ParseToolCallIdError> {
    if raw.is_empty()
        || !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'|'))
    {
        return Err(ParseToolCallIdError::new(raw));
    }
    Ok(())
}

/// One tool call the model emitted on a turn. The conversation loop
/// dispatches each call to the registered [`crate::Tool`] whose `name`
/// matches and appends the result as a tool-role message on the next
/// iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUseRequest {
    /// Provider-stable identifier the loop echoes back on the matching
    /// tool result so the model correlates request to response.
    pub call_id: ToolCallId,
    /// Name of the tool the model wants to invoke; matches a registered
    /// [`crate::Tool`]'s `name()`.
    pub name: String,
    /// JSON arguments payload the model supplied for the call.
    pub args: serde_json::Value,
}

/// Typed transport-failure surface returned by every fallible `llm`
/// transport call. Variants are deliberately coarse — the classes
/// spec'd in `specs/llm.md` — so consumers can drive retry
/// policy via [`LlmError::retry_advice`] without parsing message
/// strings. `#[non_exhaustive]` keeps the door open for future
/// HTTP-status carve-outs and provider-specific error families to land
/// additively without breaking consumer matchers.
#[non_exhaustive]
#[derive(Display, Error)]
pub enum LlmError {
    /// transport failure: {0}
    Transport(String),
    /// deadline exceeded
    Timeout,
    /// rate limited; retry after {retry_after:?}
    RateLimited {
        /// Delay parsed from the provider's `Retry-After` header, or the
        /// documented [`DEFAULT_RETRY_AFTER`] fallback when the header
        /// is missing or unparseable.
        retry_after: Duration,
    },
    /// authentication failed: {reason}
    AuthFailed {
        /// Provider-supplied auth-failure reason. Opaque at this layer;
        /// consumers surface it in their own diagnostics.
        reason: String,
    },
    /// provider returned HTTP {status}: {body}
    ProviderHttp {
        /// Raw HTTP status code the provider returned. Consumers map
        /// `>= 500` to retryable; everything else is non-retryable per
        /// [`LlmError::retry_advice`].
        status: u16,
        /// Raw response body, opaque at this layer.
        body: String,
    },
    /// malformed JSON in response: {0}
    MalformedJson(String),
    /// response failed schema validation: {0}
    SchemaViolation(String),
    /// model {model:?} is incompatible with client schema {expected:?}
    IncompatibleModel {
        /// The `ModelId` the request named.
        model: ModelId,
        /// The Client's fixed schema, set at construction time.
        expected: SchemaKind,
    },
    /// provider {provider:?} does not support {capability:?}
    UnsupportedCapability {
        /// Provider schema that rejected the request capability.
        provider: SchemaKind,
        /// Capability the request needs.
        capability: LlmCapability,
    },
    /// incompatible request: {reason}
    IncompatibleRequest {
        /// Provider-independent reason the request cannot be sent.
        reason: String,
    },
    /// underlying provider failed: {message}
    Provider {
        /// Provider-supplied diagnostic message. Documented fallback
        /// for unclassifiable cases.
        message: String,
    },
}

impl fmt::Debug for LlmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LlmError::Transport(message) => f
                .debug_struct("Transport")
                .field("message_char_len", &message.chars().count())
                .finish(),
            LlmError::Timeout => f.write_str("Timeout"),
            LlmError::RateLimited { retry_after } => f
                .debug_struct("RateLimited")
                .field("retry_after", retry_after)
                .finish(),
            LlmError::AuthFailed { reason } => f
                .debug_struct("AuthFailed")
                .field("reason_char_len", &reason.chars().count())
                .finish(),
            LlmError::ProviderHttp { status, body } => f
                .debug_struct("ProviderHttp")
                .field("status", status)
                .field("body_char_len", &body.chars().count())
                .finish(),
            LlmError::MalformedJson(message) => f
                .debug_struct("MalformedJson")
                .field("message_char_len", &message.chars().count())
                .finish(),
            LlmError::SchemaViolation(message) => f
                .debug_struct("SchemaViolation")
                .field("message_char_len", &message.chars().count())
                .finish(),
            LlmError::IncompatibleModel { model, expected } => f
                .debug_struct("IncompatibleModel")
                .field("model", model)
                .field("expected", expected)
                .finish(),
            LlmError::UnsupportedCapability {
                provider,
                capability,
            } => f
                .debug_struct("UnsupportedCapability")
                .field("provider", provider)
                .field("capability", capability)
                .finish(),
            LlmError::IncompatibleRequest { reason } => f
                .debug_struct("IncompatibleRequest")
                .field("reason_char_len", &reason.chars().count())
                .finish(),
            LlmError::Provider { message } => f
                .debug_struct("Provider")
                .field("message_char_len", &message.chars().count())
                .finish(),
        }
    }
}

/// LLM capability a provider may reject before network I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmCapability {
    /// Binary multimodal content with the supplied MIME type.
    MultimodalBinary {
        /// MIME type of the binary part that requires multimodal support.
        mime_type: MimeType,
    },
}

/// Retry classification returned by [`LlmError::retry_advice`].
/// `loom-llm` does not retry; the method returns advice and the
/// consumer composes its own backoff, jitter, and budget policy on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAdvice {
    /// Safe to retry without delay (subject to the consumer's own
    /// backoff policy).
    Retryable,
    /// Retry after the supplied delay — typically the
    /// `Retry-After` value the provider sent.
    RetryAfter(Duration),
    /// The error class does not benefit from retry.
    NonRetryable,
}

impl LlmError {
    /// Retry advice for this error per the canonical classification
    /// table in `specs/llm.md` § LlmError. Consumers drive their retry
    /// policy off this method; `loom-llm` does not retry.
    pub fn retry_advice(&self) -> RetryAdvice {
        match self {
            LlmError::Transport(_) | LlmError::Timeout => RetryAdvice::Retryable,
            LlmError::RateLimited { retry_after } => RetryAdvice::RetryAfter(*retry_after),
            LlmError::MalformedJson(_) | LlmError::SchemaViolation(_) => RetryAdvice::Retryable,
            LlmError::ProviderHttp { status, .. } => {
                if *status >= 500 {
                    RetryAdvice::Retryable
                } else {
                    RetryAdvice::NonRetryable
                }
            }
            LlmError::AuthFailed { .. }
            | LlmError::IncompatibleModel { .. }
            | LlmError::UnsupportedCapability { .. }
            | LlmError::IncompatibleRequest { .. }
            | LlmError::Provider { .. } => RetryAdvice::NonRetryable,
        }
    }
}

/// Fallback applied to [`LlmError::RateLimited`] when the provider's
/// `Retry-After` header is missing or cannot be parsed. Sixty seconds
/// is conservative — long enough that consumer retry budgets are
/// unlikely to burst through a real rate-limit window, short enough
/// that legitimate transient throttles still recover within one wait.
pub const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Parse a `Retry-After` header value into a [`Duration`] per
/// RFC 7231 §7.1.3. Accepts the documented integer-seconds form (e.g.
/// `"30"`) and the IMF-fixdate form (e.g.
/// `"Sun, 06 Nov 1994 08:49:37 GMT"`). The `now` parameter anchors
/// HTTP-date subtraction; consumers inject a clock-supplied value so
/// the helper stays pure and deterministic. Unparseable input and
/// past dates fall back to [`DEFAULT_RETRY_AFTER`] / [`Duration::ZERO`]
/// respectively.
pub fn parse_retry_after(value: &str, now: SystemTime) -> Duration {
    let trimmed = value.trim();
    if let Ok(secs) = trimmed.parse::<u64>() {
        return Duration::from_secs(secs);
    }
    match parse_imf_fixdate(trimmed) {
        Ok(target) => match target.duration_since(now) {
            Ok(duration) => duration,
            Err(_past_date) => Duration::ZERO,
        },
        Err(_invalid_date) => DEFAULT_RETRY_AFTER,
    }
}

/// invalid IMF-fixdate: {reason}
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
struct ImfFixdateParseError {
    reason: &'static str,
}

impl ImfFixdateParseError {
    const fn new(reason: &'static str) -> Self {
        Self { reason }
    }
}

/// Parse the RFC 7231 §7.1.1.1 IMF-fixdate format
/// (`"Sun, 06 Nov 1994 08:49:37 GMT"`) into a [`SystemTime`]. The
/// obsolete RFC 850 and asctime formats return a typed parse error;
/// callers (see [`parse_retry_after`]) handle the documented fallback.
fn parse_imf_fixdate(s: &str) -> Result<SystemTime, ImfFixdateParseError> {
    let s = s.trim();
    let comma = s
        .find(", ")
        .ok_or(ImfFixdateParseError::new("missing weekday comma"))?;
    let rest = s
        .get(comma + 2..)
        .ok_or(ImfFixdateParseError::new("missing date fields"))?;
    if rest.len() < 23 {
        return Err(ImfFixdateParseError::new("date is too short"));
    }
    let day = parse_decimal::<u32>(
        rest.get(0..2)
            .ok_or(ImfFixdateParseError::new("missing day"))?,
        "invalid day",
    )?;
    let month = match rest
        .get(3..6)
        .ok_or(ImfFixdateParseError::new("missing month"))?
    {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return Err(ImfFixdateParseError::new("invalid month")),
    };
    let year = parse_decimal::<i64>(
        rest.get(7..11)
            .ok_or(ImfFixdateParseError::new("missing year"))?,
        "invalid year",
    )?;
    let hour = parse_decimal::<u64>(
        rest.get(12..14)
            .ok_or(ImfFixdateParseError::new("missing hour"))?,
        "invalid hour",
    )?;
    let min = parse_decimal::<u64>(
        rest.get(15..17)
            .ok_or(ImfFixdateParseError::new("missing minute"))?,
        "invalid minute",
    )?;
    let sec = parse_decimal::<u64>(
        rest.get(18..20)
            .ok_or(ImfFixdateParseError::new("missing second"))?,
        "invalid second",
    )?;
    if rest
        .get(20..23)
        .ok_or(ImfFixdateParseError::new("missing GMT marker"))?
        != " GM"
        || !rest.ends_with("GMT")
    {
        return Err(ImfFixdateParseError::new("invalid GMT marker"));
    }
    if !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return Err(ImfFixdateParseError::new("date field out of range"));
    }
    let days = days_from_civil(year, month, day)?;
    let seconds = i64::try_from(hour * 3_600 + min * 60 + sec)
        .map_err(|_| ImfFixdateParseError::new("time field overflow"))?;
    let unix_seconds = days
        .checked_mul(86_400)
        .and_then(|days| days.checked_add(seconds))
        .ok_or(ImfFixdateParseError::new("timestamp overflow"))?;
    if unix_seconds < 0 {
        let neg = u64::try_from(-unix_seconds)
            .map_err(|_| ImfFixdateParseError::new("negative timestamp overflow"))?;
        SystemTime::UNIX_EPOCH
            .checked_sub(Duration::from_secs(neg))
            .ok_or(ImfFixdateParseError::new("system time underflow"))
    } else {
        let pos = u64::try_from(unix_seconds)
            .map_err(|_| ImfFixdateParseError::new("positive timestamp overflow"))?;
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(pos))
            .ok_or(ImfFixdateParseError::new("system time overflow"))
    }
}

fn parse_decimal<T>(raw: &str, reason: &'static str) -> Result<T, ImfFixdateParseError>
where
    T: FromStr,
{
    raw.parse::<T>()
        .map_err(|_| ImfFixdateParseError::new(reason))
}

/// Days from the proleptic-Gregorian civil date `(y, m, d)` to the
/// Unix epoch (1970-01-01), per Howard Hinnant's `days_from_civil`
/// algorithm. Negative for dates before 1970-01-01.
fn days_from_civil(y: i64, m: u32, d: u32) -> Result<i64, ImfFixdateParseError> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(ImfFixdateParseError::new("civil date out of range"));
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = u64::try_from(y - era * 400)
        .map_err(|_| ImfFixdateParseError::new("year offset out of range"))?;
    let m_shifted: u64 = if m > 2 {
        u64::from(m) - 3
    } else {
        u64::from(m) + 9
    };
    let doy = (153 * m_shifted + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let doe_i =
        i64::try_from(doe).map_err(|_| ImfFixdateParseError::new("day offset out of range"))?;
    era.checked_mul(146_097)
        .and_then(|era_days| era_days.checked_add(doe_i))
        .and_then(|days| days.checked_sub(719_468))
        .ok_or(ImfFixdateParseError::new("civil date overflow"))
}

/// The public agent-side LLM contract.
///
/// The trait is object-safe so `Arc<dyn LlmClient>` works for runtime
/// polymorphism — per-tenant Client caches, mock impls in tests, and
/// external-crate `LlmClient` impls compose through the same dyn
/// surface.
///
/// `complete` and the type-erased `complete_structured_raw` return
/// boxed futures so the trait stays dyn-compatible; the typed
/// `complete_structured::<T>` consumer surface is provided as a
/// blanket-implemented extension method on [`LlmClientExt`] that
/// generates `T`'s schema via `schemars` and routes through
/// `complete_structured_raw`. Consumers reach for
/// `complete_structured::<T>` regardless of whether they hold a
/// concrete Client or an `Arc<dyn LlmClient>`.
pub trait LlmClient: Send + Sync {
    /// Wire-format discriminator this Client targets. Per-Client schema
    /// is fixed at construction; per-call model selection varies the
    /// `ModelId` within that schema.
    fn schema(&self) -> SchemaKind;

    /// Whether this Client can dispatch the supplied `ModelId`. Default
    /// impl returns `model.schema() == self.schema()`; overrides are
    /// rare and only useful for adapters that span multiple schemas.
    fn supports(&self, model: &ModelId) -> bool {
        model.schema() == self.schema()
    }

    /// Fan a driver/observer payload into this Client's active sink chain.
    /// Built-in Clients stamp it with their configured event-session envelope.
    fn emit_driver_event(&self, _event: DriverEventPayload) {}

    /// Fan a pre-stamped event into this Client's active sink chain.
    /// Custom Clients may ignore events by using this default no-op;
    /// built-in Clients override it for callers that already own a full event.
    fn emit_event(&self, _event: &AgentEvent) {}

    /// Run a completion against the request's `ModelId`. Returns the
    /// final assistant text plus token usage.
    fn complete<'a>(
        &'a self,
        req: CompletionRequest,
    ) -> BoxFuture<'a, Result<CompletionResponse, LlmError>>;

    /// Schema-aware completion that returns the raw assistant-text JSON
    /// payload. Implementers select the right provider mechanism
    /// (synthetic forced-tool for Anthropic, `response_format` for
    /// OpenAI, `response_schema` for Gemini) using the supplied schema
    /// and type name, run the call, and return the raw text the model
    /// produced. Consumers should prefer the typed
    /// [`LlmClientExt::complete_structured`] wrapper which generates
    /// the schema from `T` and parses the returned text.
    fn complete_structured_raw<'a>(
        &'a self,
        req: CompletionRequest,
        schema: serde_json::Value,
        type_name: String,
    ) -> BoxFuture<'a, Result<String, LlmError>>;
}

impl<C: LlmClient + ?Sized> LlmClient for Box<C> {
    fn schema(&self) -> SchemaKind {
        (**self).schema()
    }

    fn supports(&self, model: &ModelId) -> bool {
        (**self).supports(model)
    }

    fn emit_driver_event(&self, event: DriverEventPayload) {
        (**self).emit_driver_event(event);
    }

    fn emit_event(&self, event: &AgentEvent) {
        (**self).emit_event(event);
    }

    fn complete<'a>(
        &'a self,
        req: CompletionRequest,
    ) -> BoxFuture<'a, Result<CompletionResponse, LlmError>> {
        (**self).complete(req)
    }

    fn complete_structured_raw<'a>(
        &'a self,
        req: CompletionRequest,
        schema: serde_json::Value,
        type_name: String,
    ) -> BoxFuture<'a, Result<String, LlmError>> {
        (**self).complete_structured_raw(req, schema, type_name)
    }
}

/// Typed consumer-facing structured-output entry point. Blanket-
/// implemented for every [`LlmClient`] (including `dyn LlmClient`), so
/// `Arc<dyn LlmClient>` dispatches `complete_structured::<T>` through
/// the dyn-safe [`LlmClient::complete_structured_raw`] without the
/// trait itself carrying a generic method (which would break
/// object-safety).
pub trait LlmClientExt: LlmClient {
    /// Run a completion that deserializes into `T`. Generates `T`'s
    /// JSON schema via `schemars`, calls
    /// [`LlmClient::complete_structured_raw`], and parses the returned
    /// text into `T`. Failure modes surface as typed [`LlmError`]
    /// variants: [`LlmError::MalformedJson`] for non-JSON / parse
    /// failures, [`LlmError::SchemaViolation`] for parsed-but-invalid
    /// responses.
    fn complete_structured<'a, T>(
        &'a self,
        req: CompletionRequest,
    ) -> BoxFuture<'a, Result<T, LlmError>>
    where
        T: DeserializeOwned + JsonSchema + Send + 'static;
}

impl<C: LlmClient + ?Sized> LlmClientExt for C {
    fn complete_structured<'a, T>(
        &'a self,
        req: CompletionRequest,
    ) -> BoxFuture<'a, Result<T, LlmError>>
    where
        T: DeserializeOwned + JsonSchema + Send + 'static,
    {
        let schema = SchemaGenerator::default()
            .into_root_schema_for::<T>()
            .to_value();
        let type_name = sanitize_schema_name(&T::schema_name());
        Box::pin(async move {
            let text = self.complete_structured_raw(req, schema, type_name).await?;
            parse_structured_payload::<T>(&text)
        })
    }
}

fn parse_structured_payload<T>(text: &str) -> Result<T, LlmError>
where
    T: DeserializeOwned,
{
    let value = serde_json::from_str::<serde_json::Value>(text)
        .map_err(|err| LlmError::MalformedJson(err.to_string()))?;
    serde_json::from_value(value).map_err(|err| LlmError::SchemaViolation(err.to_string()))
}

/// Sanitize a schemars-derived schema name for the OpenAI
/// structured-output API, which only accepts ASCII alphanumerics plus
/// `-` and `_`. The other adapters ignore the name, so the same
/// sanitization is safe across providers.
fn sanitize_schema_name(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::model_id::AnthropicModel;
    use crate::usage::TokenUsage;

    #[test]
    fn tool_call_id_validates_provider_identifiers() {
        for raw in [
            "toolu_01",
            "call-1",
            "call_es8xfFZpiG9eU4F6ONqu9AC5|fc_084fe1d174a690c2016a234c2e4d108191a893f4d1bf036187",
        ] {
            let parsed = ToolCallId::parse(raw).expect("valid tool call id parses");
            assert_eq!(parsed.as_str(), raw);
        }

        for raw in ["", "tool call", "tool.call", "tool/call", "tool;call"] {
            assert!(ToolCallId::parse(raw).is_err(), "{raw:?} rejects");
        }
    }

    /// `Arc<dyn LlmClient>` compiles and dispatches `complete` through
    /// the trait object — the object-safety promise the spec pins under
    /// Success Criteria § Public surface ("`Arc<dyn LlmClient>` compiles
    /// and dispatches `complete` / `complete_structured` correctly").
    /// `complete_structured::<T>` is the typed extension provided by
    /// [`LlmClientExt`]'s blanket impl, so it also resolves on a
    /// trait-object receiver — exercised here against the same dyn
    /// reference.
    #[test]
    fn llm_client_trait_is_object_safe() {
        #[derive(Default)]
        struct StubClient {
            captured: Arc<Mutex<Vec<String>>>,
        }

        impl LlmClient for StubClient {
            fn schema(&self) -> SchemaKind {
                SchemaKind::Anthropic
            }

            fn complete<'a>(
                &'a self,
                req: CompletionRequest,
            ) -> BoxFuture<'a, Result<CompletionResponse, LlmError>> {
                let captured = self.captured.clone();
                Box::pin(async move {
                    captured
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push(req.model.as_wire());
                    Ok(CompletionResponse {
                        text: "ok".into(),
                        usage: TokenUsage::default(),
                        tool_calls: Vec::new(),
                    })
                })
            }

            fn complete_structured_raw<'a>(
                &'a self,
                _req: CompletionRequest,
                _schema: serde_json::Value,
                _type_name: String,
            ) -> BoxFuture<'a, Result<String, LlmError>> {
                Box::pin(async move { Ok(r#"{"text":"structured"}"#.to_string()) })
            }
        }

        #[derive(serde::Deserialize, schemars::JsonSchema, PartialEq, Debug)]
        struct Shape {
            text: String,
        }

        let stub = StubClient::default();
        let captured = stub.captured.clone();
        let client: Arc<dyn LlmClient> = Arc::new(stub);

        assert_eq!(client.schema(), SchemaKind::Anthropic);
        assert!(client.supports(&ModelId::Anthropic(AnthropicModel::ClaudeSonnet46)));

        let req = CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46));
        let resp =
            tokio_test::block_on(client.complete(req)).expect("dyn dispatch reaches stub complete");
        assert_eq!(resp.text, "ok");
        assert_eq!(
            *captured.lock().unwrap_or_else(|p| p.into_inner()),
            vec!["claude-sonnet-4-6".to_string()],
        );

        let structured_req =
            CompletionRequest::new(ModelId::Anthropic(AnthropicModel::ClaudeSonnet46));
        let parsed: Shape =
            tokio_test::block_on(client.complete_structured::<Shape>(structured_req))
                .expect("dyn dispatch reaches structured path");
        assert_eq!(
            parsed,
            Shape {
                text: "structured".into()
            }
        );
    }

    /// `LlmClient::supports(&model)` returns `model.schema() ==
    /// self.schema()` for every variant pair — the default impl's
    /// promise, exercised across all `SchemaKind` cases via stub
    /// Clients whose `schema()` is fixed.
    #[test]
    fn supports_matches_schema_equality() {
        struct Stub(SchemaKind);
        impl LlmClient for Stub {
            fn schema(&self) -> SchemaKind {
                self.0
            }
            fn complete<'a>(
                &'a self,
                _req: CompletionRequest,
            ) -> BoxFuture<'a, Result<CompletionResponse, LlmError>> {
                Box::pin(async move {
                    Err(LlmError::Provider {
                        message: "stub".into(),
                    })
                })
            }
            fn complete_structured_raw<'a>(
                &'a self,
                _req: CompletionRequest,
                _schema: serde_json::Value,
                _type_name: String,
            ) -> BoxFuture<'a, Result<String, LlmError>> {
                Box::pin(async move {
                    Err(LlmError::Provider {
                        message: "stub".into(),
                    })
                })
            }
        }

        let cases: &[(SchemaKind, ModelId, bool)] = &[
            (
                SchemaKind::Anthropic,
                ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
                true,
            ),
            (
                SchemaKind::Anthropic,
                ModelId::OpenAi(crate::model_id::OpenAiModel::Gpt55),
                false,
            ),
            (
                SchemaKind::OpenAi,
                ModelId::OpenAi(crate::model_id::OpenAiModel::Gpt55),
                true,
            ),
            (
                SchemaKind::OpenAi,
                ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
                false,
            ),
            (
                SchemaKind::Gemini,
                ModelId::Gemini(crate::model_id::GeminiModel::Gemini31Pro),
                true,
            ),
            (
                SchemaKind::Gemini,
                ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
                false,
            ),
        ];
        for (client_schema, model, want) in cases {
            let stub = Stub(*client_schema);
            assert_eq!(
                stub.supports(model),
                *want,
                "client_schema={client_schema:?} model={model:?}",
            );
        }
    }

    #[test]
    fn structured_payload_distinguishes_malformed_json_from_schema_violation() {
        #[derive(Debug, PartialEq, serde::Deserialize)]
        struct Shape {
            count: u32,
        }

        let parsed = parse_structured_payload::<Shape>(r#"{"count":7}"#)
            .expect("valid JSON and valid schema parses");
        assert_eq!(parsed, Shape { count: 7 });

        match parse_structured_payload::<Shape>("not json") {
            Err(LlmError::MalformedJson(_)) => {}
            other => panic!("expected MalformedJson, got {other:?}"),
        }

        match parse_structured_payload::<Shape>(r#"{"count":"seven"}"#) {
            Err(LlmError::SchemaViolation(_)) => {}
            other => panic!("expected SchemaViolation, got {other:?}"),
        }
    }

    /// Every variant of [`LlmError`] reports the [`RetryAdvice`] the
    /// classification table in `specs/llm.md` § LlmError pins. The
    /// const list is exhaustive — adding a variant trips the
    /// `[non_exhaustive]` compile error on the const slice and forces
    /// the table to grow with the enum.
    #[test]
    fn llm_error_retry_advice_includes_multimodal_client_errors() {
        let retry_after = Duration::from_secs(42);
        let cases: [(LlmError, RetryAdvice); 12] = [
            (LlmError::Transport("dns".into()), RetryAdvice::Retryable),
            (LlmError::Timeout, RetryAdvice::Retryable),
            (
                LlmError::RateLimited { retry_after },
                RetryAdvice::RetryAfter(retry_after),
            ),
            (
                LlmError::AuthFailed {
                    reason: "bad key".into(),
                },
                RetryAdvice::NonRetryable,
            ),
            (
                LlmError::ProviderHttp {
                    status: 502,
                    body: "bad gateway".into(),
                },
                RetryAdvice::Retryable,
            ),
            (
                LlmError::ProviderHttp {
                    status: 404,
                    body: "missing".into(),
                },
                RetryAdvice::NonRetryable,
            ),
            (
                LlmError::MalformedJson("expected {".into()),
                RetryAdvice::Retryable,
            ),
            (
                LlmError::SchemaViolation("missing field".into()),
                RetryAdvice::Retryable,
            ),
            (
                LlmError::IncompatibleModel {
                    model: ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
                    expected: SchemaKind::OpenAi,
                },
                RetryAdvice::NonRetryable,
            ),
            (
                LlmError::UnsupportedCapability {
                    provider: SchemaKind::OpenAi,
                    capability: LlmCapability::MultimodalBinary {
                        mime_type: crate::request::MimeType::APPLICATION_PDF,
                    },
                },
                RetryAdvice::NonRetryable,
            ),
            (
                LlmError::IncompatibleRequest {
                    reason: "empty binary payload".into(),
                },
                RetryAdvice::NonRetryable,
            ),
            (
                LlmError::Provider {
                    message: "unknown".into(),
                },
                RetryAdvice::NonRetryable,
            ),
        ];
        for (err, want) in cases {
            assert_eq!(
                err.retry_advice(),
                want,
                "classification for {err:?} must match the spec table",
            );
        }
    }

    /// The seconds-since-now integer form returns the exact carried
    /// `Duration`; the IMF-fixdate form returns the gap between the
    /// supplied `now` and the parsed instant; a missing / unparseable
    /// header falls back to [`DEFAULT_RETRY_AFTER`].
    #[test]
    fn rate_limited_parses_retry_after_header() {
        let anchor = SystemTime::UNIX_EPOCH + Duration::from_secs(785_330_400);
        assert_eq!(parse_retry_after("30", anchor), Duration::from_secs(30));
        assert_eq!(
            parse_retry_after("  120 ", anchor),
            Duration::from_secs(120),
        );

        let later = anchor + Duration::from_secs(90);
        let later_header = format_imf_fixdate(later);
        assert_eq!(
            parse_retry_after(&later_header, anchor),
            Duration::from_secs(90),
        );

        let earlier = anchor - Duration::from_secs(30);
        let earlier_header = format_imf_fixdate(earlier);
        assert_eq!(parse_retry_after(&earlier_header, anchor), Duration::ZERO);

        assert_eq!(parse_retry_after("", anchor), DEFAULT_RETRY_AFTER);
        assert_eq!(parse_retry_after("not a date", anchor), DEFAULT_RETRY_AFTER);
    }

    /// `ProviderHttp` is retryable for `status >= 500` and
    /// non-retryable below — the threshold sits exactly at 500 so
    /// `499` reads non-retryable and `500` reads retryable.
    #[test]
    fn provider_http_retry_advice_threshold_at_500() {
        let body = "stub".to_string();
        for (status, want) in [
            (200_u16, RetryAdvice::NonRetryable),
            (399, RetryAdvice::NonRetryable),
            (400, RetryAdvice::NonRetryable),
            (499, RetryAdvice::NonRetryable),
            (500, RetryAdvice::Retryable),
            (503, RetryAdvice::Retryable),
            (599, RetryAdvice::Retryable),
        ] {
            let err = LlmError::ProviderHttp {
                status,
                body: body.clone(),
            };
            assert_eq!(err.retry_advice(), want, "status {status}");
        }
    }

    /// Format a [`SystemTime`] as an RFC 7231 IMF-fixdate string. Tests
    /// use this to build header values whose parse round-trip is the
    /// invariant under test in [`rate_limited_parses_retry_after_header`].
    fn format_imf_fixdate(t: SystemTime) -> String {
        let secs = t
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("test anchor after epoch")
            .as_secs() as i64;
        let days = secs.div_euclid(86_400);
        let tod = secs.rem_euclid(86_400);
        let hour = tod / 3_600;
        let min = (tod % 3_600) / 60;
        let sec = tod % 60;
        let (year, month, day) = civil_from_days(days);
        let day_name = day_name_from_days(days);
        let month_name = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ][(month - 1) as usize];
        format!("{day_name}, {day:02} {month_name} {year:04} {hour:02}:{min:02}:{sec:02} GMT",)
    }

    fn day_name_from_days(days: i64) -> &'static str {
        let wd = ((days + 4).rem_euclid(7)) as usize;
        ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][wd]
    }

    fn civil_from_days(z: i64) -> (i64, u32, u32) {
        let z = z + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
        let y = if m <= 2 { y + 1 } else { y };
        (y, m, d)
    }
}
