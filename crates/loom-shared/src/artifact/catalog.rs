use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

use super::{Error, context};

#[derive(Deserialize)]
struct Lockfile {
    package: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    source: Option<String>,
}

pub(super) struct Packages {
    external: BTreeSet<String>,
}

impl Packages {
    pub(super) fn read(path: &Path) -> Result<Self, Error> {
        let lock: Lockfile = toml::from_str(std::str::from_utf8(&context::read(path)?)?)?;
        let local = lock
            .package
            .iter()
            .filter(|package| package.source.is_none())
            .map(|package| package.name.replace('-', "_"))
            .collect::<BTreeSet<_>>();
        let external = lock
            .package
            .iter()
            .filter(|package| {
                package.source.as_ref().is_some_and(|source| {
                    source.starts_with("registry+") || source.starts_with("git+")
                }) && !local.contains(&package.name.replace('-', "_"))
            })
            .map(|package| package.name.clone())
            .collect();
        Ok(Self { external })
    }

    pub(super) fn unit<'a>(&self, name: &'a str) -> Option<&'a str> {
        let (package, hash) = name.rsplit_once('-')?;
        if hash.len() != 16 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        self.external.contains(package).then_some(hash)
    }
}
