use std::fmt;

/// Wrapper that hides its inner value from debug/display output.
///
/// Spec NF-10 forbids logging environment variable values and API keys. Any
/// field that may carry a secret is constructed via `Redacted(value)` and the
/// `Debug` and `Display` impls render `[REDACTED]` instead of the underlying
/// bytes. Variable *names* may still be logged through tracing fields.
///
/// ```
/// use loom_render::Redacted;
///
/// let token = Redacted("super-secret-api-key");
/// assert_eq!(format!("{token:?}"), "[REDACTED]");
/// assert_eq!(format!("{token}"), "[REDACTED]");
/// ```
pub struct Redacted<T>(pub T);

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl<T> Redacted<T> {
    /// Borrow the underlying value. Callers that need the secret (to pass to
    /// a subprocess, for example) call this explicitly — there is no implicit
    /// `Deref` so leaking via formatting is impossible.
    pub fn reveal(&self) -> &T {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::Redacted;

    #[test]
    fn debug_prints_redacted_marker() {
        let r = Redacted("hunter2");
        assert_eq!(format!("{r:?}"), "[REDACTED]");
    }

    #[test]
    fn display_prints_redacted_marker() {
        let r = Redacted(String::from("hunter2"));
        assert_eq!(format!("{r}"), "[REDACTED]");
    }

    #[test]
    fn reveal_exposes_inner_when_explicitly_requested() {
        let r = Redacted("the-secret");
        assert_eq!(*r.reveal(), "the-secret");
    }

    #[test]
    fn debug_inside_struct_does_not_leak() {
        #[derive(Debug)]
        #[expect(dead_code)]
        struct Container {
            name: &'static str,
            value: Redacted<&'static str>,
        }
        let c = Container {
            name: "API_KEY",
            value: Redacted("sk-live-abcdefg"),
        };
        let dbg = format!("{c:?}");
        assert!(dbg.contains("API_KEY"));
        assert!(dbg.contains("[REDACTED]"));
        assert!(!dbg.contains("sk-live-abcdefg"));
    }
}
