use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LoopConfig {
    /// Molecule-level: bounds `loom loop`'s outer loop on fix-up beads. Each
    /// full molecule pass — initial pass plus every verdict-gate-produced
    /// fix-up pass — consumes one slot. Recorded as
    /// `molecules.iteration_count` in the state DB and surfaced in
    /// `previous_failure` context on each retry. See
    /// `specs/harness.md` § Configuration.
    pub max_iterations: u32,
    /// In-session: bounds the per-bead retry-with-`previous_failure`
    /// budget inside one `process_one_bead` call. Independent of
    /// `max_iterations`; the two counters never share slots.
    pub max_retries: u32,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_iterations: 10,
            max_retries: 2,
        }
    }
}
