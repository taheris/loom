//! Delegated native verification uses a bounded, private user-service sandbox.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use loom_driver::clock::{Clock, SystemClock};

use displaydoc::Display;
use serde::Serialize;
use thiserror::Error;

use super::{Digest, StorePath};

mod inputs;

#[derive(Clone, Copy)]
struct Program<'a> {
    builder: &'a str,
    arguments: &'a [String],
    environment: &'a BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Profile {
    Delegated,
    Container,
}

const CONTAINER_DEVICES: [&str; 3] = ["/dev/fuse", "/dev/net/tun", "/dev/kvm"];

impl Profile {
    const fn seconds(self) -> u64 {
        match self {
            Self::Delegated => 300,
            Self::Container => 900,
        }
    }

    fn available(self) -> Result<bool, Error> {
        if self == Self::Container {
            use std::os::unix::fs::FileTypeExt as _;
            for device in CONTAINER_DEVICES {
                match std::fs::metadata(device) {
                    Ok(metadata) if metadata.file_type().is_char_device() => {}
                    Ok(_) => return Ok(false),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct Executor {
    program: PathBuf,
    artifact: Digest,
    environment: PathBuf,
    environment_artifact: Digest,
    namespace: Option<Namespace>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Namespace {
    program: PathBuf,
    artifact: Digest,
}

impl Executor {
    pub(super) fn capture(runtime: super::Runtime) -> Result<Option<Self>, Error> {
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
                let mut namespace = None;
                for directory in std::env::split_paths(&search)
                    .filter(|_| runtime == super::Runtime::NativeContainer)
                {
                    let path = directory.join("unshare");
                    if path.is_file() {
                        let program = path.canonicalize()?;
                        StorePath::root(&program).map_err(|_| Error::Executable)?;
                        namespace = Some(Namespace {
                            artifact: Digest::of(&std::fs::read(&program)?),
                            program,
                        });
                        break;
                    }
                }
                return Ok(Some(Self {
                    namespace,
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
        if let Some(namespace) = &self.namespace
            && Digest::of(&std::fs::read(&namespace.program)?) != namespace.artifact
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

    fn run(
        &self,
        program: Program<'_>,
        output: &Path,
        clock: &dyn Clock,
        profile: Profile,
        sdk: &BTreeSet<StorePath>,
    ) -> Result<String, Error> {
        self.require_current()?;
        let Program {
            builder,
            arguments,
            environment,
        } = program;
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
            "--expand-environment=no",
            "--unit",
            &unit,
        ]);
        for property in properties(&group, output, profile, sdk)? {
            command.arg(format!("--property={property}"));
        }
        command.arg("--").arg(&self.environment).arg("-i");
        let mut environment = environment.clone();
        environment.insert("out".into(), "/run/loom-report".into());
        environment.remove("LOOM_NATIVE_CGROUP_PARENT");
        if profile == Profile::Container {
            let parent = group
                .strip_prefix("/sys/fs/cgroup")
                .ok_or(Error::Directory)?;
            environment.insert("LOOM_NATIVE_CGROUP_PARENT".into(), parent.into());
        }
        for (name, value) in environment {
            command.arg(format!("{name}={value}"));
        }
        if profile == Profile::Container {
            let namespace = self.namespace.as_ref().ok_or(Error::Executable)?;
            command.arg(&namespace.program).args([
                "--user",
                "--map-root-user",
                "--mount",
                "--net",
                "--cgroup",
                "--keep-caps",
                "--fork",
                "--",
            ]);
        }
        command.arg(builder).args(arguments).env_clear();
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
                        let deadline = clock.now() + Duration::from_secs(profile.seconds() + 30);
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
        let unavailable = |detail: &str| {
            super::Observed::execution(
                target.clone(),
                super::Outcome::Unavailable {
                    capability: super::Capability::HostKernel,
                    detail: detail.into(),
                },
                vec![super::Observation::ExecutionGraph {
                    digest: self.identity.clone(),
                }],
            )
        };
        let Some(executor) = &self.managed else {
            return Ok(unavailable(
                "delegated native verification requires the trusted driver's Linux user-service lane",
            ));
        };
        let profile = match self.runtime {
            super::Runtime::Nix => return Err(super::Error::Result),
            super::Runtime::NativeCgroup => Profile::Delegated,
            super::Runtime::NativeContainer => Profile::Container,
        };
        if !Executor::available()? {
            return Ok(unavailable(
                "delegated native verification requires the trusted driver's Linux user-service lane",
            ));
        }
        if profile == Profile::Container && executor.namespace.is_none() {
            return Ok(unavailable(
                "native container verification requires an immutable user-namespace launcher",
            ));
        }
        if !profile.available()? {
            return Ok(unavailable(
                "native container verification requires /dev/fuse, /dev/net/tun and /dev/kvm character devices",
            ));
        }
        let reporter = self.reporter.as_ref().ok_or(super::Error::Result)?;
        let node = self.nodes.get(reporter).ok_or(super::Error::Root)?;
        super::frontier::require_pinned(&self.nodes, reporter, |outputs| self.present(outputs))?;
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
        let mut environment = node.env.clone();
        let mut sdk = BTreeSet::new();
        if profile == Profile::Container {
            let registration = inputs::registration(self, node, executor)?;
            self.require_current()?;
            std::fs::write(
                directory.path().join("native-input-registration"),
                registration.bytes,
            )?;
            sdk = registration.paths;
            environment.insert(
                "LOOM_NATIVE_NIX_REGISTRATION".into(),
                "/run/loom-report/native-input-registration".into(),
            );
        }
        let result = executor.run(
            Program {
                builder: &node.builder,
                arguments: &node.args,
                environment: &environment,
            },
            directory.path(),
            &clock,
            profile,
            &sdk,
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

fn properties(
    group: &str,
    output: &Path,
    profile: Profile,
    sdk: &BTreeSet<StorePath>,
) -> Result<Vec<String>, Error> {
    let path = output.to_str().ok_or(Error::Directory)?;
    if path.contains([':', ' ', '\n', '\\']) {
        return Err(Error::Directory);
    }
    let private_sys = output.join("private-sys");
    let relative = Path::new(group)
        .strip_prefix("/sys")
        .map_err(|_| Error::Directory)?;
    std::fs::create_dir_all(private_sys.join(relative))?;
    let mut result: Vec<_> = [
        "Slice=app.slice",
        "Delegate=yes",
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
        "InaccessiblePaths=-/nix/var -/etc/wrix",
        "UMask=0077",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain([
        format!("RuntimeMaxSec={}", profile.seconds()),
        if profile == Profile::Container {
            // These sysctls belong to the mandatory private network namespace.
            format!("ReadWritePaths={group} /proc/sys/net")
        } else {
            format!("ReadWritePaths={group}")
        },
        if profile == Profile::Container {
            format!(
                "TemporaryFileSystem=/run /nix/store:uid={},gid={},mode=0755",
                rustix::process::geteuid().as_raw(),
                rustix::process::getegid().as_raw()
            )
        } else {
            "TemporaryFileSystem=/run".to_owned()
        },
        format!(
            "BindPaths={path}:/run/loom-report {group}:{group}{}",
            if profile == Profile::Container {
                " /dev/fuse /dev/net/tun /dev/kvm"
            } else {
                ""
            }
        ),
    ])
    .collect();
    if profile == Profile::Container {
        result.push("DelegateSubgroup=supervisor".to_owned());
    }
    let mut readonly = if profile == Profile::Container {
        vec![
            "/sys".to_owned(),
            "/dev/null:/etc/subuid".to_owned(),
            "/dev/null:/etc/subgid".to_owned(),
        ]
    } else {
        vec![format!("{path}/private-sys:/sys")]
    };
    readonly.extend(sdk.iter().map(|path| path.as_str().to_owned()));
    for chunk in readonly.chunks(128) {
        result.push(format!("BindReadOnlyPaths={}", chunk.join(" ")));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn container_profile_keeps_isolation_and_binds_only_its_fixed_devices() {
        let output = tempfile::tempdir().unwrap();
        let group = "/sys/fs/cgroup/verification-fixture";
        let delegated =
            properties(group, output.path(), Profile::Delegated, &BTreeSet::new()).unwrap();
        let container =
            properties(group, output.path(), Profile::Container, &BTreeSet::new()).unwrap();
        for property in &delegated {
            if !property.starts_with("RuntimeMaxSec=")
                && !property.starts_with("BindReadOnlyPaths=")
                && !property.starts_with("BindPaths=")
                && !property.starts_with("TemporaryFileSystem=")
                && !property.starts_with("ReadWritePaths=")
            {
                assert!(container.contains(property), "lost isolation: {property}");
            }
        }
        assert!(container.contains(&"PrivateDevices=yes".to_owned()));
        assert!(container.contains(&"PrivateNetwork=yes".to_owned()));
        assert!(container.contains(&"NoNewPrivileges=yes".to_owned()));
        assert!(container.contains(&"ProtectHome=tmpfs".to_owned()));
        assert!(container.contains(&format!("ReadWritePaths={group} /proc/sys/net")));
        assert!(container.contains(&"DelegateSubgroup=supervisor".to_owned()));
        assert!(container.contains(
            &"BindReadOnlyPaths=/sys /dev/null:/etc/subuid /dev/null:/etc/subgid".to_owned()
        ));
        assert!(container.contains(&format!(
            "BindPaths={}:/run/loom-report {group}:{group} /dev/fuse /dev/net/tun /dev/kvm",
            output.path().display(),
        )));
        assert!(
            !delegated
                .iter()
                .any(|property| property.contains("/dev/fuse"))
        );
    }

    #[cfg(feature = "native-container-tests")]
    #[test]
    fn native_container_devices_do_not_expose_host_files_pids_or_network() {
        use std::net::{Ipv4Addr, TcpListener, TcpStream};
        let hidden = tempfile::NamedTempFile::new().unwrap();
        let host_forwarding = std::fs::read_to_string("/proc/sys/net/ipv4/ip_forward").unwrap();
        let private_forwarding = if host_forwarding.trim() == "0" {
            "1"
        } else {
            "0"
        };
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        drop(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
        assert!(Executor::available().unwrap());
        assert!(Profile::Container.available().unwrap());
        let executor = Executor::capture(super::super::Runtime::NativeContainer)
            .unwrap()
            .unwrap();
        let bash = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|path| path.join("bash"))
            .find(|path| path.is_file())
            .unwrap()
            .canonicalize()
            .unwrap();
        let nix = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|path| path.join("nix"))
            .find(|path| path.is_file())
            .unwrap()
            .canonicalize()
            .unwrap();
        let ip = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|path| path.join("ip"))
            .find(|path| path.is_file())
            .unwrap()
            .canonicalize()
            .unwrap();
        let roots = BTreeSet::from([
            StorePath::root(&ip).unwrap(),
            StorePath::root(&bash).unwrap(),
            StorePath::root(&nix).unwrap(),
            StorePath::root(&executor.environment).unwrap(),
            StorePath::root(&executor.namespace.as_ref().unwrap().program).unwrap(),
        ]);
        let sdk = inputs::capture(&nix, &roots).unwrap();
        let output = tempfile::Builder::new()
            .prefix("loom-native-")
            .tempdir()
            .unwrap();
        let script = r#"set -euo pipefail
[[ "$EUID" == 0 ]]
"$mount_program" -t devpts -o newinstance,ptmxmode=0666,mode=0620,gid=0 devpts /dev/pts
"$mount_program" --bind /dev/pts/ptmx /dev/ptmx
exec {pty}<>/dev/ptmx
exec {pty}>&-
IFS=: read -r _ _ current_group < /proc/self/cgroup
[[ "$current_group" == / && ! -w /sys/fs/cgroup/cgroup.subtree_control ]]
read -r -a delegated < "/sys/fs/cgroup$LOOM_NATIVE_CGROUP_PARENT/cgroup.controllers"
printf '+%s ' "${delegated[@]}" > "/sys/fs/cgroup$LOOM_NATIVE_CGROUP_PARENT/cgroup.subtree_control"
"$mount_program" -t cgroup2 none /sys/fs/cgroup
"$mkdir_program" -p /sys/fs/cgroup/launcher
while IFS= read -r pid < /sys/fs/cgroup/cgroup.procs; do
  printf '%s\n' "$pid" > /sys/fs/cgroup/launcher/cgroup.procs
done
read -r -a controllers < /sys/fs/cgroup/cgroup.controllers
printf '+%s ' "${controllers[@]}" > /sys/fs/cgroup/cgroup.subtree_control
[[ -w /sys/fs/cgroup/cgroup.subtree_control ]]
IFS=: read -r _ _ current_group < /proc/self/cgroup
[[ "$current_group" == /launcher ]]
"$ip_program" link set dev lo up
printf '%s\n' "$private_forwarding" > /proc/sys/net/ipv4/ip_forward
IFS= read -r actual_forwarding < /proc/sys/net/ipv4/ip_forward
[[ "$actual_forwarding" == "$private_forwarding" && ! -w /proc/sys/kernel/hostname ]]
private_store=false
while IFS= read -r line; do
  if [[ "$line" == *' /nix/store '* && "$line" == *' - tmpfs '* ]]; then private_store=true; fi
done < /proc/self/mountinfo
[[ "$private_store" == true && -r "$immutable_bash" && ! -w "$immutable_bash" ]]
printf 'owned private store\n' > /nix/store/native-container-store-control
read -r inside_uid outside_uid count < /proc/self/uid_map
[[ "$inside_uid" == 0 && "$outside_uid" == "$host_uid" && "$count" == 1 ]]
[[ -c /dev/fuse && -c /dev/net/tun && -c /dev/kvm ]]
[[ ! -s /etc/subuid && ! -s /etc/subgid ]]
[[ ! -e "$hidden" && ! -e "/proc/$host_pid" ]]
if (echo leak > /dev/tcp/127.0.0.1/$port) 2>/dev/null; then exit 10; fi
no_privileges=0
while IFS=: read -r key value; do
  if [[ "$key" == NoNewPrivs && "$value" == *1 ]]; then no_privileges=1; fi
done < /proc/self/status
[[ "$no_privileges" == 1 ]]
unset NIX_CONFIG
export NIX_REMOTE=local NIX_USER_CONF_FILES=/dev/null
export NIX_STATE_DIR="$out/nix-state" NIX_LOG_DIR="$out/nix-log" NIX_CONF_DIR="$out/nix-conf"
"$mkdir_program" -p "$NIX_CONF_DIR"
printf 'experimental-features = nix-command flakes\nsandbox = true\nsandbox-fallback = false\nsubstituters =\nbuilders =\nbuild-users-group =\nextra-platforms =\nallow-import-from-derivation = false\n' > "$NIX_CONF_DIR/nix.conf"
"$nix_store" --load-db < "$out/input-registration"
expr='derivation { name = "native-store-control"; system = builtins.currentSystem; builder = "'"$immutable_bash"'"; args = [ "-c" "echo fresh > $out" ]; }'
drv="$("$nix_instantiate" --expr "$expr")"
[[ -f "$drv" && "$drv" == /nix/store/*.drv ]]
printf 'isolated\n' > "$out/probe"
"#;
        std::fs::write(output.path().join("input-registration"), &sdk.bytes).unwrap();
        let coreutils = executor.environment.parent().unwrap();
        let nix_bin = nix.parent().unwrap();
        let env = [
            ("LOOM_NATIVE_CGROUP_PARENT".into(), "/host/group".into()),
            (
                "mount_program".into(),
                executor
                    .namespace
                    .as_ref()
                    .unwrap()
                    .program
                    .parent()
                    .unwrap()
                    .join("mount")
                    .to_str()
                    .unwrap()
                    .to_owned(),
            ),
            ("ip_program".into(), ip.to_str().unwrap().to_owned()),
            ("private_forwarding".into(), private_forwarding.into()),
            (
                "mkdir_program".into(),
                coreutils.join("mkdir").to_str().unwrap().to_owned(),
            ),
            (
                "nix_store".into(),
                nix_bin.join("nix-store").to_str().unwrap().to_owned(),
            ),
            (
                "nix_instantiate".into(),
                nix_bin.join("nix-instantiate").to_str().unwrap().to_owned(),
            ),
            ("immutable_bash".into(), bash.to_str().unwrap().to_owned()),
            ("hidden".into(), hidden.path().to_str().unwrap().to_owned()),
            ("host_pid".into(), std::process::id().to_string()),
            (
                "host_uid".into(),
                rustix::process::geteuid().as_raw().to_string(),
            ),
            (
                "port".into(),
                listener.local_addr().unwrap().port().to_string(),
            ),
        ]
        .into();
        executor
            .run(
                Program {
                    builder: bash.to_str().unwrap(),
                    arguments: &["-euc".into(), script.into()],
                    environment: &env,
                },
                output.path(),
                &SystemClock::new(),
                Profile::Container,
                &sdk.paths,
            )
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(output.path().join("probe")).unwrap(),
            "isolated\n"
        );
        assert!(hidden.path().is_file());
        assert_eq!(
            std::fs::read_to_string("/proc/sys/net/ipv4/ip_forward").unwrap(),
            host_forwarding
        );
        assert!(!Path::new("/nix/store/native-container-store-control").exists());
        drop(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
    }

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
            namespace: None,
        };
        let env = [
            ("UNTRUSTED".into(), "source-value".into()),
            ("out".into(), "/host/authority".into()),
            ("LOOM_NATIVE_CGROUP_PARENT".into(), "/host/group".into()),
        ]
        .into();
        executor
            .run(
                Program {
                    builder: "/nix/store/example/bin/bash",
                    arguments: &["producer".into()],
                    environment: &env,
                },
                output.path(),
                &SystemClock::new(),
                Profile::Delegated,
                &BTreeSet::new(),
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
        assert!(arguments[..boundary].contains(&"--expand-environment=no"));
        assert!(arguments[..boundary].contains(&"--property=ProtectHome=tmpfs"));
        assert!(arguments[..boundary].contains(&"--property=PrivateNetwork=yes"));
        assert!(arguments[..boundary].contains(&"--property=PrivateDevices=yes"));
        assert!(arguments[..boundary].contains(&"--property=PrivatePIDs=yes"));
        assert!(arguments[..boundary].contains(&"--property=ProtectControlGroups=yes"));
        assert!(arguments[boundary..].contains(&"UNTRUSTED=source-value"));
        assert!(arguments[boundary..].contains(&"out=/run/loom-report"));
        assert!(!arguments.contains(&"out=/host/authority"));
        assert!(
            !arguments
                .iter()
                .any(|argument| argument.starts_with("LOOM_NATIVE_CGROUP_PARENT="))
        );
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
