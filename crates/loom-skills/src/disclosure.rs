use serde::{Deserialize, Serialize};

/// User policy for selecting native registration or prompt disclosure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationPolicy {
    #[default]
    Auto,
    Prompt,
}

/// Effective disclosure mode selected for a materialized registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisclosureMode {
    Native,
    Prompt,
}

/// Path visibility policy for the compact skill index.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathDisplay {
    #[default]
    Needed,
    Always,
}

/// Backend native-skill capability as declared by a tested registrar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeRegistration {
    Supported,
    Unsupported,
}

impl RegistrationPolicy {
    pub fn disclosure_mode(self, native: NativeRegistration) -> DisclosureMode {
        match (self, native) {
            (Self::Auto, NativeRegistration::Supported) => DisclosureMode::Native,
            (Self::Auto | Self::Prompt, NativeRegistration::Unsupported)
            | (Self::Prompt, NativeRegistration::Supported) => DisclosureMode::Prompt,
        }
    }
}
