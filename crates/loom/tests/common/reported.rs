//! Actual immutable Nix producers for ordinary Check/System CLI regression fixtures.

#![expect(
    clippy::unwrap_used,
    clippy::print_stderr,
    reason = "isolated regression fixture setup aborts rather than fabricating missing native inputs"
)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub struct Fixture {
    directory: PathBuf,
    system: String,
}

fn write(root: &Path, relative: &str, bytes: impl AsRef<[u8]>) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn store_component(program: &str) -> PathBuf {
    let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join(program))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    let root: PathBuf = executable.components().take(4).collect();
    assert!(root.starts_with("/nix/store"));
    root
}

fn nix_string(value: &str) -> String {
    serde_json::to_string(value).unwrap().replace("${", "\\${")
}

impl Fixture {
    pub fn new(tier: &str, parser: &str, targets: &[&str]) -> Self {
        let directory = tempfile::tempdir().unwrap().keep();
        eprintln!("native reported fixture retained: {}", directory.display());
        let root = directory.as_path();
        let system = loom_gate::runner::result::current_platform();
        let bash = nix_string(store_component("bash").to_str().unwrap());
        let coreutils = nix_string(store_component("cat").to_str().unwrap());
        let encoded_targets = nix_string(&serde_json::to_string(targets).unwrap());
        write(root, ".gitignore", "/.loom/\n/target/\n");
        write(
            root,
            "verification.nix",
            include_bytes!("../../../../nix/verification.nix"),
        );
        write(
            root,
            "report-supervisor.sh",
            include_bytes!("../../../../nix/report-supervisor.sh"),
        );
        write(
            root,
            "producer.sh",
            "set -euo pipefail\nrecords=$1; shift\nstatus=0\nfor target in \"$@\"; do\n  cat \"$records/$target.output\"\n  code=$(<\"$records/$target.status\")\n  if [[ $code != 0 ]]; then\n    if [[ $status == 0 || $status == 77 || $code == 1 ]]; then status=$code; fi\n  fi\ndone\nif [[ -f \"$records/producer-exit\" ]]; then status=$(<\"$records/producer-exit\"); fi\nexit \"$status\"\n",
        );
        write(
            root,
            "flake.nix",
            format!(
                r#"{{
  outputs = {{ self }}: let
    checked = import ./verification.nix;
    bash = builtins.appendContext {bash} {{ {bash} = {{ path = true; }}; }};
    coreutils = builtins.appendContext {coreutils} {{ {coreutils} = {{ path = true; }}; }};
    producer = checked.resource {{ root = ./.; origin = "producer.sh"; }};
    records = checked.resource {{ root = ./.; origin = "records"; }};
  in {{ checks.{system}.probe = checked.reported {{
    system = "{system}";
    inherit bash coreutils;
    name = "native-reported-cli-fixture";
    resources = [ producer records ];
    evaluation = map (origin: checked.resource {{ root = ./.; inherit origin; }}) [ "flake.nix" "verification.nix" "report-supervisor.sh" ];
    program = "${{bash}}/bin/bash";
    arguments = [ producer.path records.path ] ++ builtins.fromJSON {targets};
    targets = builtins.fromJSON {targets};
    parser = {parser};
    tier = {tier};
  }}; }};
}}
"#,
                parser = nix_string(parser),
                tier = nix_string(tier),
                targets = encoded_targets
            ),
        );
        write(
            root,
            "docs/README.md",
            "- [Fixture](../specs/fixture/spec.md)\n",
        );
        write(root, "specs/fixture/spec.md", "# Report fixture\n");
        let mut criteria = String::new();
        for target in targets {
            writeln!(
                criteria,
                "- Result {target} [{tier}](nix report .#checks.{system}.probe::{target})"
            )
            .unwrap();
        }
        write(
            root,
            "specs/fixture/tests.md",
            format!("## Success Criteria\n{criteria}"),
        );
        write(root, "loom.toml", "");
        let mut git = Command::new("git");
        loom_test_support::scrub_git_local_env(&mut git);
        assert!(
            git.args(["init", "-q"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        for arguments in [
            vec!["add", "--all"],
            vec!["commit", "-qm", "native report fixture"],
        ] {
            let mut git = Command::new("git");
            loom_test_support::scrub_git_local_env(&mut git);
            assert!(
                git.current_dir(root)
                    .args([
                        "-c",
                        "user.name=Native report fixture",
                        "-c",
                        "user.email=native@fixture.invalid",
                        "-c",
                        "commit.gpgsign=false",
                        "-c",
                        "core.hooksPath=/dev/null"
                    ])
                    .args(arguments)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        Self { directory, system }
    }

    pub fn path(&self) -> &Path {
        self.directory.as_path()
    }

    pub fn address(&self, target: &str) -> String {
        format!("nix report .#checks.{}.probe::{target}", self.system)
    }

    pub fn record(&self, target: &str, output: &str, status: i32) {
        write(self.path(), &format!("records/{target}.output"), output);
        write(
            self.path(),
            &format!("records/{target}.status"),
            status.to_string(),
        );
    }

    pub fn policy(&self, tier: &str, enabled: bool) {
        let policy = if enabled {
            "sandbox-capability"
        } else {
            "deny"
        };
        write(
            self.path(),
            "loom.toml",
            format!(
                "[runner.{tier}.native]\nmatch='^nix report .*::'\nskip_policy='{policy}'\nskip_capabilities=['container-runtime','kvm']\n"
            ),
        );
    }

    pub fn run(&self, binary: &Path, tier: &str, worker: bool) -> Output {
        let mut command = Command::new(binary);
        command.args([
            "--workspace",
            self.path().to_str().unwrap(),
            "gate",
            tier,
            "--tree",
        ]);
        if worker {
            command.env("LOOM_INSIDE", "1");
        } else {
            command.env_remove("LOOM_INSIDE");
        }
        command.output().unwrap()
    }
}
