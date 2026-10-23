//! Actual compiler catalogue and ordinary CLI binding; run in the isolated verifier.

use std::fmt::Write as _;
use std::io::{BufRead as _, Write as _};
use std::path::Path;
use std::process::Command;

use loom_gate::inventory::{Inventory, Name, snapshot::Snapshot};
use loom_gate::provider::{Definition, nix::Error};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 2 {
        return Err("expected the immutable fixture and CLI artifacts".into());
    }
    let root = tempfile::tempdir()?.keep();
    writeln!(
        std::io::stderr(),
        "native Rust verification fixture retained: {}",
        root.display()
    )?;
    let project = root.join("project");
    copy(Path::new(&arguments[0]), &project)?;
    // A real, fresh source identity requires cold compilation on every invocation.
    let mut manifest = std::fs::read_to_string(project.join("Cargo.toml"))?;
    writeln!(
        manifest,
        "\n[package.metadata.native_probe]\nnonce = {:?}",
        root.to_string_lossy()
    )?;
    std::fs::write(project.join("Cargo.toml"), manifest)?;
    command(&project, "git", &["init", "-q"])?;
    command(&project, "git", &["add", "--all"])?;
    progress("locking declared fixture inputs")?;
    command(&project, "nix", &["flake", "lock", "--offline"])?;

    native_catalogue(&project)?;

    cli(&project, Path::new(&arguments[1]), "cold", 0)?;
    cli(&project, Path::new(&arguments[1]), "warm-rebuild", 0)?;
    cli_scope(
        &project,
        Path::new(&arguments[1]),
        "whole-plan",
        &["--tree"],
        0,
    )?;
    post_integration(&project, Path::new(&arguments[1]))?;
    property_failure(
        &project,
        Path::new(&arguments[1]),
        "unknown-group",
        "group(missing)",
    )?;
    property_failure(
        &project,
        Path::new(&arguments[1]),
        "empty-before-affectedness",
        "test(no_such_case)",
    )?;
    cli(&project, Path::new(&arguments[1]), "filter-correction", 0)?;
    let contract = project.join("specs/probe/tests.md");
    let mut body = std::fs::read_to_string(&contract)?;
    body.push_str("- The independent compiled case passes [test](other_case)\n");
    std::fs::write(contract, body)?;
    std::fs::write(project.join("payload/input.txt"), "invalid\n")?;
    // The project's default filter excludes native_case. This must still fail:
    // named aliases cannot silently authorize an excluded, unexecuted case.
    cli_scope(
        &project,
        Path::new(&arguments[1]),
        "semantic-failure",
        &["--tree"],
        1,
    )?;
    std::fs::write(project.join("payload/input.txt"), "valid\n")?;
    cli(&project, Path::new(&arguments[1]), "correction-recovery", 0)?;
    Ok(())
}

