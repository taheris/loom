#![cfg(test)]

use super::*;
use std::os::unix::process::ExitStatusExt;

fn contract(parser: Parser, targets: &[&str]) -> Contract {
    Contract {
        parser,
        tier: Lane::Check,
        targets: targets
            .iter()
            .map(|name| Name::try_from((*name).to_owned()).unwrap())
            .collect(),
    }
}

fn captured(code: i32, stdout: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(code << 8),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    }
}

#[test]
fn mixed_results_do_not_inherit_the_aggregate_failure() {
    let contract = contract(Parser::JsonLines, &["passing", "failing", "skipped"]);
    let output = captured(
        1,
        concat!(
            "{\"target\":\"passing\",\"pass\":true,\"evidence\":\"actually passed\"}\n",
            "{\"target\":\"failing\",\"pass\":false,\"evidence\":\"actually failed\"}\n",
            "{\"target\":\"skipped\",\"pass\":false,\"skipped\":true,\"evidence\":\"prerequisite absent\"}\n",
        ),
    );
    let report = Report::interpret(&contract, &output).unwrap();
    assert_eq!(report.aggregate().outcome(), Verdict::Fail);
    assert!(report.result("passing").unwrap().pass);
    assert_eq!(report.result("failing").unwrap().outcome(), Verdict::Fail);
    assert!(report.result("skipped").unwrap().skipped);
    assert!(!report.result("skipped").unwrap().accepted());
}

#[test]
fn incomplete_report_is_rejected_before_any_projection() {
    let contract = contract(Parser::JsonLines, &["passing", "missing"]);
    let output = captured(
        0,
        "{\"target\":\"passing\",\"pass\":true,\"evidence\":\"ok\"}",
    );
    assert!(Report::interpret(&contract, &output).is_err());
}

#[test]
fn unexpected_report_identity_is_not_admitted() {
    let contract = contract(Parser::JsonLines, &["expected"]);
    let output = captured(
        0,
        "{\"target\":\"other\",\"pass\":true,\"evidence\":\"ok\"}",
    );
    assert!(Report::interpret(&contract, &output).is_err());
}

#[test]
fn contradictory_producer_status_survives_passing_projection() {
    let contract = contract(Parser::JsonLines, &["passing", "failing"]);
    let output = captured(
        0,
        concat!(
            "{\"target\":\"passing\",\"pass\":true,\"evidence\":\"ok\"}\n",
            "{\"target\":\"failing\",\"pass\":false,\"evidence\":\"failed\"}\n",
        ),
    );
    let report = Report::interpret(&contract, &output).unwrap();
    assert_eq!(report.aggregate().outcome(), Verdict::Fail);
    assert!(report.result("passing").unwrap().producer_error.is_some());
    assert!(!report.result("passing").unwrap().accepted());
}

#[test]
fn actual_nextest_plus_identity_format_preserves_individual_results() {
    // Captured from a dependency-free Rust crate using pinned Nextest 0.9.146.
    let contract = contract(
        Parser::LibtestJson,
        &[
            "report-format-probe::report_format_probe$passes",
            "report-format-probe::report_format_probe$fails",
            "report-format-probe::report_format_probe$ignored",
        ],
    );
    let output = captured(
        100,
        concat!(
            "{\"type\":\"test\",\"event\":\"started\",\"name\":\"report-format-probe::report_format_probe$passes\"}\n",
            "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"report-format-probe::report_format_probe$passes\",\"exec_time\":0.006504711}\n",
            "{\"type\":\"test\",\"event\":\"failed\",\"name\":\"report-format-probe::report_format_probe$fails\",\"stdout\":\"intentional report-format probe\"}\n",
            "{\"type\":\"test\",\"event\":\"ignored\",\"name\":\"report-format-probe::report_format_probe$ignored\"}\n",
        ),
    );
    let report = Report::interpret(&contract, &output).unwrap();
    assert_eq!(report.aggregate().outcome(), Verdict::Fail);
    assert!(
        report
            .result("report-format-probe::report_format_probe$passes")
            .unwrap()
            .pass
    );
    assert_eq!(
        report
            .result("report-format-probe::report_format_probe$fails")
            .unwrap()
            .evidence,
        "intentional report-format probe"
    );
    assert!(
        report
            .result("report-format-probe::report_format_probe$ignored")
            .unwrap()
            .skipped
    );
}

