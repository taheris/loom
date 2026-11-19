use crate::process;
use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use regex::Regex;
use std::{
    collections::BTreeSet,
    env, fs,
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, ValueEnum)]
pub enum Mode {
    Prepare,
    Check,
}

fn store_identity(value: &str) -> Result<()> {
    ensure!(
        Regex::new(r"^/nix/store/[a-z0-9]{32}-[^/\s]+$")?.is_match(value),
        "preparation requires immutable SDK store identities"
    );
    Ok(())
}

fn invalid_paths_with(
    paths: &[String],
    mut query: impl FnMut(&[String]) -> Result<(bool, String, String)>,
) -> Result<Vec<String>> {
    let mut missing = BTreeSet::new();
    for batch in paths.chunks(128) {
        let (success, stdout, stderr) = query(batch)?;
        let invalid: Vec<_> = stdout.lines().collect();
        ensure!(
            invalid.iter().all(|p| batch.iter().any(|b| b == p)),
            "unexpected store readiness response"
        );
        ensure!(
            success || !invalid.is_empty(),
            "store readiness unavailable: {}",
            stderr.trim()
        );
        missing.extend(invalid.into_iter().map(str::to_owned));
    }
    Ok(missing.into_iter().collect())
}

fn invalid_paths(paths: &[String]) -> Result<Vec<String>> {
    invalid_paths_with(paths, |batch| {
        let result = process::capture(
            Command::new("nix-store")
                .args(["--check-validity", "--print-invalid"])
                .args(batch),
            30,
        )?;
        Ok((
            result.status.success(),
            String::from_utf8(result.stdout)?,
            String::from_utf8(result.stderr)?,
        ))
    })
}

fn manifest(sdk: &Path) -> Result<Vec<String>> {
    let paths = fs::read_to_string(sdk.join("store-paths"))?
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    ensure!(!paths.is_empty(), "invalid immutable SDK closure manifest");
    for path in &paths {
        store_identity(path).context("invalid immutable SDK closure manifest")?;
    }
    Ok(paths)
}

fn check(sdk: &str) -> Result<()> {
    ensure!(
        invalid_paths(&[sdk.to_owned()])?.is_empty(),
        "SDK unavailable: {sdk}; run prepare-verification prepare before dispatch"
    );
    let paths = manifest(Path::new(sdk))?;
    let missing = invalid_paths(&paths)?;
    ensure!(
        missing.is_empty(),
        "SDK inputs unavailable (all missing paths):\n{}",
        missing.join("\n")
    );
    writeln!(
        io::stdout().lock(),
        "verification SDK ready: {} retained paths; no verification verdict issued",
        paths.len()
    )?;
    Ok(())
}

