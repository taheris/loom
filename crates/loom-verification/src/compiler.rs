use crate::process;
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Write},
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
};
use walkdir::WalkDir;

fn fixture_source(inline: bool, panic: bool) -> String {
    let functions = (0..2000)
        .map(|n| {
            format!(
                "{}fn value_{n}(x:u64)->u64{{x.wrapping_add({n})}}",
                if inline { "#[inline(never)] " } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let calls = (0..2000)
        .map(|n| format!("value=value_{n}(std::hint::black_box(value));"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{functions}\nfn main(){{assert!(cfg!(debug_assertions));\n{}\nlet mut value=0;\n{calls}\nprintln!(\"{{value}}\");}}\n#[test] fn assertions_remain_enabled(){{assert!(cfg!(debug_assertions));}}\n",
        if panic {
            include_str!("../tests/fixtures/debug-panic.txt").trim_end()
        } else {
            ""
        }
    )
}
fn behavior(executable: &Path, expected: &str) -> Result<()> {
    let output = process::success(&Command::new(executable), 30)?;
    ensure!(
        String::from_utf8(output.stdout)?.trim() == expected,
        "compiled fixture returned stale behavior"
    );
    Ok(())
}
fn allocated(root: &Path) -> Result<u64> {
    let mut files = BTreeMap::new();
    for entry in WalkDir::new(root) {
        let entry = entry?;
        let info = fs::metadata(entry.path())?;
        if info.is_file() {
            files.insert(
                (info.dev(), info.ino()),
                info.blocks()
                    .checked_mul(512)
                    .context("allocated size overflow")?,
            );
        }
    }
    files.values().try_fold(0_u64, |sum, bytes| {
        sum.checked_add(*bytes).context("allocated size overflow")
    })
}
fn incremental_objects(root: &Path, extension: &str) -> Result<bool> {
    if !root.exists() {
        return Ok(false);
    }
    for entry in WalkDir::new(root) {
        if entry?.path().extension().is_some_and(|e| e == extension) {
            return Ok(true);
        }
    }
    Ok(false)
}
fn cargo(root: &Path, target: &Path, incremental: bool, arguments: &[&str]) -> Result<()> {
    let mut command = Command::new("cargo");
    command
        .args(arguments)
        .args(["--offline", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .current_dir(root);
    for (key, _) in env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("CARGO_PROFILE_")
            || name.starts_with("CARGO_BUILD_")
            || name.starts_with("RUST")
            || ["CARGO_INCREMENTAL", "CARGO_ENCODED_RUSTFLAGS"].contains(&name.as_ref())
        {
            command.env_remove(key);
        }
    }
    command
        .env("CARGO_HOME", root.join("home"))
        .env("CARGO_TARGET_DIR", target);
    if incremental {
        command.env("CARGO_INCREMENTAL", "1");
    }
    process::success(&command, 300)?;
    Ok(())
}

pub fn footprint(manifest: &Path) -> Result<()> {
    let manifest: toml::Value = toml::from_str(&fs::read_to_string(manifest)?)?;
    let mut profile = manifest["profile"]["dev"]
        .as_table()
        .context("missing dev profile")?
        .clone();
    profile.remove("package");
    let root = tempfile::Builder::new()
        .prefix("cargo-footprint-")
        .tempdir()?;
    let root = root.path();
    fs::create_dir(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname=\"footprint\"\nversion=\"0.1.0\"\nedition=\"2024\"\n[profile.dev]\n{}",
            toml::to_string(&profile)?
        ),
    )?;
    let source = root.join("src/main.rs");
    fs::write(&source, fixture_source(false, false))?;
    let target = root.join("target");
    cargo(root, &target, false, &["build", "-vv"])?;
    let executable = target.join("debug/footprint");
    behavior(&executable, "1999000")?;
    cargo(root, &target, false, &["test"])?;
    for extension in ["o", "bin"] {
        ensure!(
            !incremental_objects(&target.join("debug/incremental"), extension)?,
            "dev/test retained incremental artifacts"
        );
    }
    let compact = allocated(&target)?;
    let before = fs::metadata(&executable)?.modified()?;
    cargo(root, &target, false, &["build", "-vv"])?;
    ensure!(
        fs::metadata(&executable)?.modified()? == before,
        "unchanged build did not reuse executable"
    );
    ensure!(
        allocated(&target)? == compact,
        "unchanged build grew target"
    );
    fs::write(
        &source,
        fs::read_to_string(&source)?.replace("let mut value=0;", "let mut value=1;"),
    )?;
    cargo(root, &target, false, &["build"])?;
    behavior(&executable, "1999001")?;
    fs::write(
        &source,
        fs::read_to_string(&source)?.replace("let mut value=1;", "let mut value=0;"),
    )?;
    let target = root.join("incremental-target");
    cargo(root, &target, true, &["build"])?;
    cargo(root, &target, true, &["test"])?;
    ensure!(
        incremental_objects(&target.join("debug/incremental"), "o")?,
        "positive incremental control produced no objects"
    );
    let incremental = allocated(&target)?;
    ensure!(
        incremental > compact,
        "incremental control did not increase storage"
    );
    serde_json::to_writer(
        io::stdout().lock(),
        &json!({"compact_bytes":compact,"incremental_bytes":incremental,"extra_incremental_bytes":incremental-compact}),
    )?;
    writeln!(io::stdout().lock())?;
    Ok(())
}

pub fn writable(script: &Path, rustc: &Path) -> Result<()> {
    let root = tempfile::Builder::new()
        .prefix("writable-artifacts-")
        .tempdir()?;
    let root = root.path();
    let target = root.join("target/debug/deps");
    fs::create_dir_all(&target)?;
    let store = root.join("store");
    fs::create_dir(&store)?;
    let source = root.join("probe.rs");
    fs::write(&source, "pub fn value()->u32{1}\n")?;
    let compile = || {
        let mut command = Command::new(rustc);
        command
            .args([
                "--crate-name",
                "workspace_probe",
                "--crate-type",
                "lib",
                "--emit",
                "link,metadata",
                "--out-dir",
            ])
            .arg(&target)
            .arg(&source);
        command
    };
    process::success(&compile(), 60)?;
    let mut originals = BTreeMap::new();
    for artifact in fs::read_dir(&target)? {
        let artifact = artifact?;
        let retained = store.join(artifact.file_name());
        fs::rename(artifact.path(), &retained)?;
        fs::set_permissions(&retained, fs::Permissions::from_mode(0o444))?;
        originals.insert(retained.clone(), fs::read(&retained)?);
        symlink(retained, artifact.path())?;
    }
    let dependency = store.join("libdependency-hash.rmeta");
    fs::write(&dependency, b"untouched dependency")?;
    fs::set_permissions(&dependency, fs::Permissions::from_mode(0o444))?;
    symlink(&dependency, target.join("libdependency-hash.rmeta"))?;
    fs::write(&source, "pub fn value()->u32{2}\n")?;
    let rejected = process::capture(&compile(), 60)?;
    ensure!(
        !rejected.status.success()
            && String::from_utf8_lossy(&rejected.stderr).contains("not writeable"),
        "immutable artifact negative control failed: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    let prepare = |directory: &Path| -> Result<()> {
        process::success(
            Command::new("bash")
                .arg(script)
                .arg(directory)
                .arg("workspace_probe"),
            30,
        )?;
        Ok(())
    };
    prepare(&root.join("target"))?;
    for (retained, contents) in &originals {
        let artifact = target.join(retained.file_name().context("artifact missing name")?);
        ensure!(
            !artifact.is_symlink()
                && fs::metadata(&artifact)?.mode() & 0o200 != 0
                && fs::read(artifact)? == *contents,
            "workspace artifact was not copied writable and intact"
        );
    }
    process::success(&compile(), 60)?;
    ensure!(
        target.join("libdependency-hash.rmeta").is_symlink()
            && fs::read(&dependency)? == b"untouched dependency",
        "dependency artifact was mutated"
    );
    for (path, contents) in originals {
        ensure!(
            fs::read(path)? == contents,
            "retained workspace artifact was mutated"
        );
    }
    prepare(&root.join("target"))?;
    prepare(&root.join("absent"))?;
    writeln!(
        io::stdout().lock(),
        "Workspace recompilation passed; shared store and dependency links unchanged"
    )?;
    Ok(())
}

pub fn compression(hook: &Path, rustc: &Path) -> Result<()> {
    let root = tempfile::Builder::new()
        .prefix("debug-compression-")
        .tempdir()?;
    let root = root.path();
    let source = root.join("debug_probe.rs");
    fs::write(&source, fixture_source(true, true))?;
    let plain = root.join("plain");
    let compressed = root.join("compressed");
    for executable in [&plain, &compressed] {
        let mut command = if executable == &compressed {
            let mut command = Command::new("bash");
            command
                .args([
                    "-c",
                    "set -euo pipefail; source \"$1\"; shift; exec \"$@\"",
                    "_",
                ])
                .arg(hook)
                .arg(rustc);
            command
        } else {
            Command::new(rustc)
        };
        command
            .args([
                "--crate-name",
                "debug_probe",
                "--edition=2024",
                "-C",
                "debuginfo=line-tables-only",
                "-C",
                "debug-assertions=yes",
            ])
            .arg(&source)
            .arg("-o")
            .arg(executable);
        process::success(&command, 180)?;
        behavior(executable, "1999000")?;
        let panic = process::capture(
            Command::new(executable)
                .arg("panic")
                .env("RUST_BACKTRACE", "1"),
            30,
        )?;
        let stderr = String::from_utf8(panic.stderr)?;
        ensure!(
            panic.status.code() == Some(101)
                && ["compression-probe", "debug_probe.rs:", "debug_probe::main"]
                    .iter()
                    .all(|s| stderr.contains(s)),
            "code/source-line backtrace was not preserved: {stderr}"
        );
    }
    for section in [".text", ".rodata"] {
        let mut data = Vec::new();
        for executable in [&plain, &compressed] {
            let output = root.join(format!(
                "{}{section}",
                executable
                    .file_name()
                    .context("missing executable name")?
                    .to_string_lossy()
            ));
            process::success(
                Command::new("objcopy")
                    .args(["-O", "binary", &format!("--only-section={section}")])
                    .arg(executable)
                    .arg(&output),
                30,
            )?;
            data.push(fs::read(output)?);
        }
        ensure!(
            data[0] == data[1],
            "compression changed allocated {section} bytes"
        );
    }
    let plain = fs::metadata(plain)?.len();
    let compressed = fs::metadata(compressed)?.len();
    ensure!(
        compressed < plain,
        "compiler hook did not reduce debug footprint"
    );
    serde_json::to_writer(
        io::stdout().lock(),
        &json!({"plain_bytes":plain,"compressed_bytes":compressed,"code_and_backtraces_preserved":true}),
    )?;
    writeln!(io::stdout().lock())?;
    Ok(())
}

pub fn consumer(expression: &Path, manifest: &Path, native: &Path) -> Result<()> {
    let root = tempfile::Builder::new().prefix("loom-eval-").tempdir_in(
        PathBuf::from(env::var_os("TMPDIR").context("consumer fixture requires TMPDIR")?)
            .canonicalize()?,
    )?;
    let evaluate = |attribute: &str| -> Result<std::process::Output> {
        process::capture(
            Command::new("nix")
                .args([
                    "eval",
                    "--impure",
                    "--offline",
                    "--json",
                    "--no-eval-cache",
                    "--store",
                ])
                .arg(root.path())
                .args([
                    "--extra-experimental-features",
                    "nix-command flakes",
                    "--option",
                    "allow-import-from-derivation",
                    "false",
                    "--file",
                ])
                .arg(expression)
                .arg(attribute),
            120,
        )
    };
    let fixed = evaluate("fixed")?;
    ensure!(
        fixed.status.success(),
        "{}",
        String::from_utf8_lossy(&fixed.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&fixed.stdout)?;
    ensure!(
        result["check"].as_str().is_some_and(|s| Path::new(s)
            .extension()
            .is_some_and(|extension| extension == "drv"))
            && result["metadata"]
                .as_str()
                .is_some_and(|s| s.ends_with("/Cargo.toml"))
            && result["lock"]
                .as_str()
                .is_some_and(|s| s.ends_with("/Cargo.lock")),
        "invalid cold consumer metadata: {result}"
    );
    let staged = result["staged"].as_str().context("missing staged source")?;
    ensure!(
        result["vendor"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s != staged),
        "cold consumer vendor/staged identity differs"
    );
    ensure!(
        !root.path().join(staged.trim_start_matches('/')).exists(),
        "cold evaluation realized staged source"
    );
    let legacy = evaluate("legacy")?;
    let stderr = String::from_utf8(legacy.stderr)?;
    ensure!(
        !legacy.status.success()
            && stderr.contains("allow-import-from-derivation")
            && stderr.contains("loom-src-with-extras"),
        "legacy implicit Cargo metadata unexpectedly succeeded cold: {stderr}"
    );
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(manifest)?)?;
    let native: serde_json::Value = serde_json::from_slice(&fs::read(native)?)?;
    for (profile, runtimes) in manifest.as_object().context("invalid profile manifest")? {
        for (runtime, entry) in runtimes.as_object().context("invalid runtime manifest")? {
            let mut keys: Vec<_> = entry
                .as_object()
                .context("invalid manifest entry")?
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            ensure!(
                keys == ["profile_config", "ref", "source", "source_kind"],
                "invalid data-only manifest entry: {entry}"
            );
            ensure!(
                Path::new(entry["source"].as_str().context("missing image source")?).is_file(),
                "image provenance lost its Nix context"
            );
            let path = entry["profile_config"]
                .as_str()
                .context("missing profile config")?;
            let config: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
            ensure!(
                config["agent"]["kind"] == *runtime
                    && config["image"]["digest"] == "fixture-digest",
                "invalid profile config provenance"
            );
            if profile == "rust" {
                ensure!(
                    native[path] == "rust",
                    "missing native Rust profile mapping"
                );
            }
        }
    }
    writeln!(
        io::stdout().lock(),
        "Cold consumer check evaluation and data-only manifest provenance passed"
    )?;
    Ok(())
}

#[cfg(test)]
mod fixture_tests {
    use super::fixture_source;

    #[test]
    fn compiler_fixture_retains_all_functions_and_real_failure_probe() {
        let source = fixture_source(true, true);
        assert_eq!(source.matches("fn value_").count(), 2000);
        assert!(source.contains(include_str!("../tests/fixtures/debug-panic.txt").trim_end()));
        assert!(source.contains("assert!(cfg!(debug_assertions))"));
        assert!(!fixture_source(false, false).contains("compression-probe"));
    }
}
