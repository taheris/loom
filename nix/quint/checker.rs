//! Finite explicit-state checking of deterministic choices compiled from the Quint model.
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use quint_evaluator::Verbosity;
use quint_evaluator::evaluator::{CompiledExpr, CompiledExprWithArgs, Env, Interpreter};
use quint_evaluator::ir::{
    LookupDefinition, LookupTable, OpQualifier, QuintDeclaration, QuintEx, QuintOutput,
};
use quint_evaluator::value::{Value, ValueRef};
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Budget {
    steps: usize,
    seconds: u64,
    states: usize,
    transitions: usize,
    bytes: usize,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Trace {
    root: usize,
    choices: Vec<(usize, usize)>,
}
#[derive(Serialize)]
struct Receipt {
    status: &'static str,
    backend: &'static str,
    roots: usize,
    depth: usize,
    states: usize,
    transitions: usize,
}
struct Machine {
    interpreter: Interpreter,
    env: Env,
    init: CompiledExprWithArgs,
    step: CompiledExprWithArgs,
    safety: CompiledExpr,
    root_count: CompiledExpr,
    sizes: CompiledExpr,
}
fn require(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(message.into()) }
}
fn document(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(u64::try_from(limit)? + 1)
        .read_to_end(&mut bytes)?;
    require(
        bytes.len() <= limit,
        "incomplete: input document budget exhausted",
    )?;
    Ok(bytes)
}
fn integer(value: &Value) -> Result<usize> {
    match value.view() {
        ValueRef::Int(n) => Ok(usize::try_from(n)?),
        _ => Err("finite nonnegative integer required".into()),
    }
}
fn boolean(value: &Value) -> Result<bool> {
    match value.view() {
        ValueRef::Bool(v) => Ok(v),
        _ => Err("boolean action or invariant required".into()),
    }
}
fn deterministic(expr: &QuintEx, table: &LookupTable, seen: &mut HashSet<u64>) -> Result<()> {
    if !seen.insert(expr.id()) {
        return Ok(());
    }
    if let Some(LookupDefinition::Definition(QuintDeclaration::QuintOpDef(def))) =
        table.get(&expr.id())
    {
        require(
            def.qualifier != OpQualifier::Nondet,
            "undeclared nondeterminism in bounded checking interface",
        )?;
        deterministic(&def.expr, table, seen)?;
    }
    match expr {
        QuintEx::QuintApp { opcode, args, .. } => {
            require(
                !matches!(
                    opcode.as_ref(),
                    "oneOf" | "actionAny" | "q::test" | "q::testOnce" | "then" | "reps" | "next"
                ),
                "undeclared nondeterminism or multiple transitions in bounded checking interface",
            )?;
            for arg in args {
                deterministic(arg, table, seen)?;
            }
        }
        QuintEx::QuintLambda { expr, .. } => deterministic(expr, table, seen)?,
        QuintEx::QuintLet { opdef, expr, .. } => {
            require(
                opdef.qualifier != OpQualifier::Nondet,
                "undeclared nondeterminism in bounded checking interface",
            )?;
            deterministic(&opdef.expr, table, seen)?;
            deterministic(expr, table, seen)?;
        }
        _ => {}
    }
    Ok(())
}
// Admit only wrappers whose complete choice domains are exactly those enumerated below.
fn contract(
    expr: &QuintEx,
    table: &LookupTable,
    names: &mut HashMap<String, u64>,
) -> Result<String> {
    match expr {
        QuintEx::QuintInt { value, .. } => Ok(value.to_string()),
        QuintEx::QuintName { id, name } => {
            let symbol: &str = name.as_ref();
            require(
                table
                    .get(id)
                    .is_some_and(|def| names.get(symbol) == Some(&def.id())),
                "bounded wrapper reference mismatch",
            )?;
            Ok(symbol.to_owned())
        }
        QuintEx::QuintApp { id, opcode, args } => {
            let symbol: &str = opcode.as_ref();
            if let Some(def) = table.get(id) {
                require(
                    names.get(symbol) == Some(&def.id()),
                    "bounded wrapper operator mismatch",
                )?;
            }
            let args = args
                .iter()
                .map(|arg| contract(arg, table, names))
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("{symbol}({})", args.join(",")))
        }
        QuintEx::QuintLet { opdef, expr, .. } => {
            require(
                opdef.qualifier == OpQualifier::Nondet
                    && !names.contains_key(opdef.name.as_ref() as &str),
                "bounded wrapper requires distinct finite choices",
            )?;
            let domain = contract(&opdef.expr, table, names)?;
            names.insert(opdef.name.to_string(), opdef.id);
            Ok(format!(
                "nondet {}={domain};{}",
                opdef.name,
                contract(expr, table, names)?
            ))
        }
        _ => Err("unsupported bounded wrapper; no coverage claim".into()),
    }
}
impl Machine {
    fn new(mut output: QuintOutput) -> Result<Self> {
        let names = [
            "boundedInit",
            "boundedStep",
            "safety",
            "boundedRootCount",
            "boundedFamilySizes",
        ];
        let mut definitions = Vec::new();
        for name in names {
            let def = output.find_definition_by_name(name)?.clone();
            deterministic(&def.expr, &output.table, &mut HashSet::new())?;
            definitions.push(def);
        }
        let names = definitions
            .iter()
            .map(|def| (def.name.to_string(), def.id))
            .collect::<HashMap<_, _>>();
        for (name, expected) in [
            (
                "symbolicInit",
                "nondet root=oneOf(to(0,isub(boundedRootCount,1)));boundedInit(root)",
            ),
            (
                "step",
                "nondet family=oneOf(to(0,isub(length(boundedFamilySizes),1)));nondet index=oneOf(to(0,isub(nth(boundedFamilySizes,family),1)));boundedStep(family,index)",
            ),
        ] {
            let def = output.find_definition_by_name(name)?;
            require(
                contract(&def.expr, &output.table, &mut names.clone())? == expected,
                "bounded wrapper differs from complete enumerated choice relation",
            )?;
        }
        let mut ids = Vec::new();
        let mut next = output
            .table
            .keys()
            .copied()
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("IR identifier overflow")?;
        for def in &definitions {
            ids.push(next);
            output.table.insert(
                next,
                LookupDefinition::Definition(QuintDeclaration::QuintOpDef(def.clone())),
            );
            next = next.checked_add(1).ok_or("IR identifier overflow")?;
        }
        let mut interpreter = Interpreter::new(output.table);
        let env = Env::with_rand_state(interpreter.var_storage.clone(), 42, Verbosity::default());
        let init = interpreter.compile_op(&ids[0], "boundedInit");
        let step = interpreter.compile_op(&ids[1], "boundedStep");
        let safety = interpreter.compile_def(&LookupDefinition::Definition(
            QuintDeclaration::QuintOpDef(definitions[2].clone()),
        ));
        let root_count = interpreter.compile_def(&LookupDefinition::Definition(
            QuintDeclaration::QuintOpDef(definitions[3].clone()),
        ));
        let sizes = interpreter.compile_def(&LookupDefinition::Definition(
            QuintDeclaration::QuintOpDef(definitions[4].clone()),
        ));
        Ok(Self {
            interpreter,
            env,
            init,
            step,
            safety,
            root_count,
            sizes,
        })
    }
    fn load(&mut self, value: &Value) -> Result<()> {
        let ValueRef::Record(record) = value.view() else {
            return Err("state record required".into());
        };
        let storage = self.interpreter.var_storage.borrow_mut();
        for register in storage.vars.values() {
            let name = register.borrow().name.clone();
            register.borrow_mut().value =
                Some(record.get(&name).ok_or("incomplete state record")?.clone());
        }
        for register in storage.next_vars.values() {
            register.borrow_mut().value = None;
        }
        for cache in &storage.caches_to_clear {
            cache.replace(None);
        }
        self.env.trace.clear();
        self.env.diagnostics.clear();
        Ok(())
    }
    fn state(&self) -> Value {
        self.interpreter.var_storage.borrow().as_record()
    }
    fn roots(&mut self) -> Result<usize> {
        integer(&self.root_count.execute(&mut self.env)?)
    }
    fn initialize(&mut self, root: usize) -> Result<Value> {
        let before = self.env.rand.get_state();
        let result = self
            .init
            .execute(&mut self.env, vec![Value::int(i64::try_from(root)?)])?;
        require(
            self.env.rand.get_state() == before,
            "undeclared runtime nondeterminism",
        )?;
        require(boolean(&result)?, "disabled model initializer")?;
        self.env.shift();
        Ok(self.state())
    }
    fn sizes(&mut self) -> Result<Vec<usize>> {
        let before = self.env.rand.get_state();
        let sizes = self.sizes.execute(&mut self.env)?;
        require(
            self.env.rand.get_state() == before,
            "undeclared runtime nondeterminism",
        )?;
        let ValueRef::List(values) = sizes.view() else {
            return Err("finite action family list required".into());
        };
        require(
            !values.is_empty() && values.len() <= 1024,
            "incomplete: action family budget exhausted",
        )?;
        values.iter().map(integer).collect()
    }
    fn transition(&mut self, family: usize, index: usize) -> Result<Option<Value>> {
        let before = self.env.rand.get_state();
        let result = self.step.execute(
            &mut self.env,
            vec![
                Value::int(i64::try_from(family)?),
                Value::int(i64::try_from(index)?),
            ],
        )?;
        require(
            self.env.rand.get_state() == before,
            "undeclared runtime nondeterminism",
        )?;
        if !boolean(&result)? {
            return Ok(None);
        }
        self.env.shift();
        Ok(Some(self.state()))
    }
    fn safe(&mut self) -> Result<bool> {
        let before = self.env.rand.get_state();
        let result = boolean(&self.safety.execute(&mut self.env)?)?;
        require(
            self.env.rand.get_state() == before,
            "undeclared runtime nondeterminism",
        )?;
        Ok(result)
    }
    fn replay(&mut self, trace: &Trace) -> Result<Value> {
        require(
            trace.root < self.roots()? && trace.choices.len() <= 1024,
            "invalid or over-budget replay",
        )?;
        let mut state = self.initialize(trace.root)?;
        for (family, index) in &trace.choices {
            self.load(&state)?;
            let sizes = self.sizes()?;
            require(
                *family < sizes.len() && *index < sizes[*family],
                "replay action outside declared alphabet",
            )?;
            state = self
                .transition(*family, *index)?
                .ok_or("disabled replay transition")?;
        }
        Ok(state)
    }
}
fn violation(machine: &mut Machine, trace: &Trace) -> Result<()> {
    if machine.safe()? {
        return Ok(());
    }
    let value =
        serde_json::json!({"status":"violation", "trace":trace, "state":machine.state().to_itf()});
    serde_json::to_writer(std::io::stdout().lock(), &value)?;
    Err("counterexample: Quint model invariant violated; no bounded success".into())
}
fn search(machine: &mut Machine, budget: &Budget) -> Result<Receipt> {
    let deadline = Instant::now() + Duration::from_secs(budget.seconds);
    let roots = machine.roots()?;
    require(
        roots > 0 && roots <= 1024,
        "invalid finite initial-state inventory",
    )?;
    let mut seen = HashSet::new();
    let mut frontier = Vec::new();
    let mut bytes = 0usize;
    for root in 0..roots {
        let state = machine.initialize(root)?;
        let trace = Trace {
            root,
            choices: Vec::new(),
        };
        violation(machine, &trace)?;
        let size = serde_json::to_vec(&state.to_itf())?.len();
        require(
            seen.len() < budget.states && size <= budget.bytes.saturating_sub(bytes),
            "incomplete: state/byte budget exhausted",
        )?;
        bytes += size;
        seen.insert(state.clone());
        frontier.push((state, trace));
    }
    let mut transitions = 0usize;
    for _ in 0..budget.steps {
        let mut next = Vec::new();
        for (state, trace) in frontier {
            machine.load(&state)?;
            let sizes = machine.sizes()?;
            for (family, size) in sizes.into_iter().enumerate() {
                for index in 0..size {
                    require(
                        Instant::now() < deadline,
                        "incomplete: checking deadline exhausted",
                    )?;
                    require(
                        transitions < budget.transitions,
                        "incomplete: transition budget exhausted",
                    )?;
                    transitions += 1;
                    machine.load(&state)?;
                    let Some(successor) = machine.transition(family, index)? else {
                        continue;
                    };
                    let mut path = trace.clone();
                    path.choices.push((family, index));
                    violation(machine, &path)?;
                    if seen.contains(&successor) {
                        continue;
                    }
                    let size = serde_json::to_vec(&successor.to_itf())?.len();
                    require(
                        seen.len() < budget.states && size <= budget.bytes.saturating_sub(bytes),
                        "incomplete: state/byte budget exhausted",
                    )?;
                    bytes += size;
                    seen.insert(successor.clone());
                    next.push((successor, path));
                }
            }
        }
        frontier = next;
    }
    require(
        Instant::now() < deadline,
        "incomplete: checking deadline exhausted",
    )?;
    Ok(Receipt {
        status: "complete",
        backend: "quint-rust-explicit-state-v1",
        roots,
        depth: budget.steps,
        states: seen.len(),
        transitions,
    })
}
fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    require(
        args.len() == 2 || args.len() == 3,
        "usage: loom-quint-checker <compiled.json> <campaign.json> [replay.json]",
    )?;
    let output: QuintOutput =
        serde_json::from_slice(&document(Path::new(&args[0]), 64 * 1024 * 1024)?)?;
    let campaign: serde_json::Value =
        serde_json::from_slice(&document(Path::new(&args[1]), 64 * 1024)?)?;
    let budget: Budget = serde_json::from_value(
        campaign
            .get("exhaustive")
            .ok_or("missing finite exhaustive budget")?
            .clone(),
    )?;
    require(
        budget.steps > 0
            && budget.steps <= 16
            && budget.seconds > 0
            && budget.seconds <= 3600
            && budget.states > 0
            && budget.states <= 1_000_000
            && budget.transitions > 0
            && budget.transitions <= 10_000_000
            && budget.bytes > 0
            && budget.bytes <= 512 * 1024 * 1024,
        "invalid finite exhaustive budget",
    )?;
    let mut machine = Machine::new(output)?;
    if args.len() == 3 {
        let trace: Trace = serde_json::from_slice(&document(Path::new(&args[2]), 1024 * 1024)?)?;
        machine.replay(&trace)?;
        violation(&mut machine, &trace)?;
        return Ok(());
    }
    let receipt = search(&mut machine, &budget)?;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &receipt)?;
    writeln!(stdout)?;
    Ok(())
}
