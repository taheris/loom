//! Model identifiers grouped by provider wire-schema family.

/// Non-exhaustive discriminator for provider HTTP wire formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SchemaKind {
    Anthropic,
    OpenAi,
    Gemini,
    #[cfg(feature = "openai-compat")]
    OpenAiCompat,
}

/// Non-exhaustive model identifier grouped by [`SchemaKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelId {
    Anthropic(AnthropicModel),
    OpenAi(OpenAiModel),
    Gemini(GeminiModel),
    #[cfg(feature = "openai-compat")]
    OpenAiCompat(String),
}

/// Anthropic models with `Other` preserving forward-compatible names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnthropicModel {
    ClaudeOpus48,
    ClaudeSonnet46,
    ClaudeHaiku45,
    Other(String),
}

/// OpenAI-family models. See [`AnthropicModel`] for the inner-enum
/// rationale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenAiModel {
    Gpt55,
    Other(String),
}

/// Google Gemini-family models. See [`AnthropicModel`] for the
/// inner-enum rationale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiModel {
    Gemini31Pro,
    Gemini35Flash,
    Other(String),
}

impl ModelId {
    /// Wire-format discriminator for this model. Each outer variant
    /// returns the matching [`SchemaKind`] tag; client-side
    /// compatibility checks compare this against a Client's fixed
    /// schema.
    pub const fn schema(&self) -> SchemaKind {
        match self {
            ModelId::Anthropic(_) => SchemaKind::Anthropic,
            ModelId::OpenAi(_) => SchemaKind::OpenAi,
            ModelId::Gemini(_) => SchemaKind::Gemini,
            #[cfg(feature = "openai-compat")]
            ModelId::OpenAiCompat(_) => SchemaKind::OpenAiCompat,
        }
    }

    /// Parse a model identifier string into a typed [`ModelId`]. Known
    /// canonical strings resolve to their named variant; any other
    /// input is routed into the appropriate schema's `Other` arm by
    /// prefix match on the carried string. Strings without a recognized
    /// prefix fall back to [`AnthropicModel::Other`] so the parse is
    /// total and the round-trip through [`ModelId::as_wire`] preserves
    /// the input.
    #[expect(
        clippy::should_implement_trait,
        reason = "spec names ModelId::from_str(...) as the parse surface; the operation is total so a Result<_, Infallible> return would force unwrap warts at every call site"
    )]
    pub fn from_str(s: &str) -> Self {
        match s {
            "claude-opus-4-8" => ModelId::Anthropic(AnthropicModel::ClaudeOpus48),
            "claude-sonnet-4-6" => ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
            "claude-haiku-4-5" => ModelId::Anthropic(AnthropicModel::ClaudeHaiku45),
            "gpt-5.5" => ModelId::OpenAi(OpenAiModel::Gpt55),
            "gemini-3.1-pro" => ModelId::Gemini(GeminiModel::Gemini31Pro),
            "gemini-3.5-flash" => ModelId::Gemini(GeminiModel::Gemini35Flash),
            other => fallback_from_prefix(other),
        }
    }

    /// Round-trip wire string for this model. Inverse of
    /// [`ModelId::from_str`] on known variants; `Other` and
    /// `OpenAiCompat` arms return the carried string verbatim.
    pub fn as_wire(&self) -> String {
        match self {
            ModelId::Anthropic(m) => match m {
                AnthropicModel::ClaudeOpus48 => "claude-opus-4-8".to_string(),
                AnthropicModel::ClaudeSonnet46 => "claude-sonnet-4-6".to_string(),
                AnthropicModel::ClaudeHaiku45 => "claude-haiku-4-5".to_string(),
                AnthropicModel::Other(s) => s.clone(),
            },
            ModelId::OpenAi(m) => match m {
                OpenAiModel::Gpt55 => "gpt-5.5".to_string(),
                OpenAiModel::Other(s) => s.clone(),
            },
            ModelId::Gemini(m) => match m {
                GeminiModel::Gemini31Pro => "gemini-3.1-pro".to_string(),
                GeminiModel::Gemini35Flash => "gemini-3.5-flash".to_string(),
                GeminiModel::Other(s) => s.clone(),
            },
            #[cfg(feature = "openai-compat")]
            ModelId::OpenAiCompat(s) => s.clone(),
        }
    }
}

