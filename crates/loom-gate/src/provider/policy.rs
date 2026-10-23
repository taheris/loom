//! Fixed library-owned project readers; project data cannot supply callbacks or compiler claims.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::tracked::{Context, Outcome};
use super::{Candidate, Definition, Digest, Error, Execution, wire};
use crate::inventory::snapshot::Snapshot;
use crate::inventory::{Duty, Inventory, Name, PROJECT_CONTEXT, Role};

impl Definition {
    /// Register the compiled document/context reader for independently discovered project inputs.
    ///
    /// # Errors
    /// Rejects stale context, missing resources or an unreadable running artifact.
    pub fn project_context(snapshot: &Snapshot, inventory: &Inventory) -> Result<Vec<Self>, Error> {
        super::require_context(snapshot, inventory)?;
        let reader = Name::try_from(PROJECT_CONTEXT.to_owned())?;
        let Some(context) = inventory.subjects().get(&reader) else {
            return Ok(Vec::new());
        };
        let subjects: BTreeSet<_> = inventory
            .subjects()
            .values()
            .filter(|subject| {
                subject.role() == Role::Document
                    || subject.name() == &reader
                    || (subject.role() == Role::Configuration
                        && subject.members().is_subset(context.members()))
            })
            .map(|subject| subject.name().clone())
            .collect();
        let members = BTreeMap::from([(reader, context.members().clone())]);
        let mut inputs = BTreeMap::new();
        for name in &subjects {
            super::add_subject_inputs(&mut inputs, inventory.subject(name)?);
        }
        let inputs = super::capture_inputs(snapshot, inputs)?;
        let target = Name::try_from(super::PROJECT_CONTEXT_TARGET.to_owned())?;
        let responsibilities = inventory
            .obligations()
            .iter()
            .filter(|obligation| {
                subjects.contains(obligation.subject())
                    && ((obligation.duty() == Duty::Document)
                        || (obligation.duty() == Duty::Check
                            && obligation.responsible().as_str() == PROJECT_CONTEXT))
            })
            .map(super::Responsibility::from_obligation)
            .collect::<BTreeSet<_>>();
        let executable = std::env::current_exe().map_err(Error::Executable)?;
        let artifact = super::read_artifact(&executable)?;
        let checker = wire::RustChecker::Library {
            library: wire::LibraryChecker::ProjectContext,
            source: Digest::of(include_bytes!("policy.rs")),
        };
        let entry = Name::try_from("loom_gate::provider::policy::check".to_owned())?;
        let definition = Digest::of(&serde_json::to_vec(&(
            snapshot.identity(),
            &target,
            &checker,
            &responsibilities,
            &members,
            &inputs,
            &artifact,
            &entry,
        ))?);
        Ok(vec![Self {
            candidate: Candidate::TrackedRust {
                contract: wire::Contract {
                    target,
                    snapshot: snapshot.identity().clone(),
                    definition,
                    origin: context.origin().clone(),
                    responsibilities,
                    inputs,
                },
                execution: wire::Rust {
                    checker,
                    artifact: artifact.clone(),
                    entry,
                },
            },
            execution: Execution::Tracked {
                check: Arc::new(check),
                members,
                executable,
                artifact,
            },
        }])
    }
}

fn check(context: &mut Context<'_>) -> Result<Outcome, Error> {
    let reader = Name::try_from(PROJECT_CONTEXT.to_owned())?;
    for path in context.members(&reader)? {
        let body = context.text(path)?;
        if body.contains('\0') {
            return Ok(Outcome::Fail {
                detail: format!("{}: project text contains NUL", path.as_path().display()),
            });
        }
        if matches!(path.as_path().to_str(), Some("loom.toml" | "wrix.toml"))
            && let Err(error) = toml::from_str::<toml::Value>(body)
        {
            return Ok(Outcome::Fail {
                detail: format!("{}: {error}", path.as_path().display()),
            });
        }
        if (path
            .as_path()
            .file_name()
            .is_some_and(|name| name == ".pre-commit-config.yaml")
            || path.as_path() == std::path::Path::new(".beads/config.yaml"))
            && let Err(error) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(body)
        {
            return Ok(Outcome::Fail {
                detail: format!("{}: {error}", path.as_path().display()),
            });
        }
        if path.as_path() == std::path::Path::new(".beads/metadata.json")
            && let Err(error) = serde_json::from_str::<serde_json::Value>(body)
        {
            return Ok(Outcome::Fail {
                detail: format!(".beads/metadata.json: {error}"),
            });
        }
        if path
            .as_path()
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            for _ in pulldown_cmark::Parser::new(body) {}
        }
    }
    Ok(Outcome::Pass)
}
