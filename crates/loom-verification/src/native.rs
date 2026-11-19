use crate::process;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{self, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    process::Command,
    thread,
};
use walkdir::WalkDir;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
struct StorePath(String);

impl TryFrom<String> for StorePath {
    type Error = anyhow::Error;
    fn try_from(value: String) -> Result<Self> {
        let name = value.strip_prefix("/nix/store/").unwrap_or(&value);
        let (hash, label) = name
            .split_once('-')
            .context("invalid native store identity")?;
        ensure!(
            hash.len() == 32
                && hash
                    .bytes()
                    .all(|c| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&c))
                && !label.is_empty()
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"+._?=-".contains(&c)),
            "noncanonical native store identity: {value}"
        );
        Ok(Self(format!("/nix/store/{name}")))
    }
}
impl From<StorePath> for String {
    fn from(value: StorePath) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
struct Source {
    origin: PathBuf,
    path: StorePath,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Namespace {
    Packages,
    Checks,
    Apps,
}
impl std::fmt::Display for Namespace {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str(match self {
            Self::Packages => "packages",
            Self::Checks => "checks",
            Self::Apps => "apps",
        })
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum Operation {
    Build,
    Lint,
    LintAllTargets,
    Catalogue,
    Test,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum Runtime {
    Nix,
    NativeProcess,
    NativeStore,
    NativeCgroup,
    NativeContainer,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum Purpose {
    Check,
    Artifact,
}
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
struct Target {
    namespace: Namespace,
    attribute: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
struct Definition {
    app: Option<String>,
    root: StorePath,
    sources: Vec<Source>,
    evaluation: Vec<Source>,
    version: u32,
    cargo: Option<Operation>,
    reporter: Option<StorePath>,
    catalogue: Option<StorePath>,
    runtime: Runtime,
    purpose: Purpose,
}
#[derive(Deserialize)]
struct Inputs {
    srcs: Vec<StorePath>,
    drvs: BTreeMap<StorePath, Value>,
}
#[derive(Deserialize)]
struct Node {
    version: u32,
    inputs: Inputs,
    env: BTreeMap<String, Value>,
}
#[derive(Deserialize)]
struct Document {
    version: u32,
    derivations: BTreeMap<StorePath, Node>,
}
type Graph = BTreeMap<StorePath, Node>;
type Files = BTreeMap<PathBuf, PathBuf>;

fn nix(args: &[String]) -> Result<Value> {
    let output = process::success(
        Command::new("nix")
            .args(args)
            .args([
                "--extra-experimental-features",
                "nix-command flakes",
                "--option",
                "accept-flake-config",
                "false",
            ])
            .env_remove("NIX_CONFIG")
            .env_remove("NIX_PATH"),
        120,
    )?;
    Ok(serde_json::from_slice(&output.stdout)?)
}
fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}
fn flake_source(root: &Path, tracked: bool) -> Result<Value> {
    let root = root
        .canonicalize()
        .with_context(|| format!("locating native source {}", root.display()))?;
    let reference = format!(
        "{}{}",
        if tracked { "git+file://" } else { "path:" },
        root.display()
    );
    nix(&strings(&[
        "flake",
        if tracked { "archive" } else { "metadata" },
        &reference,
        "--json",
        "--offline",
        "--no-write-lock-file",
    ]))
}
fn literal(value: &impl Serialize) -> Result<String> {
    Ok(serde_json::to_string(value)?.replace("${", "\\${"))
}
fn evaluate(project: &Value, system: &str, targets: &[Target]) -> Result<Vec<Definition>> {
    let locked = literal(&project["url"])?;
    let requests = literal(&serde_json::to_string(targets)?)?;
    let expression = format!(
        r#"let flake = builtins.getFlake {locked}; system = {system}; select = target: let original = flake.${{target.namespace}}.${{system}}.${{target.attribute}}; value = if target.namespace == "apps" then original.meta.loom.definition else original; in {{ app = if target.namespace == "apps" then original.program else null; root = value.drvPath; sources = value.loom.sources; evaluation = value.loom.evaluation; version = value.loom.version; cargo = value.loom.cargo or null; reporter = value.loom.reporter.drvPath or null; catalogue = value.loom.catalogue or null; runtime = value.loom.runtime or "nix"; purpose = value.loom.purpose or "check"; }}; in map select (builtins.fromJSON {requests})"#,
        system = literal(&system)?
    );
    Ok(serde_json::from_value(nix(&strings(&[
        "eval",
        "--json",
        "--offline",
        "--no-write-lock-file",
        "--expr",
        &expression,
        "--option",
        "allow-import-from-derivation",
        "false",
    ]))?)?)
}
fn targets(project: &Value, system: &str) -> Result<Vec<Target>> {
    let expression = format!(
        r#"let flake = builtins.getFlake {locked}; system = {system}; collect = namespace: let values = flake.${{namespace}}.${{system}} or {{}}; registered = name: builtins.isAttrs values.${{name}} && (if namespace == "apps" then (values.${{name}}.meta or {{}}) ? loom else values.${{name}} ? loom); in assert builtins.all registered (builtins.attrNames values); map (attribute: {{ inherit namespace attribute; }}) (builtins.filter registered (builtins.attrNames values)); in collect "packages" ++ collect "checks" ++ collect "apps""#,
        locked = literal(&project["url"])?,
        system = literal(&system)?
    );
    Ok(serde_json::from_value(nix(&strings(&[
        "eval",
        "--json",
        "--offline",
        "--no-write-lock-file",
        "--expr",
        &expression,
        "--option",
        "allow-import-from-derivation",
        "false",
    ]))?)?)
}

fn execution_sources(
    definition: &Definition,
    graph: &Graph,
    pool: &[Source],
) -> Result<Vec<Source>> {
    let mut pending = vec![&definition.root];
    let mut visited = BTreeSet::new();
    let mut referenced = BTreeSet::new();
    while let Some(path) = pending.pop() {
        if !visited.insert(path.clone()) {
            continue;
        }
        let node = graph
            .get(path)
            .with_context(|| format!("missing native derivation: {}", path.0))?;
        ensure!(
            node.version == 4,
            "unsupported native derivation version: {}",
            path.0
        );
        referenced.extend(node.inputs.srcs.iter().cloned());
        pending.extend(node.inputs.drvs.keys());
        pending.extend(node.inputs.srcs.iter().filter(|p| graph.contains_key(*p)));
    }
    for source in &definition.sources {
        ensure!(
            referenced.contains(&source.path),
            "declared source is not in actual execution closure: {source:?}"
        );
    }
    let mut sources: BTreeMap<_, _> = definition
        .sources
        .iter()
        .map(|s| ((s.origin.clone(), s.path.clone()), s.clone()))
        .collect();
    for source in pool.iter().filter(|s| referenced.contains(&s.path)) {
        sources.insert((source.origin.clone(), source.path.clone()), source.clone());
    }
    Ok(sources.into_values().collect())
}
fn cargo_storage(definition: &Definition, graph: &Graph) -> Result<()> {
    let Some(operation) = &definition.cargo else {
        return Ok(());
    };
    let executable = definition.reporter.as_ref().unwrap_or(&definition.root);
    let environment = &graph
        .get(executable)
        .context("missing Cargo reporter derivation")?
        .env;
    ensure!(
        environment.get("doInstallCargoArtifacts") == Some(&json!("")),
        "Cargo {operation:?} has an unexpected whole-target export: {}",
        executable.0
    );
    if matches!(operation, Operation::Test | Operation::Catalogue) {
        let catalogue = if *operation == Operation::Test {
            definition
                .catalogue
                .as_ref()
                .context("missing Cargo test catalogue")?
        } else {
            executable
        };
        let environment = &graph
            .get(catalogue)
            .context("missing Cargo catalogue derivation")?
            .env;
        for (key, value) in [
            ("doInstallCargoArtifacts", ""),
            ("prePatch", "export CARGO_TARGET_DIR=\"$out/target\""),
            ("dontFixup", "1"),
            ("NIX_NO_SELF_RPATH", "1"),
        ] {
            ensure!(
                environment.get(key) == Some(&json!(value)),
                "Cargo catalogue lost its final-path compiler target: {}",
                catalogue.0
            );
        }
    }
    Ok(())
}
fn source_files(source: &Source) -> Result<Vec<(PathBuf, PathBuf)>> {
    ensure!(
        !source.origin.is_absolute()
            && !source
                .origin
                .components()
                .any(|p| matches!(p, Component::ParentDir)),
        "invalid native origin: {}",
        source.origin.display()
    );
    let origin: PathBuf = source
        .origin
        .components()
        .filter(|part| !matches!(part, Component::CurDir))
        .collect();
    let native = Path::new(&source.path.0);
    let mut files = Vec::new();
    for entry in WalkDir::new(native).sort_by_file_name() {
        let entry = entry?;
        ensure!(
            !entry.file_type().is_symlink(),
            "symlink in native source: {}",
            entry.path().display()
        );
        if entry.file_type().is_file() {
            let origin = if native.is_dir() {
                origin.join(entry.path().strip_prefix(native)?)
            } else {
                origin.clone()
            };
            files.push((origin, entry.into_path()));
        }
    }
    Ok(files)
}
fn match_file(captured: &Path, origin: &Path, native: &Path) -> Result<()> {
    let actual = captured.join(origin);
    ensure!(
        fs::read(&actual)
            .with_context(|| format!("reading captured source {}", actual.display()))?
            == fs::read(native)
                .with_context(|| format!("reading declared source {}", native.display()))?
            && (fs::metadata(actual)?.mode() & 0o111 != 0)
                == (fs::metadata(native)?.mode() & 0o111 != 0),
        "runtime Cargo source differs from declared source: {}",
        origin.display()
    );
    Ok(())
}
fn runtime_sources(captured: &Path, sources: &[Source]) -> Result<()> {
    for source in sources {
        for (origin, native) in source_files(source)? {
            match_file(captured, &origin, &native)?;
        }
    }
    Ok(())
}
fn control_plane(inputs: &Files, required: &BTreeSet<PathBuf>) -> Result<()> {
    let missing: Vec<_> = required
        .iter()
        .filter(|p| !inputs.contains_key(*p))
        .collect();
    ensure!(
        missing.is_empty(),
        "native flake source duties lack declared evaluator/runtime members: {missing:?}"
    );
    Ok(())
}
struct Group {
    inputs: Files,
    targets: Vec<Target>,
    definitions: Vec<Definition>,
}
fn reevaluate(group: &Group, system: &str) -> Result<()> {
    writeln!(
        io::stderr().lock(),
        "Reevaluating independent native partition: {:?}",
        group.targets
    )?;
    let staged = tempfile::Builder::new()
        .prefix("loom-native-partition-")
        .tempdir()?;
    let result = (|| -> Result<()> {
        for (origin, native) in &group.inputs {
            let destination = staged.path().join(origin);
            fs::create_dir_all(destination.parent().context("source has no parent")?)?;
            fs::copy(native, &destination)?;
            fs::set_permissions(
                destination,
                fs::Permissions::from_mode(fs::metadata(native)?.mode()),
            )?;
        }
        let isolated = evaluate(&flake_source(staged.path(), false)?, system, &group.targets)?;
        ensure!(
            isolated == group.definitions,
            "native source partition changes derivation: {:?}",
            group.targets
        );
        Ok(())
    })();
    if result.is_err() {
        let retained = staged.keep();
        writeln!(
            io::stderr().lock(),
            "failed native partition retained at {}",
            retained.display()
        )?;
    }
    result
}

pub fn run(root: &Path) -> Result<()> {
    let project = flake_source(root, true)?;
    let captured = PathBuf::from(
        project["path"]
            .as_str()
            .context("missing captured source")?,
    );
    if let Some(file) = env::var_os("LOOM_TEST_DECLARED_SOURCES_FILE") {
        runtime_sources(
            &captured,
            &serde_json::from_slice::<Vec<Source>>(&fs::read(&file).with_context(|| {
                format!(
                    "reading native source manifest {}",
                    Path::new(&file).display()
                )
            })?)?,
        )?;
    }
    let project = flake_source(&captured, false)?;
    let configuration = nix(&strings(&["config", "show", "--json"]))?;
    let system = configuration["system"]["value"]
        .as_str()
        .context("missing Nix system")?;
    let targets = targets(&project, system)?;
    let declared = if targets.is_empty() {
        vec![]
    } else {
        evaluate(&project, system, &targets)?
    };
    ensure!(
        declared.len() == targets.len(),
        "native metadata omitted requested targets"
    );
    let pool: Vec<_> = declared
        .iter()
        .flat_map(|d| d.sources.iter().chain(&d.evaluation))
        .cloned()
        .collect();
    let graph = if declared.is_empty() {
        BTreeMap::new()
    } else {
        let roots: BTreeSet<_> = declared.iter().map(|d| d.root.0.clone()).collect();
        let mut arguments = strings(&["derivation", "show", "--recursive"]);
        arguments.extend(roots);
        let document: Document = serde_json::from_value(nix(&arguments)?)?;
        ensure!(
            document.version == 4,
            "unsupported native derivation document"
        );
        document.derivations
    };
    let required = WalkDir::new(&captured)
        .into_iter()
        .map(|entry| -> Result<Option<PathBuf>> {
            let entry = entry?;
            Ok(
                if entry.file_type().is_file()
                    && entry.path().extension().is_some_and(|e| e == "nix")
                {
                    Some(entry.path().strip_prefix(&captured)?.to_path_buf())
                } else {
                    None
                },
            )
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    let mut groups: BTreeMap<Vec<PathBuf>, Group> = BTreeMap::new();
    let mut reports = Vec::new();
    let mut expanded_sources = BTreeMap::new();
    let mut compared = BTreeSet::new();
    for (target, definition) in targets.into_iter().zip(declared) {
        writeln!(
            io::stderr().lock(),
            "Checking native source partition: {}.{system}.{}",
            target.namespace,
            target.attribute
        )?;
        ensure!(
            definition.version == 1
                && !(definition.sources.is_empty() && definition.evaluation.is_empty()),
            "missing checked sources: {target:?}"
        );
        cargo_storage(&definition, &graph)?;
        let mut inputs = Files::new();
        for source in execution_sources(&definition, &graph, &pool)?
            .iter()
            .chain(&definition.evaluation)
        {
            let files = match expanded_sources.entry(source.clone()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(source_files(source)?)
                }
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            };
            for (origin, native) in files {
                if compared.insert((origin.clone(), native.clone())) {
                    match_file(&captured, origin, native)?;
                }
                inputs.insert(origin.clone(), native.clone());
            }
        }
        ensure!(
            inputs.contains_key(Path::new("rust-toolchain.toml")),
            "missing actual flake toolchain evaluation input: {target:?}"
        );
        if ["verification-clippy-all-targets", "verification-nextest"]
            .contains(&target.attribute.as_str())
        {
            ensure!(
                inputs.contains_key(Path::new(".gitignore")),
                "staged Git discovery is missing its ignore input"
            );
        }
        control_plane(&inputs, &required)?;
        ensure!(
            inputs.contains_key(Path::new("flake.lock")),
            "missing declared flake lock evaluation input"
        );
        reports.push(json!({"namespace":target.namespace,"target":target.attribute,"system":system,"derivation":definition.root,"runtime_files":inputs.len(),"partition_files":inputs.len()}));
        let group = groups
            .entry(inputs.keys().cloned().collect())
            .or_insert_with(|| Group {
                inputs,
                targets: vec![],
                definitions: vec![],
            });
        group.targets.push(target);
        group.definitions.push(definition);
    }
    let groups: Vec<_> = groups.values().collect();
    for batch in groups.chunks(3) {
        thread::scope(|scope| -> Result<()> {
            let workers: Vec<_> = batch
                .iter()
                .map(|group| scope.spawn(|| reevaluate(group, system)))
                .collect();
            for worker in workers {
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("partition evaluator thread failed"))??;
            }
            Ok(())
        })?;
    }
    serde_json::to_writer_pretty(io::stdout().lock(), &reports)?;
    writeln!(io::stdout().lock())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn store(name: &str) -> StorePath {
        StorePath::try_from(format!("00000000000000000000000000000000-{name}")).unwrap()
    }
    fn definition() -> Definition {
        Definition {
            app: None,
            root: store("root.drv"),
            sources: vec![],
            evaluation: vec![],
            version: 1,
            cargo: None,
            reporter: None,
            catalogue: None,
            runtime: Runtime::NativeProcess,
            purpose: Purpose::Check,
        }
    }
    fn node(dependencies: Vec<StorePath>, sources: Vec<StorePath>) -> Node {
        Node {
            version: 4,
            inputs: Inputs {
                srcs: sources,
                drvs: dependencies.into_iter().map(|p| (p, Value::Null)).collect(),
            },
            env: BTreeMap::new(),
        }
    }
    #[test]
    fn native_store_metadata_retains_the_closed_runtime_protocol() {
        let mut source = definition();
        source.runtime = Runtime::NativeStore;
        source.cargo = Some(Operation::Test);
        let encoded = serde_json::to_value(&source).unwrap();
        assert_eq!(encoded["runtime"], "native-store");
        assert_eq!(
            serde_json::from_value::<Definition>(encoded).unwrap(),
            source
        );
        assert!(serde_json::from_str::<Runtime>("\"native-host\"").is_err());
    }

    #[test]
    fn store_identity_rejects_foreign_absolute_and_nested_paths() {
        assert_eq!(
            store("asset").0,
            "/nix/store/00000000000000000000000000000000-asset"
        );
        for bad in [
            "asset",
            "/outside-store/00000000000000000000000000000000-asset",
            "nested/00000000000000000000000000000000-asset",
            "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-asset",
        ] {
            assert!(StorePath::try_from(bad.to_owned()).is_err());
        }
    }
    #[test]
    fn recursive_execution_sources_follow_embedded_derivations_and_cycles() {
        let source = Source {
            origin: "asset".into(),
            path: store("asset"),
        };
        let mut d = definition();
        d.sources.push(source.clone());
        let child = store("child.drv");
        let embedded = store("embedded.drv");
        let graph = BTreeMap::from([
            (d.root.clone(), node(vec![child.clone()], vec![])),
            (child, node(vec![d.root.clone()], vec![embedded.clone()])),
            (embedded, node(vec![], vec![source.path.clone()])),
        ]);
        assert_eq!(execution_sources(&d, &graph, &[]).unwrap(), vec![source]);
    }
    #[test]
    fn rejects_declared_sources_not_referenced_by_actual_graph() {
        let mut d = definition();
        d.sources.push(Source {
            origin: "unused".into(),
            path: store("unused"),
        });
        assert!(
            execution_sources(
                &d,
                &BTreeMap::from([(d.root.clone(), node(vec![], vec![]))]),
                &[]
            )
            .is_err()
        );
    }
    #[test]
    fn imports_only_pool_sources_referenced_by_execution_closure() {
        let d = definition();
        let source = Source {
            origin: "asset".into(),
            path: store("asset"),
        };
        let unused = Source {
            origin: "unused".into(),
            path: store("unused"),
        };
        let graph = BTreeMap::from([(d.root.clone(), node(vec![], vec![source.path.clone()]))]);
        assert_eq!(
            execution_sources(&d, &graph, &[source.clone(), unused]).unwrap(),
            vec![source]
        );
    }
    #[test]
    fn rejects_unknown_derivation_versions_and_missing_nodes() {
        let d = definition();
        assert!(execution_sources(&d, &Graph::new(), &[]).is_err());
        let mut n = node(vec![], vec![]);
        n.version = 3;
        assert!(execution_sources(&d, &BTreeMap::from([(d.root.clone(), n)]), &[]).is_err());
    }
    #[test]
    fn source_binding_rejects_changed_bytes_modes_and_missing_files() {
        let root = tempfile::tempdir().unwrap();
        let captured = root.path().join("capture");
        fs::create_dir(&captured).unwrap();
        let original = root.path().join("original.rs");
        let actual = captured.join("lib.rs");
        fs::write(&original, "fn current() {}\n").unwrap();
        fs::copy(&original, &actual).unwrap();
        match_file(&captured, Path::new("lib.rs"), &original).unwrap();
        fs::write(&actual, "fn changed() {}\n").unwrap();
        assert!(match_file(&captured, Path::new("lib.rs"), &original).is_err());
        fs::copy(&original, &actual).unwrap();
        fs::set_permissions(&actual, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&original, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(match_file(&captured, Path::new("lib.rs"), &original).is_err());
        fs::remove_file(&actual).unwrap();
        let error = match_file(&captured, Path::new("lib.rs"), &original).unwrap_err();
        assert!(error.to_string().contains(&actual.display().to_string()));
    }
    #[test]
    fn requires_all_control_plane_files() {
        let required =
            BTreeSet::from([PathBuf::from("flake.nix"), PathBuf::from("nix/module.nix")]);
        let mut inputs = Files::new();
        inputs.insert("flake.nix".into(), "source".into());
        assert!(control_plane(&inputs, &required).is_err());
        inputs.insert("nix/module.nix".into(), "source".into());
        control_plane(&inputs, &required).unwrap();
    }
    #[test]
    fn cargo_catalogue_requires_each_final_target_retention_setting() {
        let mut d = definition();
        d.cargo = Some(Operation::Catalogue);
        let mut n = node(vec![], vec![]);
        n.env = BTreeMap::from([
            ("doInstallCargoArtifacts".into(), json!("")),
            (
                "prePatch".into(),
                json!("export CARGO_TARGET_DIR=\"$out/target\""),
            ),
            ("dontFixup".into(), json!("1")),
            ("NIX_NO_SELF_RPATH".into(), json!("1")),
        ]);
        let mut graph = BTreeMap::from([(d.root.clone(), n)]);
        cargo_storage(&d, &graph).unwrap();
        for key in [
            "doInstallCargoArtifacts",
            "prePatch",
            "dontFixup",
            "NIX_NO_SELF_RPATH",
        ] {
            let original = graph.get_mut(&d.root).unwrap().env.remove(key).unwrap();
            assert!(cargo_storage(&d, &graph).is_err());
            graph
                .get_mut(&d.root)
                .unwrap()
                .env
                .insert(key.into(), json!("wrong"));
            assert!(cargo_storage(&d, &graph).is_err());
            graph
                .get_mut(&d.root)
                .unwrap()
                .env
                .insert(key.into(), original);
        }
    }
    #[test]
    fn report_producer_not_wrapper_controls_cargo_retention() {
        let mut d = definition();
        d.cargo = Some(Operation::Test);
        d.reporter = Some(store("reporter.drv"));
        d.catalogue = Some(store("catalogue.drv"));
        let mut wrapper = node(vec![], vec![]);
        wrapper
            .env
            .insert("doInstallCargoArtifacts".into(), json!(""));
        let mut reporter = node(vec![], vec![]);
        reporter
            .env
            .insert("doInstallCargoArtifacts".into(), json!(""));
        let mut catalogue = node(vec![], vec![]);
        catalogue.env = BTreeMap::from([
            ("doInstallCargoArtifacts".into(), json!("")),
            (
                "prePatch".into(),
                json!("export CARGO_TARGET_DIR=\"$out/target\""),
            ),
            ("dontFixup".into(), json!("1")),
            ("NIX_NO_SELF_RPATH".into(), json!("1")),
        ]);
        let mut graph = BTreeMap::from([
            (d.root.clone(), wrapper),
            (d.reporter.clone().unwrap(), reporter),
            (d.catalogue.clone().unwrap(), catalogue),
        ]);
        cargo_storage(&d, &graph).unwrap();
        graph
            .get_mut(d.reporter.as_ref().unwrap())
            .unwrap()
            .env
            .insert("doInstallCargoArtifacts".into(), json!("1"));
        assert!(cargo_storage(&d, &graph).is_err());
    }

    #[test]
    fn unrelated_roots_cannot_borrow_sibling_sources() {
        let d = definition();
        let sibling = store("sibling.drv");
        let source = Source {
            origin: "asset".into(),
            path: store("asset"),
        };
        let graph = BTreeMap::from([
            (d.root.clone(), node(vec![], vec![])),
            (sibling, node(vec![], vec![source.path.clone()])),
        ]);
        assert_eq!(
            execution_sources(&d, &graph, &[source]).unwrap(),
            Vec::<Source>::new()
        );
    }

    #[test]
    fn invalid_source_origins_fail_before_reading_the_store() {
        for origin in ["../outside", "/absolute"] {
            let source = Source {
                origin: origin.into(),
                path: store("source"),
            };
            assert!(source_files(&source).is_err());
        }
    }

    #[test]
    fn terminal_cargo_does_not_export_targets_or_assume_missing_is_false() {
        let mut d = definition();
        d.cargo = Some(Operation::Build);
        let mut graph = BTreeMap::from([(d.root.clone(), node(vec![], vec![]))]);
        assert!(cargo_storage(&d, &graph).is_err());
        graph
            .get_mut(&d.root)
            .unwrap()
            .env
            .insert("doInstallCargoArtifacts".into(), json!("1"));
        assert!(cargo_storage(&d, &graph).is_err());
        graph
            .get_mut(&d.root)
            .unwrap()
            .env
            .insert("doInstallCargoArtifacts".into(), json!(""));
        cargo_storage(&d, &graph).unwrap();
    }
}
