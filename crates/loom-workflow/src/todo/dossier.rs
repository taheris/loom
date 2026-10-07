//! Lossless, bounded evidence pages; the batch roster stays in the entry prompt.

use std::fmt::Write;
use std::path::{Path, PathBuf};

use loom_templates::todo::TodoContext;

use super::TodoError;

pub const PROMPT_BYTES: usize = 64 * 1024;
const PAGE_BYTES: usize = 16 * 1024;
const PAGE_LINES: usize = 200;

pub struct Dossier {
    manifest: String,
    pages: Vec<(PathBuf, String)>,
}

impl Dossier {
    pub fn build(context: &TodoContext) -> Result<Self, TodoError> {
        let mut dossier = Self {
            manifest: format!(
                "# Todo evidence\n\nWork epic: {}\nHead: {}\nFingerprint: {}\n\nPages below are complete, ordered UTF-8 slices. Concatenate a section's pages in order to reconstruct its exact content. A page can continue a long row or line. All paths are relative to this index. Read criterion and note sections for every owner; consult diffs selectively. Missing cache observations are not proof of missing implementation.\n\n",
                context.work_epic, context.todo_head, context.todo_fingerprint
            ),
            pages: Vec::new(),
        };
        for spec in &context.changed_specs {
            write!(
                dossier.manifest,
                "## {}\n\nSource: `{}`\n\n",
                spec.label, spec.spec_path
            )?;
            let rows = context
                .criterion_status
                .iter()
                .filter(|row| row.spec_label == spec.label)
                .collect::<Vec<_>>();
            let mut criteria = String::new();
            for row in &rows {
                writeln!(criteria, "- {row}\n")?;
            }
            write!(dossier.manifest, "{} criteria.\n\n", rows.len())?;
            dossier.section(spec.label.as_str(), "criteria", &criteria)?;
            let mut notes = String::new();
            for note in context
                .implementation_notes
                .iter()
                .filter(|group| group.label == spec.label)
                .flat_map(|group| &group.notes)
            {
                write!(
                    notes,
                    "<implementation-note>\n<agent-output>\n{note}\n</agent-output>\n</implementation-note>\n\n"
                )?;
            }
            dossier.section(spec.label.as_str(), "notes", &notes)?;
            if let Some(diff) = &spec.diff {
                dossier.section(spec.label.as_str(), "diff", diff)?;
            } else {
                dossier.manifest.push_str("No prior finalized cursor: consult the current spec, not an invented baseline diff.\n\n");
            }
        }
        check_bound("evidence index", dossier.manifest.len())?;
        Ok(dossier)
    }

    fn section(&mut self, label: &str, name: &str, body: &str) -> std::fmt::Result {
        write!(self.manifest, "### {name}\n\n")?;
        if body.is_empty() {
            self.manifest.push_str("Empty.\n\n");
            return Ok(());
        }
        for (index, page) in chunks(body).into_iter().enumerate() {
            let path = PathBuf::from(format!("{label}/{name}/{index:04}.md"));
            writeln!(
                self.manifest,
                "- `{}` ({} bytes)",
                path.display(),
                page.len()
            )?;
            self.pages.push((path, page.to_owned()));
        }
        self.manifest.push('\n');
        Ok(())
    }

    pub fn write(&self, scratch: &Path) -> Result<(), TodoError> {
        let root = scratch.join("evidence");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("index.md"), &self.manifest)?;
        for (relative, body) in &self.pages {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, body)?;
        }
        Ok(())
    }
}

pub const fn check_bound(surface: &'static str, bytes: usize) -> Result<(), TodoError> {
    if bytes > PROMPT_BYTES {
        return Err(TodoError::ContextTooLarge {
            surface,
            bytes,
            limit: PROMPT_BYTES,
        });
    }
    Ok(())
}

fn chunks(mut body: &str) -> Vec<&str> {
    let mut pages = Vec::new();
    while !body.is_empty() {
        let mut end = body.floor_char_boundary(body.len().min(PAGE_BYTES));
        if let Some((newline, _)) = body[..end].match_indices('\n').nth(PAGE_LINES - 1) {
            end = newline + 1;
        } else if end < body.len()
            && let Some(newline) = body[..end].rfind('\n')
        {
            end = newline + 1;
        }
        pages.push(&body[..end]);
        body = &body[end..];
    }
    pages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_preserve_long_unicode_rows_and_exact_command_bytes() {
        let body = format!(
            "first\r\n{}\n[check](bash -c \"echo  'λ'\")\nlast",
            "λ".repeat(PAGE_BYTES * 2)
        );
        let pages = chunks(&body);
        assert!(pages.len() > 2);
        assert!(pages.iter().all(|page| page.len() <= PAGE_BYTES));
        assert_eq!(pages.concat(), body);
    }

    #[test]
    fn pages_also_bound_many_short_lines() {
        let body = "\n".repeat(PAGE_BYTES * 2);
        let pages = chunks(&body);
        assert!(pages.iter().all(|page| page.lines().count() <= PAGE_LINES));
        assert_eq!(pages.concat(), body);
    }

    #[test]
    fn oversized_entry_context_is_rejected_not_truncated() {
        assert!(check_bound("prompt", PROMPT_BYTES).is_ok());
        assert!(matches!(
            check_bound("prompt", PROMPT_BYTES + 1),
            Err(TodoError::ContextTooLarge { .. })
        ));
    }
}
