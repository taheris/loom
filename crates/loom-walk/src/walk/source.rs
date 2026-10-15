//! Explicit captured-source adapters; no ambient context or temporary filesystem.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use loom_gate::inventory::snapshot::{ResourcePath, Snapshot};
use loom_gate::inventory::{Duty, Inventory, Name, Role};
use loom_gate::provider::tracked::Context;
use loom_gate::provider::{Definition, Error, TrackedSubjects};

use super::WalkInput;

pub trait Source {
    fn read(&self, path: &Path) -> std::io::Result<String>;
    fn directory(&self, path: &Path) -> std::io::Result<Vec<PathBuf>>;
    fn is_file(&self, path: &Path) -> std::io::Result<bool>;
    fn is_dir(&self, path: &Path) -> std::io::Result<bool>;
    fn scan(&self, path: &Path, extension: Option<&str>) -> std::io::Result<Vec<PathBuf>>;
    fn normalize(&self, path: &Path) -> std::io::Result<PathBuf> {
        Ok(path.to_path_buf())
    }
    fn kind(&self, path: &Path) -> std::io::Result<loom_driver::spec::package::Kind> {
        use loom_driver::spec::package::Kind;
        if self.is_file(path)? {
            Ok(Kind::File)
        } else if self.is_dir(path)? {
            Ok(Kind::Directory)
        } else {
            Err(std::io::ErrorKind::NotFound.into())
        }
    }
}

pub struct Disk;

