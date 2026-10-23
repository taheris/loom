//! Delegated native verification uses a bounded, private user-service sandbox.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use loom_driver::clock::{Clock, SystemClock};

use displaydoc::Display;
use serde::Serialize;
use thiserror::Error;

use super::{Digest, StorePath};

#[derive(Debug, Clone, Serialize)]
pub(super) struct Executor {
    program: PathBuf,
    artifact: Digest,
    environment: PathBuf,
    environment_artifact: Digest,
}

impl Executor {
    pub(super) fn capture() -> Result<Option<Self>, Error> {
        let Some(search) = std::env::var_os("PATH") else {
            return Ok(None);
        };
        for directory in std::env::split_paths(&search) {
            let program = directory.join("systemd-run");
            if program.is_file() {
                let program = program.canonicalize()?;
                StorePath::root(&program).map_err(|_| Error::Executable)?;
                let mut environment = None;
                for directory in std::env::split_paths(&search) {
                    let path = directory.join("env");
                    if path.is_file() {
                        let resolved = path.canonicalize()?;
                        let root = StorePath::root(&resolved).map_err(|_| Error::Executable)?;
                        let entry = Path::new(root.as_str()).join("bin/env");
                        if !entry.is_file() {
                            return Err(Error::Executable);
                        }
                        environment = Some(entry);
                        break;
                    }
                }
                let Some(environment) = environment else {
                    return Ok(None);
                };
                StorePath::root(&environment).map_err(|_| Error::Executable)?;
                return Ok(Some(Self {
                    artifact: Digest::of(&std::fs::read(&program)?),
                    environment_artifact: Digest::of(&std::fs::read(&environment)?),
                    environment,
                    program,
                }));
            }
        }
        Ok(None)
    }

    pub(super) fn require_current(&self) -> Result<(), Error> {
        if Digest::of(&std::fs::read(&self.program)?) != self.artifact
            || Digest::of(&std::fs::read(&self.environment)?) != self.environment_artifact
        {
            return Err(Error::Executable);
        }
        Ok(())
    }

    pub(super) fn available() -> Result<bool, Error> {
        if !cfg!(target_os = "linux") || std::env::var_os("LOOM_INSIDE").is_some() {
            return Ok(false);
        }
        let uid = rustix::process::geteuid().as_raw();
        let membership = std::fs::read_to_string("/proc/self/cgroup")?;
        Ok(membership.lines().any(|line| {
            line.starts_with(&format!(
                "0::/user.slice/user-{uid}.slice/user@{uid}.service/"
            ))
        }))
    }

    pub(super) fn run(
        &self,
        builder: &str,
        arguments: &[String],
        environment: &BTreeMap<String, String>,
        output: &Path,
        clock: &dyn Clock,
    ) -> Result<String, Error> {
        self.require_current()?;
        let unit = format!(
            "{}.service",
            output
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(Error::Directory)?
        );
        let uid = rustix::process::geteuid().as_raw();
        let group = format!(
            "/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/{unit}"
        );
        let stdout = std::fs::File::create(output.join("launcher-stdout"))?;
        let stderr = std::fs::File::create(output.join("launcher-stderr"))?;
        let mut command = Command::new(&self.program);
        command.args([
            "--user",
            "--quiet",
            "--wait",
            "--pipe",
            "--collect",
            "--unit",
            &unit,
        ]);
        for property in properties(&group, output)? {
            command.arg(format!("--property={property}"));
        }
        command.arg("--").arg(&self.environment).arg("-i");
        for (name, value) in environment {
            if name != "out" {
                command.arg(format!("{name}={value}"));
            }
        }
        command
            .arg("out=/run/loom-report")
            .arg(builder)
            .args(arguments)
            .env_clear();
        for name in ["DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command.stdin(Stdio::null()).stdout(stdout).stderr(stderr);
        let mut child = command.spawn()?;
        let status = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build()?;
                    runtime.block_on(async {
                        let deadline = clock.now() + Duration::from_secs(330);
                        loop {
                            if let Some(status) = child.try_wait()? {
                                return Ok(status);
                            }
                            if clock.now() >= deadline {
                                child.kill()?;
                                child.wait()?;
                                return Err(Error::Timeout);
                            }
                            clock.sleep(Duration::from_millis(20)).await;
                        }
                    })
                })
                .join()
                .map_err(|_| Error::Wait)?
        })?;
        if !status.success() {
            return Err(Error::Launch {
                detail: std::fs::read_to_string(output.join("launcher-stderr"))?,
            });
        }
        self.require_current()?;
        Ok(unit)
    }
}

