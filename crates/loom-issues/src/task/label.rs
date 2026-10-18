use serde::{Deserialize, Deserializer, Serialize};

/// An opaque nonempty label token, without workflow interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Label(String);

impl Label {
    /// # Errors
    /// Rejects empty labels, whitespace, delimiters and control bytes.
    pub fn new(value: impl AsRef<str>) -> Result<Self, ParseLabelError> {
        let value = value.as_ref();
        if value.is_empty()
            || value
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control() || ch == ',')
        {
            return Err(ParseLabelError(value.into()));
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for Label {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
#[derive(Debug, displaydoc::Display, thiserror::Error, PartialEq, Eq)]
/// invalid label token `{0}`
pub struct ParseLabelError(pub String);

#[cfg(test)]
mod tests {
    use super::Label;
    #[test]
    fn labels_are_checked_opaque_tokens() {
        for label in ["spec:", "urgent", "custom:Label"] {
            assert_eq!(Label::new(label).unwrap().as_str(), label);
        }
        for label in ["", "a,b", "two labels", "line\n"] {
            assert!(Label::new(label).is_err());
        }
    }
}
