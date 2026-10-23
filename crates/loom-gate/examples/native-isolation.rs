//! Fixed native constructor acceptance; requires an isolated verifier with Nix sandbox namespaces.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use loom_gate::inventory::snapshot::{ResourcePath, Snapshot};
use loom_gate::inventory::{Duty, Inventory, Name};
use loom_gate::provider::tracked::{Observation, Outcome};
use loom_gate::provider::{Definition, Registry};

fn name(value: &str) -> anyhow::Result<Name> {
    Ok(Name::try_from(value.to_owned())?)
}

fn write(root: &Path, path: &str, contents: &str) -> anyhow::Result<()> {
    let destination = root.join(path);
    std::fs::create_dir_all(destination.parent().context("fixture path has no parent")?)?;
    Ok(std::fs::write(destination, contents)?)
}

fn main() -> anyhow::Result<std::process::ExitCode> {
    let evidence = tempfile::tempdir()?.keep();
    let result = run(&evidence);
    match &result {
        Ok(code) if *code == std::process::ExitCode::SUCCESS => {
            std::fs::remove_dir_all(&evidence)?;
        }
        _ => writeln!(
            std::io::stderr().lock(),
            "native isolation evidence retained: {}",
            evidence.display()
        )?,
    }
    result
}

fn run(evidence: &Path) -> anyhow::Result<std::process::ExitCode> {
    let (_private, private_path) = tempfile::NamedTempFile::new_in(evidence)?.keep()?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let project = evidence.join("project");
    std::fs::create_dir(&project)?;
    let root = project.as_path();
    let bash = std::env::split_paths(&std::env::var_os("PATH").context("missing PATH")?)
        .map(|directory| directory.join("bash"))
        .find(|path| path.is_file())
        .context("provision immutable Bash")?
        .canonicalize()?;
    let store: PathBuf = bash.components().take(4).collect();
    let platform = Command::new("nix")
        .args(["config", "show", "--json"])
        .output()?;
    anyhow::ensure!(
        platform.status.success(),
        "native platform discovery failed"
    );
    let platform: serde_json::Value = serde_json::from_slice(&platform.stdout)?;
    let system = platform["system"]["value"]
        .as_str()
        .context("missing native system")?;
    write(root, "README.md", "# Isolated native acceptance\n")?;
    write(
        root,
        "docs/README.md",
        "- [example](../specs/example/spec.md)\n",
    )?;
    write(
        root,
        "specs/example/spec.md",
        "# Native isolation contract\n",
    )?;
    write(
        root,
        "specs/example/tests.md",
        "# Acceptance\n\n## Success Criteria\n\n- Isolation holds [check](native-isolation)\n",
    )?;
    write(
        root,
        "verification.nix",
        include_str!("../../../nix/verification.nix"),
    )?;
    let script = r#"set -euo pipefail
read -r heading < "$src/README.md"
[[ "$heading" == '# Isolated native acceptance' ]]
[[ ! -e "$hidden" ]]
if (echo undeclared > /dev/tcp/127.0.0.1/$port) 2>/dev/null; then
  exit 22
fi
printf 'isolated\n' > "$out"
printf 'isolated-report\n' > "$report"
"#;
    write(
        root,
        "flake.nix",
        &format!(
            r#"{{
  outputs = {{ self }}: let
    checked = import ./verification.nix;
    source = checked.resource {{
      root = ./.;
      filter = path: type: type == "directory" || path == toString (./. + "/README.md");
    }};
  in {{
    checks.{system}.isolation = checked.check {{
      resources = [ source ];
      build = sources: builtins.derivation {{
        name = "native-isolation-{nonce}";
        system = {system_json};
        builder = builtins.appendContext {bash} {{ {store} = {{ path = true; }}; }};
        args = [ "-c" {script} ];
        outputs = [ "out" "report" ];
        src = builtins.head sources;
        hidden = {hidden};
        port = "{port}";
      }};
    }};
  }};
}}
"#,
            nonce = root
                .file_name()
                .context("fixture has no basename")?
                .to_str()
                .context("fixture basename is not UTF-8")?,
            system_json = serde_json::to_string(system)?,
            bash = serde_json::to_string(&bash)?,
            store = serde_json::to_string(&store)?,
            script = serde_json::to_string(script)?.replace("${", "\\${"),
            hidden = serde_json::to_string(&private_path)?,
        ),
    )?;
    anyhow::ensure!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()?
            .success(),
        "fixture Git initialization failed"
    );
    let snapshot = Snapshot::discover(root)?;
    let inventory = Inventory::discover(&snapshot)?;
    let target = name("native-isolation")?;
    let definition = Definition::hermetic(
        &snapshot,
        &inventory,
        target,
        &name(&format!("nix:check:flake.nix:{system}:isolation"))?,
        &BTreeSet::from([name("resource:README.md")?]),
        &BTreeSet::from([Duty::Document]),
    )?;
    let candidate = definition.description().clone();
    let registry = Registry::new(&snapshot, vec![definition])?;
    let unit = registry.resolve(
        &snapshot,
        &inventory,
        &candidate,
        ResourcePath::try_from(PathBuf::from("specs/example/tests.md"))?,
    )?;
    let graph = unit.nix_graph().context("native graph omitted")?;
    anyhow::ensure!(
        graph
            .outputs()?
            .values()
            .all(|path| !Path::new(path.as_str()).exists()),
        "unique native outputs are not cold"
    );
    let mut stdout = std::io::stdout().lock();
    for phase in ["cold", "warm-rebuild"] {
        anyhow::ensure!(private_path.is_file(), "host-file control disappeared");
        let connection = std::net::TcpStream::connect_timeout(
            &listener.local_addr()?,
            std::time::Duration::from_secs(2),
        )
        .context("live host network control is not reachable")?;
        drop(connection);
        let observed = unit.run()?;
        serde_json::to_writer(
            &mut stdout,
            &serde_json::json!({"phase":phase,"observed":observed}),
        )?;
        writeln!(stdout)?;
        if matches!(observed.outcome(), Outcome::Unavailable { .. }) {
            return Ok(std::process::ExitCode::from(78));
        }
        anyhow::ensure!(
            matches!(observed.outcome(), Outcome::Pass),
            "native {phase} isolation is unverified: {:?}; route to the declared isolated verifier without changing sandbox policy",
            observed.outcome()
        );
        anyhow::ensure!(
            observed
                .observations()
                .iter()
                .filter(|observation| matches!(observation, Observation::Output { .. }))
                .count()
                == 2,
            "native {phase} output mapping is incomplete"
        );
    }
    Ok(std::process::ExitCode::SUCCESS)
}
