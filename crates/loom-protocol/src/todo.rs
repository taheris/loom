//! Typed wire-format contract for `loom todo` success markers.
//!
//! [`parse_todo_success`] delegates framing and phase admission to the
//! canonical decoder before projecting a typed success proposal.

use std::fmt;
use std::ops::Deref;
use std::str::FromStr;

use displaydoc::Display;
use loom_events::identifier::{BeadId, ParseBeadIdError, SpecLabel};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::acceptance::References;
use crate::oid::{GitOid, ParseGitOidError};

pub const TODO_SUCCESS_PREFIX: &str = "LOOM_TODO: ";

pub type GitSha = GitOid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct TodoFingerprint(String);

impl TodoFingerprint {
    /// Parse a canonical todo-input fingerprint.
    ///
    /// # Errors
    ///
    /// Returns [`ParseTodoFingerprintError`] unless `s` contains exactly
    /// 64 lowercase hexadecimal characters.
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
    /// Construct a string whose trimmed contents are non-empty.
    ///
    /// # Errors
    ///
    /// Returns [`ParseNonEmptyStringError`] when the value is empty or
    /// contains only whitespace.
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
    /// Construct a vector containing at least one element.
    ///
    /// # Errors
    ///
    /// Returns [`ParseNonEmptyVecError`] when `values` is empty.
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

    pub const fn len(&self) -> usize {
        self.0.len()
    }

    pub const fn is_empty(&self) -> bool {
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
    pub title: NonEmptyString,
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
    Decomposed {
        beads: NonEmptyVec<BeadId>,
        assignments: NonEmptyVec<Assignment>,
    },
    NoWork {
        reason: NonEmptyString,
    },
}

/// Agent-proposed task membership and criterion references, not trusted metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub task: BeadId,
    pub criteria: References,
}