#[test]
fn junit_failure_and_skip_are_not_replaced_by_suite_status() {
    let contract = contract(
        Parser::JunitXml,
        &["suite.passing", "suite.failing", "suite.skipped"],
    );
    let output = captured(
        1,
        "<testsuite><testcase classname=\"suite\" name=\"passing\"/><testcase classname=\"suite\" name=\"failing\"><failure message=\"broken\">detail</failure></testcase><testcase classname=\"suite\" name=\"skipped\"><skipped message=\"absent\"/></testcase></testsuite>",
    );
    let report = Report::interpret(&contract, &output).unwrap();
    assert!(report.result("suite.passing").unwrap().pass);
    assert_eq!(
        report.result("suite.failing").unwrap().outcome(),
        Verdict::Fail
    );
    assert!(report.result("suite.skipped").unwrap().skipped);
}

#[test]
fn a_shared_passing_projection_cannot_replace_the_independent_baseline() {
    use crate::provider::tracked::{Observation, Observed, Outcome};
    let contract = contract(Parser::JsonLines, &["passing", "failing"]);
    let output = captured(
        1,
        concat!(
            "{\"target\":\"passing\",\"pass\":true,\"evidence\":\"ok\"}\n",
            "{\"target\":\"failing\",\"pass\":false,\"evidence\":\"failed\"}\n",
        ),
    );
    let report = Report::interpret(&contract, &output).unwrap();
    let root = Name::try_from("baseline".to_owned()).unwrap();
    let observed = Observed::reported(root.clone(), report, Vec::<Observation>::new());
    let target = Name::try_from("passing".to_owned()).unwrap();
    let passing = observed.project(target.clone(), Some(&target)).unwrap();
    assert!(matches!(passing.outcome(), Outcome::Pass));
    let baseline = passing.project(root, None).unwrap();
    assert!(matches!(baseline.outcome(), Outcome::Fail { .. }));
}

#[test]
fn completed_nonzero_status_is_retained_when_no_case_actually_failed() {
    let contract = contract(Parser::JsonLines, &["passing"]);
    let output = captured(
        2,
        "{\"target\":\"passing\",\"pass\":true,\"evidence\":\"ok\"}",
    );
    let report = Report::interpret(&contract, &output).unwrap();
    assert_eq!(report.failure_code(), 2);
    assert!(report.aggregate().producer_error.is_some());
    assert!(!report.result("passing").unwrap().accepted());
}

#[test]
fn termination_status_cannot_admit_a_passing_projection() {
    let contract = contract(Parser::JsonLines, &["passing", "failing"]);
    let output = captured(
        143,
        concat!(
            "{\"target\":\"passing\",\"pass\":true,\"evidence\":\"ok\"}\n",
            "{\"target\":\"failing\",\"pass\":false,\"evidence\":\"failed\"}\n",
        ),
    );
    let report = Report::interpret(&contract, &output).unwrap();
    assert_eq!(report.failure_code(), 1);
    assert!(!report.result("passing").unwrap().accepted());
}

#[test]
fn public_assertion_fails_but_keeps_the_failed_producer_report() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("supervisor.sh");
    std::fs::write(&script, SUPERVISOR).unwrap();
    let report = directory.path().join("report");
    let status = std::process::Command::new("bash")
        .arg(&script)
        .args(["bash", "-c", "printf 'retained failure'; exit 100"])
        .env_remove("LOOM_NATIVE_REPORT")
        .env("out", &report)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(report.join("loom-report/status")).unwrap(),
        "100\n"
    );
    let marker = directory.path().join("assertion");
    let status = std::process::Command::new("bash")
        .arg(&script)
        .env("LOOM_NATIVE_REPORT", &report)
        .env("out", &marker)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(100));
    assert_eq!(
        std::fs::read_to_string(report.join("loom-report/stdout")).unwrap(),
        "retained failure"
    );
}

#[test]
fn public_assertion_succeeds_only_after_its_producer_succeeded() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("supervisor.sh");
    std::fs::write(&script, SUPERVISOR).unwrap();
    let report = directory.path().join("report");
    assert!(
        std::process::Command::new("bash")
            .arg(&script)
            .args(["bash", "-c", "printf 'actually ran'"])
            .env_remove("LOOM_NATIVE_REPORT")
            .env("out", &report)
            .status()
            .unwrap()
            .success()
    );
    let marker = directory.path().join("assertion");
    assert!(
        std::process::Command::new("bash")
            .arg(&script)
            .env("LOOM_NATIVE_REPORT", &report)
            .env("out", &marker)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "0\n");
}

