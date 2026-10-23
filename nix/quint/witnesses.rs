//! Every named Quint witness receives fresh upstream test state.
use std::error::Error;
use std::fs::File;
use std::io::{Read, Write};

use quint_evaluator::Verbosity;
use quint_evaluator::ir::{LookupTable, OpQualifier, QuintDeclaration, QuintError, QuintModule};
use quint_evaluator::progress::no_report;
use quint_evaluator::tester::{TestCase, TestStatus};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct TypedWitnesses {
    modules: Vec<QuintModule>,
    table: LookupTable,
    errors: Vec<serde_json::Value>,
}

#[derive(Default, Serialize)]
struct Receipt {
    passed: Vec<String>,
    failed: Vec<String>,
    ignored: Vec<String>,
    errors: Vec<quint_evaluator::ir::QuintError>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("usage: loom-quint-witnesses <typed IR> <seed> <samples>".into());
    }
    let seed = args[1].parse::<u64>()?;
    let samples = args[2].parse::<usize>()?;
    if samples == 0 || samples > 1_000_000 {
        return Err("invalid witness sample budget".into());
    }
    let mut bytes = Vec::new();
    File::open(&args[0])?
        .take(67_108_865)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 67_108_864 {
        return Err("witness input document budget exhausted".into());
    }
    let output: TypedWitnesses = serde_json::from_slice(&bytes)?;
    if !output.errors.is_empty() {
        return Err("invalid witness compilation".into());
    }
    let module = output
        .modules
        .iter()
        .find(|module| module.name.as_str() == "witness")
        .ok_or("witness module not found")?;
    let mut definitions = Vec::new();
    for declaration in &module.declarations {
        if let QuintDeclaration::QuintOpDef(definition) = declaration {
            if definition.qualifier == OpQualifier::Run && definition.name.ends_with("Test") {
                let resolved = output
                    .table
                    .get(&definition.id)
                    .ok_or("witness lookup definition not found")?;
                definitions.push((definition.name.to_string(), resolved.clone()));
            }
        }
    }
    if definitions.is_empty() || definitions.len() > 10_000 {
        return Err("empty or oversized witness suite".into());
    }
    let (name, definition) = definitions[0].clone();
    let mut test = TestCase {
        name,
        test_def: definition,
        table: output.table,
    };
    let mut receipt = Receipt::default();
    for (name, definition) in definitions {
        test.name = name;
        test.test_def = definition;
        // The upstream API owns fresh state, RNG repetition and instance overrides.
        let result = test.execute(Some(seed), samples, no_report(), Verbosity::default());
        match result.status {
            TestStatus::Passed => receipt.passed.push(result.name),
            TestStatus::Failed => receipt.failed.push(result.name),
            TestStatus::Ignored => receipt.ignored.push(result.name),
        }
        receipt.errors.extend(result.errors);
    }
    let complete =
        receipt.failed.is_empty() && receipt.ignored.is_empty() && receipt.errors.is_empty();
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &receipt)?;
    writeln!(stdout)?;
    if !complete {
        return Err("witness suite failed or incomplete".into());
    }
    Ok(())
}
