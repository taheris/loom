//! Durable distinction between an uninitialized spec and damaged cursor metadata.

use loom_driver::bd::{Bead, UpdateOpts};
use loom_protocol::todo::GitSha;

use super::TodoError;

pub const CURSOR_KEY: &str = "loom.todo_cursor";
pub const STATE_KEY: &str = "loom.todo_state";
const UNINITIALIZED: &str = "uninitialized";

#[derive(Debug, Clone)]
pub enum Cursor {
    Uninitialized,
    Finalized(GitSha),
}

impl Cursor {
    pub fn parse(bead: &Bead, label: &str) -> Result<Self, TodoError> {
        match (bead.metadata.get(CURSOR_KEY), bead.metadata.get(STATE_KEY)) {
            (None, Some(state)) if state.as_str() == Some(UNINITIALIZED) => Ok(Self::Uninitialized),
            (Some(cursor), None) => {
                let raw = cursor.as_str().unwrap_or("");
                GitSha::new(raw).map(Self::Finalized).map_err(|_| TodoError::InvalidSpecCursor {
                    label: label.to_owned(), epic_id: bead.id.to_string(),
                    cursor: cursor.as_str().map_or_else(|| cursor.to_string(), str::to_owned), reason: "not a full git SHA string".into(),
                })
            }
            (None, None) => Err(TodoError::MissingSpecCursor {
                label: label.to_owned(), epic_id: bead.id.to_string(),
            }),
            _ => Err(TodoError::InvalidSpecCursor {
                label: label.to_owned(), epic_id: bead.id.to_string(),
                cursor: format!("{:?}", bead.metadata.get(CURSOR_KEY)),
                reason: "invalid or contradictory loom.todo_state; expected uninitialized without a cursor, or a finalized cursor without state".into(),
            }),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Uninitialized => None,
            Self::Finalized(sha) => Some(sha.as_str()),
        }
    }

    pub const fn is_uninitialized(&self) -> bool {
        matches!(self, Self::Uninitialized)
    }

    pub fn creation_metadata() -> String {
        serde_json::json!({ STATE_KEY: UNINITIALIZED }).to_string()
    }

    pub fn update(&self) -> UpdateOpts {
        let mut update = UpdateOpts::default();
        match self {
            Self::Uninitialized => {
                update.unset_metadata.push(CURSOR_KEY.into());
                update
                    .set_metadata
                    .push((STATE_KEY.into(), UNINITIALIZED.into()));
            }
            Self::Finalized(sha) => {
                update
                    .set_metadata
                    .push((CURSOR_KEY.into(), sha.to_string()));
                update.unset_metadata.push(STATE_KEY.into());
            }
        }
        update
    }
}
