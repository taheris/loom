//! Shared test-support items for loom proptest call sites.
//!
//! Per `specs/tests.md` (Architecture / Property-Based Testing), the
//! CI cap on `proptest` case counts is a single named constant — one place
//! to bump, one place to grep — instead of a `with_cases(32)` literal
//! scattered across every `proptest!` block.

/// Number of `proptest` cases each property runs under `nix flake check`.
///
/// The cap exists because property tests run on every PR via `loom gate
/// verify`, and the per-property time budget is tight (Non-Functional #2
/// in `specs/tests.md` targets <30 s aggregate across `loom gate
/// test`). 32 cases keeps the wall-clock cheap while still exercising the
/// shrinker on every regression.
///
/// Local exhaustive runs override via the `PROPTEST_CASES` environment
/// variable, which `proptest` consults before falling back to the value
/// passed to `ProptestConfig::with_cases`. Setting `PROPTEST_CASES=2048`
/// (or higher) in the shell that invokes `cargo nextest run` raises the
/// case count for that invocation without touching the source. The env
/// var is therefore the local-loop knob; the constant is the CI floor.
pub const CI_PROPTEST_CASES: u32 = 32;
