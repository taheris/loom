use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
};

fn nodes<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(object) => {
            out.push(value);
            for child in object.values() {
                nodes(child, out);
            }
        }
        Value::Array(array) => {
            for child in array {
                nodes(child, out);
            }
        }
        _ => {}
    }
}
fn one<'a>(values: impl Iterator<Item = &'a Value>) -> Result<&'a Value> {
    let found: Vec<_> = values.collect();
    ensure!(
        found.len() == 1,
        "expected one mutation target, found {}",
        found.len()
    );
    Ok(found[0])
}
fn definition<'a>(program: &'a Value, name: &str) -> Result<&'a Value> {
    let module = one(program["modules"]
        .as_array()
        .context("missing Quint modules")?
        .iter()
        .filter(|m| m["name"] == "gate"))?;
    one(module["declarations"]
        .as_array()
        .context("missing gate declarations")?
        .iter()
        .filter(|d| d["name"] == name))
}
fn app(value: &Value, opcode: &str) -> bool {
    value["kind"] == "app" && value["opcode"] == opcode
}
fn field(value: &Value, name: &str) -> bool {
    app(value, "with") && value["args"][1]["value"] == name
}
fn target(program: &Value, name: &str, predicate: impl Fn(&Value) -> bool) -> Result<Value> {
    let mut all = Vec::new();
    nodes(definition(program, name)?, &mut all);
    Ok(one(all.into_iter().filter(|n| predicate(n)))?.clone())
}
fn fresh_id(program: &Value) -> Result<u64> {
    let mut all = Vec::new();
    nodes(program, &mut all);
    all.into_iter()
        .filter_map(|v| v["id"].as_u64())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .context("Quint identifier overflow")
}
fn replace_nodes(value: &mut Value, old: &Value, replacement: &Value) -> Result<usize> {
    if value.is_object() && value["id"] == old["id"] && value["kind"] == old["kind"] {
        ensure!(value == old, "inconsistent copies of mutation target");
        *value = replacement.clone();
        return Ok(1);
    }
    let mut count = 0;
    match value {
        Value::Object(object) => {
            for child in object.values_mut() {
                count += replace_nodes(child, old, replacement)?;
            }
        }
        Value::Array(array) => {
            for child in array {
                count += replace_nodes(child, old, replacement)?;
            }
        }
        _ => {}
    }
    Ok(count)
}
fn replace(program: &mut Value, old: &Value, mut replacement: Value) -> Result<()> {
    replacement["id"] = old["id"].clone();
    ensure!(
        replace_nodes(&mut program["table"], old, &replacement)? > 0,
        "mutation target absent from resolved lookup table"
    );
    Ok(())
}
#[derive(Clone, Copy)]
enum Mutation {
    AlwaysReject,
    StaleSigner,
    MissingResource,
    WrongSubject,
    ReadinessCoverage,
    WorkerWrite,
    NondurableCheckpoint,
}
impl Mutation {
    const ALL: [Self; 7] = [
        Self::AlwaysReject,
        Self::StaleSigner,
        Self::MissingResource,
        Self::WrongSubject,
        Self::ReadinessCoverage,
        Self::WorkerWrite,
        Self::NondurableCheckpoint,
    ];
    const fn witness(self) -> &'static str {
        match self {
            Self::AlwaysReject => "witnessPublicationTest",
            Self::StaleSigner => "cacheTrustReadmissionTest",
            Self::MissingResource | Self::WrongSubject => "referenceResourceOmissionDetectedTest",
            Self::ReadinessCoverage => "referencePassingReadinessBlocksPublicationTest",
            Self::WorkerWrite => "cacheWorkerCanonicalMutatorsRejectedTest",
            Self::NondurableCheckpoint => "cacheStagedCheckpointCannotPermitDeletionTest",
        }
    }
    fn apply(self, program: &mut Value) -> Result<()> {
        let (old, replacement) = match self {
            Self::AlwaysReject => {
                let parent = target(program, "attemptPush", |n| field(n, "accepted"))?;
                let old = parent["args"][2].clone();
                ensure!(
                    old["name"] == "authority",
                    "unexpected accepted-field construction"
                );
                (old, json!({"kind":"bool","value":false}))
            }
            Self::StaleSigner => {
                let old = target(program, "admissible", |n| {
                    let mut all = Vec::new();
                    nodes(n, &mut all);
                    app(n, "or")
                        && ["imported", "signer"]
                            .iter()
                            .all(|s| all.iter().any(|c| c["kind"] == "str" && c["value"] == *s))
                })?;
                (old, json!({"kind":"bool","value":true}))
            }
            Self::MissingResource => {
                let old = target(program, "manifest", |n| app(n, "union"))?;
                ensure!(
                    old["args"][1]["opcode"] == "Set",
                    "unexpected manifest union"
                );
                let replacement = old["args"][0].clone();
                (old, replacement)
            }
            Self::WrongSubject => {
                let comparison = target(program, "unit", |n| app(n, "ilt"))?;
                let old = comparison["args"][1].clone();
                ensure!(
                    old["kind"] == "int" && old["value"] == 2,
                    "unexpected responsibility comparison"
                );
                (old, json!({"kind":"int","value":1}))
            }
            Self::ReadinessCoverage => {
                let old = target(program, "complete", |n| app(n, "Set"))?;
                ensure!(
                    old["args"]
                        .as_array()
                        .context("missing readiness set")?
                        .iter()
                        .map(|n| n["value"].clone())
                        .collect::<Vec<_>>()
                        == vec![json!("pass"), json!("pending")],
                    "unexpected complete readiness set"
                );
                let mut replacement = old.clone();
                replacement["args"].as_array_mut().context("missing readiness arguments")?.push(json!({"kind":"str","id":fresh_id(program)?,"value":"UnneededPendingMarker"}));
                (old, replacement)
            }
            Self::WorkerWrite => {
                let old = definition(program, "trustedWriter")?["expr"].clone();
                ensure!(
                    app(&old, "contains"),
                    "unexpected trusted writer expression"
                );
                (old, json!({"kind":"bool","value":true}))
            }
            Self::NondurableCheckpoint => {
                let old = target(program, "consolidate", |n| field(n, "stagedCheckpoint"))?;
                ensure!(
                    old["args"][2]["name"] == "events",
                    "unexpected checkpoint staging expression"
                );
                let mut inner = old.clone();
                let id = fresh_id(program)?;
                inner["id"] = json!(id);
                inner["args"][1] = json!({"kind":"str","id":id.checked_add(1).context("Quint identifier overflow")?,"value":"checkpoint"});
                let mut replacement = old.clone();
                replacement["args"][0] = inner;
                (old, replacement)
            }
        };
        replace(program, &old, replacement)
    }
}
fn execute(
    program: &Value,
    name: &str,
    passed: bool,
    backend: &Path,
    failure: &Path,
) -> Result<()> {
    let table = program["table"]
        .as_object()
        .context("missing Quint lookup table")?;
    let test = one(table.values().filter(|d| d["name"] == name))?;
    let request = serde_json::to_vec(
        &json!({"name":name,"test_def":test,"table":table,"seed":42,"max_samples":32,"verbosity":0}),
    )?;
    let result = (|| -> Result<()> {
        let mut child = Command::new("timeout")
            .args(["--kill-after=5s", "15s"])
            .arg(backend)
            .arg("test-from-stdin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .context("missing Quint backend input pipe")?;
        let (output, written) = thread::scope(|scope| {
            let request = &request;
            let writer = scope.spawn(move || stdin.write_all(request));
            let output = child.wait_with_output();
            (output, writer.join())
        });
        written.map_err(|_| anyhow::anyhow!("Quint request writer failed"))??;
        let output = output?;
        ensure!(
            output.status.success(),
            "Quint backend failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output: Value = serde_json::from_slice(&output.stdout)?;
        ensure!(
            output["name"] == name
                && output["status"] == if passed { "passed" } else { "failed" }
                && output["nsamples"].as_u64().is_some_and(|n| n > 0),
            "unexpected Quint mutation outcome: {output}"
        );
        if passed {
            ensure!(
                output["errors"] == json!([]),
                "positive Quint witness errors: {output}"
            );
        } else {
            let errors = output["errors"]
                .as_array()
                .context("missing Quint errors")?;
            ensure!(
                errors.iter().map(|e| e["code"].clone()).collect::<Vec<_>>()
                    == vec![json!("QNT508")],
                "mutation failed outside named expect assertion: {output}"
            );
        }
        Ok(())
    })();
    if result.is_err() {
        fs::write(failure, &request)?;
    }
    result
}
pub fn run(ir: &Path, backend: &Path, failure: &Path) -> Result<()> {
    let program: Value = serde_json::from_slice(&fs::read(ir)?)?;
    ensure!(
        program["errors"] == json!([]),
        "mutation input did not typecheck"
    );
    for mutation in Mutation::ALL {
        let name = mutation.witness();
        execute(&program, name, true, backend, failure)?;
        let mut mutant = program.clone();
        mutation.apply(&mut mutant)?;
        execute(&mutant, name, false, backend, failure)?;
        writeln!(
            io::stderr().lock(),
            "mutation witness {name}: positive and negative controls passed"
        )?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_updates_all_consistent_table_copies_without_changing_identity() {
        let old = json!({"kind":"name","id":7,"name":"authority"});
        let mut program = json!({"table":{"a":old,"b":{"nested":old}}});
        replace(&mut program, &old, json!({"kind":"bool","value":false})).unwrap();
        assert_eq!(
            program["table"]["a"],
            json!({"kind":"bool","id":7,"value":false})
        );
        assert_eq!(program["table"]["a"], program["table"]["b"]["nested"]);
    }
    #[test]
    fn replacement_rejects_missing_and_inconsistent_resolved_targets() {
        let old = json!({"kind":"name","id":7,"name":"authority"});
        let mut absent = json!({"table":{}});
        assert!(replace(&mut absent, &old, json!({"kind":"bool","value":false})).is_err());
        let mut inconsistent = json!({"table":{"a":{"kind":"name","id":7,"name":"other"}}});
        assert!(
            replace(
                &mut inconsistent,
                &old,
                json!({"kind":"bool","value":false})
            )
            .is_err()
        );
    }
}
