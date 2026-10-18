use std::{fs, path::Path, process::Command};

fn copy_source(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_source(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[test]
fn issues_external_consumer_preserves_extraction_boundary() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let fixture = tempfile::tempdir().unwrap();
    let root_manifest: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("Cargo.toml")).unwrap()).unwrap();
    let mut dependencies = toml::map::Map::new();
    for name in [
        "displaydoc",
        "futures-core",
        "serde",
        "serde_json",
        "thiserror",
    ] {
        dependencies.insert(
            name.into(),
            root_manifest["workspace"]["dependencies"][name].clone(),
        );
    }
    dependencies.insert(
        "loom-events".into(),
        toml::Value::Table(toml::map::Map::from_iter([(
            "path".into(),
            "loom-events".into(),
        )])),
    );
    let manifest = toml::toml! {
        [workspace]
        resolver = "3"
        members = ["loom-events", "loom-issues", "consumer"]
    };
    let mut manifest = toml::Value::Table(manifest);
    let workspace = manifest["workspace"].as_table_mut().unwrap();
    workspace.insert(
        "package".into(),
        root_manifest["workspace"]["package"].clone(),
    );
    workspace.insert("lints".into(), root_manifest["workspace"]["lints"].clone());
    workspace.insert("dependencies".into(), toml::Value::Table(dependencies));
    fs::write(
        fixture.path().join("Cargo.toml"),
        toml::to_string(&manifest).unwrap(),
    )
    .unwrap();
    for name in ["loom-events", "loom-issues"] {
        let source = root.join("crates").join(name);
        let target = fixture.path().join(name);
        copy_source(&source.join("src"), &target.join("src")).unwrap();
        let mut package: toml::Value =
            toml::from_str(&fs::read_to_string(source.join("Cargo.toml")).unwrap()).unwrap();
        package.as_table_mut().unwrap().remove("dev-dependencies");
        fs::write(
            target.join("Cargo.toml"),
            toml::to_string(&package).unwrap(),
        )
        .unwrap();
    }
    let consumer = fixture.path().join("consumer");
    fs::create_dir_all(consumer.join("src")).unwrap();
    fs::write(consumer.join("Cargo.toml"), "[package]\nname = 'consumer'\nversion = '0.0.0'\nedition = '2024'\n[dependencies]\nloom-issues = { path = '../loom-issues' }\n").unwrap();
    fs::write(
        consumer.join("src/main.rs"),
        include_str!("fixture/consumer.rs"),
    )
    .unwrap();
    let output = Command::new(env!("CARGO"))
        .args(["check", "--offline", "-p", "consumer"])
        .current_dir(fixture.path())
        .env("CARGO_TARGET_DIR", fixture.path().join("target"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
