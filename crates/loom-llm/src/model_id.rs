//! `ModelId` — typed enum of known LLM models with an `Other(String)`
//! forward-compat fallback. Provider routing is inferred from the
//! variant; `Other` routes via prefix match on the carried string.

/// One LLM model that `LlmClient` knows how to route. Adding a known
/// model is a minor version bump; removing or renaming a variant is a
/// major bump (RS-17 closed-set discipline with `Other` for wire-additive
/// growth).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelId {
    // Anthropic — Claude family
    ClaudeOpus47,
    ClaudeSonnet46,
    ClaudeHaiku45,
    // OpenAI — GPT family
    Gpt55,
    // Google — Gemini family
    Gemini31Pro,
    Gemini35Flash,
    /// Forward-compat fallback for fine-tuned / custom / not-yet-known
    /// models. Routing falls back to a provider-prefix match on the
    /// carried string (e.g. `"claude-*"` -> Anthropic, `"gpt-*"` ->
    /// OpenAI).
    Other(String),
}

/// Coarse provider routing inferred from a `ModelId`. Concrete
/// implementations of `LlmClient` consult `provider()` to dispatch each
/// call to the right backend; the routing rules live alongside the
/// `ModelId` enum so adding a model variant is a single-file change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAi,
    Google,
    /// Provider could not be inferred from the carried model string.
    /// Concrete impls return a typed error rather than guessing.
    Unknown,
}

impl ModelId {
    /// Provider this model routes to.
    pub fn provider(&self) -> Provider {
        match self {
            ModelId::ClaudeOpus47 | ModelId::ClaudeSonnet46 | ModelId::ClaudeHaiku45 => {
                Provider::Anthropic
            }
            ModelId::Gpt55 => Provider::OpenAi,
            ModelId::Gemini31Pro | ModelId::Gemini35Flash => Provider::Google,
            ModelId::Other(s) => provider_from_prefix(s),
        }
    }

    /// Parse a model identifier string into a typed [`ModelId`]. Known
    /// model strings resolve to their named variant; any other input
    /// falls through to [`ModelId::Other`] so callers can name
    /// fine-tuned or not-yet-listed models without waiting for a minor
    /// version bump.
    ///
    /// Total: never returns an error — every input string is a valid
    /// `ModelId`. Provider routing for [`ModelId::Other`] uses the same
    /// prefix matching as [`ModelId::provider`].
    #[expect(
        clippy::should_implement_trait,
        reason = "spec names ModelId::from_str(...) as the parse surface; the operation is total so a `Result<_, Infallible>` return would force unwrap warts at every call site"
    )]
    pub fn from_str(s: &str) -> Self {
        match s {
            "claude-opus-4-7" => ModelId::ClaudeOpus47,
            "claude-sonnet-4-6" => ModelId::ClaudeSonnet46,
            "claude-haiku-4-5" => ModelId::ClaudeHaiku45,
            "gpt-5.5" => ModelId::Gpt55,
            "gemini-3.1-pro" => ModelId::Gemini31Pro,
            "gemini-3.5-flash" => ModelId::Gemini35Flash,
            other => ModelId::Other(other.to_string()),
        }
    }

    /// Round-trip wire string for this model. Inverse of
    /// [`ModelId::from_str`]: every known variant returns the canonical
    /// provider model ID; [`ModelId::Other`] returns the carried string
    /// verbatim.
    pub fn as_wire(&self) -> String {
        match self {
            ModelId::ClaudeOpus47 => "claude-opus-4-7".to_string(),
            ModelId::ClaudeSonnet46 => "claude-sonnet-4-6".to_string(),
            ModelId::ClaudeHaiku45 => "claude-haiku-4-5".to_string(),
            ModelId::Gpt55 => "gpt-5.5".to_string(),
            ModelId::Gemini31Pro => "gemini-3.1-pro".to_string(),
            ModelId::Gemini35Flash => "gemini-3.5-flash".to_string(),
            ModelId::Other(s) => s.clone(),
        }
    }
}

fn provider_from_prefix(s: &str) -> Provider {
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("claude") {
        Provider::Anthropic
    } else if lower.starts_with("gpt") || lower.starts_with("o1") || lower.starts_with("o3") {
        Provider::OpenAi
    } else if lower.starts_with("gemini") {
        Provider::Google
    } else {
        Provider::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known variants route to their owning provider.
    #[test]
    fn modelid_known_variants_route_to_expected_provider() {
        let cases = [
            (ModelId::ClaudeOpus47, Provider::Anthropic),
            (ModelId::ClaudeSonnet46, Provider::Anthropic),
            (ModelId::ClaudeHaiku45, Provider::Anthropic),
            (ModelId::Gpt55, Provider::OpenAi),
            (ModelId::Gemini31Pro, Provider::Google),
            (ModelId::Gemini35Flash, Provider::Google),
        ];
        for (model, expected) in cases {
            assert_eq!(model.provider(), expected, "model {model:?}");
        }
    }

    /// `ModelId::from_str` resolves canonical model identifier strings
    /// into their typed variant and falls through to `Other` for any
    /// string outside the known set.
    #[test]
    fn modelid_from_str_known_and_unknown() {
        assert_eq!(ModelId::from_str("claude-opus-4-7"), ModelId::ClaudeOpus47);
        assert_eq!(
            ModelId::from_str("claude-sonnet-4-6"),
            ModelId::ClaudeSonnet46
        );
        assert_eq!(
            ModelId::from_str("claude-haiku-4-5"),
            ModelId::ClaudeHaiku45
        );
        assert_eq!(ModelId::from_str("gpt-5.5"), ModelId::Gpt55);
        assert_eq!(ModelId::from_str("gemini-3.1-pro"), ModelId::Gemini31Pro);
        assert_eq!(
            ModelId::from_str("gemini-3.5-flash"),
            ModelId::Gemini35Flash
        );
        // Unknown strings round-trip through Other and route via prefix.
        match ModelId::from_str("claude-future-experimental") {
            ModelId::Other(s) => assert_eq!(s, "claude-future-experimental"),
            other => panic!("expected Other, got {other:?}"),
        }
        assert_eq!(
            ModelId::from_str("claude-future-experimental").provider(),
            Provider::Anthropic
        );
        assert_eq!(
            ModelId::from_str("llama-3-70b").provider(),
            Provider::Unknown
        );
    }

    /// Spec contract: `ModelId::Other(String)` routes provider via prefix
    /// match on the carried string — `claude-*` -> Anthropic, `gpt-*` /
    /// `o1-*` / `o3-*` -> OpenAi, `gemini-*` -> Google. Strings that
    /// don't match a known prefix fall through to `Provider::Unknown` so
    /// concrete `LlmClient` impls surface a typed error rather than
    /// guessing.
    #[test]
    fn modelid_other_fallback_routes_provider_by_prefix() {
        let cases = [
            ("claude-3-7-sonnet-future", Provider::Anthropic),
            ("Claude-Opus-Custom", Provider::Anthropic),
            ("gpt-5-preview", Provider::OpenAi),
            ("GPT-5", Provider::OpenAi),
            ("o1-mini", Provider::OpenAi),
            ("o3-pro", Provider::OpenAi),
            ("gemini-3-ultra", Provider::Google),
            ("Gemini-Flash-Lite", Provider::Google),
            ("llama-3-70b", Provider::Unknown),
            ("mistral-large", Provider::Unknown),
            ("", Provider::Unknown),
        ];
        for (carried, expected) in cases {
            let routed = ModelId::Other(carried.to_string()).provider();
            assert_eq!(routed, expected, "prefix routing for {carried:?}");
        }
    }
}
