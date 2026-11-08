//! Pinned selected-profile provisioning from authenticated operator context.

use super::{
    ImageEntry, ProfileError, ProfileImageManifest,
    catalogue::{Catalogue, Entry, store_file},
};
use crate::{
    agent::AgentRuntime,
    clock::SystemClock,
    config::LoomConfig,
    git::{GitClient, RepoGitPolicy, SignatureCheck},
    identifier::ProfileName,
    workspace::Association,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    io::Write as _,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};
use tokio::process::Command;

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
    /// private profile state is unavailable
    State(#[from] crate::lock::LockError),
    /// profile provisioning exhausted its infrastructure budget; required coverage remains pending; evidence: {log}
    Unavailable { log: PathBuf },
    /// profile factory returned an invalid store manifest: {reason}
    Output { reason: &'static str },
}

#[derive(Debug, Clone)]
pub(super) struct Provisioning {
    workspace: PathBuf,
    config: LoomConfig,
    policy: RepoGitPolicy,
    source: Arc<OnceLock<crate::git::GitOid>>,
}

impl Provisioning {
    pub(crate) fn new(workspace: &Path, config: &LoomConfig, policy: RepoGitPolicy) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
            config: config.clone(),
            policy,
            source: Arc::new(OnceLock::new()),
        }
    }

    async fn authority(
        &self,
    ) -> Result<(Association, GitClient, crate::git::GitOid), ProvisionError> {
        let association = Association::resolve(&self.workspace)?;
        let git = GitClient::open_with_integration_branch(
            association.operator(),
            self.config.loom.integration_branch.clone(),
        )?
        .with_repo_git_policy(self.policy.clone());
        let source = published_source(&association, &git).await?;
        if self.source.get_or_init(|| source.clone()) != &source {
            return Err(ProvisionError::Source {
                reason: "published source changed since profile discovery",
            });
        }
        Ok((association, git, source))
    }

    async fn execute(
        &self,
        association: &Association,
        git: &GitClient,
        source: &crate::git::GitOid,
        command: &mut Command,
        operation: &'static str,
    ) -> Result<std::process::Output, ProvisionError> {
        let clock = SystemClock::new();
        let log = association
            .operator()
            .join(".loom/diagnostics/profile-provisioning.jsonl");
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let max_attempts = self.config.loop_.infra.max_attempts.max(1);
        for attempt in 1..=max_attempts {
            association.recheck()?;
            if &published_source(association, git).await? != source {
                return Err(ProvisionError::Source {
                    reason: "published operator source changed before provisioning",
                });
            }
            let result =
                crate::process::output(command, &clock, self.config.loom.git_hook_timeout()).await;
            let record = match &result {
                Ok(output) => {
                    serde_json::json!({"source":source,"operation":operation,"attempt":attempt,"max_attempts":max_attempts,"exit_code":output.status.code(),"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"coverage_admitted":false})
                }
                Err(error) => {
                    serde_json::json!({"source":source,"operation":operation,"attempt":attempt,"max_attempts":max_attempts,"infrastructure_error":error.to_string(),"coverage_admitted":false})
                }
            };
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)?;
            file.lock()?;
            writeln!(file, "{record}")?;
            file.sync_data()?;
            drop(file);
            association.recheck()?;
            if &published_source(association, git).await? != source {
                return Err(ProvisionError::Source {
                    reason: "published operator source changed while provisioning",
                });
            }
            match result {
                Ok(output) if output.status.success() => return Ok(output),
                Err(crate::process::Error::Cleanup(error)) => return Err(error.into()),
                Ok(_) | Err(_) => {
                    tracing::warn!(attempt, max_attempts, %operation, log = %log.display(), "selected profile unavailable; no verification coverage admitted");
                }
            }
        }
        Err(ProvisionError::Unavailable { log })
    }

    async fn catalogue(&self) -> Result<Catalogue, ProvisionError> {
        let (association, git, source) = self.authority().await?;
        let mut url = url::Url::from_directory_path(association.operator()).map_err(|()| {
            ProvisionError::Source {
                reason: "operator path cannot be represented as a local Git source",
            }
        })?;
        url.query_pairs_mut()
            .append_pair("rev", source.as_str())
            .append_pair("allRefs", "1");
        url.set_fragment(Some("profile-catalogue"));
        let identity = format!("{}-catalogue", source.as_str());
        let (_directory, root) = root_for(&association, Path::new(&identity))?;
        let mut command = Command::new("nix");
        command
            .current_dir(association.operator())
            .args(["build", "--out-link"])
            .arg(&root)
            .args([
                "--json",
                "--option",
                "require-sigs",
                "true",
                &format!("git+{url}"),
            ]);
        let output = self
            .execute(&association, &git, &source, &mut command, "catalogue")
            .await?;
        store_catalogue(&output.stdout)
    }

    pub(crate) async fn resolve(
        &self,
        configured: Option<&Catalogue>,
        profile: &ProfileName,
        runtime: AgentRuntime,
    ) -> Result<ImageEntry, ProvisionError> {
        if self.source.get().is_none() {
            return Err(ProvisionError::Source {
                reason: "catalogue discovery has not authenticated the published source",
            });
        }
        let catalogue = configured.ok_or(ProvisionError::Source {
            reason: "selected provisioning requires authenticated catalogue discovery",
        })?;
        let entry =
            catalogue
                .profiles
                .get(profile)
                .ok_or_else(|| ProfileError::UnknownProfile {
                    name: profile.clone(),
                    manifest_path: catalogue.path.clone(),
                })?;
        let selected =
            entry
                .get(&runtime)
                .ok_or_else(|| ProfileError::UnknownRuntimeForProfile {
                    profile: profile.clone(),
                    runtime,
                    declared_runtimes: entry.keys().copied().collect(),
                    manifest_path: catalogue.path.clone(),
                })?;
        let (association, git, source) = self.authority().await?;
        if !store_file(&catalogue.path) || catalogue.path.canonicalize()? != catalogue.path {
            return Err(ProvisionError::Output {
                reason: "automatic realization requires an immutable catalogue",
            });
        }
        let (_catalogue_directory, catalogue_root) = root_for(&association, &catalogue.path)?;
        let mut pin = Command::new("nix-store");
        pin.current_dir(association.operator())
            .args(["--realise"])
            .arg(&catalogue.path)
            .args(["--add-root"])
            .arg(&catalogue_root)
            .args(["--indirect", "--option", "require-sigs", "true"]);
        self.execute(&association, &git, &source, &mut pin, "catalogue-root")
            .await?;
        let mut query = Command::new("nix-store");
        query
            .current_dir(association.operator())
            .args(["--query", "--outputs"])
            .arg(&selected.realization.derivation);
        let outputs = self
            .execute(&association, &git, &source, &mut query, "recipe-binding")
            .await?;
        check_output_binding(selected, &outputs.stdout)?;
        let (_root_directory, root) = root_for(&association, &selected.realization.path)?;
        let mut command = Command::new("nix-store");
        command
            .current_dir(association.operator())
            .args(["--realise"])
            .arg(&selected.realization.derivation)
            .args(["--add-root"])
            .arg(&root)
            .args(["--indirect", "--option", "require-sigs", "true"]);
        self.execute(&association, &git, &source, &mut command, "selected-entry")
            .await?;
        if std::fs::read_link(&root)? != selected.realization.path {
            return Err(ProvisionError::Output {
                reason: "selected GC root does not retain the bound manifest",
            });
        }
        let path = selected.realization.path.canonicalize()?;
        if path != selected.realization.path || !path.is_file() {
            return Err(ProvisionError::Output {
                reason: "selected manifest is not the exact immutable file",
            });
        }
        let manifest = ProfileImageManifest::from_path(&path)?;
        if manifest.len() != 1
            || manifest
                .declared_runtimes(profile)
                .is_none_or(|runtimes| runtimes.len() != 1)
        {
            return Err(ProvisionError::Output {
                reason: "selected recipe returned more than its requested entry",
            });
        }
        let image = manifest.lookup(profile, runtime)?;
        selected.check(image)?;
        Ok(image.clone())
    }
}

