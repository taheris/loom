//! `TokenUsage` — per-call token accounting carried on every
//! `CompletionResponse` and fanned out as a `DriverKind::TokenUsage`
//! `AgentEvent` for SaaS billing pipelines.

use crate::model_id::{AnthropicModel, GeminiModel, ModelId, OpenAiModel};

/// Token accounting for one `complete*` call. Cache fields surface
/// prompt-cache reads and writes separately so consumers can make
/// cost-aware decisions without re-parsing provider responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenUsage {
    /// Input tokens (prompt) consumed by the call.
    pub input: u32,
    /// Output tokens (completion) produced by the call.
    pub output: u32,
    /// Tokens served from prompt cache. Subset of `input`.
    pub cache_read: u32,
    /// Tokens written to prompt cache as part of this call.
    pub cache_write: u32,
    /// Estimated cost in 1/100ths of a US cent (so $0.01 == 100).
    /// Computed from a per-model rates table internal to `llm`.
    /// Zero when the model has no entry in the rates table (e.g.
    /// `ModelId::Other`).
    pub cost_cents: u32,
}

/// Per-million-token pricing for one model, expressed in 1/100ths of a
/// US cent so the same integer unit composes through the cost formula
/// without losing precision on sub-cent fractions.
#[derive(Debug, Clone, Copy)]
struct ModelRates {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

const MILLION: u64 = 1_000_000;

/// Rates for known models. Adding a model is a single-row change; the
/// `Other` inner arms return `None` so unknown models report
/// `cost_cents = 0` rather than guessing.
fn rates_for(model: &ModelId) -> Option<ModelRates> {
    match model {
        ModelId::Anthropic(AnthropicModel::ClaudeOpus47) => Some(ModelRates {
            input: 150_000,
            output: 750_000,
            cache_read: 15_000,
            cache_write: 187_500,
        }),
        ModelId::Anthropic(AnthropicModel::ClaudeSonnet46) => Some(ModelRates {
            input: 30_000,
            output: 150_000,
            cache_read: 3_000,
            cache_write: 37_500,
        }),
        ModelId::Anthropic(AnthropicModel::ClaudeHaiku45) => Some(ModelRates {
            input: 10_000,
            output: 50_000,
            cache_read: 1_000,
            cache_write: 12_500,
        }),
        ModelId::OpenAi(OpenAiModel::Gpt55) => Some(ModelRates {
            input: 50_000,
            output: 300_000,
            cache_read: 0,
            cache_write: 0,
        }),
        ModelId::Gemini(GeminiModel::Gemini31Pro) => Some(ModelRates {
            input: 12_500,
            output: 100_000,
            cache_read: 0,
            cache_write: 0,
        }),
        ModelId::Gemini(GeminiModel::Gemini35Flash) => Some(ModelRates {
            input: 15_000,
            output: 90_000,
            cache_read: 1_500,
            cache_write: 0,
        }),
        ModelId::Anthropic(AnthropicModel::Other(_))
        | ModelId::OpenAi(OpenAiModel::Other(_))
        | ModelId::Gemini(GeminiModel::Other(_)) => None,
        #[cfg(feature = "openai-compat")]
        ModelId::OpenAiCompat(_) => None,
    }
}

/// Compute `cost_cents` (in 1/100ths of a US cent) for `model` given
/// raw token counts. Cache reads and writes are billed at their own
/// rate; the regular-input rate applies only to the portion of `input`
/// that is neither cache-read nor cache-write so providers that report
/// `input` as the inclusive total don't double-bill the cached lanes.
pub(crate) fn cost_cents_for(
    model: &ModelId,
    input: u32,
    output: u32,
    cache_read: u32,
    cache_write: u32,
) -> u32 {
    let Some(rates) = rates_for(model) else {
        return 0;
    };
    let regular_input = u64::from(input)
        .saturating_sub(u64::from(cache_read))
        .saturating_sub(u64::from(cache_write));
    let total = regular_input.saturating_mul(rates.input)
        + u64::from(output).saturating_mul(rates.output)
        + u64::from(cache_read).saturating_mul(rates.cache_read)
        + u64::from(cache_write).saturating_mul(rates.cache_write);
    u32::try_from(total / MILLION).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known models produce a non-zero `cost_cents` when any token
    /// counter is non-zero; the rate table is wired up and the formula
    /// reaches the per-model row.
    #[test]
    fn cost_cents_non_zero_for_known_model() {
        let cost = cost_cents_for(
            &ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
            1_000_000,
            0,
            0,
            0,
        );
        assert_eq!(cost, 30_000);
    }

    /// `Other` inner arms have no entry in the rates table, so cost is
    /// zero rather than a guessed value.
    #[test]
    fn cost_cents_zero_for_other_model() {
        let cost = cost_cents_for(
            &ModelId::Anthropic(AnthropicModel::Other("unknown-model-xyz".to_string())),
            1_000_000,
            1_000_000,
            0,
            0,
        );
        assert_eq!(cost, 0);
    }

    /// Cache reads and writes bill at their own per-token rates; the
    /// regular-input rate applies only to the `input - cache_read -
    /// cache_write` remainder so a fully-cached prompt costs the
    /// cache-read rate, not the regular-input rate, even when the
    /// provider reports `input` as the inclusive total.
    #[test]
    fn cost_cents_bills_cache_lanes_at_cache_rates() {
        let cost = cost_cents_for(
            &ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
            1_000_000,
            0,
            1_000_000,
            0,
        );
        assert_eq!(cost, 3_000);
    }

    /// Cache-write tokens bill at the cache-write rate (typically more
    /// expensive than regular input on Anthropic). Same invariant as
    /// the cache-read case: regular-input lane bills only the leftover.
    #[test]
    fn cost_cents_bills_cache_write_at_cache_write_rate() {
        let cost = cost_cents_for(
            &ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
            1_000_000,
            0,
            0,
            1_000_000,
        );
        assert_eq!(cost, 37_500);
    }
}
