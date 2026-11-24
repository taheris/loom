#![cfg(test)]

use super::*;
use std::fmt::Write as _;
use std::fs;

struct Workspace {
    _parent: tempfile::TempDir,
    root: PathBuf,
}

impl Workspace {
    fn new(member: &str) -> Self {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("checkout");
        fs::create_dir(&root).unwrap();
        loom_driver::git::init_test_repo(&root).unwrap();
        let workspace = Self {
            _parent: parent,
            root,
        };
        workspace.write(
            "Cargo.toml",
            &format!("[workspace]\nresolver='3'\nmembers=['{member}']\n"),
        );
        workspace.write(
            "Cargo.lock",
            "version = 4\n[[package]]\nname='wrix-service-server'\nversion='0.1.0'\n",
        );
        workspace
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn manifest(&self, member: &str, targets: &str) {
        self.write(
            &format!("{member}/Cargo.toml"),
            &format!(
                "[package]\nname='wrix-service-server'\nversion='0.1.0'\nedition='2024'\n{targets}"
            ),
        );
    }

    fn target(&self, member: &str, source: &str) {
        self.manifest(member, &format!("[[bin]]\nname='probe'\npath='{source}'\n"));
    }

    fn discover(&self) -> Result<(Snapshot, BTreeMap<Name, Subject>), Error> {
        let snapshot = Snapshot::discover(&self.root)?;
        let subjects = discover_captured(&snapshot)?;
        Ok((snapshot, subjects))
    }
}

fn discover_captured(snapshot: &Snapshot) -> Result<BTreeMap<Name, Subject>, Error> {
    let staged = snapshot.stage()?;
    let mut subjects = BTreeMap::new();
    discover(snapshot, staged.path(), &mut subjects)?;
    Ok(subjects)
}

fn resource(value: &str) -> ResourcePath {
    ResourcePath::try_from(PathBuf::from(value)).unwrap()
}

#[test]
fn changed_test_owners_do_not_follow_reverse_dependencies() {
    let workspace = Workspace::new("producer");
    workspace.write(
        "Cargo.toml",
        "[workspace]\nresolver='3'\nmembers=['producer','consumer']\n",
    );
    workspace.write("Cargo.lock", "version = 4\n[[package]]\nname='consumer'\nversion='0.1.0'\ndependencies=['producer']\n[[package]]\nname='producer'\nversion='0.1.0'\n");
    workspace.write(
        "producer/Cargo.toml",
        "[package]\nname='producer'\nversion='0.1.0'\nedition='2024'\n",
    );
    workspace.write("consumer/Cargo.toml", "[package]\nname='consumer'\nversion='0.1.0'\nedition='2024'\n[dependencies]\nproducer={path='../producer'}\n");
    workspace.write("producer/src/lib.rs", "pub fn value() -> i32 { 1 }\n");
    workspace.write(
        "consumer/src/lib.rs",
        "pub fn use_value() -> i32 { producer::value() }\n",
    );
    workspace.write("consumer/tests/fixtures/sample.txt", "input\n");
    workspace.write(
        "flake.nix",
        "throw \"owner discovery must not evaluate Nix\"\n",
    );
    let snapshot = Snapshot::discover(&workspace.root).unwrap();
    for (path, expected) in [
        ("producer/src/lib.rs", vec![resource("producer/Cargo.toml")]),
        ("producer/Cargo.toml", vec![resource("producer/Cargo.toml")]),
        (
            "consumer/tests/fixtures/sample.txt",
            vec![resource("consumer/Cargo.toml")],
        ),
        (
            "Cargo.lock",
            vec![
                resource("consumer/Cargo.toml"),
                resource("producer/Cargo.toml"),
            ],
        ),
        ("README.md", Vec::new()),
    ] {
        assert_eq!(
            super::super::Inventory::discover_changed_test_owners(
                &snapshot,
                &BTreeSet::from([resource(path)]),
            )
            .unwrap(),
            expected.into_iter().collect(),
            "{path}"
        );
    }
}

#[test]
fn wrix_sibling_targets_keep_their_original_cargo_names_and_kinds() {
    // The explicit parent-relative targets of Wrix's wrix-service-server package.
    let workspace = Workspace::new("crates/wrix-service-server");
    let mut manifest = String::new();
    let mut expected = BTreeMap::new();
    for (kind, directory, names) in [
        (
            "bin",
            "src/bin",
            "wrix-host-controller wrix-egress-gateway wrix-shared-service wrix-nix-pool-owner wrix-nix-worker-broker",
        ),
        (
            "example",
            "examples",
            "host-controller-fixture wrix-native-fixtures",
        ),
        (
            "test",
            "tests",
            "backend_compiler_drain backend_drain bd_wrapper celler_auth deployment_endpoints deployment_identity deployment_lifecycle deployment_selection gateway_native generation_handoff host_control host_service lifecycle nix_pool nix_store_gc recovery_message shared_startup",
        ),
    ] {
        for name in names.split_whitespace() {
            let path = format!("wrix-service/{directory}/{name}.rs");
            write!(manifest, "[[{kind}]]\nname='{name}'\npath='../{path}'\n").unwrap();
            workspace.write(&format!("crates/{path}"), "fn main() {}\n");
            expected.insert(
                Name::try_from(format!("cargo:wrix-service-server:{kind}:{name}")).unwrap(),
                BTreeSet::from([resource(&format!("crates/{path}"))]),
            );
        }
    }
    workspace.manifest("crates/wrix-service-server", &manifest);
    let (snapshot, subjects) = workspace.discover().unwrap();
    assert_eq!(expected.len(), 24);
    assert_eq!(
        subjects
            .into_iter()
            .map(|(name, subject)| (name, subject.members))
            .collect::<BTreeMap<_, _>>(),
        expected
    );
    let sibling = "crates/wrix-service/src/bin/wrix-host-controller.rs";
    workspace.write(sibling, "fn main() { panic!(\"changed\"); }\n");
    assert!(matches!(
        snapshot.require_current(),
        Err(super::super::snapshot::Error::Changed)
    ));
    let (fresh, _) = workspace.discover().unwrap();
    assert_ne!(fresh.identity(), snapshot.identity());
    assert_ne!(
        fresh.files()[&resource(sibling)],
        snapshot.files()[&resource(sibling)]
    );
}

#[test]
fn equivalent_multi_parent_spellings_have_one_resource_identity() {
    let member = "crates/nested/package";
    let workspace = Workspace::new(member);
    workspace.write("crates/shared/main.rs", "fn main() {}\n");
    workspace.write(&format!("{member}/dir/keep.txt"), "captured directory\n");
    for spelling in ["../../shared/main.rs", "./dir/../../../shared/./main.rs"] {
        workspace.target(member, spelling);
        let (_, subjects) = workspace.discover().unwrap();
        let subject =
            &subjects[&Name::try_from("cargo:wrix-service-server:bin:probe".to_owned()).unwrap()];
        assert_eq!(
            subject.members,
            BTreeSet::from([resource("crates/shared/main.rs")])
        );
    }
    assert!(ResourcePath::try_from(PathBuf::from("crates/nested/../shared/main.rs")).is_err());
}

#[test]
fn root_escape_and_same_prefix_neighbor_are_outside() {
    let workspace = Workspace::new("package");
    let neighbor = workspace.root.with_file_name("checkout-neighbor");
    fs::create_dir(&neighbor).unwrap();
    assert!(matches!(
        metadata_source(
            &workspace.root,
            Path::new("package"),
            &neighbor.join("main.rs")
        ),
        Err(Error::Outside { .. })
    ));
    fs::write(
        workspace
            .root
            .with_file_name("checkout-neighbor")
            .join("main.rs"),
        "fn main() {}\n",
    )
    .unwrap();
    for path in ["../../main.rs", "../../checkout-neighbor/main.rs"] {
        workspace.target("package", path);
        assert!(matches!(workspace.discover(), Err(Error::Outside { .. })));
    }
}

#[test]
fn rejects_a_missing_target() {
    let workspace = Workspace::new("package");
    workspace.target("package", "../missing.rs");
    assert!(matches!(
        workspace.discover(),
        Err(Error::Snapshot(
            super::super::snapshot::Error::Missing { .. }
        ))
    ));
}

#[cfg(unix)]
#[test]
fn rejects_symlink_escape_even_if_cargo_flattens_the_declared_path() {
    let workspace = Workspace::new("package");
    let neighbor = workspace.root.with_file_name("outside");
    fs::create_dir_all(neighbor.join("deep")).unwrap();
    fs::write(neighbor.join("main.rs"), "not captured Rust source\n").unwrap();
    workspace.write(".gitignore", "/package/link\n");
    workspace.write("package/main.rs", "fn main() {}\n");
    std::os::unix::fs::symlink(neighbor.join("deep"), workspace.root.join("package/link")).unwrap();
    for path in ["link/../main.rs", "link/../../main.rs"] {
        workspace.target("package", path);
        assert!(
            matches!(workspace.discover(), Err(Error::CargoDirectory { path }) if path == Path::new("package/link"))
        );
    }
}

#[cfg(unix)]
#[test]
fn directory_substitution_cannot_rebind_a_discovered_target() {
    let workspace = Workspace::new("package");
    workspace.target("package", "../source/main.rs");
    workspace.write("source/main.rs", "fn main() {}\n");
    workspace.write(".gitignore", "/identical/\n");
    loom_driver::git::commit_paths_in(
        &workspace.root,
        &["source/main.rs"],
        "Capture the sibling source",
    )
    .unwrap();
    let (snapshot, subjects) = workspace.discover().unwrap();
    // Even an in-root alias to the identical bytes is not the captured directory.
    let substitute = workspace.root.join("identical");
    fs::rename(workspace.root.join("source"), &substitute).unwrap();
    std::os::unix::fs::symlink(substitute, workspace.root.join("source")).unwrap();
    let staged = discover_captured(&snapshot).unwrap();
    assert_eq!(
        staged.keys().collect::<Vec<_>>(),
        subjects.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        snapshot.text(&resource("source/main.rs")).unwrap(),
        "fn main() {}\n"
    );
    let current = snapshot.require_current();
    assert!(
        matches!(&current, Err(super::super::snapshot::Error::Unsupported { path })
            if *path == resource("source") || *path == resource("source/main.rs")),
        "replacement must be refused: {current:?}"
    );
}