fn unmaterialized_report_node() -> Node {
    let output = Name::try_from("out".to_owned()).unwrap();
    let path = StorePath::try_from(
        "/nix/store/00000000000000000000000000000000-unmaterialized-report".to_owned(),
    )
    .unwrap();
    Node {
        builder: "bash".into(),
        system: Name::try_from("x86_64-linux".to_owned()).unwrap(),
        args: vec![],
        env: [("LOOM_REPORT_SUPERVISOR".into(), path.as_str().to_owned())].into(),
        structured: None,
        fixed: false,
        sources: BTreeSet::new(),
        dependencies: BTreeMap::new(),
        outputs: [(output, path)].into(),
    }
}

#[test]
fn shell_injection_environment_is_rejected_before_loading_a_supervisor() {
    let path = StorePath::try_from(
        "/nix/store/00000000000000000000000000000000-unmaterialized-report.drv".to_owned(),
    )
    .unwrap();
    for name in [
        "BASH_ENV",
        "ENV",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "BASH_FUNC_read%%",
    ] {
        let mut reporter = unmaterialized_report_node();
        reporter.env.insert(name.to_owned(), "injected".into());
        assert!(matches!(
            super::require_link(&reporter, &path, &reporter),
            Err(Error::Result)
        ));
    }
}

#[test]
fn public_assertion_cannot_change_output_names() {
    let path = StorePath::try_from(
        "/nix/store/00000000000000000000000000000000-unmaterialized-report.drv".to_owned(),
    )
    .unwrap();
    let reporter = unmaterialized_report_node();
    let mut root = reporter.clone();
    let output = root.outputs.pop_first().unwrap().1;
    root.outputs
        .insert(Name::try_from("other".to_owned()).unwrap(), output);
    assert!(matches!(
        super::require_link(&root, &path, &reporter),
        Err(Error::Result)
    ));
}

#[test]
fn fresh_recipe_reinstantiation_preserves_actual_native_derivation_fields() {
    let tool = crate::provider::nix::locate_tool().unwrap();
    let bash = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join("bash"))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    let store = StorePath::root(&bash).unwrap();
    let literal = |value: &str| serde_json::to_string(value).unwrap();
    for outputs in ["", "outputs = [ \"out\" ];"] {
        let expression = format!(
            "let builder = builtins.appendContext {} {{ {} = {{ path = true; }}; }}; dependency = builtins.derivation {{ name = \"report-recipe-dependency\"; system = {}; inherit builder; args = [ \"-c\" \"printf dependency > \\\"$out\\\"\" ]; }}; in (builtins.derivation {{ name = \"report-recipe-roundtrip\"; system = {}; inherit builder dependency; args = [ \"-c\" \"printf source-owned > \\\"$out\\\"\" ]; __structuredAttrs = false; preferLocalBuild = true; allowSubstitutes = false; {outputs} }}).drvPath",
            literal(bash.to_str().unwrap()),
            literal(store.as_str()),
            literal(&crate::runner::result::current_platform()),
            literal(&crate::runner::result::current_platform()),
        );
        let evaluate = |expression: &str| {
            let bytes = crate::provider::nix::query(
                &tool,
                &[
                    "eval",
                    "--raw",
                    "--expr",
                    expression,
                    "--offline",
                    "--option",
                    "allow-import-from-derivation",
                    "false",
                ],
            )
            .unwrap();
            StorePath::try_from(String::from_utf8(bytes).unwrap()).unwrap()
        };
        let original_path = evaluate(&expression);
        let mut original_graph = crate::provider::nix::read_graph(&tool, &original_path).unwrap();
        let mut original = original_graph
            .remove(&original_path)
            .unwrap()
            .as_ref()
            .clone();
        let fresh_path =
            evaluate(&super::fresh_expression(&original, "actual-recipe-roundtrip").unwrap());
        let mut fresh_graph = crate::provider::nix::read_graph(&tool, &fresh_path).unwrap();
        let mut fresh = fresh_graph.remove(&fresh_path).unwrap().as_ref().clone();
        assert_ne!(original_path, fresh_path);
        assert_eq!(
            fresh.env.remove("LOOM_REPORT_NONCE").as_deref(),
            Some("actual-recipe-roundtrip")
        );
        assert_ne!(original.outputs, fresh.outputs);
        for node in [&mut original, &mut fresh] {
            for name in node.outputs.keys() {
                node.env.remove(name.as_str());
            }
            node.outputs.clear();
        }
        assert_eq!(original, fresh);
        assert_eq!(original_graph, fresh_graph);
    }
}

#[test]
fn infrastructure_exit_without_semantic_failure_has_no_report() {
    let contract = contract(Parser::JsonLines, &["passing"]);
    let output = captured(
        78,
        "{\"target\":\"passing\",\"pass\":true,\"evidence\":\"ok\"}",
    );
    assert!(Report::interpret(&contract, &output).is_err());
}
