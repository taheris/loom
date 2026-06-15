//! Typed wire-format contract for `loom todo` success markers.
//!
//! [`parse_todo_success`] accepts the final `LOOM_TODO: <json>` line
//! and returns a [`TodoSuccess`] whose identifiers and success payloads
//! have been parsed into constrained domain types.

use std::fmt;
use std::ops::Deref;
use std::str::FromStr;

use displaydoc::Display;
use loom_events::identifier::{BeadId, ParseBeadIdError, SpecLabel};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::oid::{GitOid, ParseGitOidError};

pub const TODO_SUCCESS_PREFIX: &str = "LOOM_TODO: ";

pub type GitSha = GitOid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct TodoFingerprint(String);

impl TodoFingerprint {
    pub fn new(s: &str) -> Result<Self, ParseTodoFingerprintError> {
        if s.len() != 64 {
            return Err(ParseTodoFingerprintError(s.to_owned()));
        }
        if !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ParseTodoFingerprintError(s.to_owned()));
        }
        Ok(Self(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TodoFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for TodoFingerprint {
    type Err = ParseTodoFingerprintError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl<'de> Deserialize<'de> for TodoFingerprint {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Display, Error, PartialEq, Eq)]
/// invalid todo fingerprint `{0}`: expected 64 lowercase hex characters
pub struct ParseTodoFingerprintError(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NonEmptyString(String);

impl NonEmptyString {
    pub fn new(s: impl Into<String>) -> Result<Self, ParseNonEmptyStringError> {
        let s = s.into();
        if s.trim().is_empty() {
            return Err(ParseNonEmptyStringError);
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NonEmptyString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for NonEmptyString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NonEmptyString {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Display, Error, PartialEq, Eq)]
/// expected a non-empty string
pub struct ParseNonEmptyStringError;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NonEmptyVec<T>(Vec<T>);

impl<T> NonEmptyVec<T> {
    pub fn new(values: Vec<T>) -> Result<Self, ParseNonEmptyVecError> {
        if values.is_empty() {
            return Err(ParseNonEmptyVecError);
        }
        Ok(Self(values))
    }

    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    pub fn into_vec(self) -> Vec<T> {
        self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }
}

impl<T> Deref for NonEmptyVec<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: Serialize> Serialize for NonEmptyVec<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, T> Deserialize<'de> for NonEmptyVec<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let values = Vec::<T>::deserialize(d)?;
        Self::new(values).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Display, Error, PartialEq, Eq)]
/// expected a non-empty list
pub struct ParseNonEmptyVecError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TodoSuccess {
    pub head: GitSha,
    pub fingerprint: TodoFingerprint,
    pub work_epic: BeadId,
    pub specs: NonEmptyVec<TodoSpecSuccess>,
}

impl<'de> Deserialize<'de> for TodoSuccess {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        RawTodoSuccess::deserialize(d)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TodoSpecSuccess {
    pub label: SpecLabel,
    #[serde(flatten)]
    pub outcome: TodoSpecOutcome,
}

impl<'de> Deserialize<'de> for TodoSpecSuccess {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        RawTodoSpecSuccess::deserialize(d)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum TodoSpecOutcome {
    Decomposed { beads: NonEmptyVec<BeadId> },
    NoWork { reason: NonEmptyString },
}

impl<'de> Deserialize<'de> for TodoSpecOutcome {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        RawTodoSpecOutcome::deserialize(d)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

pub fn parse_todo_success(final_line: &str) -> Result<TodoSuccess, ParseTodoSuccessError> {
    let payload = todo_payload(final_line)?;
    let raw: RawTodoSuccess = serde_json::from_str(payload)
        .map_err(|source| ParseTodoSuccessError::InvalidJson { source })?;
    raw.try_into()
}

pub fn parse_success_marker(final_line: &str) -> Result<TodoSuccess, ParseTodoSuccessError> {
    parse_todo_success(final_line)
}

fn todo_payload(final_line: &str) -> Result<&str, ParseTodoSuccessError> {
    if let Some(payload) = final_line.strip_prefix(TODO_SUCCESS_PREFIX) {
        return Ok(payload);
    }
    if final_line.starts_with("LOOM_TODO") {
        return Err(ParseTodoSuccessError::MalformedPrefix);
    }
    Err(ParseTodoSuccessError::MissingPrefix)
}

impl TryFrom<RawTodoSuccess> for TodoSuccess {
    type Error = ParseTodoSuccessError;

    fn try_from(raw: RawTodoSuccess) -> Result<Self, Self::Error> {
        let head =
            GitSha::new(&raw.head).map_err(|source| ParseTodoSuccessError::InvalidGitSha {
                value: raw.head,
                source,
            })?;
        let fingerprint = TodoFingerprint::new(&raw.fingerprint).map_err(|source| {
            ParseTodoSuccessError::InvalidFingerprint {
                value: raw.fingerprint,
                source,
            }
        })?;
        let work_epic = parse_bead_id(raw.work_epic)?;
        let specs = raw
            .specs
            .into_iter()
            .map(TodoSpecSuccess::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let specs = NonEmptyVec::new(specs).map_err(|_| ParseTodoSuccessError::EmptySpecs)?;
        Ok(Self {
            head,
            fingerprint,
            work_epic,
            specs,
        })
    }
}

impl TryFrom<RawTodoSpecSuccess> for TodoSpecSuccess {
    type Error = ParseTodoSuccessError;

    fn try_from(raw: RawTodoSpecSuccess) -> Result<Self, Self::Error> {
        let label = raw
            .label
            .parse::<SpecLabel>()
            .map_err(|_| ParseTodoSuccessError::InvalidSpecLabel { value: raw.label })?;
        let outcome = match raw.outcome {
            RawTodoSpecOutcome::Decomposed { beads } => {
                if beads.is_empty() {
                    return Err(ParseTodoSuccessError::EmptyBeadList { label });
                }
                let beads = beads
                    .into_iter()
                    .map(parse_bead_id)
                    .collect::<Result<Vec<_>, _>>()?;
                let beads =
                    NonEmptyVec::new(beads).map_err(|_| ParseTodoSuccessError::EmptyBeadList {
                        label: label.clone(),
                    })?;
                TodoSpecOutcome::Decomposed { beads }
            }
            RawTodoSpecOutcome::NoWork { reason } => {
                let reason = NonEmptyString::new(reason).map_err(|_| {
                    ParseTodoSuccessError::EmptyNoWorkReason {
                        label: label.clone(),
                    }
                })?;
                TodoSpecOutcome::NoWork { reason }
            }
        };
        Ok(Self { label, outcome })
    }
}

impl TryFrom<RawTodoSpecOutcome> for TodoSpecOutcome {
    type Error = ParseTodoSuccessError;

    fn try_from(raw: RawTodoSpecOutcome) -> Result<Self, Self::Error> {
        match raw {
            RawTodoSpecOutcome::Decomposed { beads } => {
                if beads.is_empty() {
                    return Err(ParseTodoSuccessError::EmptyOutcomeBeadList);
                }
                let beads = beads
                    .into_iter()
                    .map(parse_bead_id)
                    .collect::<Result<Vec<_>, _>>()?;
                let beads = NonEmptyVec::new(beads)
                    .map_err(|_| ParseTodoSuccessError::EmptyOutcomeBeadList)?;
                Ok(Self::Decomposed { beads })
            }
            RawTodoSpecOutcome::NoWork { reason } => {
                let reason = NonEmptyString::new(reason)
                    .map_err(|_| ParseTodoSuccessError::EmptyOutcomeNoWorkReason)?;
                Ok(Self::NoWork { reason })
            }
        }
    }
}

fn parse_bead_id(value: String) -> Result<BeadId, ParseTodoSuccessError> {
    BeadId::new(&value).map_err(|source| ParseTodoSuccessError::InvalidBeadId { value, source })
}

#[derive(Debug, Display, Error)]
pub enum ParseTodoSuccessError {
    /// missing `LOOM_TODO:` marker prefix
    MissingPrefix,
    /// malformed `LOOM_TODO:` marker prefix; expected `LOOM_TODO: <json>`
    MalformedPrefix,
    /// invalid `LOOM_TODO` JSON payload
    InvalidJson {
        #[source]
        source: serde_json::Error,
    },
    /// invalid todo head SHA
    InvalidGitSha {
        value: String,
        #[source]
        source: ParseGitOidError,
    },
    /// invalid todo fingerprint
    InvalidFingerprint {
        value: String,
        #[source]
        source: ParseTodoFingerprintError,
    },
    /// invalid bead id in todo success payload
    InvalidBeadId {
        value: String,
        #[source]
        source: ParseBeadIdError,
    },
    /// invalid spec label in todo success payload
    InvalidSpecLabel { value: String },
    /// todo success must report at least one spec
    EmptySpecs,
    /// decomposed todo outcome must name at least one bead
    EmptyBeadList { label: SpecLabel },
    /// no-work todo outcome must carry a non-empty reason
    EmptyNoWorkReason { label: SpecLabel },
    /// decomposed todo outcome must name at least one bead
    EmptyOutcomeBeadList,
    /// no-work todo outcome must carry a non-empty reason
    EmptyOutcomeNoWorkReason,
}

#[derive(Deserialize)]
struct RawTodoSuccess {
    head: String,
    fingerprint: String,
    work_epic: String,
    specs: Vec<RawTodoSpecSuccess>,
}

#[derive(Deserialize)]
struct RawTodoSpecSuccess {
    label: String,
    #[serde(flatten)]
    outcome: RawTodoSpecOutcome,
}

#[derive(Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
enum RawTodoSpecOutcome {
    Decomposed { beads: Vec<String> },
    NoWork { reason: String },
}

#[cfg(test)]
mod tests {
    use super::{
        NonEmptyString, ParseTodoSuccessError, TodoFingerprint, TodoSpecOutcome, parse_todo_success,
    };

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const FINGERPRINT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn marker(json: &str) -> String {
        format!("LOOM_TODO: {json}")
    }

    fn success_json(outcome: &str) -> String {
        format!(
            r#"{{"head":"{SHA}","fingerprint":"{FINGERPRINT}","work_epic":"lm-work.1","specs":[{{"label":"templates",{outcome}}}]}}"#
        )
    }

    #[test]
    fn todo_success_marker_parses_to_typed_protocol() {
        let payload = success_json(r#""outcome":"decomposed","beads":["lm-task.1","lm-task.2"]"#);
        let parsed = parse_todo_success(&marker(&payload)).unwrap();

        assert_eq!(parsed.head.as_str(), SHA);
        assert_eq!(parsed.fingerprint.as_str(), FINGERPRINT);
        assert_eq!(parsed.work_epic.as_str(), "lm-work.1");
        assert_eq!(parsed.specs.len(), 1);
        let spec = &parsed.specs[0];
        assert_eq!(spec.label.as_str(), "templates");
        let TodoSpecOutcome::Decomposed { beads } = &spec.outcome else {
            panic!("expected decomposed outcome");
        };
        assert_eq!(beads.len(), 2);
        assert_eq!(beads[0].as_str(), "lm-task.1");
        assert_eq!(beads[1].as_str(), "lm-task.2");
    }

    #[test]
    fn no_work_success_payload_parses() {
        let payload = success_json(r#""outcome":"no-work","reason":"typo-only spec wording""#);
        let parsed = parse_todo_success(&marker(&payload)).unwrap();

        let spec = &parsed.specs[0];
        assert_eq!(spec.label.as_str(), "templates");
        let TodoSpecOutcome::NoWork { reason } = &spec.outcome else {
            panic!("expected no-work outcome");
        };
        assert_eq!(reason.as_str(), "typo-only spec wording");
    }

    #[test]
    fn todo_success_serializes_flat_outcome_wire_shape() {
        let payload = success_json(r#""outcome":"decomposed","beads":["lm-task.1"]"#);
        let parsed = parse_todo_success(&marker(&payload)).unwrap();
        let serialized = serde_json::to_string(&parsed).unwrap();

        assert!(
            serialized
                .contains(r#""label":"templates","outcome":"decomposed","beads":["lm-task.1"]"#)
        );
        assert!(!serialized.contains(r#""outcome":{"#));
    }

    #[test]
    fn missing_prefix_returns_missing_prefix_error() {
        let err = parse_todo_success("LOOM_COMPLETE").unwrap_err();

        assert!(matches!(err, ParseTodoSuccessError::MissingPrefix));
    }

    #[test]
    fn malformed_prefix_returns_malformed_prefix_error() {
        let err = parse_todo_success(&format!(
            "LOOM_TODO:{}",
            success_json(r#""outcome":"no-work","reason":"audit""#)
        ))
        .unwrap_err();

        assert!(matches!(err, ParseTodoSuccessError::MalformedPrefix));
    }

    #[test]
    fn invalid_json_returns_invalid_json_error() {
        let err = parse_todo_success("LOOM_TODO: not-json").unwrap_err();

        assert!(matches!(err, ParseTodoSuccessError::InvalidJson { .. }));
    }

    #[test]
    fn missing_required_field_returns_invalid_json_error() {
        let payload = format!(
            r#"{{"head":"{SHA}","fingerprint":"{FINGERPRINT}","specs":[{{"label":"templates","outcome":"no-work","reason":"audit"}}]}}"#
        );
        let err = parse_todo_success(&marker(&payload)).unwrap_err();

        assert!(matches!(err, ParseTodoSuccessError::InvalidJson { .. }));
    }

    #[test]
    fn invalid_sha_returns_invalid_git_sha_error() {
        let payload =
            success_json(r#""outcome":"no-work","reason":"audit""#).replace(SHA, "not-a-sha");
        let err = parse_todo_success(&marker(&payload)).unwrap_err();

        assert!(
            matches!(err, ParseTodoSuccessError::InvalidGitSha { value, .. } if value == "not-a-sha")
        );
    }

    #[test]
    fn invalid_fingerprint_returns_invalid_fingerprint_error() {
        let payload = success_json(r#""outcome":"no-work","reason":"audit""#)
            .replace(FINGERPRINT, "not-a-fingerprint");
        let err = parse_todo_success(&marker(&payload)).unwrap_err();

        assert!(
            matches!(err, ParseTodoSuccessError::InvalidFingerprint { value, .. } if value == "not-a-fingerprint")
        );
    }

    #[test]
    fn invalid_work_epic_returns_invalid_bead_id_error() {
        let payload = success_json(r#""outcome":"no-work","reason":"audit""#)
            .replace("lm-work.1", "not a bead");
        let err = parse_todo_success(&marker(&payload)).unwrap_err();

        assert!(
            matches!(err, ParseTodoSuccessError::InvalidBeadId { value, .. } if value == "not a bead")
        );
    }

    #[test]
    fn invalid_decomposed_bead_returns_invalid_bead_id_error() {
        let payload = success_json(r#""outcome":"decomposed","beads":["bad bead"]"#);
        let err = parse_todo_success(&marker(&payload)).unwrap_err();

        assert!(
            matches!(err, ParseTodoSuccessError::InvalidBeadId { value, .. } if value == "bad bead")
        );
    }

    #[test]
    fn invalid_spec_label_returns_invalid_spec_label_error() {
        let payload = success_json(r#""outcome":"no-work","reason":"audit""#)
            .replace("templates", "bad label");
        let err = parse_todo_success(&marker(&payload)).unwrap_err();

        assert!(
            matches!(err, ParseTodoSuccessError::InvalidSpecLabel { value } if value == "bad label")
        );
    }

    #[test]
    fn empty_specs_returns_empty_specs_error() {
        let payload = format!(
            r#"{{"head":"{SHA}","fingerprint":"{FINGERPRINT}","work_epic":"lm-work.1","specs":[]}}"#
        );
        let err = parse_todo_success(&marker(&payload)).unwrap_err();

        assert!(matches!(err, ParseTodoSuccessError::EmptySpecs));
    }

    #[test]
    fn empty_decomposed_beads_returns_empty_bead_list_error() {
        let payload = success_json(r#""outcome":"decomposed","beads":[]"#);
        let err = parse_todo_success(&marker(&payload)).unwrap_err();

        assert!(
            matches!(err, ParseTodoSuccessError::EmptyBeadList { label } if label.as_str() == "templates")
        );
    }

    #[test]
    fn empty_no_work_reason_returns_empty_no_work_reason_error() {
        let payload = success_json(r#""outcome":"no-work","reason":"""#);
        let err = parse_todo_success(&marker(&payload)).unwrap_err();

        assert!(
            matches!(err, ParseTodoSuccessError::EmptyNoWorkReason { label } if label.as_str() == "templates")
        );
    }

    #[test]
    fn blank_no_work_reason_returns_empty_no_work_reason_error() {
        let payload = success_json(r#""outcome":"no-work","reason":"   ""#);
        let err = parse_todo_success(&marker(&payload)).unwrap_err();

        assert!(
            matches!(err, ParseTodoSuccessError::EmptyNoWorkReason { label } if label.as_str() == "templates")
        );
    }

    #[test]
    fn typed_fingerprint_rejects_uppercase_hex() {
        let err = TodoFingerprint::new(
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        )
        .unwrap_err();

        assert_eq!(
            err.0,
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        );
    }

    #[test]
    fn non_empty_string_preserves_surrounding_whitespace() {
        let reason = NonEmptyString::new("  audited no work  ").unwrap();

        assert_eq!(reason.as_str(), "  audited no work  ");
    }
}
