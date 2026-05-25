/// Default cap for the `run ↔ check` auto-iteration loop, mirroring
/// `[loop] max_iterations = 3` in `specs/harness.md`.
pub const DEFAULT_MAX_ITERATIONS: u32 = 3;

/// Bound on the run/check auto-iteration loop. After `max` unsuccessful
/// reviews (each creating fix-up beads without a clarify), `loom review`
/// escalates the newest fix-up bead to `loom:clarify` instead of looping.
#[derive(Debug, Clone, Copy)]
pub struct IterationCap {
    pub max: u32,
}

impl Default for IterationCap {
    fn default() -> Self {
        Self {
            max: DEFAULT_MAX_ITERATIONS,
        }
    }
}

impl IterationCap {
    pub fn new(max: u32) -> Self {
        Self { max }
    }

    /// `true` when `current` has already consumed every retry the cap allows.
    pub fn is_exhausted(&self, current: u32) -> bool {
        current >= self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cap_matches_spec() {
        assert_eq!(IterationCap::default().max, 3);
    }

    #[test]
    fn is_exhausted_true_at_or_above_cap() {
        let c = IterationCap::new(3);
        assert!(!c.is_exhausted(0));
        assert!(!c.is_exhausted(2));
        assert!(c.is_exhausted(3));
        assert!(c.is_exhausted(4));
    }

    #[test]
    fn zero_cap_exhausts_immediately() {
        assert!(IterationCap::new(0).is_exhausted(0));
    }
}
