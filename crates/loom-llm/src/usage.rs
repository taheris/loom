//! `TokenUsage` — per-call token accounting carried on every
//! `CompletionResponse` and fanned out as a `DriverKind::TokenUsage`
//! `AgentEvent` for SaaS billing pipelines.

/// Token accounting for one `complete*` call. Cache fields surface
/// prompt-cache reads and writes separately so consumers can make
/// cost-aware decisions without re-parsing provider responses.
///
/// Pricing is contract-variable (regional rates, enterprise contracts,
/// customer-hosted models) and lives in consumer-owned `ModelId → cost`
/// mappings rather than this crate.
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
}
