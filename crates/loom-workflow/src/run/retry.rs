/// Retry policy for `loom run` — see `[loop] max_retries` in
/// `specs/harness.md`.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Maximum number of retries after the initial attempt. Default is 2 —
    /// the bead runs at most three times before escalating to clarify.
    pub max_retries: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_retries: 2 }
    }
}

/// What the run loop should do after an agent failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryDecision {
    /// Retry the bead with `previous_failure` injected into the next prompt.
    Retry { previous_failure: String },
    /// Out of retries — apply `loom:clarify` and stop processing this bead.
    GiveUp,
}

impl RetryPolicy {
    /// Decide whether to retry given the number of retries already consumed.
    /// `retries_used` is 0 when the *first* failure occurs (no retries yet).
    /// The function moves the failure body into the [`RetryDecision::Retry`]
    /// variant so the caller can thread it back into the next prompt.
    pub fn decide(&self, retries_used: u32, failure: String) -> RetryDecision {
        if retries_used >= self.max_retries {
            RetryDecision::GiveUp
        } else {
            RetryDecision::Retry {
                previous_failure: failure,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_two_retries() {
        assert_eq!(RetryPolicy::default().max_retries, 2);
    }

    #[test]
    fn retries_when_attempts_remain() {
        let p = RetryPolicy { max_retries: 2 };
        match p.decide(0, "boom".to_string()) {
            RetryDecision::Retry { previous_failure } => assert_eq!(previous_failure, "boom"),
            other => panic!("expected Retry, got {other:?}"),
        }
        match p.decide(1, "boom2".to_string()) {
            RetryDecision::Retry { previous_failure } => assert_eq!(previous_failure, "boom2"),
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn gives_up_after_max_retries() {
        let p = RetryPolicy { max_retries: 2 };
        assert_eq!(p.decide(2, "boom".to_string()), RetryDecision::GiveUp);
        assert_eq!(p.decide(3, "boom".to_string()), RetryDecision::GiveUp);
    }

    #[test]
    fn zero_retries_gives_up_immediately() {
        let p = RetryPolicy { max_retries: 0 };
        assert_eq!(p.decide(0, "boom".to_string()), RetryDecision::GiveUp);
    }
}
