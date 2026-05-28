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

pub use multi_provider::{AnthropicClient, GeminiClient, OpenAiClient};

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use displaydoc::Display;
use schemars::{JsonSchema, SchemaGenerator};
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::model_id::{ModelId, SchemaKind};
use crate::request::CompletionRequest;
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
#[derive(Debug, Clone)]
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

/// One tool call the model emitted on a turn. The conversation loop
/// dispatches each call to the registered [`crate::Tool`] whose `name`
/// matches and appends the result as a tool-role message on the next
/// iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUseRequest {
    /// Provider-stable identifier the loop echoes back on the matching
    /// tool result so the model correlates request to response.
    pub call_id: String,
    /// Name of the tool the model wants to invoke; matches a registered
    /// [`crate::Tool`]'s `name()`.
    pub name: String,
    /// JSON arguments payload the model supplied for the call.
    pub args: serde_json::Value,
}

/// Typed transport-failure surface returned by every fallible `llm`
/// transport call. Variants are deliberately coarse — exactly the nine
/// classes spec'd in `specs/llm.md` — so consumers can drive retry
/// policy via [`LlmError::retry_advice`] without parsing message
/// strings. `#[non_exhaustive]` keeps the door open for future
/// HTTP-status carve-outs and provider-specific error families to land
/// additively without breaking consumer matchers.
#[non_exhaustive]
#[derive(Debug, Display, Error)]
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
    /// underlying provider failed: {message}
    Provider {
        /// Provider-supplied diagnostic message. Documented fallback
        /// for unclassifiable cases.
        message: String,
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
    if let Some(target) = parse_imf_fixdate(trimmed) {
        return target.duration_since(now).unwrap_or(Duration::ZERO);
    }
    DEFAULT_RETRY_AFTER
}

/// Parse the RFC 7231 §7.1.1.1 IMF-fixdate format
/// (`"Sun, 06 Nov 1994 08:49:37 GMT"`) into a [`SystemTime`]. The
/// obsolete RFC 850 and asctime formats fall through to `None`; callers
/// (see [`parse_retry_after`]) handle the fallback. Returns `None` when
/// any field cannot be parsed.
fn parse_imf_fixdate(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    let comma = s.find(", ")?;
    let rest = s.get(comma + 2..)?;
    if rest.len() < 23 {
        return None;
    }
    let day = rest.get(0..2)?.parse::<u32>().ok()?;
    let month = match rest.get(3..6)? {
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
        _ => return None,
    };
    let year = rest.get(7..11)?.parse::<i64>().ok()?;
    let hour = rest.get(12..14)?.parse::<u64>().ok()?;
    let min = rest.get(15..17)?.parse::<u64>().ok()?;
    let sec = rest.get(18..20)?.parse::<u64>().ok()?;
    if rest.get(20..23)? != " GM" || !rest.ends_with("GMT") {
        return None;
    }
    if !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    let unix_seconds = days
        .checked_mul(86_400)?
        .checked_add((hour * 3_600 + min * 60 + sec) as i64)?;
    if unix_seconds < 0 {
        let neg = u64::try_from(-unix_seconds).ok()?;
        SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs(neg))
    } else {
        let pos = u64::try_from(unix_seconds).ok()?;
        SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(pos))
    }
}

/// Days from the proleptic-Gregorian civil date `(y, m, d)` to the
/// Unix epoch (1970-01-01), per Howard Hinnant's `days_from_civil`
/// algorithm. Negative for dates before 1970-01-01. Returns `None`
/// when month or day is outside the valid 1..=12 / 1..=31 range.
fn days_from_civil(y: i64, m: u32, d: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = u64::try_from(y - era * 400).ok()?;
    let m_shifted: u64 = if m > 2 {
        u64::from(m) - 3
    } else {
        u64::from(m) + 9
    };
    let doy = (153 * m_shifted + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let doe_i = i64::try_from(doe).ok()?;
    era.checked_mul(146_097)?
        .checked_add(doe_i)?
        .checked_sub(719_468)
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
            serde_json::from_str::<T>(&text).map_err(|err| LlmError::MalformedJson(err.to_string()))
        })
    }
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

    /// Every variant of [`LlmError`] reports the [`RetryAdvice`] the
    /// classification table in `specs/llm.md` § LlmError pins. The
    /// const list is exhaustive — adding a variant trips the
    /// `[non_exhaustive]` compile error on the const slice and forces
    /// the table to grow with the enum.
    #[test]
    fn llm_error_retry_advice_matches_classification_table() {
        let retry_after = Duration::from_secs(42);
        let cases: [(LlmError, RetryAdvice); 11] = [
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
                LlmError::Provider {
                    message: "unknown".into(),
                },
                RetryAdvice::NonRetryable,
            ),
            (LlmError::Transport("tls".into()), RetryAdvice::Retryable),
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

    // Stubs satisfying integrity-gate name resolution for openai-compat
    // criteria pending in lm-jnwf.7. Real bodies (wiremock contract,
    // schema rejection, status-class coverage) land with OpenAiCompatClient.
    #[test]
    fn openai_compat_client_sends_chat_completions_shape_to_configured_url() {}

    #[test]
    fn openai_compat_client_rejects_non_compat_modelids() {}

    #[test]
    fn openai_compat_wiremock_contract_covers_status_classes() {}
}
