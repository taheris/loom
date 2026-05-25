//! `CacheControl` — typed per-content-block prompt-cache markers. The
//! TTL set matches Anthropic's prompt-cache breakpoint API; providers
//! that do not support typed per-block cache markers no-op the marker
//! without error.

/// Cache-control marker attached per content block. `None` is the
/// default; `Ephemeral(CacheTtl)` requests a cache breakpoint with the
/// named lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheControl {
    /// No cache marker on this block.
    #[default]
    None,
    /// Request an ephemeral cache entry with the given lifetime. The
    /// underlying provider treats the boundary as a breakpoint; cache
    /// reads on subsequent matching prefixes return cached bytes.
    Ephemeral(CacheTtl),
}

/// Supported cache lifetimes — matches Anthropic's documented set.
/// Other providers no-op a value they cannot honour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheTtl {
    Minutes5,
    Hours1,
    Hours24,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec contract: `CacheTtl` enumerates exactly the Anthropic-supported
    /// prompt-cache breakpoint lifetimes — 5 minutes, 1 hour, 24 hours —
    /// and no others. Adding or removing a variant must be a deliberate
    /// API surface change.
    ///
    /// The exhaustive `match` and the canonical list both have to be
    /// updated in lock-step with the enum: an added variant trips the
    /// match's exhaustiveness check; a renamed or removed variant fails
    /// to compile in the list. The runtime assertion then pins the
    /// expected breakpoint set against the canonical Anthropic values.
    #[test]
    fn cache_control_ttl_set_matches_anthropic_supported() {
        let all = [CacheTtl::Minutes5, CacheTtl::Hours1, CacheTtl::Hours24];
        let breakpoints: Vec<u32> = all
            .iter()
            .map(|ttl| match ttl {
                CacheTtl::Minutes5 => 5 * 60,
                CacheTtl::Hours1 => 60 * 60,
                CacheTtl::Hours24 => 24 * 60 * 60,
            })
            .collect();
        assert_eq!(breakpoints, vec![300, 3_600, 86_400]);

        let ephemeral = CacheControl::Ephemeral(CacheTtl::Hours1);
        assert!(matches!(
            ephemeral,
            CacheControl::Ephemeral(CacheTtl::Hours1)
        ));
        assert!(matches!(CacheControl::default(), CacheControl::None));
    }
}
