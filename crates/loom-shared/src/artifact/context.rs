use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Error, Reuse};

const STAMP: &str = ".loom-shared-context";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Stamp {
    pub(super) fingerprint: Vec<u8>,
    pub(super) reuse: Reuse,
}

impl Stamp {
    pub(super) fn read(target: &Path) -> Result<Self, Error> {
        Ok(toml::from_str(std::str::from_utf8(&read(
            &target.join(STAMP),
        )?)?)?)
    }

    pub(super) fn write(&self, target: &Path) -> Result<(), Error> {
        Ok(fs::write(target.join(STAMP), toml::to_string(self)?)?)
    }
}

pub(super) fn directory(path: &Path) -> Result<PathBuf, Error> {
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(Error::Storage(path.to_path_buf()));
    }
    Ok(path.canonicalize()?)
}

pub(super) fn read(path: &Path) -> Result<Vec<u8>, Error> {
    let mut file = fs::File::from(
        rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    if !file.metadata()?.is_file() {
        return Err(Error::Storage(path.to_path_buf()));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn sharing_allowed(workspace: &Path) -> Result<bool, Error> {
    let path = workspace.join("wrix.toml");
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            let policy: toml::Value = toml::from_str(std::str::from_utf8(&read(&path)?)?)?;
            if policy
                .get("tool_caches")
                .is_some_and(|value| !value.is_table())
            {
                return Err(Error::Storage(path));
            }
            match policy
                .get("tool_caches")
                .and_then(|table| table.get("shared"))
            {
                None => Ok(true),
                Some(toml::Value::Boolean(shared)) => Ok(*shared),
                Some(_) => Err(Error::Storage(path)),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn fingerprint(workspace: &Path, image: &Path) -> Result<Vec<u8>, Error> {
    let cargo = workspace.join(".cargo");
    if fs::symlink_metadata(&cargo).is_ok() {
        directory(&cargo)?;
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"loom-shared-v2");
    hash.update(image.as_os_str().as_encoded_bytes());
    for name in [
        "Cargo.lock",
        "Cargo.toml",
        ".cargo/config",
        ".cargo/config.toml",
        "rust-toolchain",
        "rust-toolchain.toml",
    ] {
        let path = workspace.join(name);
        hash.update(name.as_bytes());
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                let bytes = read(&path)?;
                hash.update(&bytes.len().to_le_bytes());
                hash.update(&bytes);
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && !matches!(name, "Cargo.lock" | "Cargo.toml") =>
            {
                hash.update(b"absent");
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(hash.finalize().as_bytes().to_vec())
}
