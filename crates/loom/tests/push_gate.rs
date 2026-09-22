//! Live inspection cannot publish, mutate Beads, or certify a partial review.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use loom_driver::identifier::{BeadId, MoleculeId, SpecLabel};
    use loom_driver::lock::LockManager;
    use loom_driver::state::CacheDb;
    use loom_gate::{
        GateRun, HandoffEvidence, append_gate_run_lifecycle_events, parse_gate_runs_from_jsonl,
    };

    struct Fixture {
        dir: tempfile::TempDir,
        home: PathBuf,
        bin: PathBuf,
        state: PathBuf,
        base: String,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            loom_driver::git::init_test_repo_with_integration(dir.path()).unwrap();
            let base = git(dir.path(), &["rev-parse", "HEAD"]);
            let home = dir.path().join(".loom/fixture");
            let bin = home.join("bin");
            let state = home.join("bd-state");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::create_dir_all(&state).unwrap();
            std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_bd-shim"), bin.join("bd")).unwrap();
            executable(
                &bin.join("wrix"),
                &format!(
                    "set -euo pipefail\nprintf '%s\\n' \"$*\" >> \"$LOOM_TEST_WRIX_LOG\"\nexec {:?} \"$@\"\n",
                    env!("CARGO_BIN_EXE_mock-loom-agent")
                ),
            );
            executable(
                &bin.join("prek"),
                "set -euo pipefail\nprintf '%s\\n' \"$*\" >> \"$LOOM_TEST_PREK_LOG\"\nexit 0\n",
            );
            std::fs::write(home.join("profiles.json"), r#"{"base":{"pi":{"ref":"fixture-pi","source":"/fixture/image","source_kind":"nix-descriptor"}}}"#).unwrap();
            let bead = state.join("lm-work");
            std::fs::create_dir_all(&bead).unwrap();
            for (field, value) in [
                ("title", "active work"),
                ("description", "intent"),
                ("status", "open"),
                ("priority", "2"),
                ("issue_type", "epic"),
                ("labels", "loom:active\nspec:acme\n"),
            ] {
                std::fs::write(bead.join(field), value).unwrap();
            }
            let fixture = Self {
                dir,
                home,
                bin,
                state,
                base,
            };
            fixture.spec("acme", "- Finding status output\n");
            fixture.commit();
            let db = fixture.db();
            db.rebuild(
                fixture.root(),
                &loom_driver::testing::epic_fixture(
                    MoleculeId::new("lm-work").unwrap(),
                    SpecLabel::new("acme").unwrap(),
                    Some(fixture.base.clone()),
                )
                .unwrap(),
            )
            .unwrap();
            db.set_iteration(&MoleculeId::new("lm-work").unwrap(), 7)
                .unwrap();
            std::fs::write(
                fixture.root().join(".loom/marker.json"),
                "existing operator marker\n",
            )
            .unwrap();
            fixture
        }

        fn root(&self) -> &Path {
            self.dir.path()
        }
        fn db(&self) -> CacheDb {
            CacheDb::open(self.root().join(".loom/cache.db")).unwrap()
        }
        fn range(&self) -> String {
            format!(
                "{}..{}",
                self.base,
                git(self.root(), &["rev-parse", "HEAD"])
            )
        }
        fn spec(&self, label: &str, body: &str) {
            std::fs::create_dir_all(self.root().join("specs")).unwrap();
            std::fs::write(
                self.root().join(format!("specs/{label}.md")),
                format!("# {label}\n\n## Success Criteria\n\n{body}"),
            )
            .unwrap();
        }
        fn file(&self, path: &str, body: &str) {
            let path = self.root().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        fn commit(&self) {
            loom_driver::git::commit_all_in(self.root(), "fixture content").unwrap();
        }
        fn command(&self, args: &[&str], mode: &str) -> Command {
            let ambient = std::env::var_os("PATH").unwrap_or_default();
            let mut paths = vec![self.bin.clone()];
            paths.extend(std::env::split_paths(&ambient));
            let mut command = Command::new(env!("CARGO_BIN_EXE_loom"));
            command
                .args(["--workspace"])
                .arg(self.root())
                .args(["--host-key", "--agent", "pi", "gate"])
                .args(args)
                .env("PATH", std::env::join_paths(paths).unwrap())
                .env("LOOM_WRIX_BIN", self.bin.join("wrix"))
                .env("LOOM_TEST_WRIX_LOG", self.home.join("wrix.log"))
                .env("LOOM_TEST_PREK_LOG", self.home.join("prek.log"))
                .env("LOOM_TEST_AGENT_MODE", mode)
                .env("LOOM_PROFILES_MANIFEST", self.home.join("profiles.json"))
                .env("BD_STATE_DIR", &self.state)
                .env("XDG_STATE_HOME", self.home.join("user-state"))
                .env("GIT_TRACE2_EVENT", self.home.join("git.jsonl"))
                .env_remove("LOOM_INSIDE")
                .env_remove("LOOM_WRIX_SPAWN_BIN")
                .env_remove("LOOM_REVIEW_INSPECTION_ONLY")
                .env_remove("LOOM_REVIEW_EMIT_STDOUT")
                .env_remove("LOOM_REVIEW_SPEC_LABEL")
                .env_remove("LOOM_REVIEW_PHASE_WHEN_MILLIS")
                .env_remove("LOOM_REVIEW_VERIFIED_LOG");
            loom_test_support::scrub_git_local_env(&mut command);
            command
        }
        fn events(&self) -> Vec<loom_events::AgentEvent> {
            self.logs()
                .into_iter()
                .flat_map(|path| {
                    std::fs::read_to_string(path)
                        .unwrap()
                        .lines()
                        .map(|line| serde_json::from_str(line).unwrap())
                        .collect::<Vec<_>>()
                })
                .collect()
        }
        fn logs(&self) -> Vec<PathBuf> {
            let path = self.root().join(".loom/logs/review");
            if !path.exists() {
                return vec![];
            }
            std::fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
                .collect()
        }
        fn prompt(&self) -> String {
            let prompts: Vec<_> = self
                .events()
                .into_iter()
                .filter_map(|event| match event {
                    loom_events::AgentEvent::AgentInput { text, .. } => Some(text),
                    _ => None,
                })
                .collect();
            assert_eq!(
                prompts.len(),
                1,
                "one inspection session, never a recovery loop"
            );
            prompts.into_iter().next().unwrap()
        }
        fn review_runs(&self) -> Vec<GateRun> {
            self.logs()
                .iter()
                .flat_map(|path| parse_gate_runs_from_jsonl(path))
                .collect()
        }
        fn verified_log(&self) -> PathBuf {
            let path = self.home.join("verify.jsonl");
            let run = GateRun::successful_verify(
                self.range(),
                loom_driver::git::head_tree_oid_sync(self.root())
                    .unwrap()
                    .to_string(),
                blake3::hash(&std::fs::read(self.root().join(".pre-commit-config.yaml")).unwrap())
                    .to_hex()
                    .to_string(),
                path.clone(),
                loom_gate::pre_push_hook_coverage_from_config(self.root()).unwrap(),
            );
            append_gate_run_lifecycle_events(&path, &run).unwrap();
            assert!(
                HandoffEvidence::from_runs(parse_gate_runs_from_jsonl(&path))
                    .verified
                    .is_some()
            );
            path
        }
        fn assert_read_only(&self, before: &BTreeMap<PathBuf, Vec<u8>>) {
            assert_eq!(
                &bead_files(&self.state),
                before,
                "inspection cannot write Beads state"
            );
            let bd_log =
                std::fs::read_to_string(self.state.join(".invocations.log")).unwrap_or_default();
            assert!(
                !bd_log.is_empty(),
                "bd instrumentation must observe real reads"
            );
            for line in bd_log.lines() {
                assert!(
                    matches!(
                        line.split_whitespace().next(),
                        Some("list" | "show" | "find")
                    ),
                    "unexpected bd operation: {line}"
                );
            }
            let wrix_log = std::fs::read_to_string(self.home.join("wrix.log")).unwrap();
            assert_eq!(
                wrix_log.lines().count(),
                1,
                "only one backend spawn: {wrix_log}"
            );
            assert!(wrix_log.starts_with("spawn "), "no beads push: {wrix_log}");
            let trace = std::fs::read_to_string(self.home.join("git.jsonl")).unwrap();
            let git_commands: Vec<_> = trace
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .filter(|value| value["event"] == "cmd_name")
                .collect();
            assert!(
                !git_commands.is_empty(),
                "Git trace must record actual commands"
            );
            for command in git_commands {
                assert!(
                    !matches!(
                        command["name"].as_str(),
                        Some(
                            "push"
                                | "commit"
                                | "merge"
                                | "rebase"
                                | "reset"
                                | "clean"
                                | "stash"
                                | "update-ref"
                        )
                    ),
                    "inspection mutated Git: {command}"
                );
            }
            assert_eq!(
                std::fs::read_to_string(self.root().join(".loom/marker.json")).unwrap(),
                "existing operator marker\n"
            );
            assert_eq!(
                self.db()
                    .work_epic(&MoleculeId::new("lm-work").unwrap())
                    .unwrap()
                    .unwrap()
                    .iteration_count,
                7
            );
        }
    }

    fn executable(path: &Path, body: &str) {
        std::fs::write(path, loom_test_support::bash_script(body)).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn git(workspace: &Path, args: &[&str]) -> String {
        let mut command = Command::new("git");
        loom_test_support::scrub_git_local_env(&mut command);
        let output = command
            .arg("-C")
            .arg(workspace)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
    fn bead_files(state: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        walkdir::WalkDir::new(state)
            .into_iter()
            .map(Result::unwrap)
            .filter(|entry| entry.file_type().is_file() && entry.file_name() != ".invocations.log")
            .map(|entry| {
                (
                    entry.path().strip_prefix(state).unwrap().to_owned(),
                    std::fs::read(entry.path()).unwrap(),
                )
            })
            .collect()
    }
    fn success(output: Output) -> String {
        assert!(
            output.status.success(),
            "stdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn review_commands_are_read_only_without_internal_environment_flags() {
        for command in ["review", "rubric", "judge", "audit"] {
            for tree in [false, true] {
                for mode in ["complete-marker", "finding-concern"] {
                    let fixture = Fixture::new();
                    let before = bead_files(&fixture.state);
                    let range = fixture.range();
                    let args = if tree {
                        vec![command, "--tree"]
                    } else {
                        vec![command, "--diff", &range]
                    };
                    let stdout = success(fixture.command(&args, mode).output().unwrap());
                    assert!(
                        stdout.trim_end().ends_with("LOOM_COMPLETE")
                            || stdout.lines().last().unwrap().starts_with("LOOM_CONCERN:"),
                        "terminal protocol: {stdout}"
                    );
                    fixture.assert_read_only(&before);
                    assert!(
                        fixture.review_runs().is_empty(),
                        "unverified inspection cannot produce ReviewedScope"
                    );
                }
            }
        }
    }

    #[test]
    fn review_bead_context_takes_no_work_root_lock() {
        let fixture = Fixture::new();
        let manager =
            LockManager::with_state_home(fixture.root(), fixture.home.join("user-state")).unwrap();
        let _guard = manager
            .acquire_work_root(&BeadId::new("lm-work").unwrap())
            .unwrap();
        let before = bead_files(&fixture.state);
        success(
            fixture
                .command(
                    &["review", "--diff", &fixture.range(), "--bead", "lm-work"],
                    "complete-marker",
                )
                .output()
                .unwrap(),
        );
        assert!(fixture.prompt().contains("Intent/context bead: `lm-work`"));
        fixture.assert_read_only(&before);
    }

    #[test]
    fn judge_target_selects_exact_annotation_and_preserves_selector() {
        let fixture = Fixture::new();
        let target = "../judges/shared.sh#selected";
        fixture.spec("acme", &format!("- wanted [judge]({target})\n- sibling [judge](../judges/shared.sh#sibling)\n- unrelated [judge](../judges/missing.sh#other)\n- unrelated test [test](../tests/missing.sh)\n"));
        fixture.file(
            "judges/shared.sh",
            "selected() { echo selected; }\nsibling() { echo sibling; }\n",
        );
        fixture.commit();
        let before = bead_files(&fixture.state);
        success(
            fixture
                .command(&["judge", "--target", target], "complete-marker")
                .output()
                .unwrap(),
        );
        let prompt = fixture.prompt();
        assert!(prompt.contains(&format!("--target {target}")));
        assert!(prompt.contains("selected() { echo selected; }"));
        assert!(!prompt.contains("../judges/shared.sh#sibling"));
        assert!(!prompt.contains("../judges/missing.sh#other"));
        assert!(!prompt.contains("## Review Dimensions"));
        fixture.assert_read_only(&before);
        assert!(fixture.review_runs().is_empty());
    }

    #[test]
    fn judge_target_crosses_context_labels_and_deduplicates_shared_declarations() {
        for context in [None, Some("acme")] {
            let fixture = Fixture::new();
            let target = "../judges/wanted.sh::selected";
            for label in ["beta", "gamma"] {
                fixture.spec(label, &format!("- wanted [judge]({target})\n"));
            }
            fixture.file("judges/wanted.sh", "UNIQUE_SELECTED_RUBRIC");
            fixture.commit();
            let mut command = fixture.command(&["judge", "--target", target], "complete-marker");
            if let Some(context) = context {
                command.env("LOOM_REVIEW_SPEC_LABEL", context);
            }
            success(command.output().unwrap());
            let prompt = fixture.prompt();
            assert_eq!(prompt.matches("UNIQUE_SELECTED_RUBRIC").count(), 1);
            for label in ["beta", "gamma"] {
                assert!(prompt.contains(&format!("in specs/{label}.md")));
            }
        }
    }

    #[test]
    fn judge_target_rejects_unknown_partial_and_wrong_tier_matches_before_dispatch() {
        let fixture = Fixture::new();
        fixture.spec(
            "acme",
            "- selected [judge](../judge.sh#selected)\n- check [check](true)\n",
        );
        fixture.file("judge.sh", "selected() {}\n");
        fixture.commit();
        for target in ["../judge.sh", "../judge.sh#select", "../judge.sh#*", "true"] {
            let output = fixture
                .command(&["judge", "--target", target], "complete-marker")
                .output()
                .unwrap();
            assert!(
                !output.status.success(),
                "target {target} cannot select a judge"
            );
            assert!(!fixture.home.join("wrix.log").exists());
            assert!(fixture.events().is_empty());
        }
    }

    #[test]
    fn full_review_rejects_stale_verified_fingerprint_before_dispatch() {
        let fixture = Fixture::new();
        let log = fixture.verified_log();
        fixture.file(".pre-commit-config.yaml", "repos: []\n");
        let output = fixture
            .command(&["review", "--diff", &fixture.range()], "complete-marker")
            .env("LOOM_REVIEW_VERIFIED_LOG", log)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("does not match verified scope"));
        assert!(!fixture.home.join("wrix.log").exists());
        assert!(fixture.review_runs().is_empty());
    }

    #[test]
    fn judge_files_does_not_apply_test_input_intersection() {
        let fixture = Fixture::new();
        fixture.spec("acme", "- first [judge](../judges/one.sh#first)\n");
        fixture.spec("beta", "- second [judge](../judges/two.sh#second)\n");
        fixture.file("judges/one.sh", "FIRST_JUDGE_RUBRIC");
        fixture.file("judges/two.sh", "SECOND_JUDGE_RUBRIC");
        fixture.commit();
        let before = bead_files(&fixture.state);
        success(
            fixture
                .command(&["judge", "--files", "unrelated.txt"], "complete-marker")
                .output()
                .unwrap(),
        );
        let prompt = fixture.prompt();
        assert!(prompt.contains("FIRST_JUDGE_RUBRIC") && prompt.contains("SECOND_JUDGE_RUBRIC"));
        assert!(prompt.contains("--files") && prompt.contains("unrelated.txt"));
        fixture.assert_read_only(&before);
    }

    #[test]
    fn only_full_matching_verified_review_produces_push_evidence() {
        let fixture = Fixture::new();
        let verified_log = fixture.verified_log();
        let before = bead_files(&fixture.state);
        let stdout = success(
            fixture
                .command(&["review", "--diff", &fixture.range()], "complete-marker")
                .env("LOOM_REVIEW_VERIFIED_LOG", verified_log)
                .env("LOOM_REVIEW_EMIT_STDOUT", "1")
                .output()
                .unwrap(),
        );
        let walk = loom_workflow::review::WalkOutput::from_stdout(
            &stdout,
            loom_workflow::review::DispatchScope::PerBead,
            &loom_workflow::review::WorkspaceFindingValidator::new(fixture.root()),
        );
        assert!(matches!(
            walk.terminal(),
            loom_workflow::review::TerminalSurface::Complete
        ));
        assert!(
            walk.findings().is_empty() && walk.finding_errors().is_empty(),
            "rendered context must not pollute the parent handoff: {stdout}"
        );
        let evidence = HandoffEvidence::from_runs(fixture.review_runs());
        assert_eq!(evidence.reviewed.unwrap().push_range(), fixture.range());
        fixture.assert_read_only(&before);
    }

    #[test]
    fn partial_inspections_cannot_consume_full_verified_scope() {
        for args in [
            vec!["judge", "--diff"],
            vec!["rubric", "--diff"],
            vec!["review", "--tree"],
            vec!["judge", "--files", "README.md"],
            vec!["judge", "--target", "../judge.sh#selected"],
        ] {
            let fixture = Fixture::new();
            fixture.spec("acme", "- selected [judge](../judge.sh#selected)\n");
            fixture.file("judge.sh", "selected() { echo selected; }\n");
            fixture.commit();
            let range = fixture.range();
            let mut args = args;
            if args.last() == Some(&"--diff") {
                args.push(&range);
            }
            let verified_log = fixture.verified_log();
            let output = fixture
                .command(&args, "complete-marker")
                .env("LOOM_REVIEW_VERIFIED_LOG", verified_log)
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("partial inspection"));
            assert!(
                !fixture.home.join("wrix.log").exists(),
                "reject before agent dispatch"
            );
            assert!(fixture.review_runs().is_empty());
        }
    }

    #[test]
    fn rejected_review_walks_never_produce_completed_push_evidence() {
        for mode in [
            "finding-concern",
            "finding-complete",
            "blocked-marker",
            "no-marker",
        ] {
            let fixture = Fixture::new();
            let before = bead_files(&fixture.state);
            let verified_log = fixture.verified_log();
            let stdout = success(
                fixture
                    .command(&["review", "--diff", &fixture.range()], mode)
                    .env("LOOM_REVIEW_VERIFIED_LOG", verified_log)
                    .output()
                    .unwrap(),
            );
            if mode == "finding-complete" {
                assert!(
                    stdout.contains("LOOM_FINDING:")
                        && stdout.trim_end().ends_with("LOOM_COMPLETE"),
                    "malformed pairing fixture: {stdout}"
                );
            }
            let runs = fixture.review_runs();
            assert_eq!(
                runs.len(),
                1,
                "record failed evidence, never silently authorize: {stdout}"
            );
            assert!(HandoffEvidence::from_runs(runs).reviewed.is_none());
            fixture.assert_read_only(&before);
        }
    }

    #[test]
    fn concern_then_complete_live_path_remains_inspection_only() {
        let fixture = Fixture::new();
        let before = bead_files(&fixture.state);
        let stdout = success(
            fixture
                .command(
                    &["review", "--diff", &fixture.range()],
                    "concern-then-complete",
                )
                .output()
                .unwrap(),
        );
        assert!(stdout.contains("inspection Complete"));
        assert!(stdout.trim_end().ends_with("LOOM_COMPLETE"));
        fixture.assert_read_only(&before);
    }

    #[test]
    fn live_llm_commands_use_shared_renderer_pipeline() {
        let fixture = Fixture::new();
        let output = fixture
            .command(&["review", "--diff", &fixture.range()], "complete-marker")
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr.clone()).unwrap();
        let stdout = success(output);
        let transcript = stderr.find("# Post-Epic Review").expect("rendered input");
        let marker = stderr.find("LOOM_COMPLETE").expect("rendered output");
        assert!(transcript < marker);
        assert!(
            !stdout.contains("# Post-Epic Review"),
            "protocol stdout excludes prompt examples"
        );
        assert!(stdout.contains("loom review: inspection Complete"));
        assert!(stdout.trim_end().ends_with("LOOM_COMPLETE"));
    }
}
