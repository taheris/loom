//! The pinned Nix front end owns builds; its direct verifiers cannot use native Cargo output.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) fn command(program: &OsStr, workspace: &Path) -> Command {
    match selected(program, workspace) {
        Ok(Some(pin)) => Command::new(pin),
        Ok(None) => Command::new(program),
        Err(error) => {
            tracing::debug!(
                ?error,
                "Nix identity unavailable; retaining managed native output"
            );
            Command::new(program)
        }
    }
}

fn pin() -> std::io::Result<Option<PathBuf>> {
    let Some(path) = option_env!("LOOM_NATIVE_NIX_BIN") else {
        return Ok(None);
    };
    let path = Path::new(path).canonicalize()?;
    let metadata = path.metadata()?;
    if !path.starts_with("/nix/store")
        || !metadata.is_file()
        || metadata.permissions().mode() & 0o222 != 0
    {
        return Ok(None);
    }
    Ok(Some(path))
}

fn selected(program: &OsStr, workspace: &Path) -> std::io::Result<Option<PathBuf>> {
    let Some(pin) = pin()? else {
        return Ok(None);
    };
    let program = Path::new(program);
    let paths = if program.components().count() > 1 || program.is_absolute() {
        vec![workspace.join(program)]
    } else {
        std::env::var_os("PATH")
            .map(|path| {
                std::env::split_paths(&path)
                    .map(|entry| workspace.join(entry).join(program))
                    .collect()
            })
            .unwrap_or_default()
    };
    for path in paths {
        let metadata = match path.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return Ok((path.canonicalize()? == pin).then_some(pin));
        }
    }
    Ok(None)
}

pub(super) fn owns_output(command: &Command) -> bool {
    match pin() {
        Ok(Some(pin)) if command.get_program() == pin.as_os_str() => (),
        Ok(_) => return false,
        Err(error) => {
            tracing::debug!(
                ?error,
                "Nix pin unavailable; retaining managed native output"
            );
            return false;
        }
    }
    let mut args = command.get_args();
    match args.next().and_then(OsStr::to_str) {
        Some(
            "build" | "run" | "eval" | "path-info" | "--version" | "--help" | "--print-inputs",
        ) => true,
        Some("flake") => matches!(
            args.next().and_then(OsStr::to_str),
            Some("check" | "show" | "metadata")
        ),
        Some("derivation") => args.next() == Some(OsStr::new("show")),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
