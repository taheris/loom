//! Typed dirty-workspace recovery context rendered by `loom loop`.

use std::fmt;

use loom_protocol::todo::GitSha;

/// Loop-only context for an unapplied recovery stash created before dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRecovery {
    pub pre_stash_status: String,
    pub stash: RecoveryStash,
    pub integration_tip: GitSha,
    pub alignment: WorkspaceAlignment,
}

/// Git stash identity and description captured at dispatch time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryStash {
    pub selector: String,
    pub commit: GitSha,
    pub message: String,
}

/// Bead-branch alignment outcome after recovery stash preservation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceAlignment {
    Clean,
    Rebased {
        previous_head: GitSha,
        current_head: GitSha,
    },
    Conflict {
        files: Vec<String>,
    },
}

impl WorkspaceAlignment {
    pub fn is_conflict(&self) -> bool {
        matches!(self, Self::Conflict { .. })
    }

    pub fn conflict_files(&self) -> &[String] {
        match self {
            Self::Conflict { files } => files,
            Self::Clean | Self::Rebased { .. } => &[],
        }
    }
}

impl fmt::Display for WorkspaceAlignment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clean => f.write_str("Clean"),
            Self::Rebased {
                previous_head,
                current_head,
            } => write!(
                f,
                "Rebased (previous head `{previous_head}`, current head `{current_head}`)"
            ),
            Self::Conflict { files } if files.is_empty() => {
                f.write_str("Conflict (no files reported)")
            }
            Self::Conflict { files } => {
                write!(f, "Conflict ({})", files.join(", "))
            }
        }
    }
}
