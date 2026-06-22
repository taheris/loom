use std::path::{Path, PathBuf};

use loom_driver::identifier::SpecLabel;
use loom_gate::{
    CommandResolver, FsCommandResolver, RustWorkspaceTestResolver, TestPathResolver, Tier,
    annotation,
};
use tracing::warn;

use super::finding::FindingValidator;

pub struct WorkspaceFindingValidator {
    workspace: PathBuf,
    annotations: Vec<annotation::Annotation>,
    command_resolver: FsCommandResolver,
    test_resolver: Option<RustWorkspaceTestResolver>,
}

impl WorkspaceFindingValidator {
    #[must_use]
    pub fn new(workspace: &Path) -> Self {
        let workspace = workspace.to_path_buf();
        let annotations = load_annotations(&workspace);
        let test_resolver = load_test_resolver(&workspace);
        let command_resolver = FsCommandResolver::new(&workspace);
        Self {
            workspace,
            annotations,
            command_resolver,
            test_resolver,
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
        path_exists_from(&self.workspace, target)
    }

    fn declared_annotation_resolves(&self, target: &str) -> Option<bool> {
        let mut saw_target = false;
        for ann in self
            .annotations
            .iter()
            .filter(|ann| ann.target.trim() == target)
        {
            saw_target = true;
            if self.annotation_record_resolves(ann) {
                return Some(true);
            }
        }
        saw_target.then_some(false)
    }

    fn annotation_record_resolves(&self, ann: &annotation::Annotation) -> bool {
        match ann.tier {
            Tier::Check | Tier::System => self.command_target_resolves(&ann.target),
            Tier::Test => self
                .test_resolver
                .as_ref()
                .is_some_and(|resolver| resolver.resolves(&ann.target)),
            Tier::Judge => self.judge_target_resolves(&ann.target, &ann.source_spec),
        }
    }

    fn command_target_resolves(&self, target: &str) -> bool {
        if self.target_path_exists(target) {
            return true;
        }
        let Some(first_token) = target.split_whitespace().next() else {
            return false;
        };
        self.command_resolver.resolves(first_token)
    }

    fn judge_target_resolves(&self, target: &str, source_spec: &Path) -> bool {
        if self.target_path_exists(target) {
            return true;
        }
        let base = match source_spec.parent() {
            Some(parent) if parent.is_absolute() => parent.to_path_buf(),
            Some(parent) => self.workspace.join(parent),
            None => self.workspace.clone(),
        };
        path_exists_from(&base, target)
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
        let target = target_string.trim();
        if target.is_empty() {
            return false;
        }
        if let Some(resolved) = self.declared_annotation_resolves(target) {
            return resolved;
        }
        self.command_target_resolves(target)
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

fn load_annotations(workspace: &Path) -> Vec<annotation::Annotation> {
    let specs_dir = workspace.join("specs");
    match annotation::parse(&specs_dir) {
        Ok(parsed) => parsed.annotations,
        Err(err) => {
            warn!(path = %specs_dir.display(), error = ?err, "failed to parse spec annotations for finding validation");
            Vec::new()
        }
    }
}

fn load_test_resolver(workspace: &Path) -> Option<RustWorkspaceTestResolver> {
    match RustWorkspaceTestResolver::scan(workspace) {
        Ok(resolver) => Some(resolver),
        Err(err) => {
            warn!(workspace = %workspace.display(), error = ?err, "failed to scan tests for finding validation");
            None
        }
    }
}

fn path_exists_from(base: &Path, target: &str) -> bool {
    let path = WorkspaceFindingValidator::selector_path(target.trim());
    if path.is_empty() {
        return false;
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        candidate.exists()
    } else {
        base.join(candidate).exists()
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

    #[test]
    fn annotation_resolves_declared_test_targets_only_when_test_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("specs")).expect("specs dir");
        std::fs::create_dir_all(tmp.path().join("crates/loom-agent/src/pi")).expect("source dir");
        std::fs::write(
            tmp.path().join("specs/agent.md"),
            "# Agent\n\n## Success Criteria\n\n- Malformed JSON is handled [test](malformed_json_returns_invalid_json_error)\n- Missing verifier is declared [test](declared_but_missing_test)\n",
        )
        .expect("write spec");
        std::fs::write(
            tmp.path().join("crates/loom-agent/src/pi/parser.rs"),
            "#[test]\nfn malformed_json_returns_invalid_json_error() {}\n#[test]\nfn undeclared_bare_test_name() {}\n",
        )
        .expect("write source");
        let validator = WorkspaceFindingValidator::new(tmp.path());

        assert!(validator.annotation_resolves("malformed_json_returns_invalid_json_error"));
        assert!(!validator.annotation_resolves("declared_but_missing_test"));
        assert!(!validator.annotation_resolves("undeclared_bare_test_name"));
    }

    #[test]
    fn tree_walk_accepts_declared_bare_test_annotation_targets() {
        use super::super::finding::{DispatchScope, FindingTarget, parse_walk_output};

        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("specs")).expect("specs dir");
        std::fs::create_dir_all(tmp.path().join("crates/loom-agent/src/direct/tools"))
            .expect("direct tools dir");
        std::fs::create_dir_all(tmp.path().join("crates/loom-agent/src/pi")).expect("pi dir");
        std::fs::write(
            tmp.path().join("specs/agent.md"),
            "# Agent\n\n## Success Criteria\n\n- Malformed JSON streams continue [test](malformed_json_returns_invalid_json_error)\n- Direct spawn reaches wrix [test](direct_session_spawn_invokes_wrix_spawn_with_direct_runtime)\n- Direct reads use the workspace mount [test](direct_tools_read_against_container_workspace_mount)\n",
        )
        .expect("write spec");
        std::fs::write(
            tmp.path().join("crates/loom-agent/src/direct/backend.rs"),
            "#[test]\nfn direct_session_spawn_invokes_wrix_spawn_with_direct_runtime() {}\n",
        )
        .expect("write backend test");
        std::fs::write(
            tmp.path()
                .join("crates/loom-agent/src/direct/tools/read.rs"),
            "#[tokio::test]\nasync fn direct_tools_read_against_container_workspace_mount() {}\n",
        )
        .expect("write read test");
        std::fs::write(
            tmp.path().join("crates/loom-agent/src/pi/parser.rs"),
            "#[test]\nfn malformed_json_returns_invalid_json_error() {}\n",
        )
        .expect("write parser test");
        let validator = WorkspaceFindingValidator::new(tmp.path());
        let output = concat!(
            r#"LOOM_FINDING: {"token":"verifier-bypass","route":"deferred","bonds":["agent"],"target":{"kind":"Annotation","target_string":"direct_session_spawn_invokes_wrix_spawn_with_direct_runtime"},"evidence":"spawn test does not execute the entrypoint path it claims to verify"}"#,
            "\n",
            r#"LOOM_FINDING: {"token":"coincidental-pass","route":"deferred","bonds":["agent"],"target":{"kind":"Annotation","target_string":"direct_tools_read_against_container_workspace_mount"},"evidence":"read test passes due to host filesystem reads"}"#,
            "\n",
            r#"LOOM_FINDING: {"token":"verifier-bypass","route":"deferred","bonds":["agent"],"target":{"kind":"Annotation","target_string":"malformed_json_returns_invalid_json_error"},"evidence":"parser test does not drive a live stream continuation"}"#,
            "\nLOOM_CONCERN: {\"summary\":\"Verifier findings cite declared test annotations\"}\n",
        );
        let findings = parse_walk_output(output, DispatchScope::Tree, &validator)
            .expect("declared bare test annotations should parse");

        let targets: Vec<&str> = findings
            .iter()
            .map(|finding| match &finding.target {
                FindingTarget::Annotation { target_string } => target_string.as_str(),
                other => panic!("expected annotation target, got {other:?}"),
            })
            .collect();
        assert_eq!(
            targets,
            vec![
                "direct_session_spawn_invokes_wrix_spawn_with_direct_runtime",
                "direct_tools_read_against_container_workspace_mount",
                "malformed_json_returns_invalid_json_error",
            ]
        );
    }
}
