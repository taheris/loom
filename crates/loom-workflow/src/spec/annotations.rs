//! Parse `## Success Criteria` bullet lists in spec markdown.
//!
//! Each top-level bullet pairs with the first `[verify](path#fn)` or
//! `[judge](path#fn)` link inside the same list item. Items without an
//! annotation become entries of type [`AnnotationKind::None`]. Bullets
//! carry their annotation but no `[ ]` / `[x]` checkbox prefix — status
//! is derived live from running the verifier, not stored in the spec
//! (specs/harness.md FR14). Legacy checkbox bullets are still
//! accepted: the marker is consumed and discarded. Headings inside
//! fenced code blocks are skipped by the structural parser.

use std::fs;
use std::path::{Path, PathBuf};

use loom_driver::markdown::{Event, HeadingLevel, Tag, TagEnd, section_events};

use super::error::SpecError;

/// Kind of annotation attached to a [`Annotation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationKind {
    /// `[verify](path#fn)` — automated verification entry point.
    Verify,
    /// `[judge](path#fn)` — model-judged entry point.
    Judge,
    /// Criterion has no machine-readable annotation.
    None,
}

/// One row from `## Success Criteria`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub criterion: String,
    pub kind: AnnotationKind,
    pub file: Option<PathBuf>,
    pub function: Option<String>,
}

/// Parse all annotations in `spec_path`. Returns the rows in order.
/// Returns [`SpecError::NoSuccessCriteria`] when the file has no
/// `## Success Criteria` section.
pub fn parse_spec_annotations(spec_path: &Path) -> Result<Vec<Annotation>, SpecError> {
    let body = fs::read_to_string(spec_path).map_err(|source| SpecError::Io {
        path: spec_path.to_path_buf(),
        source,
    })?;
    let spec_dir = spec_path.parent().unwrap_or_else(|| Path::new(""));
    parse_body(&body, spec_dir).ok_or_else(|| SpecError::NoSuccessCriteria {
        path: spec_path.to_path_buf(),
    })
}

