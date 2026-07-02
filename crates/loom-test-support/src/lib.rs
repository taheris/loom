//! Shared test-support items for loom tests.
//!
//! Per `specs/tests.md` (Architecture / Property-Based Testing), the
//! CI cap on `proptest` case counts is a single named constant — one place
//! to bump, one place to grep — instead of a `with_cases(32)` literal
//! scattered across every `proptest!` block.

/// Number of `proptest` cases each property runs under `nix flake check`.
///
/// The cap exists because property tests run on every PR via `loom gate
/// verify`, and the per-property time budget is tight (Non-Functional #2
/// in `specs/tests.md` targets <30 s aggregate across `loom gate
/// test`). 32 cases keeps the wall-clock cheap while still exercising the
/// shrinker on every regression.
///
/// Local exhaustive runs override via the `PROPTEST_CASES` environment
/// variable, which `proptest` consults before falling back to the value
/// passed to `ProptestConfig::with_cases`. Setting `PROPTEST_CASES=2048`
/// (or higher) in the shell that invokes `cargo nextest run` raises the
/// case count for that invocation without touching the source. The env
/// var is therefore the local-loop knob; the constant is the CI floor.
pub const CI_PROPTEST_CASES: u32 = 32;

pub const GIT_LOCAL_ENV_VARS: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

pub fn scrub_git_local_env(command: &mut std::process::Command) {
    for &name in GIT_LOCAL_ENV_VARS {
        command.env_remove(name);
    }
}

pub fn bash_path() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("bash");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    std::path::PathBuf::from("/usr/bin/env bash")
}

pub fn bash_script(body: &str) -> String {
    format!("#!{}\n{body}", bash_path().display())
}

#[cfg(unix)]
pub fn write_executable_bash_script(
    path: impl AsRef<std::path::Path>,
    body: &str,
) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    let path = path.as_ref();
    let parent = path.parent().unwrap_or(std::path::Path::new("."));
    let script = bash_script(body);

    for _ in 0..32 {
        let nonce = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(".loom-script-{}-{nonce}.tmp", std::process::id()));
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };

        if let Err(error) = file.write_all(script.as_bytes()) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error);
        }
        if let Err(error) = file.sync_all() {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error);
        }
        drop(file);

        if let Err(error) =
            std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o755))
        {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error);
        }
        if let Err(error) = std::fs::rename(&temp_path, path) {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error);
        }
        return Ok(());
    }

    Err(Error::new(
        ErrorKind::AlreadyExists,
        "could not allocate a temporary executable-script path",
    ))
}