impl Source for Disk {
    fn normalize(&self, path: &Path) -> std::io::Result<PathBuf> {
        match std::fs::canonicalize(path) {
            Ok(path) => Ok(path),
            // Nonexistent filter paths cannot match any present file in the walk scope.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
            Err(error) => Err(error),
        }
    }
    fn kind(&self, path: &Path) -> std::io::Result<loom_driver::spec::package::Kind> {
        use loom_driver::spec::package::Kind;
        let metadata = std::fs::symlink_metadata(path)?;
        Ok(if metadata.is_file() {
            Kind::File
        } else if metadata.is_dir() {
            Kind::Directory
        } else {
            Kind::Other
        })
    }
    fn read(&self, path: &Path) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }
    fn directory(&self, path: &Path) -> std::io::Result<Vec<PathBuf>> {
        let mut paths = std::fs::read_dir(path)?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();
        Ok(paths)
    }
    fn is_file(&self, path: &Path) -> std::io::Result<bool> {
        match std::fs::metadata(path) {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
    fn is_dir(&self, path: &Path) -> std::io::Result<bool> {
        match std::fs::metadata(path) {
            Ok(metadata) => Ok(metadata.is_dir()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
    fn scan(&self, path: &Path, extension: Option<&str>) -> std::io::Result<Vec<PathBuf>> {
        if !self.is_dir(path)? {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        for entry in walkdir::WalkDir::new(path) {
            let entry = entry.map_err(std::io::Error::other)?;
            if entry.file_type().is_file()
                && extension.is_none_or(|ext| {
                    entry.path().extension().and_then(|s| s.to_str()) == Some(ext)
                })
            {
                files.push(entry.into_path());
            }
        }
        files.sort();
        Ok(files)
    }
}

pub fn workspace_root(input: &WalkInput) -> PathBuf {
    if input.source.is_some() {
        return PathBuf::new();
    }
    if let Some(root) = &input.root {
        return root.clone();
    }
    let cwd = input
        .record(Path::new("."), std::env::current_dir())
        .unwrap_or_else(|| PathBuf::from("."));
    for ancestor in cwd.ancestors() {
        // Absence is expected while searching ancestors, other I/O errors are not.
        match std::fs::read_to_string(ancestor.join("Cargo.toml")) {
            Ok(body) if body.contains("[workspace]") && body.contains("crates/loom-driver") => {
                return ancestor.to_path_buf();
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                input.record::<String>(&ancestor.join("Cargo.toml"), Err(error));
                return cwd;
            }
        }
    }
    cwd
}

fn resource(path: &Path) -> Result<ResourcePath, Error> {
    Ok(ResourcePath::try_from(path.to_path_buf())?)
}

/// Used only for constructing input descriptions, never for invoking rule bodies.
pub struct Captured<'a>(pub &'a Snapshot);

impl Source for Captured<'_> {
    fn read(&self, path: &Path) -> std::io::Result<String> {
        let bytes = self
            .0
            .files()
            .iter()
            .find(|(p, _)| p.as_path() == path)
            .map(|(_, bytes)| bytes)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
        String::from_utf8(bytes.clone()).map_err(std::io::Error::other)
    }
    fn directory(&self, path: &Path) -> std::io::Result<Vec<PathBuf>> {
        Ok(self
            .0
            .files()
            .keys()
            .filter_map(|p| {
                let relative = p.as_path().strip_prefix(path).ok()?;
                Some(path.join(relative.components().next()?))
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect())
    }
    fn is_file(&self, path: &Path) -> std::io::Result<bool> {
        Ok(self.0.files().keys().any(|p| p.as_path() == path))
    }
    fn is_dir(&self, path: &Path) -> std::io::Result<bool> {
        Ok(self
            .0
            .files()
            .keys()
            .any(|p| p.as_path() != path && p.as_path().starts_with(path)))
    }
    fn scan(&self, path: &Path, extension: Option<&str>) -> std::io::Result<Vec<PathBuf>> {
        Ok(self
            .0
            .files()
            .keys()
            .filter(|p| {
                p.as_path().starts_with(path)
                    && extension.is_none_or(|ext| {
                        p.as_path().extension().and_then(|s| s.to_str()) == Some(ext)
                    })
            })
            .map(|p| p.as_path().to_path_buf())
            .collect())
    }
}

pub struct Tracked<'a, 's> {
    context: RefCell<&'a mut Context<'s>>,
    error: RefCell<Option<Error>>,
}

impl<'a, 's> Tracked<'a, 's> {
    pub const fn new(context: &'a mut Context<'s>) -> Self {
        Self {
            context: RefCell::new(context),
            error: RefCell::new(None),
        }
    }
    fn record<T>(&self, result: Result<T, Error>) -> std::io::Result<T> {
        result.map_err(|error| {
            let message = error.to_string();
            *self.error.borrow_mut() = Some(error);
            std::io::Error::other(message)
        })
    }
    pub fn finish(&self) -> Result<(), Error> {
        self.error.borrow_mut().take().map_or(Ok(()), Err)
    }
}

impl Source for Tracked<'_, '_> {
    fn read(&self, path: &Path) -> std::io::Result<String> {
        self.record(
            resource(path)
                .and_then(|path| self.context.borrow_mut().text(&path).map(str::to_owned)),
        )
    }
    fn directory(&self, path: &Path) -> std::io::Result<Vec<PathBuf>> {
        let path = if path.as_os_str().is_empty() {
            None
        } else {
            Some(self.record(resource(path))?)
        };
        if let Some(path) = &path
            && !self.context.borrow_mut().directory_exists(path)
        {
            return Err(std::io::ErrorKind::NotFound.into());
        }
        Ok(self
            .context
            .borrow_mut()
            .directory(path.as_ref())
            .into_iter()
            .map(|p| p.as_path().to_path_buf())
            .collect())
    }
    fn is_file(&self, path: &Path) -> std::io::Result<bool> {
        self.record(resource(path).and_then(|path| self.context.borrow_mut().exists(&path)))
    }
    fn is_dir(&self, path: &Path) -> std::io::Result<bool> {
        let path = self.record(resource(path))?;
        Ok(self.context.borrow_mut().directory_exists(&path))
    }
    fn scan(&self, path: &Path, extension: Option<&str>) -> std::io::Result<Vec<PathBuf>> {
        let prefix = if path.as_os_str().is_empty() {
            None
        } else {
            Some(self.record(resource(path))?)
        };
        self.record(self.context.borrow_mut().scan(prefix.as_ref(), extension))
            .map(|paths| {
                paths
                    .into_iter()
                    .map(|p| p.as_path().to_path_buf())
                    .collect()
            })
    }
}

impl loom_driver::spec::package::Reader for WalkInput<'_> {
    fn read(&self, path: &Path) -> std::io::Result<String> {
        self.reader().read(path)
    }
    fn directory(&self, path: &Path) -> std::io::Result<Vec<PathBuf>> {
        self.reader().directory(path)
    }
    fn kind(&self, path: &Path) -> std::io::Result<loom_driver::spec::package::Kind> {
        self.reader().kind(path)
    }
}

pub fn register<F>(
    snapshot: &Snapshot,
    inventory: &Inventory,
    target: &str,
    checker: &str,
    check: F,
) -> Result<Definition, Error>
where
    F: for<'a> Fn(&mut Context<'a>) -> Result<loom_gate::provider::tracked::Outcome, Error>
        + Send
        + Sync
        + 'static,
{
    let source = Captured(snapshot);
    let input = WalkInput::captured(&source);
    let target_name = Name::try_from(target.to_owned())?;
    let walk = super::lookup(target).ok_or_else(|| Error::Missing {
        target: target_name.clone(),
    })?;
    let paths: BTreeSet<_> = (walk.inputs)(Path::new(""), &input).into_iter().collect();
    if !input.errors.borrow().is_empty() {
        return Err(Error::Inconsistent {
            target: target_name,
            producer: resource(Path::new(checker))?,
        });
    }
    let analysis: BTreeSet<_> = inventory
        .subjects()
        .values()
        .filter(|subject| {
            paths.contains(subject.origin().as_path())
                || subject
                    .members()
                    .iter()
                    .any(|p| paths.contains(p.as_path()))
        })
        .map(|subject| subject.name().clone())
        .collect();
    let responsibility: BTreeSet<_> = inventory
        .subjects()
        .values()
        .filter(|subject| analysis.contains(subject.name()) && subject.role() == Role::RustSource)
        .map(|subject| subject.name().clone())
        .collect();
    let duties = if responsibility.is_empty() {
        BTreeSet::new()
    } else {
        BTreeSet::from([Duty::Lint])
    };
    Definition::tracked_analysis(
        snapshot,
        inventory,
        target_name,
        resource(Path::new(checker))?,
        TrackedSubjects::new(&analysis, &responsibility),
        &duties,
        check,
    )
}

macro_rules! registration {
    ($target:literal) => {
        pub(super) fn register(
            snapshot: &loom_gate::inventory::snapshot::Snapshot,
            inventory: &loom_gate::inventory::Inventory,
        ) -> Result<loom_gate::provider::Definition, loom_gate::provider::Error> {
            $crate::walk::source::register(snapshot, inventory, $target, file!(), checked)
        }
        pub(super) fn checked(
            context: &mut loom_gate::provider::tracked::Context<'_>,
        ) -> Result<loom_gate::provider::tracked::Outcome, loom_gate::provider::Error> {
            let source = $crate::walk::source::Tracked::new(context);
            let input = $crate::walk::WalkInput::captured(&source);
            let verdict = run(&input);
            source.finish()?;
            Ok($crate::walk::manifest::outcome(verdict))
        }
    };
}
pub(super) use registration;

#[cfg(test)]
mod tests {
    use super::*;
    use loom_gate::provider::{Registry, tracked::Outcome};
    use tempfile::TempDir;

    fn path(value: &str) -> ResourcePath {
        PathBuf::from(value).try_into().unwrap()
    }
    fn name(value: &str) -> Name {
        value.to_owned().try_into().unwrap()
    }
    fn write(root: &Path, relative: &str, body: &str) {
        let file = root.join(relative);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body).unwrap();
    }
    fn fixture() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        loom_driver::git::init_test_repo(root).unwrap();
        std::fs::remove_file(root.join(".pre-commit-config.yaml")).unwrap();
        write(
            root,
            "Cargo.toml",
            "[package]\nname='checker'\nversion='0.1.0'\nedition='2024'\n[lib]\npath='crates/loom-walk/src/lib.rs'\n",
        );
        write(
            root,
            "crates/loom-walk/src/lib.rs",
            "pub mod walk; pub mod types;\n",
        );
        write(
            root,
            "crates/loom-walk/src/types.rs",
            "pub struct Forbidden;\n",
        );
        write(
            root,
            "crates/loom-walk/src/walk/mod.rs",
            "pub mod no_event_sentinels; pub mod no_types_or_error_files; pub mod workspace_deps_pinned; pub mod pre_push_config_marker_wrapper_contract;\n",
        );
        for rule in [
            "no_event_sentinels",
            "no_types_or_error_files",
            "workspace_deps_pinned",
            "pre_push_config_marker_wrapper_contract",
        ] {
            let body = std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("src/walk/{rule}.rs")),
            )
            .unwrap();
            write(
                root,
                &format!("crates/loom-walk/src/walk/{rule}.rs"),
                body.split("\n#[cfg(test)]\n").next().unwrap(),
            );
        }
        write(root, "docs/README.md", "- [fixture](../specs/fixture.md)\n");
        write(
            root,
            "specs/fixture.md",
            "# Fixture\n\n## Success Criteria\n- Scan [check](no_event_sentinels)\n- Probe [check](no_types_or_error_files)\n- Config [check](workspace_deps_pinned)\n- Hook [check](pre_push_config_marker_wrapper_contract)\n",
        );
        assert!(
            std::process::Command::new("cargo")
                .args(["generate-lockfile", "--offline"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        dir
    }

    fn omitted<F>(target: &str, check: F) -> Error
    where
        F: for<'a> Fn(&mut Context<'a>) -> Result<Outcome, Error> + Send + Sync + 'static,
    {
        let dir = fixture();
        let snapshot = Snapshot::discover(dir.path()).unwrap();
        let inventory = Inventory::discover(&snapshot).unwrap();
        // Code identity includes the native crate, but is deliberately NOT analysis access.
        let unrelated = inventory
            .subjects()
            .values()
            .find(|subject| subject.origin() == &path("docs/README.md"))
            .unwrap()
            .name()
            .clone();
        let analysis = BTreeSet::from([unrelated]);
        let definition = Definition::tracked_analysis(
            &snapshot,
            &inventory,
            name(target),
            path(&format!("crates/loom-walk/src/walk/{target}.rs")),
            TrackedSubjects::new(&analysis, &BTreeSet::new()),
            &BTreeSet::new(),
            check,
        )
        .unwrap();
        let candidate = definition.description().clone();
        let registry = Registry::new(&snapshot, vec![definition]).unwrap();
        let unit = registry
            .resolve(&snapshot, &inventory, &candidate, path("specs/fixture.md"))
            .unwrap();
        assert!(
            unit.inputs()
                .iter()
                .any(|input| input.path() == &path("crates/loom-walk/src/types.rs"))
        );
        unit.run().unwrap_err()
    }

    #[test]
    fn formerly_absent_configuration_requires_fresh_captured_analysis_authority() {
        let dir = fixture();
        let root = dir.path();
        let target = "pre_push_config_marker_wrapper_contract";
        let snapshot = Snapshot::discover(root).unwrap();
        let inventory = Inventory::discover(&snapshot).unwrap();
        let document = inventory
            .subjects()
            .values()
            .find(|subject| subject.origin() == &path("docs/README.md"))
            .unwrap()
            .name()
            .clone();
        let definition = Definition::tracked_analysis(
            &snapshot,
            &inventory,
            name(target),
            path("crates/loom-walk/src/walk/pre_push_config_marker_wrapper_contract.rs"),
            TrackedSubjects::new(&BTreeSet::from([document]), &BTreeSet::new()),
            &BTreeSet::new(),
            super::super::pre_push_config_marker_wrapper_contract::checked,
        )
        .unwrap();
        let candidate = definition.description().clone();
        let registry = Registry::new(&snapshot, vec![definition]).unwrap();
        let unit = registry
            .resolve(&snapshot, &inventory, &candidate, path("specs/fixture.md"))
            .unwrap();
        assert!(matches!(unit.run(), Err(Error::Snapshot(_))));
        let project = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap();
        write(
            root,
            ".pre-commit-config.yaml",
            &std::fs::read_to_string(project.join(".pre-commit-config.yaml")).unwrap(),
        );
        let mut manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        manifest.push_str("\n[[package.metadata.loom.verify.resource]]\nname='precommit-fixture'\nrole='test-fixture'\ndirectory='.pre-commit-config.yaml'\nconsumers=['cargo:checker:lib:checker']\n");
        write(root, "Cargo.toml", &manifest);
        assert!(registry.query(&[name(target)]).is_err());
        let snapshot = Snapshot::discover(root).unwrap();
        let inventory = Inventory::discover(&snapshot).unwrap();
        let definitions =
            super::super::checked_definitions(&snapshot, &inventory, &[name(target)]).unwrap();
        let registry = Registry::new(&snapshot, definitions).unwrap();
        let query = registry.query(&[name(target)]).unwrap();
        let units = registry
            .resolve_query(
                &snapshot,
                &inventory,
                &query,
                &std::collections::BTreeMap::from([(name(target), path("specs/fixture.md"))]),
            )
            .unwrap();
        assert_eq!(units[0].run().unwrap().outcome(), &Outcome::Pass);
        assert!((super::super::lookup(target).unwrap().run)(&WalkInput::for_root(root)).pass);
    }

    #[test]
    fn captured_root_discovery_never_reads_the_host_workspace() {
        let dir = fixture();
        let snapshot = Snapshot::discover(dir.path()).unwrap();
        let captured = Captured(&snapshot);
        let input = WalkInput::captured(&captured);
        assert_eq!(workspace_root(&input), PathBuf::new());
        assert!(input.errors.borrow().is_empty());
    }

    #[test]
    fn swallowed_namespace_and_content_errors_are_not_standalone_passes() {
        struct Unreadable;
        impl Source for Unreadable {
            fn read(&self, _path: &Path) -> std::io::Result<String> {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }
            fn directory(&self, _path: &Path) -> std::io::Result<Vec<PathBuf>> {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }
            fn is_file(&self, _path: &Path) -> std::io::Result<bool> {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }
            fn is_dir(&self, _path: &Path) -> std::io::Result<bool> {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }
            fn scan(
                &self,
                _path: &Path,
                _extension: Option<&str>,
            ) -> std::io::Result<Vec<PathBuf>> {
                Err(std::io::ErrorKind::PermissionDenied.into())
            }
        }
        for target in [
            "no_event_sentinels",
            "no_types_or_error_files",
            "workspace_deps_pinned",
        ] {
            let input = WalkInput::captured(&Unreadable);
            let verdict = (super::super::lookup(target).unwrap().run)(&input);
            assert!(!verdict.pass, "{target}");
            assert!(
                verdict.evidence.contains("source errors:"),
                "{target}: {}",
                verdict.evidence
            );
        }
    }

    #[test]
    fn actual_scan_rejects_omitted_members_even_if_the_rule_swallows_the_scan_error() {
        assert!(matches!(
            omitted(
                "no_event_sentinels",
                super::super::no_event_sentinels::checked
            ),
            Error::Outside { .. }
        ));
    }

    #[test]
    fn actual_absence_rule_cannot_turn_an_undeclared_existing_file_into_a_pass() {
        assert!(
            matches!(omitted("no_types_or_error_files", super::super::no_types_or_error_files::checked), Error::Outside { path: file } if file == path("crates/loom-walk/src/types.rs"))
        );
    }

    #[test]
    fn actual_config_rule_cannot_read_configuration_via_checker_build_identity() {
        assert!(
            matches!(omitted("workspace_deps_pinned", super::super::workspace_deps_pinned::checked), Error::Outside { path: file } if file == path("Cargo.toml"))
        );
    }
}
