use std::{fs, path::Path, process::Command};

#[test]
fn external_consumer_fixture_survives_checkout_relocation() {
    let package = std::env::current_dir().unwrap();
    let root = package.parent().unwrap().parent().unwrap();
    let fixture = tempfile::tempdir().unwrap();
    let original = fixture.path().join("original");
    let relocated = fixture.path().join("relocated");
    fs::create_dir(&original).unwrap();
    let mut manifest: toml::Value =
        toml::from_str(&fs::read_to_string(root.join("Cargo.toml")).unwrap()).unwrap();
    manifest["workspace"]["members"] =
        toml::Value::try_from(["crates/loom-events", "crates/loom-issues"]).unwrap();
    manifest["workspace"]
        .as_table_mut()
        .unwrap()
        .remove("default-members");
    fs::write(
        original.join("Cargo.toml"),
        toml::to_string(&manifest).unwrap(),
    )
    .unwrap();
    fs::copy(root.join("Cargo.lock"), original.join("Cargo.lock")).unwrap();
    for name in ["loom-events", "loom-issues"] {
        copy_source(
            &root.join("crates").join(name),
            &original.join("crates").join(name),
        )
        .unwrap();
    }
    let output = Command::new(env!("CARGO"))
        .args([
            "test",
            "--offline",
            "-p",
            "loom-issues",
            "--test",
            "external",
            "--no-run",
            "--message-format=json",
        ])
        .current_dir(&original)
        .env("CARGO_TARGET_DIR", fixture.path().join("target"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let executable = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .find_map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["executable"]
                .as_str()
                .map(str::to_owned)
        })
        .unwrap();
    fs::rename(&original, &relocated).unwrap();
    assert!(!original.exists());
    let output = Command::new(executable)
        .args([
            "--exact",
            "issues_external_consumer_preserves_extraction_boundary",
            "--nocapture",
        ])
        .current_dir(relocated.join("crates/loom-issues"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

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
    // Cargo and Nextest run integration tests in the current package directory.
    let package = std::env::current_dir().unwrap();
    let root = package.parent().unwrap().parent().unwrap();
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