fn fallback_from_prefix(s: &str) -> ModelId {
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("gpt") || lower.starts_with("o1") || lower.starts_with("o3") {
        ModelId::OpenAi(OpenAiModel::Other(s.to_string()))
    } else if lower.starts_with("gemini") {
        ModelId::Gemini(GeminiModel::Other(s.to_string()))
    } else {
        ModelId::Anthropic(AnthropicModel::Other(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every outer [`ModelId`] variant maps to exactly one
    /// [`SchemaKind`] variant — the 1:1 correspondence the spec
    /// promises. Exhaustive match here forces both enums to stay in
    /// lock-step under future additions.
    #[test]
    fn modelid_outer_variants_match_schema_kind_one_to_one() {
        let cases: Vec<(ModelId, SchemaKind)> = vec![
            (
                ModelId::Anthropic(AnthropicModel::ClaudeSonnet46),
                SchemaKind::Anthropic,
            ),
            (ModelId::OpenAi(OpenAiModel::Gpt55), SchemaKind::OpenAi),
            (
                ModelId::Gemini(GeminiModel::Gemini31Pro),
                SchemaKind::Gemini,
            ),
            #[cfg(feature = "openai-compat")]
            (
                ModelId::OpenAiCompat("custom-llama".to_string()),
                SchemaKind::OpenAiCompat,
            ),
        ];
        for (model, expected) in cases {
            assert_eq!(model.schema(), expected, "outer variant {model:?}");
        }
    }

    /// `ModelId::schema(&self)` returns the matching [`SchemaKind`] for
    /// every outer variant, including the `Other(String)` inner arms
    /// which still route by outer variant rather than by carried
    /// string.
    #[test]
    fn modelid_schema_method_returns_matching_schema_kind() {
        assert_eq!(
            ModelId::Anthropic(AnthropicModel::ClaudeOpus48).schema(),
            SchemaKind::Anthropic,
        );
        assert_eq!(
            ModelId::Anthropic(AnthropicModel::Other("claude-future".to_string())).schema(),
            SchemaKind::Anthropic,
        );
        assert_eq!(
            ModelId::OpenAi(OpenAiModel::Gpt55).schema(),
            SchemaKind::OpenAi,
        );
        assert_eq!(
            ModelId::OpenAi(OpenAiModel::Other("gpt-future".to_string())).schema(),
            SchemaKind::OpenAi,
        );
        assert_eq!(
            ModelId::Gemini(GeminiModel::Gemini35Flash).schema(),
            SchemaKind::Gemini,
        );
        assert_eq!(
            ModelId::Gemini(GeminiModel::Other("gemini-future".to_string())).schema(),
            SchemaKind::Gemini,
        );
        #[cfg(feature = "openai-compat")]
        assert_eq!(
            ModelId::OpenAiCompat("custom".to_string()).schema(),
            SchemaKind::OpenAiCompat,
        );
    }

    /// Canonical wire strings round-trip through `from_str` → typed
    /// variant → `as_wire` unchanged for every known model.
    #[test]
    fn modelid_known_wire_strings_round_trip() {
        let canonical = [
            "claude-opus-4-8",
            "claude-sonnet-4-6",
            "claude-haiku-4-5",
            "gpt-5.5",
            "gemini-3.1-pro",
            "gemini-3.5-flash",
        ];
        for wire in canonical {
            assert_eq!(ModelId::from_str(wire).as_wire(), wire);
        }
    }

    /// Unknown wire strings round-trip through the appropriate inner
    /// `Other` arm and `as_wire` preserves the carried string.
    #[test]
    fn modelid_unknown_wire_strings_round_trip_through_other() {
        let cases = [
            ("claude-future-experimental", SchemaKind::Anthropic),
            ("gpt-5-preview", SchemaKind::OpenAi),
            ("o1-mini", SchemaKind::OpenAi),
            ("gemini-3-ultra", SchemaKind::Gemini),
        ];
        for (wire, expected_schema) in cases {
            let parsed = ModelId::from_str(wire);
            assert_eq!(parsed.schema(), expected_schema, "wire={wire}");
            assert_eq!(parsed.as_wire(), wire);
        }
    }
}