fn native_catalogue(project: &Path) -> Result<()> {
    let snapshot = Snapshot::discover(project)?;
    progress("discovering independent inventory")?;
    let inventory = Inventory::discover(&snapshot)?;
    progress("constructing actual compiler producer")?;
    let suites = Definition::native_rust_suites(&snapshot, &inventory)?;
    if suites.len() != 1 {
        return Err("expected one actual current-platform compiler producer".into());
    }
    let suite = &suites[0];
    progress("preparing actual Cargo/Nextest facts")?;
    let catalogue = suite
        .nix_graph()
        .ok_or("missing compiler graph")?
        .prepare_catalogue(suite.description().target().clone())?
        .require_verified()?;
    let target = Name::try_from("native_case".to_owned())?;
    if catalogue.resolve(&target)?.ignored() {
        return Err("live native case was ignored".into());
    }
    if !matches!(
        catalogue.resolve(&Name::try_from("fabricated".to_owned())?),
        Err(Error::MissingCase { .. })
    ) {
        return Err("fabricated native identity accepted".into());
    }
    if !matches!(
        catalogue.resolve(&Name::try_from("same_case".to_owned())?),
        Err(Error::AmbiguousCase { .. })
    ) {
        return Err("ambiguous native identity accepted".into());
    }
    catalogue.resolve(&Name::try_from("tests::a::same_case".to_owned())?)?;
    if suite
        .native_rust_cases(
            &snapshot,
            &inventory,
            &catalogue,
            &[Name::try_from("ignored_case".to_owned())?].into(),
        )
        .is_ok()
    {
        return Err("ignored native identity authorized an execution".into());
    }
    suite.native_rust_cases(&snapshot, &inventory, &catalogue, &[target.clone()].into())?;
    let declarations = loom_gate::provider::plan::property::declarations(&snapshot)?;
    let requests: Vec<_> = declarations
        .iter()
        .map(
            |(source, filter)| loom_gate::provider::plan::property::Request {
                source,
                filter,
                applicability: loom_gate::provider::plan::property::Applicability::Applicable,
            },
        )
        .collect();
    let partition = catalogue.partition(&requests)?;
    if partition
        .groups()
        .iter()
        .map(|group| group.members().len())
        .collect::<Vec<_>>()
        != [1, 1, 2]
        || partition.ordinary().len() != 2
        || !partition
            .ordinary()
            .iter()
            .any(|case| case.name().as_str().ends_with("unmatched_case"))
        || !partition
            .ordinary()
            .iter()
            .any(loom_gate::provider::plan::property::Test::ignored)
    {
        return Err(
            "actual default/group context lost property or unmatched/ignored membership".into(),
        );
    }
    writeln!(
        std::io::stdout(),
        "{}",
        serde_json::json!({"phase":"native-catalogue", "test_count": catalogue.tests().test_count})
    )?;

    std::fs::write(project.join("payload/input.txt"), "invalid\n")?;
    let changed = Snapshot::discover(project)?;
    let changed_inventory = Inventory::discover(&changed)?;
    if suite
        .native_rust_cases(&changed, &changed_inventory, &catalogue, &[target].into())
        .is_ok()
    {
        return Err("old compiler facts rebound a changed snapshot".into());
    }
    std::fs::write(project.join("payload/input.txt"), "valid\n")?;
    Ok(())
}

fn progress(stage: &str) -> Result<()> {
    writeln!(std::io::stderr(), "native Rust probe: {stage}")?;
    Ok(())
}

fn copy(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy(&entry.path(), &path)?;
        } else {
            std::fs::write(path, std::fs::read(entry.path())?)?;
        }
    }
    Ok(())
}

