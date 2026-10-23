//! Reports come only from freshly realized, constructor-supervised executions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{ExitStatus, Output};

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

use super::{Error, Name, Node, StorePath};
use crate::Verdict;
use crate::dispatch::{VerifierVerdict, interpret_captured_output};
use crate::runner::{BuiltinParser, RunnerSpec};

pub(super) const SUPERVISOR: &str = "#!/usr/bin/env bash\nset -euo pipefail\n: \"${out:?native report output is required}\"\nif [[ -n \"${LOOM_NATIVE_REPORT:-}\" ]]; then\n  read -r code < \"$LOOM_NATIVE_REPORT/loom-report/status\"\n  printf '%s\\n' \"$code\" > \"$out\"\n  exit \"$code\"\nfi\nmkdir -p \"$out/loom-report\"\nif \"$@\" > \"$out/loom-report/stdout\" 2> \"$out/loom-report/stderr\"; then\n  code=0\nelse\n  code=$?\nfi\nprintf '%s\\n' \"$code\" > \"$out/loom-report/status\"\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Parser {
    JsonLines,
    JunitXml,
    LibtestJson,
}

impl Parser {
    const fn builtin(self) -> BuiltinParser {
        match self {
            Self::JsonLines => BuiltinParser::JsonLines,
            Self::JunitXml => BuiltinParser::JunitXml,
            Self::LibtestJson => BuiltinParser::LibtestJson,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Lane {
    #[default]
    Check,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Contract {
    pub parser: Parser,
    pub targets: BTreeSet<Name>,
    #[serde(default)]
    pub tier: Lane,
}

/// Captured result data is not a deserializable observation or an execution grant.
#[derive(Debug, Clone)]
pub struct Report {
    aggregate: VerifierVerdict,
    results: BTreeMap<String, VerifierVerdict>,
    producer_code: i32,
}

impl Report {
    pub(crate) const fn members(&self) -> &BTreeMap<String, VerifierVerdict> {
        &self.results
    }

    pub(crate) const fn aggregate(&self) -> &VerifierVerdict {
        &self.aggregate
    }

    pub(crate) fn failure_code(&self) -> i32 {
        if self
            .results
            .values()
            .any(|result| result.outcome() == Verdict::Fail)
            || self.producer_code == 0
        {
            1
        } else {
            self.producer_code
        }
    }

    pub(crate) fn result(&self, target: &str) -> Result<&VerifierVerdict, Error> {
        self.results.get(target).ok_or_else(|| Error::Report {
            detail: format!("supervised execution omitted `{target}`"),
        })
    }

    pub(super) fn read(output: &StorePath, contract: &Contract) -> Result<Self, Error> {
        Self::read_directory(Path::new(output.as_str()), contract)
    }

    pub(super) fn read_directory(output: &Path, contract: &Contract) -> Result<Self, Error> {
        let directory = output.join("loom-report");
        let status = std::fs::read_to_string(directory.join("status"))?;
        let status: u8 = status.trim().parse().map_err(|_| Error::Result)?;
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt;
            ExitStatus::from_raw(i32::from(status) << 8)
        };
        #[cfg(not(unix))]
        return Err(Error::Report {
            detail: "supervised Nix execution requires Unix status semantics".into(),
        });
        let captured = Output {
            status,
            stdout: std::fs::read(directory.join("stdout"))?,
            stderr: std::fs::read(directory.join("stderr"))?,
        };
        Self::interpret(contract, &captured)
    }

    fn interpret(contract: &Contract, captured: &Output) -> Result<Self, Error> {
        if contract.targets.is_empty() {
            return Err(Error::Report {
                detail: "supervised execution has no captured case identities".into(),
            });
        }
        let parsed = crate::runner::parse_runner_output(
            contract.parser.builtin(),
            &String::from_utf8_lossy(&captured.stdout),
            &String::from_utf8_lossy(&captured.stderr),
            captured.status.success(),
        )
        .map_err(|error| Error::Report {
            detail: error.to_string(),
        })?;
        if parsed.keys().any(|target| {
            !contract
                .targets
                .iter()
                .any(|expected| expected.as_str() == target)
        }) {
            return Err(Error::Report { detail: "supervised producer reported an identity outside its complete captured catalogue".into() });
        }
        let targets: Vec<_> = contract
            .targets
            .iter()
            .map(|target| target.as_str().to_owned())
            .collect();
        let spec = RunnerSpec::compile(
            "checked-report",
            None,
            "",
            "{name}",
            " ",
            contract.parser.builtin(),
            None,
        )
        .map_err(|error| Error::Report {
            detail: error.to_string(),
        })?;
        // No policy is supplied by producer JSON. The owning driver applies its
        // validated applicability/skip policy to these retained observations.
        let mut results: BTreeMap<_, _> = targets
            .iter()
            .cloned()
            .zip(interpret_captured_output(
                &spec,
                "checked supervised execution",
                captured,
                &targets,
            ))
            .map(|(target, result)| result.map(|result| (target, result)))
            .collect::<Result<_, _>>()
            .map_err(|error| match error {
                crate::dispatch::DispatchError::Infrastructure { code, evidence, .. } => {
                    Error::ReportInfrastructure {
                        code,
                        detail: evidence,
                    }
                }
                error => Error::Report {
                    detail: error.to_string(),
                },
            })?;
        let producer_code = captured.status.code().unwrap_or(128);
        if producer_code >= 128 {
            for result in results.values_mut() {
                result.producer_error = Some(format!(
                    "producer terminated with status {producer_code}; successful projections are not admitted"
                ));
            }
        }
        let failed = results
            .values()
            .any(|result| result.outcome() == Verdict::Fail || result.producer_error.is_some());
        let outcome = if failed {
            Verdict::Fail
        } else if captured.status.success() {
            Verdict::Pass
        } else {
            Verdict::Skipped
        };
        let mut aggregate = VerifierVerdict::from_outcome(
            outcome,
            String::from_utf8_lossy(&captured.stderr).into_owned(),
        );
        aggregate.producer_error = results
            .values()
            .find_map(|result| result.producer_error.clone());
        Ok(Self {
            aggregate,
            results,
            producer_code,
        })
    }
}

/// Reinstantiate the captured recipe, not its project evaluator. Exact graph
/// comparison below remains mandatory before any realization.
fn fresh_expression(node: &Node, nonce: &str) -> Result<String, Error> {
    let mut attributes: BTreeMap<String, serde_json::Value> = node
        .env
        .iter()
        .map(|(name, value)| (name.clone(), serde_json::Value::String(value.clone())))
        .collect();
    for name in node.outputs.keys() {
        attributes.remove(name.as_str());
    }
    if node.env.contains_key("outputs") {
        attributes.insert(
            "outputs".into(),
            serde_json::json!(node.outputs.keys().map(Name::as_str).collect::<Vec<_>>()),
        );
    }
    attributes.insert("builder".into(), serde_json::json!(node.builder));
    attributes.insert("system".into(), serde_json::json!(node.system));
    attributes.insert("args".into(), serde_json::json!(node.args));
    attributes.insert("LOOM_REPORT_NONCE".into(), serde_json::json!(nonce));
    for name in [
        "__structuredAttrs",
        "__impure",
        "__contentAddressed",
        "__noChroot",
        "__darwinAllowLocalNetworking",
        "allowSubstitutes",
        "preferLocalBuild",
    ] {
        if let Some(value) = node.env.get(name) {
            let boolean = match value.as_str() {
                "" => false,
                "1" => true,
                _ => {
                    return Err(Error::Report {
                        detail: format!("unsupported captured boolean attribute `{name}`"),
                    });
                }
            };
            attributes.insert(name.into(), serde_json::json!(boolean));
        }
    }
    let mut context = BTreeMap::new();
    for path in &node.sources {
        context.insert(path.as_str(), serde_json::json!({"path":true}));
    }
    for (path, outputs) in &node.dependencies {
        let entry = context
            .entry(path.as_str())
            .or_insert_with(|| serde_json::json!({}));
        entry.as_object_mut().ok_or(Error::Result)?.insert(
            "outputs".into(),
            serde_json::json!(outputs.iter().map(Name::as_str).collect::<Vec<_>>()),
        );
    }
    let literal = |value: &serde_json::Value| -> Result<String, Error> {
        Ok(serde_json::to_string(&serde_json::to_string(value)?)?.replace("${", "\\${"))
    };
    let attributes = literal(&serde_json::json!(attributes))?;
    let context = literal(&serde_json::json!(context))?;
    Ok(format!(
        "let context = builtins.fromJSON {context}; bind = value: builtins.appendContext value context; attributes = builtins.fromJSON {attributes}; in (builtins.derivation (attributes // {{ builder = bind attributes.builder; args = map bind attributes.args; }})).drvPath"
    ))
}

impl super::Graph {
    pub(super) fn fresh_report(&self) -> Result<(StorePath, super::Nodes), Error> {
        let nonce = tempfile::tempdir()?;
        let nonce = nonce.path().to_str().ok_or(Error::Encoding)?;
        let reporter = self.reporter.as_ref().ok_or(Error::Result)?;
        let original = self.nodes.get(reporter).ok_or(Error::Root)?;
        let expression = fresh_expression(original, nonce)?;
        let bytes = super::query(
            &self.tool,
            &[
                "eval",
                "--raw",
                "--expr",
                &expression,
                "--offline",
                "--no-write-lock-file",
                "--option",
                "allow-import-from-derivation",
                "false",
            ],
        )?;
        let requested =
            StorePath::try_from(String::from_utf8(bytes).map_err(|_| Error::Encoding)?)?;
        let nodes = super::read_graph(&self.tool, &requested)?;
        let fresh = nodes.get(&requested).ok_or(Error::Root)?;
        if requested == *reporter
            || requested == self.root
            || nodes.len() + 1 != self.nodes.len()
            || fresh.env.get("LOOM_REPORT_NONCE").map(String::as_str) != Some(nonce)
            || fresh.outputs.keys().ne(original.outputs.keys())
            || matches!(self.presence(&fresh.outputs)?, super::Presence::Present)
            || nodes
                .iter()
                .any(|(path, node)| path != &requested && self.nodes.get(path) != Some(node))
        {
            return Err(Error::Report {
                detail:
                    "fresh realization changed the admitted dependency graph or reused an output"
                        .into(),
            });
        }
        let normalize = |node: &Node| {
            let mut node = node.clone();
            for name in node.outputs.keys() {
                node.env.remove(name.as_str());
            }
            node.outputs.clear();
            node.env.remove("LOOM_REPORT_NONCE");
            node
        };
        if normalize(original) != normalize(fresh) {
            return Err(Error::Report {
                detail: "fresh realization changed the admitted execution contract".into(),
            });
        }
        Ok((requested, nodes))
    }
}

pub(super) fn require_supervisor(path: &str) -> Result<(), Error> {
    StorePath::try_from(path.to_owned())?;
    if std::fs::read_to_string(path)? != SUPERVISOR {
        return Err(Error::Report {
            detail: "report supervisor differs from the checked constructor".into(),
        });
    }
    Ok(())
}

pub(super) fn require_link(root: &Node, path: &StorePath, reporter: &Node) -> Result<(), Error> {
    if reporter.env.contains_key("LOOM_NATIVE_REPORT")
        || reporter.env.iter().any(|(name, value)| {
            !value.is_empty()
                && (matches!(
                    name.as_str(),
                    "BASH_ENV" | "ENV" | "LD_PRELOAD" | "LD_LIBRARY_PATH"
                ) || name.starts_with("BASH_FUNC_"))
        })
        || reporter.outputs.len() != 1
        || root.outputs.keys().ne(reporter.outputs.keys())
    {
        return Err(Error::Result);
    }
    let output = reporter
        .outputs
        .iter()
        .find(|(name, _)| name.as_str() == "out")
        .ok_or(Error::Result)?
        .1;
    let supervisor = if let Some(supervisor) = reporter.env.get("LOOM_REPORT_SUPERVISOR") {
        supervisor
    } else {
        reporter.args.first().ok_or(Error::Result)?
    };
    require_supervisor(supervisor)?;
    let mut expected = reporter.clone();
    expected.args = vec![supervisor.clone()];
    expected
        .env
        .insert("LOOM_NATIVE_REPORT".into(), output.as_str().to_owned());
    expected
        .env
        .insert("LOOM_NATIVE_REPORT_INPUTS".into(), reporter.args.join(" "));
    for (name, output) in &root.outputs {
        expected
            .env
            .insert(name.as_str().to_owned(), output.as_str().to_owned());
    }
    expected.outputs.clone_from(&root.outputs);
    expected.dependencies.insert(
        path.clone(),
        [Name::try_from("out".to_owned()).map_err(|_| Error::Result)?].into(),
    );
    if &expected != root {
        return Err(Error::Report { detail: "public native assertion differs from the fixed report-status check or adds execution prerequisites".into() });
    }
    Ok(())
}

pub(super) fn require_execution(node: &Node, contract: &Contract) -> Result<(), Error> {
    let [supervisor, program, ..] = node.args.as_slice() else {
        return Err(Error::Result);
    };
    require_supervisor(supervisor)?;
    StorePath::root(Path::new(program))?;
    if Path::new(&node.builder)
        .file_name()
        .is_none_or(|name| name != "bash")
        || contract.targets.is_empty()
        || node.env.get("LOOM_REPORT_CONTRACT") != Some(&serde_json::to_string(contract)?)
    {
        return Err(Error::Report {
            detail: "report execution is not bound to its fixed supervisor and complete identities"
                .into(),
        });
    }
    Ok(())
}
