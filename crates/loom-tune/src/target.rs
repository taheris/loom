use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use displaydoc::Display;
use loom_skills::identity::{PhaseName, SkillName};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Concrete tuning target selector.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Target {
    Skill { name: SkillName },
    Phase { name: PhaseName },
    Partial { name: PartialName },
}

impl Target {
    pub fn kind(&self) -> Kind {
        match self {
            Self::Skill { .. } => Kind::Skill,
            Self::Phase { .. } => Kind::Phase,
            Self::Partial { .. } => Kind::Partial,
        }
    }

    pub fn intersects_any<'a>(&self, targets: impl IntoIterator<Item = &'a Self>) -> bool {
        targets.into_iter().any(|target| target == self)
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Skill { name } => write!(f, "skill:{name}"),
            Self::Phase { name } => write!(f, "phase:{name}"),
            Self::Partial { name } => write!(f, "partial:{name}"),
        }
    }
}

impl FromStr for Target {
    type Err = ParseTargetError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "all" {
            return Err(ParseTargetError::Wildcard {
                value: value.into(),
            });
        }
        let (prefix, name) =
            value
                .split_once(':')
                .ok_or_else(|| ParseTargetError::MissingPrefix {
                    value: value.to_owned(),
                })?;
        if name.is_empty() || name == "*" {
            return Err(ParseTargetError::Wildcard {
                value: value.to_owned(),
            });
        }
        match prefix {
            "skill" => Ok(Self::Skill {
                name: SkillName::new(name).map_err(|source| ParseTargetError::InvalidSkill {
                    value: value.to_owned(),
                    source,
                })?,
            }),
            "phase" => Ok(Self::Phase {
                name: PhaseName::new(name).map_err(|source| ParseTargetError::InvalidPhase {
                    value: value.to_owned(),
                    source,
                })?,
            }),
            "partial" => Ok(Self::Partial {
                name: PartialName::new(name).map_err(|source| {
                    ParseTargetError::InvalidPartial {
                        value: value.to_owned(),
                        source,
                    }
                })?,
            }),
            _ => Err(ParseTargetError::UnknownPrefix {
                value: value.to_owned(),
            }),
        }
    }
}

impl Serialize for Target {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Target {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Tune target kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Skill,
    Phase,
    Partial,
}

/// Workflow partial identity used by tune selectors.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PartialName(String);

impl PartialName {
    pub fn new(value: impl Into<String>) -> Result<Self, ParsePartialNameError> {
        value.into().parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PartialName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PartialName {
    type Err = ParsePartialNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty()
            || value.starts_with(['.', '-', '_'])
            || value.ends_with(['.', '-', '_'])
            || value.contains("..")
            || value.contains('*')
            || value.contains('/')
            || value.contains('\\')
            || value.contains(':')
            || !value.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
            })
        {
            return Err(ParsePartialNameError::Invalid {
                value: value.to_owned(),
            });
        }
        Ok(Self(value.to_owned()))
    }
}

impl<'de> Deserialize<'de> for PartialName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Partial name parse failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum ParsePartialNameError {
    /// invalid partial name `{value}`: expected lowercase partial identity
    Invalid { value: String },
}

/// Target selector parse failures.
#[derive(Debug, Error, Display)]
pub enum ParseTargetError {
    /// tune target `{value}` must use `skill:`, `phase:`, or `partial:`
    MissingPrefix { value: String },
    /// tune target `{value}` uses an unknown target prefix
    UnknownPrefix { value: String },
    /// tune target `{value}` cannot use wildcards or `all`
    Wildcard { value: String },
    /// tune target `{value}` has an invalid skill name
    InvalidSkill {
        value: String,
        #[source]
        source: loom_skills::identity::ParseSkillNameError,
    },
    /// tune target `{value}` has an invalid phase name
    InvalidPhase {
        value: String,
        #[source]
        source: loom_skills::identity::ParsePhaseNameError,
    },
    /// tune target `{value}` has an invalid partial name
    InvalidPartial {
        value: String,
        #[source]
        source: ParsePartialNameError,
    },
}

/// Known tune target catalog.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Catalog {
    known: BTreeSet<Target>,
}

impl Catalog {
    pub fn new(targets: impl IntoIterator<Item = Target>) -> Self {
        Self {
            known: targets.into_iter().collect(),
        }
    }

    pub fn contains(&self, target: &Target) -> bool {
        self.known.contains(target)
    }

    pub fn require_known(&self, target: &Target) -> Result<(), TargetCatalogError> {
        if self.contains(target) {
            Ok(())
        } else {
            Err(TargetCatalogError::Unknown {
                target: target.clone(),
            })
        }
    }

    pub fn targets(&self) -> impl Iterator<Item = &Target> {
        self.known.iter()
    }
}

/// Target catalog validation failures.
#[derive(Debug, Clone, PartialEq, Eq, Display, Error)]
pub enum TargetCatalogError {
    /// tune target `{target}` is not known
    Unknown { target: Target },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_parses_skill_phase_and_partial_selectors() {
        assert_eq!(
            "skill:loom-context-before-edit"
                .parse::<Target>()
                .expect("skill target")
                .to_string(),
            "skill:loom-context-before-edit"
        );
        assert_eq!(
            "phase:gate.review"
                .parse::<Target>()
                .expect("phase target")
                .to_string(),
            "phase:gate.review"
        );
        assert_eq!(
            "partial:style_rules.md"
                .parse::<Target>()
                .expect("partial target")
                .to_string(),
            "partial:style_rules.md"
        );
    }

    #[test]
    fn target_rejects_wildcards_and_unknown_prefixes() {
        for input in ["all", "skill:*", "phase:", "template:loop", "partial:../x"] {
            assert!(input.parse::<Target>().is_err(), "{input}");
        }
    }
}
