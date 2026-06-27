//! `loom inbox` template: interactive session for resolving inbox items.

use askama::Template;

use crate::SkillIndexMarkdown;

/// Context for `loom inbox chat` rendering the visible decision queue.
#[derive(Template)]
#[template(path = "inbox.md", escape = "none")]
pub struct InboxContext {
    pub pinned_context: String,
    pub companion_paths: Vec<String>,
    pub inbox_items: Vec<InboxItem>,
    pub scratchpad_path: String,
    pub skill_index: SkillIndexMarkdown,
}

/// A single visible inbox item surfaced to the chat session.
#[derive(Debug, Clone)]
pub struct InboxItem {
    pub index: u32,
    pub id: String,
    pub bead_id: String,
    pub spec_label: String,
    pub title: String,
    pub body: String,
    pub notes: Option<String>,
    pub options_summary: Option<String>,
    pub options: Vec<ClarifyOption>,
    pub kind: ItemKind,
    pub infra: Option<InfraItem>,
    pub tune: Option<TuneItem>,
}

impl InboxItem {
    pub fn is_blocked(&self) -> bool {
        matches!(self.kind, ItemKind::Blocked)
    }

    pub fn is_infra(&self) -> bool {
        matches!(self.kind, ItemKind::Infra)
    }

    pub fn is_tune(&self) -> bool {
        matches!(self.kind, ItemKind::Tune)
    }

    pub fn kind_tag(&self) -> &'static str {
        self.kind.tag()
    }
}

impl ItemKind {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Clarify => "clarify",
            Self::Blocked => "blocked",
            Self::Infra => "infra",
            Self::Tune => "tune",
        }
    }
}

/// Which inbox flow an item belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Clarify,
    Blocked,
    Infra,
    Tune,
}

/// A single option under a clarify bead's `## Options` block.
#[derive(Debug, Clone)]
pub struct ClarifyOption {
    pub n: u32,
    pub title: Option<String>,
    pub body: Option<String>,
}

/// Infra diagnostics rendered for operator-resolution inbox items.
#[derive(Debug, Clone)]
pub struct InfraItem {
    pub phase: Option<String>,
    pub first_event_seen: Option<bool>,
    pub attempt: Option<String>,
    pub max_attempts: Option<String>,
    pub exit_status: Option<String>,
    pub stderr_tail: Option<String>,
    pub spawn_error_tail: Option<String>,
    pub log_path: Option<String>,
}

/// Tune proposal metadata rendered for tune inbox items.
#[derive(Debug, Clone)]
pub struct TuneItem {
    pub state: String,
    pub proposal_branch: Option<String>,
    pub proposal_head: Option<String>,
    pub base_commit: Option<String>,
    pub envelope_path: String,
    pub repo_path: String,
    pub manifest_path: String,
    pub evidence_path: String,
}
