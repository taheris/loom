//! Multi-spec working-tree-diff fan-out for `loom todo`.
//!
//! Walks every spec whose markdown differs from `HEAD` (anchor + siblings)
//! and emits the corresponding diff so the rendered prompt can address
//! every touched spec in a single decomposition pass.
//!
//! Replaces the four-tier `base_commit`-anchored walk with a stateless
//! diff: HEAD is the implicit base, the working tree is the source. Specs
//! never tracked by bd inherit the same HEAD-vs-working-tree comparison —
//! no per-spec cursor table is required.

use std::path::PathBuf;

use loom_driver::git::GitClient;
use loom_driver::identifier::SpecLabel;

use super::error::TodoError;

/// One spec whose markdown differs from `HEAD` in the working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchedSpec {
    pub label: SpecLabel,
    pub spec_path: PathBuf,
    pub diff: String,
}

/// Compute the touched set: every spec whose `specs/<X>.md` differs from
/// `HEAD` in the working tree. Returns the per-spec diff bodies so callers
/// can render the multi-spec fan-out block directly.
pub async fn touched_specs(git: &GitClient) -> Result<Vec<TouchedSpec>, TodoError> {
    let paths = git
        .workdir_changed_specs()
        .await
        .map_err(|e| TodoError::Io(std::io::Error::other(e.to_string())))?;
    let mut out = Vec::with_capacity(paths.len());
    for spec_path in paths {
        let Some(stem) = spec_path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let diff = git
            .workdir_diff_spec(&spec_path)
            .await
            .map_err(|e| TodoError::Io(std::io::Error::other(e.to_string())))?;
        if diff.is_empty() {
            continue;
        }
        out.push(TouchedSpec {
            label: SpecLabel::new(stem.to_string()),
            spec_path,
            diff,
        });
    }
    Ok(out)
}

/// Format the per-spec fan-out: `=== <spec_path> ===` header followed by
/// the diff body, each touched spec separated by a blank line.
pub fn render_fanout_block(touched: &[TouchedSpec]) -> String {
    let mut out = String::new();
    for (idx, spec) in touched.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str("=== ");
        out.push_str(&spec.spec_path.to_string_lossy());
        out.push_str(" ===\n");
        out.push_str(&spec.diff);
        if !spec.diff.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_fanout_emits_path_markers_and_blank_separators() {
        let touched = vec![
            TouchedSpec {
                label: SpecLabel::new("alpha"),
                spec_path: PathBuf::from("specs/alpha.md"),
                diff: "alpha diff\n".into(),
            },
            TouchedSpec {
                label: SpecLabel::new("beta"),
                spec_path: PathBuf::from("specs/beta.md"),
                diff: "beta diff".into(),
            },
        ];
        let rendered = render_fanout_block(&touched);
        assert!(rendered.contains("=== specs/alpha.md ==="));
        assert!(rendered.contains("alpha diff"));
        assert!(rendered.contains("=== specs/beta.md ==="));
        assert!(rendered.contains("beta diff"));
    }

    #[test]
    fn empty_touched_renders_empty() {
        assert_eq!(render_fanout_block(&[]), "");
    }
}
