//! A real linked-analysis project for CLI admission and diagnostic execution tests.

use std::path::Path;
use std::process::Command;

pub const TARGET: &str = "cargo run -p loom-walk -- loom_llm_deps";

pub fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

pub fn native(root: &Path, command: &str, args: &[&str]) {
    let output = Command::new(command)
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    native(root, "git", &["init"]);
    write(root, ".gitignore", "/.loom/\n/target/\n");
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers=['crates/*']\nresolver='3'\n",
    );
    for package in ["loom-walk", "loom-llm", "loom-driver"] {
        write(
            root,
            &format!("crates/{package}/Cargo.toml"),
            &format!("[package]\nname='{package}'\nversion='0.1.0'\nedition='2024'\n"),
        );
        write(
            root,
            &format!("crates/{package}/src/lib.rs"),
            "pub fn fixture() {}\n",
        );
    }
    write(
        root,
        "crates/loom-walk/src/lib.rs",
        "#[path=\"walk/loom_llm_deps.rs\"] mod loom_llm_deps;\n",
    );
    write(
        root,
        "crates/loom-walk/src/walk/loom_llm_deps.rs",
        include_str!("../../../loom-walk/src/walk/loom_llm_deps.rs"),
    );
    write(
        root,
        "docs/README.md",
        "- [fixture](../specs/fixture/spec.md)\n",
    );
    write(root, "specs/fixture/spec.md", "# Fixture\n");
    write(
        root,
        "specs/fixture/tests.md",
        &format!("# Fixture\n\n## Success Criteria\n\n- Leaf dependency [check]({TARGET})\n"),
    );
    write(root, "loom.toml", "");
    native(root, "cargo", &["generate-lockfile", "--offline"]);
    dir
}
