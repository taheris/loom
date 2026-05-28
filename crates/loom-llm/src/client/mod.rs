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

pub use multi_provider::Client;

use std::time::{Duration, SystemTime};

use displaydoc::Display;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::model_id::{ModelId, SchemaKind};
use crate::request::CompletionRequest;
use crate::usage::TokenUsage;

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

/// The public agent-side LLM contract. Per-call model selection;
/// `complete_structured::<T>` hides provider-specific structured-output
/// mechanism behind a single typed method.
pub trait LlmClient: Send + Sync {
    /// Run a completion against the request's `ModelId`. Returns the
    /// final assistant text plus token usage.
    fn complete(
        &self,
        req: CompletionRequest,
    ) -> impl Future<Output = Result<CompletionResponse, LlmError>> + Send;

    /// Run a completion that deserializes into `T`. Internally selects
    /// the right provider mechanism (synthetic forced-tool for
    /// Anthropic, `response_format` for OpenAI, `response_schema` for
    /// Gemini) and returns the parsed value.
    fn complete_structured<T>(
        &self,
        req: CompletionRequest,
    ) -> impl Future<Output = Result<T, LlmError>> + Send
    where
        T: DeserializeOwned + JsonSchema + Send;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_id::AnthropicModel;

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
}