fn ensure_directory(directory: &Path, uid: u32) -> Result<()> {
    ensure!(
        directory.is_absolute(),
        "verification state directory must be absolute"
    );
    let mut ancestors: Vec<_> = directory.ancestors().collect();
    ancestors.reverse();
    for ancestor in ancestors {
        match fs::symlink_metadata(ancestor) {
            Ok(info) => {
                ensure!(
                    !info.file_type().is_symlink(),
                    "indirect verification state ancestor: {}",
                    ancestor.display()
                );
                ensure!(
                    info.is_dir() && (info.uid() == 0 || info.uid() == uid),
                    "unowned verification state ancestor: {}",
                    ancestor.display()
                );
                let sticky_root = info.uid() == 0 && info.mode() & 0o1000 != 0;
                ensure!(
                    info.mode() & 0o022 == 0 || sticky_root,
                    "writable verification state ancestor: {}",
                    ancestor.display()
                );
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::DirBuilder::new().mode(0o700).create(ancestor)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn state_directory(sdk: &str) -> Result<PathBuf> {
    let base = match env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => PathBuf::from(
            env::var_os("HOME").context("verification state requires HOME or XDG_STATE_HOME")?,
        )
        .join(".local/state"),
    };
    let directory = base
        .join("loom/verification")
        .join(Path::new(sdk).file_name().context("missing SDK name")?);
    ensure_directory(&directory, rustix::process::geteuid().as_raw())?;
    Ok(directory)
}

fn provision(
    derivation: &str,
    sdk: &Path,
    directory: &Path,
    execute: impl FnOnce(&Command, u32) -> Result<std::process::Output>,
) -> Result<()> {
    let output = execute(
        Command::new("nix")
            .args(["build", &format!("{derivation}^out"), "--out-link"])
            .arg(directory.join("sdk"))
            .args([
                "--option",
                "connect-timeout",
                "10",
                "--option",
                "stalled-download-timeout",
                "30",
            ]),
        1200,
    )?;
    let log = directory.join("provision.log");
    fs::write(&log, [output.stdout, output.stderr].concat())?;
    ensure!(
        output.status.success(),
        "SDK provisioning failed (exit {}); driver network/cache approval required; see {}; worker dispatch remains blocked",
        output.status,
        log.display()
    );
    ensure!(
        directory.join("sdk").canonicalize()? == sdk,
        "provisioning did not retain the selected SDK output"
    );
    Ok(())
}

pub fn run(derivation: &str, sdk: &str, mode: Mode) -> Result<()> {
    store_identity(derivation)?;
    store_identity(sdk)?;
    ensure!(
        Path::new(derivation)
            .extension()
            .is_some_and(|extension| extension == "drv"),
        "preparation requires immutable SDK store identities"
    );
    if matches!(mode, Mode::Prepare) {
        let directory = state_directory(sdk)?;
        provision(derivation, Path::new(sdk), &directory, process::capture)?;
    }
    check(sdk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn preparation_retains_only_selected_sdk_without_overriding_store_connection() {
        use std::os::unix::process::ExitStatusExt;
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let sdk = root.join("selected-sdk");
        fs::create_dir(&sdk).unwrap();
        symlink(&sdk, root.join("sdk")).unwrap();
        let execute = |command: &Command, seconds| {
            assert_eq!(seconds, 1200);
            assert_eq!(command.get_program(), "nix");
            let arguments: Vec<_> = command
                .get_args()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            assert_eq!(&arguments[..3], ["build", "selected.drv^out", "--out-link"]);
            assert_eq!(arguments[3], root.join("sdk").to_str().unwrap());
            assert!(!command.get_envs().any(|(key, _)| key == "NIX_REMOTE"));
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: vec![],
                stderr: vec![],
            })
        };
        provision("selected.drv", &sdk, &root, execute).unwrap();
        let wrong = root.join("wrong-sdk");
        fs::create_dir(&wrong).unwrap();
        assert!(provision("selected.drv", &wrong, &root, execute).is_err());
    }

    #[test]
    fn successful_exit_with_invalid_stdout_still_reports_missing() {
        let paths = vec!["missing".into(), "ready".into()];
        assert_eq!(
            invalid_paths_with(&paths, |_| Ok((true, "missing\n".into(), String::new()))).unwrap(),
            vec!["missing"]
        );
        assert!(
            invalid_paths_with(&paths, |_| Ok((false, String::new(), "unavailable".into())))
                .is_err()
        );
        assert!(
            invalid_paths_with(&paths, |_| Ok((true, "foreign\n".into(), String::new()))).is_err()
        );
    }

    #[test]
    fn checks_every_batch_and_deduplicates_missing_paths() {
        let paths: Vec<_> = (0..257).map(|i| format!("path-{i}")).collect();
        let mut sizes = Vec::new();
        let missing = invalid_paths_with(&paths, |batch| {
            sizes.push(batch.len());
            Ok((false, format!("{}\n", batch[0]), String::new()))
        })
        .unwrap();
        assert_eq!(sizes, vec![128, 128, 1]);
        assert_eq!(missing.len(), 3);
    }

    #[test]
    fn rejects_empty_or_mutable_sdk_manifests() {
        let root = tempfile::tempdir().unwrap();
        for contents in [
            "",
            "relative\n",
            "/outside-store/00000000000000000000000000000000-sdk\n",
        ] {
            fs::write(root.path().join("store-paths"), contents).unwrap();
            assert!(manifest(root.path()).is_err());
        }
        let path = "/nix/store/00000000000000000000000000000000-sdk";
        fs::write(root.path().join("store-paths"), format!("{path}\n")).unwrap();
        assert_eq!(manifest(root.path()).unwrap(), vec![path]);
    }

    #[test]
    fn state_creation_refuses_indirect_and_writable_ancestors() {
        let root = tempfile::tempdir().unwrap();
        let actual = root.path().canonicalize().unwrap();
        ensure_directory(
            &actual.join("safe/child"),
            rustix::process::geteuid().as_raw(),
        )
        .unwrap();
        symlink(actual.join("safe"), actual.join("alias")).unwrap();
        assert!(
            ensure_directory(
                &actual.join("alias/child"),
                rustix::process::geteuid().as_raw()
            )
            .is_err()
        );
        fs::set_permissions(actual.join("safe"), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            ensure_directory(
                &actual.join("safe/child"),
                rustix::process::geteuid().as_raw()
            )
            .is_err()
        );
        assert!(
            ensure_directory(Path::new("relative"), rustix::process::geteuid().as_raw()).is_err()
        );
    }
}
