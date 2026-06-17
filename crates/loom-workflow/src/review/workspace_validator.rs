use std::path::{Path, PathBuf};

use loom_driver::identifier::SpecLabel;
use loom_gate::{CommandResolver, FsCommandResolver, annotation};

use super::finding::FindingValidator;

pub struct WorkspaceFindingValidator {
    workspace: PathBuf,
}

impl WorkspaceFindingValidator {
    #[must_use]
    pub fn new(workspace: &Path) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
        }
    }

    fn spec_path(&self, label: &SpecLabel) -> PathBuf {
        self.workspace.join("specs").join(format!("{label}.md"))
    }

    fn selector_path(target: &str) -> &str {
        let without_hash = target.split_once('#').map_or(target, |(path, _)| path);
        without_hash
            .split_once("::")
            .map_or(without_hash, |(path, _)| path)
    }

    fn target_path_exists(&self, target: &str) -> bool {
        let path = Self::selector_path(target.trim());
        if path.is_empty() {
            return false;
        }
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            candidate.exists()
        } else {
            self.workspace.join(candidate).exists()
        }
    }

    fn criterion_anchor_matches(&self, spec: &SpecLabel, anchor: &str, body: &str) -> bool {
        let normalized_anchor = markdown_slug(anchor);
        body.lines()
            .filter_map(markdown_heading_anchor)
            .any(|candidate| anchor_matches(&candidate, anchor, &normalized_anchor))
            || criterion_aliases(spec, body)
                .iter()
                .any(|candidate| anchor_matches(candidate, anchor, &normalized_anchor))
    }
}

impl FindingValidator for WorkspaceFindingValidator {
    fn spec_label_is_known(&self, label: &SpecLabel) -> bool {
        self.spec_path(label).is_file()
    }

    fn criterion_anchor_resolves(&self, spec: &SpecLabel, anchor: &str) -> bool {
        let Ok(body) = std::fs::read_to_string(self.spec_path(spec)) else {
            return false;
        };
        self.criterion_anchor_matches(spec, anchor, &body)
    }

    fn annotation_resolves(&self, target_string: &str) -> bool {
        if self.target_path_exists(target_string) {
            return true;
        }
        let Some(first_token) = target_string.split_whitespace().next() else {
            return false;
        };
        FsCommandResolver::new(&self.workspace).resolves(first_token)
    }

    fn file_exists(&self, path: &str) -> bool {
        self.target_path_exists(path)
    }

    fn invariant_resolves(&self, spec: &SpecLabel, section: &str, tag: &str) -> bool {
        let Ok(body) = std::fs::read_to_string(self.spec_path(spec)) else {
            return false;
        };
        let section_anchor = markdown_slug(section);
        let tag_anchor = markdown_slug(tag);
        let body_anchor = markdown_slug(&body);
        body.lines()
            .filter_map(markdown_heading_anchor)
            .any(|candidate| candidate == section_anchor)
            && invariant_tag_resolves(&body_anchor, &tag_anchor)
    }
}

fn criterion_aliases(spec: &SpecLabel, body: &str) -> Vec<String> {
    let parsed = annotation::parse_content(&PathBuf::from(format!("specs/{spec}.md")), body);
    let mut aliases = Vec::new();
    for criterion in &parsed.criteria {
        aliases.push(annotation::criterion_id_for(spec, &criterion.text));
        aliases.push(markdown_slug(&criterion.text));
        aliases.extend(
            parsed
                .annotations
                .iter()
                .filter(|ann| ann.criterion_line == criterion.line)
                .flat_map(|ann| annotation_aliases(&ann.target)),
        );
    }
    aliases
}

fn annotation_aliases(target: &str) -> Vec<String> {
    let mut aliases = vec![target.trim().to_owned()];
    if let Some(tail) = target_tail(target) {
        aliases.push(tail.to_owned());
    }
    aliases
}

fn target_tail(target: &str) -> Option<&str> {
    let token = target
        .split_whitespace()
        .next_back()
        .unwrap_or(target)
        .trim();
    let token = token.rsplit_once('#').map_or(token, |(_, tail)| tail);
    let token = token.rsplit_once("::").map_or(token, |(_, tail)| tail);
    if token.is_empty() { None } else { Some(token) }
}

fn anchor_matches(candidate: &str, anchor: &str, normalized_anchor: &str) -> bool {
    candidate == anchor || markdown_slug(candidate) == normalized_anchor
}

fn markdown_heading_anchor(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let text = trimmed.strip_prefix('#')?.trim_start_matches('#').trim();
    if text.is_empty() {
        None
    } else {
        Some(markdown_slug(text))
    }
}

fn invariant_tag_resolves(body_anchor: &str, tag_anchor: &str) -> bool {
    !tag_anchor.is_empty()
        && (body_anchor.contains(tag_anchor)
            || tag_anchor
                .split('-')
                .filter(|part| !part.is_empty())
                .all(|part| body_anchor.split('-').any(|body_part| body_part == part)))
}

fn markdown_slug(input: &str) -> String {
    let mut out = String::new();
    let mut previous_was_separator = true;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            previous_was_separator = false;
        } else if !previous_was_separator {
            out.push('-');
            previous_was_separator = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn criterion_aliases_include_attached_verifier_function_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("specs")).expect("specs dir");
        std::fs::write(
            tmp.path().join("specs/beads.md"),
            "# Beads\n\n## Success Criteria\n\n- Push failures fall back only after fast-forward rejection\n  [judge](tests/judges/beads.sh test_beadspush_pushes_before_pulls)\n",
        )
        .expect("write spec");
        let validator = WorkspaceFindingValidator::new(tmp.path());
        let beads = SpecLabel::new("beads");

        assert!(validator.criterion_anchor_resolves(&beads, "test_beadspush_pushes_before_pulls"));
    }

    #[test]
    fn criterion_aliases_include_stable_criterion_ids() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("specs")).expect("specs dir");
        let body = "# Gate\n\n## Success Criteria\n\n- A thing must hold [check](true)\n";
        std::fs::write(tmp.path().join("specs/gate.md"), body).expect("write spec");
        let parsed = annotation::parse_content(&PathBuf::from("specs/gate.md"), body);
        let gate = SpecLabel::new("gate");
        let criterion_id = annotation::criterion_id_for(&gate, &parsed.criteria[0].text);
        let validator = WorkspaceFindingValidator::new(tmp.path());

        assert!(validator.criterion_anchor_resolves(&gate, &criterion_id));
    }
}
