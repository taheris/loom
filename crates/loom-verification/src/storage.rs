use crate::process;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Deserialize)]
struct Info {
    #[serde(rename = "narSize")]
    nar_size: u64,
}

fn capture(log: &Path, name: &str, command: &Command) -> Result<PathBuf> {
    let output = process::capture(command, 180)?;
    let path = log.join(name);
    fs::write(&path, &output.stdout)?;
    fs::write(log.join(format!("{name}.stderr")), &output.stderr)?;
    ensure!(
        output.status.success(),
        "{name} failed with exit {}; inspect {}",
        output.status,
        log.display()
    );
    Ok(path)
}
#[expect(
    clippy::cast_precision_loss,
    reason = "Storage diagnostics intentionally round logical byte counts to approximate GiB."
)]
fn gib(bytes: u64, digits: i32) -> f64 {
    let scale = 10_f64.powi(digits);
    ((bytes as f64 / 1_073_741_824.0) * scale).round() / scale
}
fn sizes(path: &Path) -> Result<BTreeMap<String, Info>> {
    serde_json::from_slice(&fs::read(path)?).with_context(|| {
        format!(
            "unexpected store metadata; inspect {}; no zero-size fallback",
            path.display()
        )
    })
}
fn ranked(info: &BTreeMap<String, Info>) -> Vec<&String> {
    let mut paths: Vec<_> = info.keys().collect();
    paths.sort_by(|a, b| {
        info[*b]
            .nar_size
            .cmp(&info[*a].nar_size)
            .then_with(|| a.cmp(b))
    });
    paths
}
fn total<'a>(mut values: impl Iterator<Item = &'a Info>) -> Result<u64> {
    values.try_fold(0_u64, |sum, value| {
        sum.checked_add(value.nar_size)
            .context("NAR size accounting overflow")
    })
}
fn rows(info: &BTreeMap<String, Info>, paths: impl Iterator<Item = impl AsRef<str>>) -> Vec<Value> {
    paths
        .take(25)
        .map(|p| {
            let p = p.as_ref();
            json!({"path":p,"NAR_GiB":gib(info[p].nar_size,3)})
        })
        .collect()
}
fn temporary_directories(bases: &BTreeSet<PathBuf>) -> (Vec<PathBuf>, Vec<Value>) {
    let mut paths = Vec::new();
    let mut errors = Vec::new();
    for base in bases {
        let result = (|| -> Result<()> {
            if !base.is_dir() {
                return Ok(());
            }
            let mut entries = fs::read_dir(base)?.collect::<std::result::Result<Vec<_>, _>>()?;
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("nix-build-")
                    && entry.file_type()?.is_dir()
                {
                    paths.push(entry.path());
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            errors.push(json!({"path":base,"error":format!("{error:#}")}));
        }
    }
    (paths, errors)
}
fn dead_summary(info: &BTreeMap<String, Info>, dead: &[String], summary: &mut Value) -> Result<()> {
    let unknown: Vec<_> = dead.iter().filter(|p| !info.contains_key(*p)).collect();
    let dead_set: BTreeSet<_> = dead.iter().collect();
    summary["dead_paths"] = json!(dead.len());
    summary["dead_paths_missing_from_metadata"] = json!(unknown);
    summary["dead_NAR_GiB"] = if unknown.is_empty() {
        json!(gib(total(dead.iter().map(|p| &info[p]))?, 2))
    } else {
        Value::Null
    };
    summary["largest_dead_paths"] = json!(rows(
        info,
        ranked(info).into_iter().filter(|p| dead_set.contains(p))
    ));
    Ok(())
}

pub fn run(dead: bool) -> Result<()> {
    let repo = env::current_dir()?;
    let parent = repo.join(".loom/logs");
    fs::create_dir_all(&parent)?;
    let log = tempfile::Builder::new()
        .prefix("storage-")
        .tempdir_in(&parent)?
        .keep();
    let mut summary = json!({"logs":log,"sizes":"Nix NAR logical bytes from the store database, NOT physical du bytes or guaranteed reclaimed bytes","filesystems":{},"compiler_environment":{}});
    for key in [
        "RUSTC",
        "RUSTDOC",
        "RUSTUP_TOOLCHAIN",
        "RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC",
        "CARGO_TARGET_DIR",
        "SCCACHE_DIR",
    ] {
        summary["compiler_environment"][key] =
            json!(env::var_os(key).map(|v| v.to_string_lossy().into_owned()));
    }
    for path in [&repo, Path::new("/nix/store")] {
        let stats = rustix::fs::statvfs(path)?;
        let free = stats
            .f_bavail
            .checked_mul(stats.f_frsize)
            .context("filesystem size overflow")?;
        let total = stats
            .f_blocks
            .checked_mul(stats.f_frsize)
            .context("filesystem size overflow")?;
        summary["filesystems"][path.to_str().context("non-UTF8 filesystem path")?] =
            json!({"free_GiB":gib(free,2),"total_GiB":gib(total,2)});
    }
    let config_file = capture(
        &log,
        "store-config.json",
        Command::new("nix").args([
            "--extra-experimental-features",
            "nix-command flakes",
            "config",
            "show",
            "--json",
        ]),
    )?;
    let config: Value = serde_json::from_slice(&fs::read(config_file)?)?;
    summary["keep_failed_builds"] = config["keep-failed"]["value"].clone();
    let mut bases = BTreeSet::from([
        env::temp_dir(),
        PathBuf::from("/tmp"),
        PathBuf::from("/var/tmp"),
        PathBuf::from("/nix/var/nix/builds"),
    ]);
    if let Some(base) = config["build-dir"]["value"].as_str() {
        bases.insert(base.into());
    }
    let (temporary, errors) = temporary_directories(&bases);
    summary["temporary_build_directories"] = json!(temporary);
    summary["temporary_directory_inspection_errors"] = json!(errors);
    summary["temporary_build_directory_warning"] = json!(
        "Names only, no recursive size scan. These paths may be active or contain diagnostics; they are NOT disposal authorization."
    );
    let info_file = capture(
        &log,
        "store-paths.json",
        Command::new("nix").args([
            "--extra-experimental-features",
            "nix-command flakes",
            "path-info",
            "--all",
            "--json",
            "--json-format",
            "1",
        ]),
    )?;
    let info = sizes(&info_file)?;
    summary["store_paths"] = json!(info.len());
    summary["store_NAR_GiB"] = json!(gib(total(info.values())?, 2));
    summary["largest_store_paths"] = json!(rows(&info, ranked(&info).into_iter()));
    if dead {
        let dead_file = capture(
            &log,
            "dead-paths",
            Command::new("nix-store").args(["--gc", "--print-dead"]),
        )?;
        capture(
            &log,
            "gc-roots",
            Command::new("nix-store").args(["--gc", "--print-roots"]),
        )?;
        let dead = fs::read_to_string(dead_file)?
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        dead_summary(&info, &dead, &mut summary)?;
    }
    fs::write(
        log.join("summary.json"),
        format!("{}\n", serde_json::to_string_pretty(&summary)?),
    )?;
    serde_json::to_writer_pretty(io::stdout().lock(), &summary)?;
    writeln!(io::stdout().lock())?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_rejects_malformed_sizes_instead_of_zero() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("metadata.json");
        for value in [json!(true), json!(-1), json!("large"), Value::Null] {
            fs::write(
                &path,
                serde_json::to_vec(&json!({"path":{"narSize":value}})).unwrap(),
            )
            .unwrap();
            assert!(
                sizes(&path)
                    .unwrap_err()
                    .to_string()
                    .contains("no zero-size fallback")
            );
        }
    }
    #[test]
    fn metadata_counts_logical_sizes_and_ranks_largest_first() {
        let info: BTreeMap<String, Info> = serde_json::from_value(
            json!({"live":{"narSize":2_147_483_648_u64},"dead":{"narSize":1_073_741_824_u64}}),
        )
        .unwrap();
        assert_eq!(gib(total(info.values()).unwrap(), 2), 3.0);
        assert_eq!(rows(&info, ranked(&info).into_iter())[0]["path"], "live");
        let mut summary = json!({});
        dead_summary(&info, &["dead".into()], &mut summary).unwrap();
        assert_eq!(summary["dead_NAR_GiB"], 1.0);
        assert_eq!(summary["dead_paths_missing_from_metadata"], json!([]));
    }
    #[test]
    fn unknown_dead_paths_have_no_complete_reclaimable_total() {
        let info = BTreeMap::new();
        let mut summary = json!({});
        dead_summary(&info, &["unknown".into()], &mut summary).unwrap();
        assert!(summary["dead_NAR_GiB"].is_null());
        assert_eq!(
            summary["dead_paths_missing_from_metadata"],
            json!(["unknown"])
        );
    }
    #[test]
    fn temporary_build_inspection_preserves_diagnostics_and_refuses_symlinked_directories() {
        let root = tempfile::tempdir().unwrap();
        let build = root.path().join("nix-build-probe");
        fs::create_dir(&build).unwrap();
        fs::write(build.join("diagnostic"), "retained").unwrap();
        std::os::unix::fs::symlink(&build, root.path().join("nix-build-alias")).unwrap();
        let (paths, errors) = temporary_directories(&BTreeSet::from([root.path().into()]));
        assert_eq!(paths, vec![build.clone()]);
        assert_eq!(errors, Vec::<Value>::new());
        assert_eq!(
            fs::read_to_string(build.join("diagnostic")).unwrap(),
            "retained"
        );
    }
}