/// Load discovery metadata without provisioning an image matrix.
///
/// # Errors
/// Rejects malformed inputs, unavailable authority and unauthenticated factory source.
pub async fn bootstrap_loop(
    workspace: &Path,
    config: &LoomConfig,
    policy: RepoGitPolicy,
) -> Result<ProfileImageManifest, ProvisionError> {
    let provisioning = Provisioning::new(workspace, config, policy);
    let configured =
        ProfileImageManifest::from_catalogue(Arc::new(provisioning.catalogue().await?));
    Ok(configured.with_provisioner(provisioning))
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
        git.verify_operator_commit(&head).await?,
        SignatureCheck::Verified
    ) {
        return Err(ProvisionError::Source {
            reason: "the published factory source has no authenticated commit signature",
        });
    }
    Ok(head)
}

fn check_output_binding(selected: &Entry, output: &[u8]) -> Result<(), ProvisionError> {
    let text = std::str::from_utf8(output).map_err(|_| ProvisionError::Output {
        reason: "invalid recipe output encoding",
    })?;
    let paths = text.lines().collect::<Vec<_>>();
    if paths.len() != 1 || Path::new(paths[0]) != selected.realization.path {
        return Err(ProvisionError::Output {
            reason: "recipe output is not exactly the bound selected manifest",
        });
    }
    Ok(())
}