impl super::Graph {
    pub(super) fn run_managed(
        &self,
        target: super::Name,
        contract: &super::report::Contract,
    ) -> Result<super::Observed, super::Error> {
        let unavailable = || {
            super::Observed::execution(
            target.clone(),
            super::Outcome::Unavailable {
                capability: super::Capability::HostKernel,
                detail: "delegated native verification requires the trusted driver's Linux user-service lane".into(),
            },
            vec![super::Observation::ExecutionGraph { digest: self.identity.clone() }],
        )
        };
        let Some(executor) = &self.managed else {
            return Ok(unavailable());
        };
        if !Executor::available()? {
            return Ok(unavailable());
        }
        let reporter = self.reporter.as_ref().ok_or(super::Error::Result)?;
        let node = self.nodes.get(reporter).ok_or(super::Error::Root)?;
        super::frontier::require_pinned(&self.nodes, reporter, |outputs| self.presence(outputs))?;
        let installables: Vec<String> = node
            .dependencies
            .iter()
            .map(|(path, outputs)| {
                format!(
                    "{}^{}",
                    path.as_str(),
                    outputs
                        .iter()
                        .map(super::Name::as_str)
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect();
        if !installables.is_empty() {
            let mut args = vec![
                "build",
                "--no-link",
                "--offline",
                "--no-substitute",
                "--option",
                "sandbox",
                "true",
                "--option",
                "sandbox-fallback",
                "false",
                "--option",
                "builders",
                "",
            ];
            args.extend(installables.iter().map(String::as_str));
            let prepared = super::invoke(&self.tool, &args)?;
            if !prepared.status.success() {
                return Err(super::Error::Report {
                    detail: format!(
                        "native compiler/input preparation failed: {}",
                        super::diagnostic(&prepared.stderr)
                    ),
                });
            }
        }
        let directory = tempfile::Builder::new().prefix("loom-native-").tempdir()?;
        let clock = SystemClock::new();
        let result = executor.run(
            &node.builder,
            &node.args,
            &node.env,
            directory.path(),
            &clock,
        );
        let unit = match result {
            Ok(unit) => unit,
            Err(error) => {
                let retained = directory.keep();
                let detail = format!("{error}; diagnostics retained at {}", retained.display());
                if matches!(error, Error::Executable | Error::Directory) {
                    return Err(Error::Launch { detail }.into());
                }
                return Err(super::Error::ReportInfrastructure { code: 78, detail });
            }
        };
        let report = match super::report::Report::read_directory(directory.path(), contract) {
            Ok(report) => report,
            Err(error) => {
                let retained = directory.keep();
                return Err(super::Error::Report {
                    detail: format!("{error}; native report retained at {}", retained.display()),
                });
            }
        };
        self.require_current()?;
        let retained = directory.keep();
        Ok(super::Observed::reported(
            target,
            report,
            vec![
                super::Observation::ExecutionGraph {
                    digest: self.identity.clone(),
                },
                super::Observation::NativeExecution {
                    unit: super::Name::try_from(unit).map_err(|_| Error::Directory)?,
                },
                super::Observation::NativeReport {
                    directory: retained,
                },
            ],
        ))
    }
}

fn properties(group: &str, output: &Path) -> Result<Vec<String>, Error> {
    let path = output.to_str().ok_or(Error::Directory)?;
    if path.contains([':', ' ', '\n', '\\']) {
        return Err(Error::Directory);
    }
    let private_sys = output.join("private-sys");
    let relative = Path::new(group)
        .strip_prefix("/sys")
        .map_err(|_| Error::Directory)?;
    std::fs::create_dir_all(private_sys.join(relative))?;
    Ok([
        "Slice=app.slice",
        "Delegate=yes",
        "RuntimeMaxSec=300",
        "TimeoutStopSec=10",
        "KillMode=control-group",
        "ProtectSystem=strict",
        "ProtectHome=tmpfs",
        "PrivateTmp=yes",
        "PrivateNetwork=yes",
        "PrivateDevices=yes",
        "PrivatePIDs=yes",
        "ProtectControlGroups=yes",
        "ProtectKernelTunables=yes",
        "NoNewPrivileges=yes",
        "CapabilityBoundingSet=",
        "TemporaryFileSystem=/run",
        "InaccessiblePaths=-/nix/var -/etc/wrix",
        "UMask=0077",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain([
        format!("ReadWritePaths={group}"),
        format!("BindReadOnlyPaths={path}/private-sys:/sys"),
        format!("BindPaths={path}:/run/loom-report {group}:{group}"),
    ])
    .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn source_environment_cannot_change_the_host_launcher_or_isolation_profile() {
        let work = tempfile::tempdir().unwrap();
        let output = tempfile::Builder::new()
            .prefix("loom-native-")
            .tempdir()
            .unwrap();
        let paths: Vec<_> = std::env::split_paths(&std::env::var_os("PATH").unwrap()).collect();
        let bash = paths
            .iter()
            .map(|path| path.join("bash"))
            .find(|path| path.is_file())
            .unwrap()
            .canonicalize()
            .unwrap();
        let environment = paths
            .iter()
            .map(|path| path.join("env"))
            .find(|path| path.is_file())
            .unwrap()
            .canonicalize()
            .unwrap();
        let program = work.path().join("systemd-run");
        std::fs::write(
            &program,
            format!(
                "#!{}\nprintf 'parent=%s\\n' \"${{UNTRUSTED:-absent}}\"\nprintf '%s\\n' \"$@\"\n",
                bash.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let executor = Executor {
            artifact: Digest::of(&std::fs::read(&program).unwrap()),
            environment_artifact: Digest::of(&std::fs::read(&environment).unwrap()),
            program,
            environment: environment.clone(),
        };
        let env = [
            ("UNTRUSTED".into(), "source-value".into()),
            ("out".into(), "/host/authority".into()),
        ]
        .into();
        executor
            .run(
                "/nix/store/example/bin/bash",
                &["producer".into()],
                &env,
                output.path(),
                &SystemClock::new(),
            )
            .unwrap();
        let transcript = std::fs::read_to_string(output.path().join("launcher-stdout")).unwrap();
        let arguments: Vec<_> = transcript.lines().collect();
        assert_eq!(arguments[0], "parent=absent");
        let boundary = arguments
            .iter()
            .position(|value| *value == environment.to_str().unwrap())
            .unwrap();
        assert_eq!(arguments[boundary - 1], "--");
        assert_eq!(arguments[boundary + 1], "-i");
        assert!(arguments[..boundary].contains(&"--property=ProtectHome=tmpfs"));
        assert!(arguments[..boundary].contains(&"--property=PrivateNetwork=yes"));
        assert!(arguments[..boundary].contains(&"--property=PrivateDevices=yes"));
        assert!(arguments[..boundary].contains(&"--property=PrivatePIDs=yes"));
        assert!(arguments[..boundary].contains(&"--property=ProtectControlGroups=yes"));
        assert!(arguments[boundary..].contains(&"UNTRUSTED=source-value"));
        assert!(arguments[boundary..].contains(&"out=/run/loom-report"));
        assert!(!arguments.contains(&"out=/host/authority"));
    }
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// isolated native executor I/O failed
    Io(#[from] std::io::Error),
    /// delegated native verification requires an immutable systemd-run executable
    Executable,
    /// native verifier output directory is invalid
    Directory,
    /// isolated native verifier exceeded its bounded lifetime
    Timeout,
    /// bounded native service wait failed
    Wait,
    /// isolated native verifier could not start: {detail}
    Launch { detail: String },
}
