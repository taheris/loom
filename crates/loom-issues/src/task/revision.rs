/// An opaque backend revision token; its spelling grants no update guarantee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision(String);

impl Revision {
    /// # Errors
    /// Rejects empty tokens and control characters.
    pub fn new(value: &str) -> Result<Self, ParseRevisionError> {
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(ParseRevisionError);
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Debug, displaydoc::Display, thiserror::Error)]
/// invalid revision token
pub struct ParseRevisionError;
