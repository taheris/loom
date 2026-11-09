//! Real immutable Nix inputs and signed, published Git factories for CLI tests.
//!
//! Only external Wrix/Beads transports remain mocked by the calling fixture.
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// Prepare the production discovery boundary, rather than injecting a manifest.
/// Execution methods return preparation failures without starting a child.
pub trait ProfileFactoryCommand {
    fn profile_factory(&mut self, manifest: impl AsRef<Path>) -> FactoryCommand<'_>;
}
impl ProfileFactoryCommand for Command {
    fn profile_factory(&mut self, manifest: impl AsRef<Path>) -> FactoryCommand<'_> {
        let arguments = self.get_args().collect::<Vec<_>>();
        let workspace = arguments
            .windows(2)
            .find(|pair| pair[0] == "--workspace")
            .map(|pair| PathBuf::from(pair[1]))
            .or_else(|| self.get_current_dir().map(Path::to_path_buf))
            .or_else(|| manifest.as_ref().parent().map(Path::to_path_buf));
        let (workspace, preparation_error) = match workspace {
            Some(workspace) => {
                let error = if workspace
                    .join(".git/profile-factory-fixture/published")
                    .is_file()
                {
                    None
                } else {
                    publish(&workspace, manifest.as_ref()).err()
                };
                (workspace, error)
            }
            None => (
                PathBuf::new(),
                Some(io::Error::other("fixture workspace is unavailable")),
            ),
        };
        self.env(
            "GIT_CONFIG_GLOBAL",
            workspace.join(".git/profile-factory-fixture/host-gitconfig"),
        );
        self.env(
            "XDG_STATE_HOME",
            std::env::temp_dir().join("profile-fixture-state"),
        );
        FactoryCommand {
            command: self,
            workspace,
            preparation_error,
        }
    }
}

/// Fixture builder retaining the host-only storage boundary across later overrides.
pub struct FactoryCommand<'a> {
    command: &'a mut Command,
    workspace: PathBuf,
    preparation_error: Option<io::Error>,
}
impl FactoryCommand<'_> {
    fn prepared(&self) -> io::Result<()> {
        match &self.preparation_error {
            Some(error) => Err(io::Error::new(error.kind(), error.to_string())),
            None => Ok(()),
        }
    }

    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        if self.preparation_error.is_some() {
            return self;
        }
        let path = Path::new(value.as_ref());
        if key.as_ref() == "XDG_STATE_HOME" && path.starts_with(&self.workspace) {
            self.command
                .env(key, std::env::temp_dir().join("profile-fixture-state"));
        } else {
            if key.as_ref() == "BD_STATE_DIR"
                && let Ok(relative) = path.strip_prefix(&self.workspace)
            {
                let exclude = self.workspace.join(".git/info/exclude");
                if let Ok(mut ignored) = std::fs::read_to_string(&exclude) {
                    ignored.push('/');
                    ignored.push_str(&relative.to_string_lossy());
                    ignored.push('\n');
                    let _ = std::fs::write(exclude, ignored);
                }
            }
            self.command.env(key, value);
        }
        self
    }
    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.command.env_remove(key);
        self
    }
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.command.arg(arg);
        self
    }
    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.args(args);
        self
    }
    pub fn stdin(&mut self, value: impl Into<std::process::Stdio>) -> &mut Self {
        self.command.stdin(value);
        self
    }
    pub fn stdout(&mut self, value: impl Into<std::process::Stdio>) -> &mut Self {
        self.command.stdout(value);
        self
    }
    pub fn stderr(&mut self, value: impl Into<std::process::Stdio>) -> &mut Self {
        self.command.stderr(value);
        self
    }
    pub fn process_group(&mut self, group: i32) -> &mut Self {
        std::os::unix::process::CommandExt::process_group(self.command, group);
        self
    }
    /// # Errors
    /// Returns the underlying process execution error.
    pub fn output(&mut self) -> io::Result<Output> {
        self.prepared()?;
        self.command.output()
    }
    /// # Errors
    /// Returns the underlying process execution error.
    pub fn spawn(&mut self) -> io::Result<std::process::Child> {
        self.prepared()?;
        self.command.spawn()
    }
    /// # Errors
    /// Returns the underlying process execution error.
    pub fn status(&mut self) -> io::Result<std::process::ExitStatus> {
        self.prepared()?;
        self.command.status()
    }
}

