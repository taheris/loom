//! Kernel lease for the full lifetime of a shared trybuild fixture.

use std::path::Path;

/// Resolve Cargo's output directory and lock the package's generated fixtures.
///
/// # Errors
///
/// Returns an error if Cargo metadata or the kernel lease is unavailable.
pub fn lock(manifest_directory: &Path, package: &str) -> std::io::Result<std::fs::File> {
    let output = std::process::Command::new("cargo")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(manifest_directory.join("Cargo.toml"))
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "cargo metadata for compile fixture: {}",
            String::from_utf8_lossy(&output.stderr),
        )));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let target = metadata["target_directory"]
        .as_str()
        .ok_or_else(|| std::io::Error::other("cargo metadata has no target_directory"))?;
    lock_target(Path::new(target), package)
}

/// Hold this lease until the fixture's `TestCases` is dropped.
///
/// # Errors
///
/// Returns an error if the directory or kernel lease cannot be created.
pub fn lock_target(target: &Path, package: &str) -> std::io::Result<std::fs::File> {
    let directory = target.join("tests");
    std::fs::create_dir_all(&directory)?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(format!(".{package}-trybuild.lock")))?;
    file.lock()?;
    Ok(file)
}
