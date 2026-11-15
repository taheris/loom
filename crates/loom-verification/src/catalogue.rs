use crate::process;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
use walkdir::WalkDir;

fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn text(path: &Path) -> Result<String> {
    Ok(fs::read_to_string(path)?)
}
fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}
fn path(path: &Path) -> Result<String> {
    Ok(path.to_str().context("non-UTF8 fixture path")?.to_owned())
}
fn normalize(value: Value, baseline: &str, candidate: &str) -> Value {
    match value {
        Value::String(value) if value == baseline || value.starts_with(&format!("{baseline}/")) => {
            json!(format!("{candidate}{}", &value[baseline.len()..]))
        }
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(k, v)| (k, normalize(v, baseline, candidate)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|v| normalize(v, baseline, candidate))
                .collect(),
        ),
        other => other,
    }
}
fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in WalkDir::new(source).follow_links(true) {
        let entry = entry?;
        let target = destination.join(entry.path().strip_prefix(source)?);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
            fs::set_permissions(
                &target,
                fs::Permissions::from_mode(entry.metadata()?.permissions().mode() | 0o200),
            )?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
fn edges(metadata: &Value) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let packages = metadata["packages"]
        .as_array()
        .context("missing packages")?;
    let package = packages
        .iter()
        .find(|p| p["name"] == "catalogue-reuse")
        .context("missing catalogue package")?;
    let node = metadata["resolve"]["nodes"]
        .as_array()
        .context("missing resolved nodes")?
        .iter()
        .find(|n| n["id"] == package["id"])
        .context("missing catalogue node")?;
    let names = node["deps"]
        .as_array()
        .context("missing dependencies")?
        .iter()
        .map(|d| {
            Ok(d["name"]
                .as_str()
                .context("missing dependency name")?
                .to_owned())
        })
        .collect::<Result<_>>()?;
    let features = node["features"]
        .as_array()
        .context("missing package features")?
        .iter()
        .map(|feature| Ok(feature.as_str().context("invalid feature name")?.to_owned()))
        .collect::<Result<_>>()?;
    Ok((names, features))
}
fn selected(tests: &Value) -> Result<BTreeSet<(String, String)>> {
    let mut selected = BTreeSet::new();
    for suite in tests["rust-suites"]
        .as_object()
        .context("missing suites")?
        .values()
    {
        for (name, case) in suite["testcases"]
            .as_object()
            .context("missing test cases")?
        {
            if case["filter-match"]["status"] == "matches" {
                selected.insert((
                    suite["package-name"]
                        .as_str()
                        .context("missing suite package")?
                        .to_owned(),
                    name.clone(),
                ));
            }
        }
    }
    Ok(selected)
}
#[derive(Clone, Copy)]
enum Expected {
    Code(i32),
    Failure,
}
struct Runner {
    work: PathBuf,
    output: PathBuf,
    commands: Vec<Value>,
}
impl Runner {
    fn run(
        &mut self,
        name: &str,
        arguments: &[String],
        expected: Expected,
        environment: &BTreeMap<&str, String>,
        cwd: Option<&Path>,
    ) -> Result<String> {
        let mut command = Command::new(arguments.first().context("missing fixture program")?);
        command
            .args(&arguments[1..])
            .envs(environment)
            .current_dir(cwd.unwrap_or(&self.work));
        let result = process::capture(&command, 90)?;
        fs::write(
            self.work.join("diagnostics").join(format!("{name}.stdout")),
            &result.stdout,
        )?;
        fs::write(
            self.work.join("diagnostics").join(format!("{name}.stderr")),
            &result.stderr,
        )?;
        writeln!(io::stdout().lock(), "catalogue conformance: {name}")?;
        io::stdout().lock().write_all(&result.stdout)?;
        io::stderr().lock().write_all(&result.stderr)?;
        let work = path(&self.work)?;
        self.commands.push(json!({"name":name,"arguments":arguments.iter().map(|a|a.replace(&work,"$WORK")).collect::<Vec<_>>(),"status":result.status.code()}));
        fs::write(
            self.output.join("commands.json"),
            serde_json::to_vec_pretty(&self.commands)?,
        )?;
        let matched = match expected {
            Expected::Code(code) => result.status.code() == Some(code),
            Expected::Failure => !result.status.success(),
        };
        ensure!(
            matched,
            "{name}: unexpected status {}; {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        Ok(String::from_utf8(result.stdout)?)
    }
    fn json(
        &mut self,
        name: &str,
        arguments: &[String],
        environment: &BTreeMap<&str, String>,
        cwd: Option<&Path>,
    ) -> Result<Value> {
        Ok(serde_json::from_str(&self.run(
            name,
            arguments,
            Expected::Code(0),
            environment,
            cwd,
        )?)?)
    }
}
fn replace_argument(arguments: &[String], key: &str, value: String) -> Result<Vec<String>> {
    let mut result = arguments.to_vec();
    let index = result
        .iter()
        .position(|s| s == key)
        .context("missing command option")?;
    *result
        .get_mut(index + 1)
        .context("missing command option value")? = value;
    Ok(result)
}
fn append(base: &[String], extra: &[&str]) -> Vec<String> {
    base.iter().cloned().chain(args(extra)).collect()
}

pub fn run(
    candidate: &Path,
    baseline: &Path,
    source: &Path,
    output: &Path,
    owned_script: &Path,
) -> Result<()> {
    fs::create_dir(output)?;
    let work = tempfile::tempdir()?.keep().canonicalize()?;
    fs::create_dir(work.join("diagnostics"))?;
    let mut runner = Runner {
        work: work.clone(),
        output: output.into(),
        commands: vec![],
    };
    let empty = BTreeMap::new();
    let mut names = args(&[
        "cargo.json",
        "binaries.json",
        "nextest.json",
        "default.json",
    ]);
    let groups = text(&candidate.join("groups.txt"))?;
    ensure!(
        groups == text(&baseline.join("groups.txt"))?,
        "catalogue groups differ"
    );
    let groups = groups.lines().collect::<Vec<_>>();
    names.extend((0..groups.len()).map(|i| format!("group-{i}.json")));
    for name in &names {
        ensure!(
            read(&candidate.join(name))?
                == normalize(
                    read(&baseline.join(name))?,
                    &path(baseline)?,
                    &path(candidate)?
                ),
            "catalogue JSON differs: {name}"
        );
    }
    let calls = text(&candidate.join("cargo-calls"))?;
    let calls = calls.lines().collect::<Vec<_>>();
    let old_calls = text(&baseline.join("cargo-calls"))?;
    let old_calls = old_calls.lines().collect::<Vec<_>>();
    ensure!(
        calls.len() == 2 && calls.iter().all(|s| s.starts_with("first ")),
        "candidate reentered Cargo"
    );
    ensure!(
        calls.iter().any(|s| s.contains(" metadata ")
            && s.contains("--all-features")
            && s.contains("--filter-platform")),
        "listing graph metadata call missing"
    );
    ensure!(
        calls.iter().any(|s| s.contains(" test ")
            && s.contains("--no-run")
            && s.contains("--workspace")
            && s.contains("--all-targets")),
        "first compilation omitted full scope"
    );
    ensure!(
        calls
            .iter()
            .filter(|s| s.contains(" test "))
            .all(|s| !s.contains("--all-features")),
        "compilation broadened features"
    );
    ensure!(
        old_calls.len() == 2 * (1 + 2 + groups.len()),
        "legacy Cargo call count changed"
    );
    let wrappers = text(&candidate.join("wrappers.log"))?;
    let mut wrappers = wrappers.lines().map(str::to_owned).collect::<Vec<_>>();
    let baseline_path = path(baseline)?;
    let candidate_path = path(candidate)?;
    let mut old_wrappers = text(&baseline.join("wrappers.log"))?
        .lines()
        .map(|s| {
            let normalized = normalize(json!(s), &baseline_path, &candidate_path);
            Ok(normalized
                .as_str()
                .context("normalized wrapper is not a string")?
                .to_owned())
        })
        .collect::<Result<Vec<_>>>()?;
    wrappers.sort();
    old_wrappers.sort();
    ensure!(
        !wrappers.is_empty() && wrappers == old_wrappers,
        "wrappers differ"
    );
    let full = read(&candidate.join("nextest.json"))?;
    let suites = full["rust-suites"].as_object().context("missing suites")?;
    let mut cases = BTreeSet::new();
    let mut packages = BTreeSet::new();
    let mut ignored = false;
    for suite in suites.values() {
        packages.insert(
            suite["package-name"]
                .as_str()
                .context("missing package name")?,
        );
        for (name, case) in suite["testcases"]
            .as_object()
            .context("missing test cases")?
        {
            cases.insert(name.clone());
            if name.ends_with("ignored_bad") && case["ignored"] == true {
                ignored = true;
            }
        }
    }
    for suffix in ["alpha_case", "example_case", "integration_case"] {
        ensure!(
            cases.iter().any(|n| n.ends_with(suffix)),
            "missing compiled fixture case: {suffix}"
        );
    }
    ensure!(
        !cases
            .iter()
            .any(|n| n.ends_with("beta_case") || n.ends_with("optional_dependency_feature_case"))
            && ignored,
        "default-feature or ignored catalogue changed"
    );
    ensure!(
        packages
            == BTreeSet::from([
                "catalogue-reuse",
                "catalogue-member",
                "optional_beta",
                "platform_windows"
            ]),
        "compiled fixture packages differ"
    );
    let (compile_edges, compile_features) = edges(&read(&candidate.join("cargo.json"))?)?;
    let (listing_edges, listing_features) = edges(&read(&candidate.join("listing-cargo.json"))?)?;
    ensure!(
        compile_edges.is_empty() && !compile_features.contains("optional_beta"),
        "compilation graph broadened"
    );
    ensure!(
        listing_edges.contains("optional_beta")
            && listing_features.contains("optional_beta")
            && listing_edges.contains("catalogue_member")
            && !listing_edges.contains("platform_windows"),
        "listing graph platform/feature edges differ"
    );
    let base = vec![
        "cargo-nextest".into(),
        "nextest".into(),
        "list".into(),
        "--cargo-metadata".into(),
        path(&candidate.join("listing-cargo.json"))?,
        "--binaries-metadata".into(),
        path(&candidate.join("binaries.json"))?,
        "--workspace-remap".into(),
        path(source)?,
        "--target-dir-remap".into(),
        path(&candidate.join("target"))?,
    ];
    ensure!(
        runner.json(
            "unrelated-cwd",
            &append(
                &base,
                &[
                    "--run-ignored",
                    "all",
                    "--ignore-default-filter",
                    "--message-format",
                    "json"
                ]
            ),
            &empty,
            None
        )? == full,
        "unrelated cwd catalogue differs"
    );
    let wrong = replace_argument(
        &base,
        "--cargo-metadata",
        path(&candidate.join("cargo.json"))?,
    )?;
    let wrong_default = runner.json(
        "default-feature-graph-counterexample",
        &append(
            &wrong,
            &["--run-ignored", "all", "--message-format", "json"],
        ),
        &empty,
        None,
    )?;
    ensure!(
        wrong_default != read(&candidate.join("default.json"))?,
        "default-feature graph counterexample no longer detected"
    );
    let mut execution = args(&["cargo-nextest", "nextest", "run"]);
    execution.extend_from_slice(&base[3..]);
    execution.extend(args(&[
        "--ignore-default-filter",
        "--no-fail-fast",
        "--test-threads",
        "1",
        "--no-tests",
        "fail",
    ]));
    runner.run("positive", &execution, Expected::Code(0), &empty, None)?;
    runner.run(
        "real-ignored-assertion",
        &append(
            &execution,
            &["--run-ignored", "all", "-E", "test(ignored_bad)"],
        ),
        Expected::Code(100),
        &empty,
        None,
    )?;
    let malformed = work.join("malformed.json");
    fs::write(&malformed, "{")?;
    runner.run(
        "malformed-binaries",
        &append(
            &replace_argument(&base, "--binaries-metadata", path(&malformed)?)?,
            &["--message-format", "json"],
        ),
        Expected::Failure,
        &empty,
        None,
    )?;
    let missing = work.join("missing");
    copy_tree(&candidate.join("target"), &missing)?;
    let metadata = read(&candidate.join("binaries.json"))?;
    let binary = metadata["rust-binaries"]
        .as_object()
        .context("missing binaries")?
        .values()
        .next()
        .context("no binaries")?["binary-path"]
        .as_str()
        .context("missing binary path")?;
    fs::remove_file(missing.join(Path::new(binary).strip_prefix(candidate.join("target"))?))?;
    runner.run(
        "missing-binary",
        &append(
            &replace_argument(&base, "--target-dir-remap", path(&missing)?)?,
            &[
                "--run-ignored",
                "all",
                "--ignore-default-filter",
                "--message-format",
                "json",
            ],
        ),
        Expected::Failure,
        &empty,
        None,
    )?;
    let changed = work.join("changed-config");
    copy_tree(source, &changed)?;
    let config = changed.join(".config/nextest.toml");
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600))?;
    fs::write(
        &config,
        text(&config)?.replace("rdeps(optional_beta)", "package(=platform_windows)"),
    )?;
    let changed_default = runner.json(
        "current-config",
        &append(
            &replace_argument(&base, "--workspace-remap", path(&changed)?)?,
            &["--run-ignored", "all", "--message-format", "json"],
        ),
        &empty,
        None,
    )?;
    ensure!(
        selected(&changed_default)? != selected(&read(&candidate.join("default.json"))?)?
            && selected(&changed_default)?
                .iter()
                .map(|(p, _)| p.as_str())
                .collect::<BTreeSet<_>>()
                == BTreeSet::from(["platform_windows"]),
        "changed current configuration did not change facts"
    );
    let configured = work.join("configured-source");
    copy_tree(source, &configured)?;
    let platform = metadata["rust-build-meta"]["platforms"]["host"]["platform"]["triple"]
        .as_str()
        .context("missing host triple")?;
    fs::create_dir(configured.join(".cargo"))?;
    let configured_build = path(&work.join("configured-build"))?;
    fs::write(
        configured.join(".cargo/config.toml"),
        format!(
            "[build]\ntarget={}\nbuild-dir={}\n",
            serde_json::to_string(platform)?,
            serde_json::to_string(&configured_build)?
        ),
    )?;
    let source_file = configured.join("src/lib.rs");
    fs::set_permissions(&source_file, fs::Permissions::from_mode(0o600))?;
    fs::write(
        &source_file,
        format!(
            "{}\n#[test]\nfn changed_source_case() {{}}\n",
            text(&source_file)?
        ),
    )?;
    let environment = BTreeMap::from([
        ("CARGO_TARGET_DIR", path(&work.join("configured-target"))?),
        ("CARGO_BUILD_JOBS", "1".into()),
    ]);
    let mut ordinary = args(&[
        "cargo-nextest",
        "nextest",
        "list",
        "--offline",
        "--locked",
        "--manifest-path",
    ]);
    ordinary.push(path(&configured.join("Cargo.toml"))?);
    ordinary.extend(args(&[
        "--workspace",
        "--all-targets",
        "--cargo-profile",
        "dev",
        "--run-ignored",
        "all",
        "--ignore-default-filter",
    ]));
    let configured_binaries = work.join("configured-binaries.json");
    fs::write(
        &configured_binaries,
        runner.run(
            "configured-target-first-compiler",
            &append(
                &ordinary,
                &["--list-type", "binaries-only", "--message-format", "json"],
            ),
            Expected::Code(0),
            &environment,
            Some(&configured),
        )?,
    )?;
    let configured_metadata = read(&configured_binaries)?;
    let platforms = &configured_metadata["rust-build-meta"]["platforms"];
    ensure!(
        platforms["targets"]
            .as_array()
            .is_some_and(|t| t.len() == 1),
        "configured target list differs"
    );
    let actual_target = platforms["targets"][0]["platform"]["triple"]
        .as_str()
        .context("missing configured target")?;
    ensure!(
        actual_target == platform
            && configured_metadata["rust-build-meta"]["build-directory"] == configured_build,
        "target or separate build directory not preserved"
    );
    let configured_graph = work.join("configured-listing-cargo.json");
    let mut graph_args = args(&[
        "cargo",
        "metadata",
        "--offline",
        "--locked",
        "--manifest-path",
    ]);
    graph_args.push(path(&configured.join("Cargo.toml"))?);
    graph_args.extend(args(&[
        "--format-version",
        "1",
        "--all-features",
        "--filter-platform",
        actual_target,
    ]));
    fs::write(
        &configured_graph,
        runner.run(
            "configured-target-graph",
            &graph_args,
            Expected::Code(0),
            &environment,
            Some(&configured),
        )?,
    )?;
    let configured_full = runner.json(
        "configured-target-legacy",
        &append(&ordinary, &["--message-format", "json"]),
        &environment,
        Some(&configured),
    )?;
    let reuse = vec![
        "cargo-nextest".into(),
        "nextest".into(),
        "list".into(),
        "--cargo-metadata".into(),
        path(&configured_graph)?,
        "--binaries-metadata".into(),
        path(&configured_binaries)?,
        "--workspace-remap".into(),
        path(&configured)?,
        "--target-dir-remap".into(),
        environment["CARGO_TARGET_DIR"].clone(),
        "--build-dir-remap".into(),
        configured_build.clone(),
        "--run-ignored".into(),
        "all".into(),
        "--ignore-default-filter".into(),
        "--message-format".into(),
        "json".into(),
    ];
    ensure!(
        configured_full == runner.json("configured-target-reuse", &reuse, &environment, None)?,
        "configured target reuse differs from legacy"
    );
    ensure!(
        selected(&configured_full)?
            .iter()
            .any(|(_, n)| n.ends_with("changed_source_case"))
            && !selected(&full)?
                .iter()
                .any(|(_, n)| n.ends_with("changed_source_case")),
        "changed source was not recompiled"
    );
    let owned = text(owned_script)?;
    let start = owned
        .find("listing_platform=$(")
        .context("missing owned platform parser")?;
    let end=owned.find("cargo metadata --offline --locked --manifest-path \"$src/Cargo.toml\" --format-version 1 --all-features").context("missing owned listing graph command")?;
    ensure!(end > start, "owned platform parser boundaries differ");
    let parser = format!(
        "{}printf \"%s\\n%s\\n\" \"$listing_target\" \"$listing_build_directory\"\n",
        &owned[start..end]
    );
    let parser_root = work.join("platform-schema");
    fs::create_dir(&parser_root)?;
    let parser_env = BTreeMap::from([("out", path(&parser_root)?)]);
    let mut real_configured = configured_metadata.clone();
    real_configured["rust-build-meta"]["platforms"]["host"]["platform"]["triple"] =
        json!("schema-only-host-sentinel");
    let parser_args = vec!["bash".into(), "-euc".into(), parser];
    fs::write(
        parser_root.join("binaries.json"),
        serde_json::to_vec(&real_configured)?,
    )?;
    let chosen = runner.run(
        "owned-target-parser",
        &parser_args,
        Expected::Code(0),
        &parser_env,
        None,
    )?;
    ensure!(
        chosen.lines().collect::<Vec<_>>() == vec![actual_target, &configured_build],
        "owned parser did not prefer explicit target"
    );
    real_configured["rust-build-meta"]["platforms"]["targets"][0]["platform"]["custom-json"] =
        json!("{}");
    fs::write(
        parser_root.join("binaries.json"),
        serde_json::to_vec(&real_configured)?,
    )?;
    let custom = runner.run(
        "owned-custom-target-parser",
        &parser_args,
        Expected::Code(0),
        &parser_env,
        None,
    )?;
    ensure!(
        custom.lines().next() == Some(path(&parser_root.join("listing-target.json"))?.as_str())
            && read(&parser_root.join("listing-target.json"))? == json!({}),
        "owned custom target parser did not preserve JSON"
    );
    let target = real_configured["rust-build-meta"]["platforms"]["targets"][0].clone();
    real_configured["rust-build-meta"]["platforms"]["targets"] = json!([target, target]);
    fs::write(
        parser_root.join("binaries.json"),
        serde_json::to_vec(&real_configured)?,
    )?;
    runner.run(
        "owned-multiple-target-rejection",
        &parser_args,
        Expected::Failure,
        &parser_env,
        None,
    )?;
    let mut foreign_args = args(&[
        "cargo",
        "metadata",
        "--offline",
        "--locked",
        "--manifest-path",
    ]);
    foreign_args.push(path(&source.join("Cargo.toml"))?);
    foreign_args.extend(args(&[
        "--format-version",
        "1",
        "--all-features",
        "--filter-platform",
        "x86_64-pc-windows-msvc",
    ]));
    let foreign = runner.json(
        "foreign-platform-metadata-only",
        &foreign_args,
        &empty,
        None,
    )?;
    let (foreign_edges, _) = edges(&foreign)?;
    ensure!(
        foreign_edges.contains("platform_windows") && !foreign_edges.contains("catalogue_member"),
        "foreign metadata edges differ"
    );
    let result = json!({"json_equal":names,"compiled_cases":cases,"groups":groups,"wrappers_equal":true,"first_compilation_full_scope":true,"compilation_all_features":false,"compile_graph_edges":compile_edges,"listing_graph_edges":listing_edges,"foreign_metadata_only_edges":foreign_edges,"default_feature_graph_counterexample_rejected":true,"secondary_nextest_cargo_reentries":0,"legacy_secondary_cargo_reentries":old_calls.len()-2,"extra_owned_listing_graph_metadata_commands":1,"actual_ignored_failure":100,"current_config_changes_facts":true,"changed_source_recompiled":true,"configured_target":actual_target,"configured_target_json_equal":true,"separate_build_directory_preserved":true,"owned_platform_parser_semantic_controls":["explicit-target-before-host","custom-json","reject-multiple-targets"],"scope":"Actual std-only constructor/legacy conformance, not Gate or foreign execution"});
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    Ok(())
}
