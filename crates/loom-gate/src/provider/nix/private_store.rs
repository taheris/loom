#![cfg(test)]

//! Real Nix operations in a disposable store, never the operator's GC namespace.

use std::path::{Path, PathBuf};

pub(super) struct Store {
    directory: tempfile::TempDir,
    tool: PathBuf,
}

impl Store {
    pub(super) fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let native = super::locate_tool().unwrap();
        for name in ["nix", "nix-store"] {
            loom_test_support::write_executable_bash_script(
                directory.path().join(name),
                &format!(
                    "set -euo pipefail\nexec '{}' --store \"local?root=${{0%/*}}/store\" \"$@\"\n",
                    native.with_file_name(name).display()
                ),
            )
            .unwrap();
        }
        let tool = directory.path().join("nix");
        Self { directory, tool }
    }

    pub(super) fn tool(&self) -> &Path {
        &self.tool
    }

    pub(super) fn physical(&self, path: &super::StorePath) -> PathBuf {
        self.directory
            .path()
            .join("store")
            .join(path.as_str().trim_start_matches('/'))
    }
}
