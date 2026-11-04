use std::{collections::BTreeSet, path::Path};

use super::{Error, StorePath, diagnostic, invoke, query};

/// GC roots keep captured immutable artifacts from becoming executable dependencies.
pub(super) struct Inputs {
    directory: tempfile::TempDir,
}

impl Inputs {
    pub(super) fn acquire(tool: &Path, paths: &BTreeSet<StorePath>) -> Result<Self, Error> {
        let lease = Self {
            directory: tempfile::tempdir()?,
        };
        if !paths.is_empty() {
            let inputs =
                serde_json::to_string(&serde_json::to_string(paths)?)?.replace("${", "\\${");
            let expression = format!(
                "builtins.toFile \"loom-native-inputs\" (builtins.concatStringsSep \"\\n\" (map builtins.storePath (builtins.fromJSON {inputs})))"
            );
            let metadata: StorePath = serde_json::from_slice(&query(
                tool,
                &[
                    "eval",
                    "--json",
                    "--offline",
                    "--impure",
                    "--option",
                    "allow-import-from-derivation",
                    "false",
                    "--expr",
                    &expression,
                ],
            )?)?;
            let root = lease.root();
            let root = root.to_str().ok_or(Error::StorePath)?;
            let store = tool.with_file_name("nix-store");
            let output = invoke(
                &store,
                &[
                    "--option",
                    "substitute",
                    "false",
                    "--add-root",
                    root,
                    "--indirect",
                    "--realise",
                    metadata.as_str(),
                ],
            )?;
            if !output.status.success() {
                return Err(Error::Query {
                    detail: diagnostic(&output.stderr),
                });
            }
            if std::fs::read_link(root)? != Path::new(metadata.as_str()) {
                return Err(Error::Result);
            }
        }
        Ok(lease)
    }

    fn root(&self) -> std::path::PathBuf {
        self.directory.path().join("inputs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_lease_roots_the_actual_closure_and_releases_its_link() {
        let tool = super::super::locate_tool().unwrap();
        let artifact = StorePath::root(&tool).unwrap();
        let lease = Inputs::acquire(&tool, &[artifact.clone()].into()).unwrap();
        let root = lease.root();
        assert!(root.is_symlink());
        let references = query(
            &tool.with_file_name("nix-store"),
            &["--query", "--references", root.to_str().unwrap()],
        )
        .unwrap();
        assert!(
            String::from_utf8(references)
                .unwrap()
                .lines()
                .any(|line| line == artifact.as_str())
        );
        drop(lease);
        assert!(!root.exists());
    }

    #[test]
    fn active_artifact_leases_prevent_collection_of_the_captured_input() {
        let tool = super::super::locate_tool().unwrap();
        let nonce = tempfile::tempdir().unwrap();
        let expression = format!(
            "builtins.toFile \"native-lease-probe\" {}",
            serde_json::to_string(nonce.path().to_str().unwrap()).unwrap()
        );
        let artifact: StorePath = serde_json::from_slice(
            &query(
                &tool,
                &["eval", "--json", "--offline", "--expr", &expression],
            )
            .unwrap(),
        )
        .unwrap();
        let lease = Inputs::acquire(&tool, &[artifact.clone()].into()).unwrap();
        let store = tool.with_file_name("nix-store");
        let deletion = invoke(&store, &["--delete", artifact.as_str()]).unwrap();
        assert!(!deletion.status.success());
        assert!(Path::new(artifact.as_str()).exists());
        let root = lease.root();
        let roots = query(&store, &["--query", "--roots", artifact.as_str()]).unwrap();
        assert!(
            String::from_utf8(roots)
                .unwrap()
                .contains(root.to_str().unwrap())
        );
        drop(lease);
        assert!(!root.exists());
    }

    #[test]
    fn missing_artifacts_cannot_be_substituted_or_realized_by_pinning() {
        let tool = super::super::locate_tool().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let absent = StorePath::try_from(format!(
            "/nix/store/{}-missing-native-input-{}",
            "a".repeat(32),
            directory.path().file_name().unwrap().to_str().unwrap()
        ))
        .unwrap();
        assert!(!Path::new(absent.as_str()).exists());
        assert!(Inputs::acquire(&tool, &[absent.clone()].into()).is_err());
        assert!(!Path::new(absent.as_str()).exists());
    }

    #[test]
    fn derivation_documents_are_rooted_without_realizing_their_outputs() {
        let tool = super::super::locate_tool().unwrap();
        let document: StorePath = serde_json::from_slice(&query(&tool, &["eval", "--json", "--offline", "--expr", "(builtins.derivation { name = \"readonly-native-data\"; system = \"builtin\"; builder = \"/no-such-immutable-builder\"; args = []; }).drvPath"]).unwrap()).unwrap();
        let lease = Inputs::acquire(&tool, &[document.clone()].into()).unwrap();
        assert!(lease.root().exists());
        assert!(Path::new(document.as_str()).is_file());
        let outputs = query(
            &tool.with_file_name("nix-store"),
            &["--query", "--outputs", document.as_str()],
        )
        .unwrap();
        for output in String::from_utf8(outputs).unwrap().lines() {
            assert!(!Path::new(output).exists());
        }
    }
}
