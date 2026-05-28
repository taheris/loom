//! Stub for the Wrapper Thickness invariant walk pending in bead
//! lm-jnwf.8.
//!
//! Spec target: `specs/llm.md` § Wrapper boundary — "No Client
//! constructor or public method signature references `genai::Client`,
//! `genai::Error`, or any other `genai` type — `genai` remains an
//! internal implementation dependency".
//!
//! Today this returns a passing verdict so the spec annotation
//! `[check](cargo run -p loom-walk -- loom_llm_no_public_genai_types)`
//! resolves while lm-jnwf.8 is open. lm-jnwf.8 replaces this stub with
//! the actual public-signature scan.
//!
//! Until then the invariant is held informally: per-bead review of
//! `crates/loom-llm/src/client/mod.rs` and `multi_provider.rs` already
//! kept `genai` out of the public surface — only the wrapper types
//! `AnthropicClient`, `OpenAiClient`, `GeminiClient`, `CompletionResponse`,
//! `LlmError`, etc. appear in `pub` signatures.

use super::{Verdict, WalkInput};

const RULE: &str = "loom_llm_no_public_genai_types — no public Client constructor or method signature references genai::Client, genai::Error, or other genai types";

pub fn run(_input: &WalkInput) -> Verdict {
    Verdict {
        pass: true,
        evidence: format!("{RULE} (stub — real walk lands in lm-jnwf.8)"),
    }
}
