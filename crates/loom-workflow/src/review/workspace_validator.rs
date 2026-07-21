use std::path::{Path, PathBuf};

use loom_driver::{config::LoomConfig, identifier::SpecLabel};
use loom_gate::{
    CommandResolver, FsCommandResolver, RustWorkspaceTestResolver, TestPathResolver, Tier,
    annotation,
    integrity::runner_owns_target,
    runner::{RunnerSpec, integrity_runner_specs},
};
use tracing::warn;

use super::finding::FindingValidator;

pub struct WorkspaceFindingValidator {
    workspace: PathBuf,
    annotations: Vec<annotation::Annotation>,
    runner_specs: Vec<RunnerSpec>,
    command_resolver: FsCommandResolver,
    test_resolver: Option<RustWorkspaceTestResolver>,
}

impl WorkspaceFindingValidator {
    #[must_use]
    pub fn new(workspace: &Path) -> Self {
        let workspace = workspace.to_path_buf();
        let annotations = load_annotations(&workspace);
        let runner_specs = load_runner_specs(&workspace);
        let test_resolver = load_test_resolver(&workspace);
        let command_resolver = FsCommandResolver::new(&workspace);
        Self {
            workspace,
            annotations,
            runner_specs,
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
            Tier::Check | Tier::System => {
                runner_owns_target(&self.runner_specs, ann.tier, &ann.target)
                    || self.command_target_resolves(&ann.target)
            }
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

fn load_runner_specs(workspace: &Path) -> Vec<RunnerSpec> {
    let config_path = LoomConfig::resolve_path(workspace);
    let config = match LoomConfig::load(&config_path) {
        Ok(config) => config,
        Err(err) => {
            warn!(path = %config_path.display(), error = ?err, "failed to load runner config for finding validation");
            return Vec::new();
        }
    };
    match integrity_runner_specs(&config) {
        Ok(specs) => specs,
        Err(err) => {
            warn!(path = %config_path.display(), error = ?err, "failed to compile runners for finding validation");
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
    use std::os::unix::fs::PermissionsExt;

    use loom_gate::{DispatchOptions, TierCwds, run_check, run_system};

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
    fn runner_owned_logical_targets_dispatch_and_survive_tree_finding_validation() {
        use super::super::finding::{DispatchScope, FindingTarget, parse_walk_output};

        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("specs")).expect("specs dir");
        std::fs::create_dir_all(tmp.path().join("bin")).expect("bin dir");
        std::fs::write(
            tmp.path().join("specs/gate.md"),
            concat!(
                "# Gate\n\n## Success Criteria\n\n",
                "- Check verify [check](verify:example.target)\n",
                "- Check CI [check](test-ci:example)\n",
                "- System verify [system](verify:example.target)\n",
                "- System CI [system](test-ci:example)\n",
            ),
        )
        .expect("write spec");
        std::fs::write(
            tmp.path().join("loom.toml"),
            concat!(
                "[runner.check.verify]\nmatch = '^verify:(.+)$'\ncommand = \"bin/logical-runner {targets}\"\ntarget = \"{capture_1}\"\njoin = \" \"\nparse = \"json-lines\"\n\n",
                "[runner.check.test-ci]\nmatch = '^test-ci:(.+)$'\ncommand = \"bin/logical-runner {targets}\"\ntarget = \"{capture_1}\"\njoin = \" \"\nparse = \"json-lines\"\n\n",
                "[runner.system.verify]\nmatch = '^verify:(.+)$'\ncommand = \"bin/logical-runner {targets}\"\ntarget = \"{capture_1}\"\njoin = \" \"\nparse = \"json-lines\"\n\n",
                "[runner.system.test-ci]\nmatch = '^test-ci:(.+)$'\ncommand = \"bin/logical-runner {targets}\"\ntarget = \"{capture_1}\"\njoin = \" \"\nparse = \"json-lines\"\n",
            ),
        )
        .expect("write config");
        let runner_path = tmp.path().join("bin/logical-runner");
        std::fs::write(
            &runner_path,
            "#!/bin/sh\nfor target in \"$@\"; do printf '{\"target\":\"%s\",\"pass\":true,\"evidence\":\"ok\"}\\n' \"$target\"; done\n",
        )
        .expect("write runner");
        std::fs::set_permissions(&runner_path, std::fs::Permissions::from_mode(0o755))
            .expect("runner permissions");

        let parsed = annotation::parse(&tmp.path().join("specs")).expect("parse annotations");
        let config = LoomConfig::load(LoomConfig::resolve_path(tmp.path())).expect("load config");
        let runners = integrity_runner_specs(&config).expect("compile runners");
        let options = DispatchOptions::default();
        let tier_cwds = TierCwds::default();
        let outcomes = run_check(
            &parsed.annotations,
            &runners,
            &options,
            tmp.path(),
            &tier_cwds,
        )
        .into_iter()
        .chain(run_system(
            &parsed.annotations,
            &runners,
            &options,
            tmp.path(),
            &tier_cwds,
        ))
        .map(|result| result.expect("logical target dispatches through its runner"))
        .collect::<Vec<_>>();
        assert_eq!(outcomes.len(), 4);
        assert!(outcomes.iter().all(|outcome| outcome.verdict.pass));

        let mut output = String::new();
        for target in [
            "verify:example.target",
            "test-ci:example",
            "verify:example.target",
            "test-ci:example",
        ] {
            let payload = serde_json::json!({
                "token": "verifier-bypass",
                "route": "deferred",
                "bonds": ["gate"],
                "target": {"kind": "Annotation", "target_string": target},
                "evidence": "configured verifier completed before review",
            });
            output.push_str(&format!("LOOM_FINDING: {payload}\n"));
        }
        output.push_str("LOOM_CONCERN: {\"summary\":\"Logical runner targets remain valid\"}\n");
        let validator = WorkspaceFindingValidator::new(tmp.path());
        let findings = parse_walk_output(&output, DispatchScope::Tree, &validator)
            .expect("runner-owned targets remain valid during finding parsing");

        assert_eq!(findings.len(), 4);
        assert!(
            findings
                .iter()
                .all(|finding| matches!(&finding.target, FindingTarget::Annotation { .. }))
        );
    }

    #[test]
    fn runner_ownership_is_exact_to_annotation_tier() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("specs")).expect("specs dir");
        std::fs::write(
            tmp.path().join("specs/gate.md"),
            concat!(
                "# Gate\n\n## Success Criteria\n\n",
                "- Check target [check](verify:check-wrong-tier)\n",
                "- System target [system](verify:system-wrong-tier)\n",
            ),
        )
        .expect("write spec");
        std::fs::write(
            tmp.path().join("loom.toml"),
            concat!(
                "[runner.system.check-only]\nmatch = '^verify:check-wrong-tier$'\ncommand = \"true\"\nparse = \"exit-code\"\n\n",
                "[runner.check.system-only]\nmatch = '^verify:system-wrong-tier$'\ncommand = \"true\"\nparse = \"exit-code\"\n",
            ),
        )
        .expect("write config");
        let validator = WorkspaceFindingValidator::new(tmp.path());

        assert!(!validator.annotation_resolves("verify:check-wrong-tier"));
        assert!(!validator.annotation_resolves("verify:system-wrong-tier"));
    }

    #[test]
    fn unowned_missing_command_keeps_actionable_finding_error() {
        use super::super::finding::{DispatchScope, FindingParseError, WalkOutput};

        const TARGET: &str = "loom-missing-ad-hoc-command-7c3a --verify";
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("specs")).expect("specs dir");
        std::fs::write(
            tmp.path().join("specs/gate.md"),
            format!("# Gate\n\n## Success Criteria\n\n- Ad hoc check [check]({TARGET})\n"),
        )
        .expect("write spec");
        let output = format!(
            "LOOM_FINDING: {{\"token\":\"verifier-bypass\",\"route\":\"deferred\",\"bonds\":[\"gate\"],\"target\":{{\"kind\":\"Annotation\",\"target_string\":\"{TARGET}\"}},\"evidence\":\"missing command\"}}\nLOOM_CONCERN: {{\"summary\":\"Missing command\"}}\n"
        );
        let walk = WalkOutput::from_stdout(
            &output,
            DispatchScope::Tree,
            &WorkspaceFindingValidator::new(tmp.path()),
        );

        assert_eq!(walk.finding_errors().len(), 1);
        match &walk.finding_errors()[0] {
            FindingParseError::UnresolvedTarget { detail, .. } => assert_eq!(
                detail,
                "annotation target `loom-missing-ad-hoc-command-7c3a --verify` does not resolve"
            ),
            other => panic!("expected unresolved annotation target, got {other:?}"),
        }
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