impl<'de> Deserialize<'de> for TodoSpecOutcome {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        RawTodoSpecOutcome::deserialize(d)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

/// Parse the canonical final `LOOM_TODO` success marker.
///
/// # Errors
///
/// Returns [`ParseTodoSuccessError`] when the prefix, JSON payload, or
/// typed success fields violate the todo wire contract.
pub fn parse_todo_success(output: &str) -> Result<TodoSuccess, ParseTodoSuccessError> {
    let session = crate::output::decode(output, crate::output::Phase::Todo)?;
    if let crate::output::Message::Todo(success) = &session.terminal().message {
        return Ok(success.clone());
    }
    Err(ParseTodoSuccessError::WrongTerminal {
        context: session.into_context(),
    })
}

/// Alias for [`parse_todo_success`].
///
/// # Errors
///
/// Returns [`ParseTodoSuccessError`] under the same conditions as
/// [`parse_todo_success`].
pub fn parse_success_marker(final_line: &str) -> Result<TodoSuccess, ParseTodoSuccessError> {
    parse_todo_success(final_line)
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
        let title =
            NonEmptyString::new(raw.title).map_err(|_| ParseTodoSuccessError::EmptyTitle)?;
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
            title,
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
            RawTodoSpecOutcome::Decomposed { beads, assignments } => {
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
                TodoSpecOutcome::Decomposed {
                    beads,
                    assignments: NonEmptyVec::new(assignments)
                        .map_err(|_| ParseTodoSuccessError::EmptyAssignments)?,
                }
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
            RawTodoSpecOutcome::Decomposed { beads, assignments } => {
                if beads.is_empty() {
                    return Err(ParseTodoSuccessError::EmptyOutcomeBeadList);
                }
                let beads = beads
                    .into_iter()
                    .map(parse_bead_id)
                    .collect::<Result<Vec<_>, _>>()?;
                let beads = NonEmptyVec::new(beads)
                    .map_err(|_| ParseTodoSuccessError::EmptyOutcomeBeadList)?;
                Ok(Self::Decomposed {
                    beads,
                    assignments: NonEmptyVec::new(assignments)
                        .map_err(|_| ParseTodoSuccessError::EmptyAssignments)?,
                })
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
    /// todo output violates the agent-output contract
    Contract(#[from] crate::output::Failure),
    /// todo session did not propose a success payload
    WrongTerminal { context: crate::output::Context },
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
    /// todo success must carry a non-empty work epic title
    EmptyTitle,
    /// todo success must report at least one spec
    EmptySpecs,
    /// decomposed todo outcome must name at least one bead
    EmptyBeadList { label: SpecLabel },
    /// no-work todo outcome must carry a non-empty reason
    EmptyNoWorkReason { label: SpecLabel },
    /// decomposed todo outcome must name at least one bead
    EmptyOutcomeBeadList,
    /// decomposed todo outcome must propose task criterion assignments
    EmptyAssignments,
    /// no-work todo outcome must carry a non-empty reason
    EmptyOutcomeNoWorkReason,
}

#[derive(Deserialize)]
struct RawTodoSuccess {
    head: String,
    fingerprint: String,
    work_epic: String,
    title: String,
    specs: Vec<RawTodoSpecSuccess>,
}

#[derive(Deserialize)]
struct RawTodoSpecSuccess {
    label: String,
    #[serde(flatten)]
    outcome: RawTodoSpecOutcome,
}

#[derive(Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case", deny_unknown_fields)]
enum RawTodoSpecOutcome {
    Decomposed {
        beads: Vec<String>,
        assignments: Vec<Assignment>,
    },
    NoWork {
        reason: String,
    },
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
        let mut outcome = outcome.to_string();
        if outcome.contains(r#""outcome":"decomposed""#) {
            outcome.push_str(r#","assignments":[{"task":"lm-task.1","criteria":[{"spec_label":"templates","criterion_id":"criterion-0123456789abcdef"}]}]"#);
        }
        format!(
            r#"{{"head":"{SHA}","fingerprint":"{FINGERPRINT}","work_epic":"lm-work.1","title":"Pin template prompts","specs":[{{"label":"templates",{outcome}}}]}}"#
        )
    }

    #[test]
    fn todo_success_marker_parses_to_typed_protocol() {
        let payload = success_json(r#""outcome":"decomposed","beads":["lm-task.1","lm-task.2"]"#);
        let parsed = parse_todo_success(&marker(&payload)).unwrap();

        assert_eq!(parsed.head.as_str(), SHA);
        assert_eq!(parsed.fingerprint.as_str(), FINGERPRINT);
        assert_eq!(parsed.work_epic.as_str(), "lm-work.1");
        assert_eq!(parsed.title.as_str(), "Pin template prompts");
        assert_eq!(parsed.specs.len(), 1);
        let spec = &parsed.specs[0];
        assert_eq!(spec.label.as_str(), "templates");
        let TodoSpecOutcome::Decomposed { beads, .. } = &spec.outcome else {
            panic!("expected decomposed outcome");
        };
        assert_eq!(beads.len(), 2);
        assert_eq!(beads[0].as_str(), "lm-task.1");
        assert_eq!(beads[1].as_str(), "lm-task.2");
    }

    #[test]
    fn todo_proposals_require_typed_nonempty_assignments() {
        let valid = success_json(r#""outcome":"decomposed","beads":["lm-task.1"]"#);
        let value: serde_json::Value = serde_json::from_str(&valid).unwrap();
        for assignments in [
            serde_json::json!([]),
            serde_json::json!([{"task":"bad task","criteria":[{"spec_label":"templates","criterion_id":"criterion-0123456789abcdef"}]}]),
            serde_json::json!([{"task":"lm-task.1","criteria":[]}]),
            serde_json::json!([{"task":"lm-task.1","criteria":[{"spec_label":"bad label","criterion_id":"criterion-0123456789abcdef"}]}]),
            serde_json::json!([{"task":"lm-task.1","criteria":[{"spec_label":"templates","criterion_id":"bad"}]}]),
            serde_json::json!([{"task":"lm-task.1","criteria":[{"spec_label":"templates","criterion_id":"criterion-0123456789abcdef","resolved":true}]}]),
        ] {
            let mut invalid = value.clone();
            invalid["specs"][0]["assignments"] = assignments;
            assert!(serde_json::from_value::<super::TodoSuccess>(invalid).is_err());
        }
        let mut missing = value;
        missing["specs"][0]
            .as_object_mut()
            .unwrap()
            .remove("assignments");
        assert!(serde_json::from_value::<super::TodoSuccess>(missing).is_err());
    }

    #[test]
    fn no_work_cannot_silently_discard_assignments() {
        let payload = success_json(
            r#""outcome":"no-work","reason":"audited","assignments":[{"task":"bad task","criteria":[]}]"#,
        );
        assert!(parse_todo_success(&marker(&payload)).is_err());
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

    fn assert_contract_error(output: &str, expected: &str) {
        let ParseTodoSuccessError::Contract(failure) = parse_todo_success(output).unwrap_err()
        else {
            panic!("expected shared contract failure");
        };
        assert_eq!(failure.context().raw(), output);
        assert!(
            failure.diagnostics().iter().any(|diagnostic| {
                if let crate::output::Error::Json { source } = &diagnostic.error {
                    source.to_string().contains(expected)
                } else {
                    diagnostic.error.to_string().contains(expected)
                }
            }),
            "{failure:?}"
        );
    }

    #[test]
    fn todo_adapter_rejects_wrong_phase_completion() {
        assert_contract_error("LOOM_COMPLETE", "not admitted in phase Todo");
    }

    #[test]
    fn todo_adapter_accepts_canonical_colon_without_space() {
        let output = format!(
            "LOOM_TODO:{}",
            success_json(r#""outcome":"no-work","reason":"audit""#)
        );
        assert!(parse_todo_success(&output).is_ok());
    }

    #[test]
    fn todo_adapter_preserves_invalid_json_diagnostics() {
        assert_contract_error(
            "LOOM_TODO: not-json",
            "requires a colon followed by one JSON object",
        );
    }

    #[test]
    fn todo_adapter_preserves_missing_field_diagnostics() {
        let payload = format!(
            r#"{{"head":"{SHA}","fingerprint":"{FINGERPRINT}","specs":[{{"label":"templates","outcome":"no-work","reason":"audit"}}]}}"#
        );
        assert_contract_error(&marker(&payload), "missing field");
    }

    #[test]
    fn todo_adapter_rejects_invalid_identifiers() {
        let valid = success_json(r#""outcome":"decomposed","beads":["lm-task.1"]"#);
        for (original, invalid, expected) in [
            (SHA, "not-a-sha", "invalid todo head SHA"),
            (FINGERPRINT, "not-a-fingerprint", "invalid todo fingerprint"),
            ("lm-work.1", "not a bead", "invalid bead id"),
            ("lm-task.1", "bad bead", "invalid bead id"),
            ("templates", "bad label", "invalid spec label"),
        ] {
            assert_contract_error(&marker(&valid.replace(original, invalid)), expected);
        }
    }

    #[test]
    fn todo_adapter_rejects_empty_success_fields() {
        let valid = success_json(r#""outcome":"decomposed","beads":["lm-task.1"]"#);
        for (original, invalid, expected) in [
            ("Pin template prompts", "   ", "non-empty work epic title"),
            ("[\"lm-task.1\"]", "[]", "at least one bead"),
        ] {
            assert_contract_error(&marker(&valid.replace(original, invalid)), expected);
        }
        let payload = format!(
            r#"{{"head":"{SHA}","fingerprint":"{FINGERPRINT}","work_epic":"lm-work.1","title":"Pin template prompts","specs":[]}}"#
        );
        assert_contract_error(&marker(&payload), "at least one spec");
    }

    #[test]
    fn todo_adapter_rejects_blank_no_work_reasons() {
        for reason in ["", "   "] {
            let payload = success_json(&format!(r#""outcome":"no-work","reason":"{reason}""#));
            assert_contract_error(&marker(&payload), "non-empty reason");
        }
    }

    #[test]
    fn todo_adapter_admits_multiline_success_after_decision_records() {
        let payload: serde_json::Value =
            serde_json::from_str(&success_json(r#""outcome":"no-work","reason":"audit""#)).unwrap();
        let output = format!(
            "LOOM_CLARIFY: {{\"decisions\":[\"lm-decision.1\"]}}\nLOOM_TODO: {}",
            serde_json::to_string_pretty(&payload).unwrap()
        );
        assert_eq!(
            parse_todo_success(&output).unwrap().work_epic.as_str(),
            "lm-work.1"
        );
    }

    #[test]
    fn todo_adapter_retains_valid_decisions_when_success_is_malformed() {
        let output = "LOOM_CLARIFY: {\"decisions\":[\"lm-decision.1\"]}\nLOOM_TODO: {}";
        let ParseTodoSuccessError::Contract(failure) = parse_todo_success(output).unwrap_err()
        else {
            panic!("expected contract failure");
        };
        assert_eq!(failure.context().decisions().len(), 1);
        assert_eq!(failure.diagnostics()[0].span.line, 2);
        assert_eq!(failure.context().raw(), output);
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