fn command(root: &Path, program: &str, arguments: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "fixture preparation {program}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

fn post_integration(project: &Path, artifact: &Path) -> Result<()> {
    std::fs::create_dir_all(project.join(".loom"))?;
    std::fs::write(
        project.join(".loom/project-hook.sh"),
        "printf 'actual-project-hook\\n' >> .loom/project-hook-ran\n",
    )?;
    std::fs::write(
        project.join(".pre-commit-config.yaml"),
        "repos:\n  - repo: local\n    hooks:\n      - id: native-project-hook\n        name: Native project hook\n        entry: bash .loom/project-hook.sh\n        language: system\n        always_run: true\n        pass_filenames: false\n",
    )?;
    command(project, "git", &["add", "--all"])?;
    command(
        project,
        "git",
        &[
            "-c",
            "user.name=Native verifier",
            "-c",
            "user.email=native@fixture.invalid",
            "commit",
            "-q",
            "-m",
            "native baseline",
        ],
    )?;
    let source = project.join("src/lib.rs");
    let body = std::fs::read_to_string(&source)?;
    std::fs::write(source, format!("{body}\n"))?;
    command(project, "git", &["add", "--all"])?;
    command(
        project,
        "git",
        &[
            "-c",
            "user.name=Native verifier",
            "-c",
            "user.email=native@fixture.invalid",
            "commit",
            "-q",
            "-m",
            "native source change",
        ],
    )?;
    cli_scope(
        project,
        artifact,
        "post-integration",
        &["--diff", "HEAD^..HEAD"],
        0,
    )?;
    if std::fs::read_to_string(project.join(".loom/project-hook-ran"))? != "actual-project-hook\n" {
        return Err(
            "ordinary diff verification omitted or duplicated the actual project hook".into(),
        );
    }
    Ok(())
}

fn property_failure(project: &Path, artifact: &Path, phase: &str, filter: &str) -> Result<()> {
    progress(phase)?;
    let path = project.join("specs/probe/tests.md");
    let original = std::fs::read_to_string(&path)?;
    std::fs::write(&path, original.replace("group(probe)", filter))?;
    let output = Command::new(artifact)
        .args(["gate", "verify", "--files", "docs/README.md"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .current_dir(project)
        .output()?;
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    if output.status.code() != Some(1)
        || !diagnostic.contains("property filter")
        || diagnostic.contains("loom gate plan:")
        || diagnostic.contains("\"verdict\":")
    {
        return Err(format!(
            "invalid property contract reached selection/execution ({phase}): {diagnostic}"
        )
        .into());
    }
    std::fs::write(path, original)?;
    writeln!(
        std::io::stdout(),
        "{}",
        serde_json::json!({"phase":phase, "exit_code":1, "stderr":diagnostic})
    )?;
    Ok(())
}

fn cli(project: &Path, artifact: &Path, phase: &str, expected: i32) -> Result<()> {
    cli_scope(
        project,
        artifact,
        phase,
        &["--target", "native_case"],
        expected,
    )
}

fn cli_scope(
    project: &Path,
    artifact: &Path,
    phase: &str,
    scope: &[&str],
    expected: i32,
) -> Result<()> {
    progress(phase)?;
    let mut child = Command::new(artifact)
        .args(["gate", "verify"])
        .args(scope)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env(
            "RUST_LOG",
            "loom_gate::inventory=debug,loom_gate::provider::nix=debug",
        )
        .current_dir(project)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let stderr = child
        .stderr
        .take()
        .ok_or("native CLI stderr was not captured")?;
    let reader = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut reader = std::io::BufReader::new(stderr);
        let mut captured = Vec::new();
        let mut line = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            std::io::stderr().write_all(&line)?;
            captured.extend_from_slice(&line);
        }
        Ok(captured)
    });
    let mut output = child.wait_with_output()?;
    output.stderr = reader
        .join()
        .map_err(|_| "native CLI stderr reader panicked")??;
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    if output.status.code() != Some(expected) {
        return Err(format!(
            "native CLI {phase}: expected {expected}, got {}: {diagnostic}",
            output.status
        )
        .into());
    }
    let verdict = if expected == 0 { "pass" } else { "fail" };
    if !diagnostic.contains(&format!("\"verdict\":\"{verdict}\"")) {
        return Err(format!(
            "native CLI {phase} had no actual {verdict} observation: {diagnostic}"
        )
        .into());
    }
    let observed: Vec<serde_json::Value> = diagnostic
        .lines()
        .filter(|line| line.starts_with("loom gate ["))
        .filter_map(|line| line.split_once("]: "))
        .filter_map(|(_, evidence)| serde_json::from_str(evidence).ok())
        .collect();
    let native_case = observed
        .iter()
        .find(|item| item["target"] == "native_case")
        .ok_or("missing actual native case report")?;
    if native_case["verdict"]["pass"] != (expected == 0)
        || !native_case["observations"]
            .as_array()
            .is_some_and(|observations| {
                observations
                    .iter()
                    .any(|item| item["kind"] == "fresh-execution")
            })
    {
        return Err(format!(
            "native case omitted its retained verdict or fresh execution witness: {diagnostic}"
        )
        .into());
    }
    if matches!(
        phase,
        "whole-plan" | "post-integration" | "semantic-failure"
    ) {
        if phase == "semantic-failure" {
            let passing = observed
                .iter()
                .find(|item| item["target"] == "other_case")
                .ok_or("missing independent passing native case")?;
            let baseline = observed
                .iter()
                .find(|item| {
                    item["target"]
                        .as_str()
                        .is_some_and(|target| target.ends_with(".native-tests"))
                })
                .ok_or("missing independent native test baseline")?;
            if passing["outcome"]["verdict"] != "pass"
                || passing["verdict"]["pass"] != true
                || baseline["outcome"]["verdict"] != "fail"
            {
                return Err(format!("native case inherited its suite outcome or erased its failing baseline: {diagnostic}").into());
            }
        }
        let check = observed.iter().position(|item| {
            item["target"]
                .as_str()
                .is_some_and(|target| target.ends_with(".native-build"))
        });
        let test = observed
            .iter()
            .position(|item| item["target"] == "native_case");
        if !matches!((check, test), (Some(check), Some(test)) if check < test) {
            return Err(format!(
                "ordinary CLI ran a Test before its Check lane ({phase}): {diagnostic}"
            )
            .into());
        }
    }
    writeln!(
        std::io::stdout(),
        "{}",
        serde_json::json!({"phase":phase, "exit_code":output.status.code(), "stdout":String::from_utf8_lossy(&output.stdout), "stderr":diagnostic})
    )?;
    Ok(())
}
