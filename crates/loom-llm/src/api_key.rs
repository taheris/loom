//! `ApiKey` — newtype wrapping a non-empty credential string. Constructed
//! at the configuration boundary so invalid input fails at parse time
//! rather than at first request.

use displaydoc::Display;
use thiserror::Error;

/// Non-empty API credential. Constructed via [`ApiKey::new`]; the
/// constructor rejects empty input so downstream calls cannot be issued
/// with an unset credential. Per RS-7 / RS-8 the type does not derive
/// `From` / `Into`; consumers parse-then-pass at the boundary.
#[derive(Clone)]
pub struct ApiKey(String);

/// Failures from [`ApiKey::new`].
#[derive(Debug, Display, Error, PartialEq, Eq)]
pub enum ApiKeyError {
    /// API key was empty
    Empty,
}

impl ApiKey {
    /// Construct an `ApiKey` from a credential string. Empty input
    /// returns [`ApiKeyError::Empty`].
    pub fn new(value: String) -> Result<Self, ApiKeyError> {
        if value.is_empty() {
            Err(ApiKeyError::Empty)
        } else {
            Ok(Self(value))
        }
    }

    /// Borrow the carried credential string. Callers SHOULD NOT log or
    /// emit this value; per RS-15 secret-bearing values are wrapped in
    /// a `Redacted` for display.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ApiKey").field(&"[REDACTED]").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ApiKey::new` rejects empty input with [`ApiKeyError::Empty`] so
    /// downstream call sites never issue a request with an unset
    /// credential. Non-empty input succeeds.
    #[test]
    fn api_key_newtype_rejects_empty() {
        assert_eq!(ApiKey::new(String::new()).err(), Some(ApiKeyError::Empty));
        let ok = ApiKey::new("sk-test".to_string()).expect("non-empty key accepted");
        assert_eq!(ok.expose(), "sk-test");
    }

    /// `Debug` formatting must not leak the credential.
    #[test]
    fn api_key_debug_redacts_value() {
        let key = ApiKey::new("super-secret".to_string()).expect("non-empty");
        let rendered = format!("{key:?}");
        assert!(!rendered.contains("super-secret"), "redacted: {rendered}");
        assert!(
            rendered.contains("REDACTED"),
            "redaction marker: {rendered}"
        );
    }
}
