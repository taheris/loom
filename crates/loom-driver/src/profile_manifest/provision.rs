//! Pinned profile provisioning from the published operator source, never a worker checkout.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tokio::process::Command;

use super::{ProfileError, ProfileImageManifest};
use crate::clock::{Clock, SystemClock};
use crate::config::LoomConfig;
use crate::git::{GitClient, RepoGitPolicy, SignatureCheck};
use crate::workspace::Association;

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum ProvisionError {
    /// profile configuration is unavailable
    Profile(#[from] ProfileError),
    /// existing workspace authority is unavailable; no provisioning authority was recreated
    Authority(#[from] crate::workspace::Error),
    /// published driver source cannot be inspected
    Git(#[from] crate::git::GitError),
    /// automatic profile provisioning requires clean, signature-verified published operator source: {reason}
    Source { reason: &'static str },
    /// profile provisioning I/O failed
    Io(#[from] std::io::Error),
    /// profile provisioning exhausted its infrastructure budget; required coverage remains pending; evidence: {log}
    Unavailable { log: PathBuf },
    /// profile factory returned an invalid store manifest: {reason}
    Output { reason: &'static str },
}

/// Load configured profiles, provisioning missing or legacy native feedback from the published driver.
///
/// A failed image realization preserves a valid legacy manifest so independent profiles can proceed.
/// Authority, signature and malformed-configuration failures remain blocking diagnostics.
///
/// # Errors
/// Rejects unavailable authority, unadmitted driver source and exhausted provisioning without a manifest.
pub async fn bootstrap_loop(
    workspace: &Path,
    config: &LoomConfig,
    policy: RepoGitPolicy,
) -> Result<ProfileImageManifest, ProvisionError> {
    let configured = match ProfileImageManifest::from_env() {
        Ok(manifest) => Some(manifest),
        Err(ProfileError::ManifestEnvUnset) => None,
        Err(ProfileError::ManifestNotFound { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            None
        }
        Err(error) => return Err(error.into()),
    };
    let git = GitClient::open(workspace)?;
    let native = git
        .tracked_files()
        .await?
        .iter()
        .any(|path| path == Path::new("Cargo.toml"));
    if let Some(manifest) = configured.as_ref()
        && (!native || !manifest.legacy_native())
    {
        return Ok(manifest.clone());
    }
    let association = Association::resolve(workspace)?;
    if workspace != association.operator() {
        return Err(ProvisionError::Source {
            reason: "profile factories are owned by the registered operator checkout",
        });
    }
    let git =
        GitClient::open_with_integration_branch(workspace, config.loom.integration_branch.clone())?
            .with_repo_git_policy(policy);
    let source = published_source(&association, &git).await?;
    let mut url = url::Url::from_directory_path(association.operator()).map_err(|()| {
        ProvisionError::Source {
            reason: "operator path cannot be represented as a local Git source",
        }
    })?;
    url.query_pairs_mut()
        .append_pair("rev", source.as_str())
        .append_pair("allRefs", "1");
    url.set_fragment(Some("profile-images"));
    let installable = format!("git+{url}");
    let clock = SystemClock::new();
    let log = association
        .operator()
        .join(".loom/logs/gate/profile-provisioning.jsonl");
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let max_attempts = config.loop_.infra.max_attempts.max(1);
    for attempt in 1..=max_attempts {
        association.recheck()?;
        if published_source(&association, &git).await? != source {
            return Err(ProvisionError::Source {
                reason: "published operator source changed during provisioning",
            });
        }
        let mut command = Command::new("nix");
        command.current_dir(association.operator()).args([
            "build",
            "--no-link",
            "--json",
            "--option",
            "require-sigs",
            "true",
            &installable,
        ]);
        let result =
            crate::process::output(&mut command, &clock, config.loom.git_hook_timeout()).await;
        let record = match &result {
            Ok(output) => serde_json::json!({
                "source":source, "installable":installable, "attempt":attempt,
                "max_attempts":max_attempts, "exit_code":output.status.code(),
                "status":output.status.to_string(),
                "stdout":String::from_utf8_lossy(&output.stdout),
                "stderr":String::from_utf8_lossy(&output.stderr), "coverage_admitted":false,
            }),
            Err(error) => serde_json::json!({
                "source":source, "installable":installable, "attempt":attempt,
                "max_attempts":max_attempts, "infrastructure_error":error.to_string(), "coverage_admitted":false,
            }),
        };
        let mut output_log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)?;
        output_log.lock()?;
        writeln!(output_log, "{record}")?;
        output_log.sync_data()?;
        drop(output_log);
        association.recheck()?;
        if published_source(&association, &git).await? != source {
            return Err(ProvisionError::Source {
                reason: "published operator source changed while its factory ran",
            });
        }
        match result {
            Ok(output) if output.status.success() => {
                let provisioned = store_manifest(&output.stdout)?;
                let manifest = match configured {
                    Some(manifest) => manifest.provision_native_from(&provisioned),
                    None => provisioned,
                };
                if native && manifest.legacy_native() {
                    return Err(ProvisionError::Output {
                        reason: "published factory does not provision native readiness",
                    });
                }
                return Ok(manifest);
            }
            Err(crate::process::Error::Cleanup(error)) => return Err(error.into()),
            Ok(_) | Err(_) => {
                tracing::warn!(attempt, max_attempts, log = %log.display(), "profile realization unavailable; no verification coverage admitted");
            }
        }
        if attempt < max_attempts {
            clock.sleep(std::time::Duration::from_secs(1)).await;
        }
    }
    configured.ok_or(ProvisionError::Unavailable { log })
}

async fn published_source(
    association: &Association,
    git: &GitClient,
) -> Result<crate::git::GitOid, ProvisionError> {
    association.recheck()?;
    let head = git.head_commit_sha().await?;
    let published = git
        .resolve_commit_sha(&format!("refs/remotes/origin/{}", git.integration_branch()))
        .await?;
    if head != published || !git.status().await?.is_empty() {
        return Err(ProvisionError::Source {
            reason: "operator must match the clean published commit",
        });
    }
    if !matches!(
        git.verify_commit_range(&format!("{head}^!")).await?,
        SignatureCheck::Verified
    ) {
        return Err(ProvisionError::Source {
            reason: "the published factory source has no authenticated commit signature",
        });
    }
    Ok(head)
}

#[derive(Deserialize)]
struct Build {
    outputs: BTreeMap<String, PathBuf>,
}

fn store_manifest(bytes: &[u8]) -> Result<ProfileImageManifest, ProvisionError> {
    let builds: Vec<Build> = serde_json::from_slice(bytes).map_err(|_| ProvisionError::Output {
        reason: "Nix did not return a complete build result",
    })?;
    let [build] = builds.as_slice() else {
        return Err(ProvisionError::Output {
            reason: "the fixed factory must produce exactly one build result",
        });
    };
    let path = build.outputs.get("out").ok_or(ProvisionError::Output {
        reason: "the fixed factory has no out result",
    })?;
    let path = std::fs::canonicalize(path)?;
    if !path.starts_with("/nix/store") || !path.is_file() {
        return Err(ProvisionError::Output {
            reason: "the factory manifest is not an immutable store file",
        });
    }
    Ok(ProfileImageManifest::from_path(&path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_output_rejects_partial_results_and_mutable_manifests() {
        for document in [b"{".as_slice(), b"[]", b"[{\"outputs\":{}}]"] {
            assert!(matches!(
                store_manifest(document),
                Err(ProvisionError::Output { .. })
            ));
        }
        let directory = tempfile::tempdir().expect("fixture");
        let manifest = directory.path().join("mutable.json");
        std::fs::write(&manifest, "{}").expect("mutable external result");
        let result = serde_json::to_vec(&serde_json::json!([{"outputs":{"out":manifest}}]))
            .expect("external Nix result");
        assert!(matches!(
            store_manifest(&result),
            Err(ProvisionError::Output { .. })
        ));
    }
}