fn run(command: &mut Command) -> io::Result<Output> {
    crate::scrub_git_local_env(command);
    let output = command.output()?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(io::Error::other(format!(
            "{command:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}
fn git(root: &Path, arguments: &[&OsStr]) -> io::Result<Output> {
    run(loom_driver::git::GitClient::fixture_command(root).args(arguments))
}
fn text(output: Output) -> io::Result<String> {
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_owned())
        .map_err(io::Error::other)
}
fn store(path: &Path) -> io::Result<PathBuf> {
    text(run(Command::new("nix-store").arg("--add").arg(path))?).map(PathBuf::from)
}
fn literal(value: &str) -> String {
    // JSON string syntax is also valid Nix string syntax after escaping interpolation.
    json!(value).to_string().replace("${", "\\${")
}
fn shell() -> io::Result<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join("bash"))
        .filter_map(|path| path.canonicalize().ok())
        .find(|path| path.starts_with("/nix/store") && path.is_file())
        .ok_or_else(|| io::Error::other("fixture requires its declared Nix-store Bash dependency"))
}
fn contexts(paths: &[PathBuf]) -> String {
    let mut result = String::new();
    for path in paths {
        result.push_str(&literal(&path.to_string_lossy()));
        result.push_str(" = { path = true; };");
    }
    result
}
fn recipe(shell: &Path, system: &str, body: &str, dependencies: &[PathBuf]) -> io::Result<Value> {
    let context = contexts(dependencies);
    let expression = format!(
        "let d = builtins.derivation {{ name = \"loom-profile-fixture\"; system = {}; builder = builtins.storePath {}; args = [ \"-c\" \"printf '%s' \\\"$1\\\" > \\\"$out\\\"\" \"fixture\" (builtins.appendContext {} {{ {} }}) ]; }}; in {{ path = d.outPath; derivation = d.drvPath; }}",
        literal(system),
        literal(&shell.to_string_lossy()),
        literal(body),
        context
    );
    let output =
        run(Command::new("nix").args(["eval", "--impure", "--json", "--expr", &expression]))?;
    serde_json::from_slice(&output.stdout).map_err(io::Error::other)
}

/// Freeze fixture image data and publish a genuinely authenticated operator factory.
///
/// # Errors
/// External Git/Nix/SSH failures, malformed fixtures, or unsupported runtime tools.
pub fn publish(workspace: &Path, manifest: &Path) -> io::Result<()> {
    publish_with_key(workspace, manifest, None)
}

/// Publish using a fixture-owned signing key when the CLI exercises repository-key mode.
///
/// # Errors
/// Returns filesystem, Git, Nix or SSH fixture preparation failures.
pub fn publish_with_key(
    workspace: &Path,
    manifest: &Path,
    signing: Option<&Path>,
) -> io::Result<()> {
    let workspace = workspace.canonicalize()?;
    if !workspace.starts_with(std::env::temp_dir().canonicalize()?) {
        return Err(io::Error::other(
            "profile factory fixtures require the private test temporary root",
        ));
    }
    if !workspace.join(".git").exists() {
        git(
            &workspace,
            &[
                "init".as_ref(),
                "-q".as_ref(),
                "-b".as_ref(),
                "main".as_ref(),
            ],
        )?;
    }
    let metadata = workspace.join(".git/profile-factory-fixture");
    std::fs::create_dir_all(&metadata)?;
    let shell = shell()?;
    let system = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => "aarch64-darwin",
        ("x86_64", "macos") => "x86_64-darwin",
        ("aarch64", "linux") => "aarch64-linux",
        ("x86_64", "linux") => "x86_64-linux",
        _ => return Err(io::Error::other("unsupported fixture system")),
    };
    let payload = metadata.join("image.json");
    std::fs::write(&payload, "{}")?;
    let payload = store(&payload)?;
    let mut matrix: Value =
        serde_json::from_slice(&std::fs::read(manifest)?).map_err(io::Error::other)?;
    let mut catalogue = json!({"schema":1,"profiles":{}});
    let mut derivations = Vec::new();
    for (profile, runtimes) in matrix
        .as_object_mut()
        .ok_or_else(|| io::Error::other("profile map"))?
    {
        for (runtime, image) in runtimes
            .as_object_mut()
            .ok_or_else(|| io::Error::other("runtime map"))?
        {
            let config = image["profile_config"].as_str().map(PathBuf::from);
            let config_body = config.as_deref().filter(|path| path.is_file()).map(std::fs::read).transpose()?
                .unwrap_or_else(|| json!({"profile":{"name":profile,"mounts":[]},"agent":{"kind":runtime},"image":image}).to_string().into_bytes());
            let name = config.as_deref().and_then(Path::file_name).map_or_else(
                || format!("{profile}-{runtime}-profile.json"),
                |name| name.to_string_lossy().into_owned(),
            );
            let name = if config
                .as_deref()
                .is_some_and(|path| path.starts_with("/nix/store"))
                && name.as_bytes().get(32) == Some(&b'-')
            {
                name[33..].to_owned()
            } else {
                name
            };
            let config_path = metadata.join(name);
            std::fs::write(&config_path, config_body)?;
            let config_path = store(&config_path)?;
            image["profile_config"] = json!(config_path);
            image["source"] = json!(payload);
            let selected = json!({profile:{runtime:image}}).to_string();
            let realization = recipe(&shell, system, &selected, &[payload.clone(), config_path])?;
            catalogue["profiles"][profile][runtime] = json!({"ref":image["ref"],"source_kind":image["source_kind"],"realization":realization});
            derivations.push(PathBuf::from(
                realization["derivation"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("recipe derivation"))?,
            ));
        }
    }
    std::fs::write(manifest, matrix.to_string())?;
    // Pin tiny fixture outputs while the temporary operator exists. No containers run.
    for (index, derivation) in derivations.iter().enumerate() {
        run(Command::new("nix-store")
            .arg("--realise")
            .arg(derivation)
            .arg("--add-root")
            .arg(metadata.join(format!("output-{index}")))
            .arg("--indirect"))?;
    }
    let context = contexts(&derivations);
    let shell_root = shell
        .ancestors()
        .find(|path| path.parent() == Some(Path::new("/nix/store")))
        .ok_or_else(|| io::Error::other("store shell root"))?;
    let checked = workspace.join(".profile-fixture/verification.nix");
    std::fs::create_dir_all(
        checked
            .parent()
            .ok_or_else(|| io::Error::other("fixture source parent"))?,
    )?;
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../nix/verification.nix"),
        &checked,
    )?;
    let flake = format!(
        "{{ outputs = {{ self }}: let checked = import ./.profile-fixture/verification.nix; in {{ packages.{}.profile-catalogue = checked.check {{ resources = []; evaluation = [ (checked.resource {{ root = ./.; origin = \"flake.nix\"; }}) (checked.resource {{ root = ./.; origin = \".profile-fixture/verification.nix\"; }}) ]; build = _: builtins.derivation {{ name = \"fixture-catalogue.json\"; system = {}; builder = builtins.appendContext {} {{ {} = {{ path = true; }}; }}; args = [ \"-c\" \"printf '%s' \\\"$1\\\" > \\\"$out\\\"\" \"fixture\" (builtins.appendContext {} {{ {} }}) ]; }}; }}; }}; }}\n",
        literal(system),
        literal(system),
        literal(&shell.to_string_lossy()),
        literal(&shell_root.to_string_lossy()),
        literal(&catalogue.to_string()),
        context
    );
    std::fs::write(workspace.join("flake.nix"), flake)?;
    // A private exclude keeps transport fixtures and runtime state out of source authority.
    let exclude = workspace.join(".git/info/exclude");
    let mut ignored = std::fs::read_to_string(&exclude).unwrap_or_default();
    ignored.push_str("\n/.loom/\n/.beads/\n/.wrix/\n/.loom-test-state/\n/bd-bin/\n/native-bd-state/\n/native-bd-bin/\n");
    if let Ok(relative) = manifest.strip_prefix(&workspace) {
        ignored.push('/');
        ignored.push_str(&relative.to_string_lossy());
        ignored.push('\n');
    }
    std::fs::write(exclude, ignored)?;
    let key = metadata.join("key");
    if let Some(signing) = signing {
        std::fs::copy(signing, &key)?;
        std::fs::copy(signing.with_extension("pub"), key.with_extension("pub"))?;
    }
    if !key.exists() {
        run(Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key))?;
    }
    let email = "profile-fixture@example.invalid";
    let allowed = workspace.join(".git/wrix/allowed_signers");
    if let Some(parent) = allowed.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let signers = format!(
        "{email} {}",
        std::fs::read_to_string(key.with_extension("pub"))?
    );
    std::fs::write(&allowed, &signers)?;
    std::fs::write(workspace.join(".git/loom-allowed-signers"), &signers)?;
    let host_signers = metadata.join("host-allowed-signers");
    std::fs::write(&host_signers, &signers)?;
    let inherited = std::env::var_os("GIT_CONFIG_GLOBAL").map(PathBuf::from);
    let include = inherited
        .filter(|path| path.is_file())
        .map(|path| format!("[include]\npath = {}\n", literal(&path.to_string_lossy())))
        .unwrap_or_default();
    std::fs::write(
        metadata.join("host-gitconfig"),
        format!(
            "{include}[gpg]\nformat = ssh\n[gpg \"ssh\"]\nprogram = ssh-keygen\nallowedSignersFile = {}\n[user]\nsigningkey = {}\n",
            literal(&host_signers.to_string_lossy()),
            literal(&key.to_string_lossy())
        ),
    )?;
    for (name, value) in [
        ("user.name", "Profile factory fixture"),
        ("user.email", email),
        ("gpg.format", "ssh"),
        ("commit.gpgsign", "true"),
        ("gpg.ssh.program", "ssh-keygen"),
    ] {
        git(
            &workspace,
            &[
                "config".as_ref(),
                "--local".as_ref(),
                name.as_ref(),
                value.as_ref(),
            ],
        )?;
    }
    git(
        &workspace,
        &[
            "config".as_ref(),
            "--local".as_ref(),
            "user.signingkey".as_ref(),
            key.as_os_str(),
        ],
    )?;
    git(
        &workspace,
        &[
            "config".as_ref(),
            "--local".as_ref(),
            "gpg.ssh.allowedSignersFile".as_ref(),
            allowed.as_os_str(),
        ],
    )?;
    git(&workspace, &["add".as_ref(), "--all".as_ref()])?;
    git(
        &workspace,
        &[
            "-c".as_ref(),
            "core.hooksPath=/dev/null".as_ref(),
            "commit".as_ref(),
            "--allow-empty".as_ref(),
            "-qm".as_ref(),
            "Publish isolated profile fixture".as_ref(),
        ],
    )?;
    let origin = git(
        &workspace,
        &["remote".as_ref(), "get-url".as_ref(), "origin".as_ref()],
    )
    .and_then(text)
    .ok();
    let local_origin = origin
        .as_deref()
        .map(|value| Path::new(value.strip_prefix("file://").unwrap_or(value)))
        .is_some_and(|path| path.starts_with(std::env::temp_dir()));
    if !local_origin {
        let remote_name = origin
            .as_deref()
            .and_then(|value| value.rsplit('/').next())
            .filter(|value| {
                value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            })
            .unwrap_or("fixture.git");
        let remote = metadata.join(remote_name);
        run(loom_driver::git::GitClient::fixture_command(&workspace)
            .args(["init", "-q", "--bare"])
            .arg(&remote))?;
        if let Some(origin) = &origin {
            let rewrite = format!("url.file://{}.insteadOf", remote.display());
            git(
                &workspace,
                &[
                    "config".as_ref(),
                    "--local".as_ref(),
                    rewrite.as_ref(),
                    origin.as_ref(),
                ],
            )?;
        } else {
            git(
                &workspace,
                &[
                    "remote".as_ref(),
                    "add".as_ref(),
                    "origin".as_ref(),
                    remote.as_os_str(),
                ],
            )?;
        }
    }
    git(
        &workspace,
        &[
            "push".as_ref(),
            "-q".as_ref(),
            "origin".as_ref(),
            "HEAD:main".as_ref(),
        ],
    )?;
    git(
        &workspace,
        &[
            "fetch".as_ref(),
            "-q".as_ref(),
            "origin".as_ref(),
            "main".as_ref(),
        ],
    )?;
    let integration = workspace.join(".loom/integration");
    if !integration.join(".git").is_dir() {
        if integration.exists() {
            return Err(io::Error::other(
                "fixture integration exists without Git authority",
            ));
        }
        run(loom_driver::git::GitClient::fixture_command(&workspace)
            .args(["clone", "-q", "--no-hardlinks"])
            .arg(&workspace)
            .arg(&integration))?;
    }
    std::fs::write(metadata.join("published"), "published\n")?;
    Ok(())
}