fn parse_body(body: &str, spec_dir: &Path) -> Option<Vec<Annotation>> {
    let events = section_events(body, HeadingLevel::H2, |t| {
        t.starts_with("Success Criteria")
    })?;

    let mut out = Vec::new();
    let mut list_depth: usize = 0;
    let mut item: Option<ItemState> = None;

    for (event, _) in events {
        match event {
            Event::Start(Tag::List(_)) => list_depth += 1,
            Event::End(TagEnd::List(_)) => list_depth = list_depth.saturating_sub(1),
            Event::Start(Tag::Item) if list_depth == 1 => {
                item = Some(ItemState::default());
            }
            Event::End(TagEnd::Item) if list_depth == 1 => {
                if let Some(state) = item.take()
                    && let Some(annotation) = state.into_annotation(spec_dir)
                {
                    out.push(annotation);
                }
            }
            Event::TaskListMarker(_) => {
                // Legacy `[ ]` / `[x]` markers are accepted and discarded
                // — status is verifier-driven (specs/harness.md FR14).
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                if let Some(state) = item.as_mut() {
                    state.start_link(dest_url.into_string());
                }
            }
            Event::End(TagEnd::Link) => {
                if let Some(state) = item.as_mut() {
                    state.end_link();
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(state) = item.as_mut() {
                    state.push_text(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(state) = item.as_mut() {
                    state.push_text(" ");
                }
            }
            _ => {}
        }
    }
    Some(out)
}

#[derive(Default)]
struct ItemState {
    criterion: String,
    pending_link_text: Option<String>,
    pending_link_url: Option<String>,
    annotation: Option<(AnnotationKind, String)>,
}

impl ItemState {
    fn start_link(&mut self, url: String) {
        self.pending_link_text = Some(String::new());
        self.pending_link_url = Some(url);
    }

    fn end_link(&mut self) {
        let (Some(text), Some(url)) = (self.pending_link_text.take(), self.pending_link_url.take())
        else {
            return;
        };
        if self.annotation.is_none() {
            let kind = match text.as_str() {
                "verify" => Some(AnnotationKind::Verify),
                "judge" => Some(AnnotationKind::Judge),
                _ => None,
            };
            if let Some(k) = kind {
                self.annotation = Some((k, url));
                return;
            }
        }
        if !text.is_empty() {
            self.criterion.push_str(&text);
        }
    }

    fn push_text(&mut self, text: &str) {
        if let Some(buf) = self.pending_link_text.as_mut() {
            buf.push_str(text);
        } else {
            self.criterion.push_str(text);
        }
    }

    fn into_annotation(self, spec_dir: &Path) -> Option<Annotation> {
        let criterion = self.criterion.trim().to_string();
        if criterion.is_empty() && self.annotation.is_none() {
            return None;
        }
        match self.annotation {
            Some((kind, target)) => {
                let (file, function) = resolve_annotation_link(&target, spec_dir);
                Some(Annotation {
                    criterion,
                    kind,
                    file: Some(file),
                    function,
                })
            }
            None => Some(Annotation {
                criterion,
                kind: AnnotationKind::None,
                file: None,
                function: None,
            }),
        }
    }
}

fn resolve_annotation_link(target: &str, spec_dir: &Path) -> (PathBuf, Option<String>) {
    let (file_part, fn_part) = match target.split_once('#') {
        Some((f, fnname)) => (f.to_string(), Some(fnname.to_string())),
        None => match target.split_once("::") {
            Some((f, fnname)) => (f.to_string(), Some(fnname.to_string())),
            None => (target.to_string(), None),
        },
    };
    let path = if file_part.starts_with("../") {
        normalize(&spec_dir.join(&file_part))
    } else {
        PathBuf::from(file_part)
    };
    (path, fn_part.filter(|s| !s.is_empty()))
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    fn write_spec(dir: &Path, name: &str, body: &str) -> Result<PathBuf> {
        let path = dir.join(name);
        fs::write(&path, body)?;
        Ok(path)
    }

    #[test]
    fn returns_no_criteria_error_when_section_missing() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = write_spec(dir.path(), "x.md", "# Title\n\nbody\n")?;
        let err = parse_spec_annotations(&path)
            .err()
            .ok_or_else(|| anyhow::anyhow!("expected error"))?;
        assert!(matches!(err, SpecError::NoSuccessCriteria { .. }));
        Ok(())
    }

    #[test]
    fn pairs_bullet_with_following_verify_link() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = "# X\n\n## Success Criteria\n\n- Run thing\n  [verify](tests/x.sh#test_thing)\n";
        let path = write_spec(dir.path(), "x.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.kind, AnnotationKind::Verify);
        assert_eq!(row.criterion, "Run thing");
        assert_eq!(row.file.as_deref(), Some(Path::new("tests/x.sh")));
        assert_eq!(row.function.as_deref(), Some("test_thing"));
        Ok(())
    }

    #[test]
    fn legacy_checkbox_marker_is_accepted_and_discarded() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = "## Success Criteria\n\n- [ ] open\n  [verify](t.sh#a)\n- [x] done\n  [judge](specs/foo.md)\n";
        let path = write_spec(dir.path(), "x.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, AnnotationKind::Verify);
        assert_eq!(rows[0].criterion, "open");
        assert_eq!(rows[1].kind, AnnotationKind::Judge);
        assert_eq!(rows[1].criterion, "done");
        Ok(())
    }

    #[test]
    fn criterion_without_annotation_yields_none_kind() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = "## Success Criteria\n\n- orphan\n- paired\n  [verify](t.sh#x)\n";
        let path = write_spec(dir.path(), "x.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, AnnotationKind::None);
        assert_eq!(rows[0].criterion, "orphan");
        assert_eq!(rows[1].kind, AnnotationKind::Verify);
        Ok(())
    }

    #[test]
    fn fenced_code_blocks_are_skipped() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body =
            "## Success Criteria\n\n```\n- not a real entry\n```\n\n- real\n  [verify](t.sh#x)\n";
        let path = write_spec(dir.path(), "x.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].criterion, "real");
        Ok(())
    }

    #[test]
    fn next_h2_terminates_section() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = "## Success Criteria\n\n- a\n  [verify](t.sh#x)\n\n## Other\n\n- not parsed\n";
        let path = write_spec(dir.path(), "x.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        assert_eq!(rows.len(), 1);
        Ok(())
    }

    #[test]
    fn relative_paths_normalize_against_spec_dir() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let specs = dir.path().join("specs");
        fs::create_dir_all(&specs)?;
        let body = "## Success Criteria\n\n- a\n  [verify](../tests/foo.sh#test_a)\n";
        let path = write_spec(&specs, "foo.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        let resolved = rows[0]
            .file
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("file must be set"))?;
        assert_eq!(resolved, dir.path().join("tests/foo.sh"));
        Ok(())
    }

    #[test]
    fn legacy_double_colon_separator_supported() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = "## Success Criteria\n\n- a\n  [verify](tests/foo.sh::test_a)\n";
        let path = write_spec(dir.path(), "x.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        assert_eq!(rows[0].function.as_deref(), Some("test_a"));
        Ok(())
    }

    #[test]
    fn fenced_success_criteria_example_does_not_anchor() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let body = "\
# X

````markdown
## Success Criteria

- fake entry
  [verify](fake.sh#x)
````

## Success Criteria

- real
  [verify](t.sh#x)
";
        let path = write_spec(dir.path(), "x.md", body)?;
        let rows = parse_spec_annotations(&path)?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].criterion, "real");
        Ok(())
    }
}