fn root_for(
    association: &Association,
    selected: &Path,
) -> Result<(crate::scratch::directory::Directory, PathBuf), ProvisionError> {
    use sha2::{Digest as _, Sha256};
    let state = crate::lock::resolve_xdg_state_home()?;
    let identity = format!(
        "{:x}",
        Sha256::digest(association.operator().as_os_str().as_encoded_bytes())
    );
    let parent = state.join("loom/profile-provisions").join(identity);
    let directory = crate::scratch::directory::Directory::ensure(&parent)?;
    directory.require_private()?;
    let canonical = parent.canonicalize()?;
    if canonical.starts_with(association.operator()) {
        return Err(ProvisionError::Source {
            reason: "profile roots require private host-owned state outside the workspace",
        });
    }
    let name = selected.file_name().ok_or(ProvisionError::Output {
        reason: "selected manifest has no store identity",
    })?;
    let root = canonical.join(name);
    match root.symlink_metadata() {
        Ok(_) => {
            let target = std::fs::read_link(&root)?;
            if (store_file(selected) && target != selected) || !store_file(&target) {
                return Err(ProvisionError::Source {
                    reason: "existing selected root has conflicting ownership",
                });
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok((directory, root))
}

#[derive(Deserialize)]
struct Build {
    outputs: BTreeMap<String, PathBuf>,
}

fn store_catalogue(bytes: &[u8]) -> Result<Catalogue, ProvisionError> {
    let builds: Vec<Build> = serde_json::from_slice(bytes).map_err(|_| ProvisionError::Output {
        reason: "Nix did not return a complete build result",
    })?;
    let [build] = builds.as_slice() else {
        return Err(ProvisionError::Output {
            reason: "the fixed catalogue factory must produce exactly one result",
        });
    };
    let path = build.outputs.get("out").ok_or(ProvisionError::Output {
        reason: "the factory has no out result",
    })?;
    if !store_file(path) {
        return Err(ProvisionError::Output {
            reason: "the catalogue is not an immutable store file",
        });
    }
    let canonical = path.canonicalize()?;
    if !store_file(&canonical) || !canonical.is_file() {
        return Err(ProvisionError::Output {
            reason: "the catalogue target is not an immutable store file",
        });
    }
    Ok(Catalogue::from_path(&canonical)?)
}

#[cfg(test)]
mod tests;
