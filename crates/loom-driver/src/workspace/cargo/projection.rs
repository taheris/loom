use super::*;
use crate::clock::MockClock;
use crate::config::{CargoTargetReuse, LoomTopConfig};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

struct Launch(Child, PathBuf);

impl Drop for Launch {
    fn drop(&mut self) {
        if let Err(error) = fs::write(&self.1, "release") {
            tracing::warn!(?error, "could not release projection fixture workload");
        }
        match self.0.try_wait() {
            Ok(Some(_)) => (),
            Ok(None) => {
                if let Err(error) = self.0.kill() {
                    tracing::warn!(?error, "could not stop projection fixture launcher");
                }
                if let Err(error) = self.0.wait() {
                    tracing::warn!(?error, "could not reap projection fixture launcher");
                }
            }
            Err(error) => tracing::warn!(?error, "could not inspect projection fixture launcher"),
        }
    }
}

#[test]
#[ignore = "requires actual Linux Wrix/Podman and an explicitly selected Rust Pi profile"]
fn wrix_cargo_storage_projection_matches_admitted_profile() {
    let launcher =
        PathBuf::from(std::env::var_os("LOOM_CONFORMANCE_WRIX").expect("select host Wrix"));
    let profile_source = PathBuf::from(
        std::env::var_os("LOOM_CONFORMANCE_PROFILE").expect("select Rust Pi ProfileConfig"),
    );
    let (root, association, worker, profile) = tests::cases::fixture();
    let retained = root.keep();
    eprintln!("Wrix projection evidence: {}", retained.display());
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(profile_source).unwrap()).unwrap();
    assert_eq!(value["agent"]["kind"], "pi");
    assert_eq!(value["profile"]["name"], "rust");
    value["services"]["nix_cache"]["enable"] = false.into();
    fs::write(&profile, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .arg(&worker)
            .status()
            .unwrap()
            .success()
    );
    let auth = retained.join("auth.json");
    fs::write(&auth, "{}").unwrap();
    let cache = retained.join("worker-cache");
    fs::create_dir(&cache).unwrap();
    fs::write(cache.join("host-cache-sentinel"), "shared-worker-pool").unwrap();
    fs::write(
        worker.join("probe.ts"),
        r"import { spawnSync } from 'node:child_process';
const result = spawnSync('bash', ['/workspace/probe.sh'], {stdio: 'inherit'});
process.exit(result.status ?? 1);
export default function() {}
",
    )
    .unwrap();
    fs::write(
        worker.join("probe.sh"),
        r#"set -euo pipefail
[[ "$CARGO_TARGET_DIR" == /loom-cargo-target ]]
[[ "$CARGO_BUILD_TARGET_DIR" == /loom-cargo-target ]]
[[ "$CARGO_BUILD_BUILD_DIR" == /loom-cargo-target ]]
printf 'guest-target-write' > /loom-cargo-target/guest-sentinel
if [[ "$PROBE_SHARED" == 1 ]]; then
  [[ "$SCCACHE_DIR" == /sccache ]]
  [[ "$RUSTC_WRAPPER" == sccache ]]
  [[ "$CARGO_BUILD_RUSTC_WRAPPER" == sccache ]]
  [[ "$(< /sccache/host-cache-sentinel)" == shared-worker-pool ]]
else
  [[ ! -e /sccache/host-cache-sentinel ]]
fi
printf 'pub fn answer() -> u32 { 42 }\n' > /workspace/lib.rs
sccache rustc --crate-type lib /workspace/lib.rs --out-dir /loom-cargo-target
sccache --show-stats --stats-format=json > /workspace/stats.json
printf ready > /workspace/ready
while [[ ! -e /workspace/release ]]; do sleep 0.1; done
"#,
    )
    .unwrap();
    let config = LoomTopConfig {
        sccache_dir: Some(cache.clone()),
        ..LoomTopConfig::default()
    };
    for shared in [true, false] {
        for name in ["ready", "release", "stats.json"] {
            let path = worker.join(name);
            if path.exists() {
                fs::remove_file(path).unwrap();
            }
        }
        fs::write(
            worker.join("wrix.toml"),
            format!("[tool_caches]\nshared = {shared}\n"),
        )
        .unwrap();
        let admitted = shared_cache_config(&config, &worker).unwrap();
        let namespace = prepare(
            &association,
            &worker,
            &profile,
            CargoTargetReuse::Off,
            retention::Policy::default().default_request(),
            &MockClock::new(),
        )
        .unwrap();
        let mut spawn: SpawnConfig = serde_json::from_value(serde_json::json!({
            "image_ref": value["image"]["ref"], "workspace": worker, "env": [],
            "initial_prompt": "fixture", "agent_args": ["--extension", "/workspace/probe.ts", "--mode", "rpc"],
            "repin": {"orientation":"fixture", "pinned_context":"fixture", "partial_bodies":[]}
        })).unwrap();
        spawn.env.extend(admitted.container_sccache_env());
        spawn
            .env
            .push(("PROBE_SHARED".into(), u8::from(shared).to_string()));
        if let Some(source) = &admitted.sccache_dir {
            spawn.mounts.push(MountSpec {
                host_path: source.clone(),
                container_path: admitted.sccache_container_path.clone(),
                read_only: false,
            });
        }
        namespace.configure(&mut spawn).unwrap();
        association
            .validate_worker(&worker, &spawn.mounts, Some(&profile))
            .unwrap();
        let spawn_path = retained.join(format!("spawn-{shared}.json"));
        fs::write(
            &spawn_path,
            serde_json::to_vec_pretty(&spawn.to_wrix_json().unwrap()).unwrap(),
        )
        .unwrap();
        let log_path = retained.join(format!("launch-{shared}.log"));
        let log = File::create(&log_path).unwrap();
        let mut command = Command::new(&launcher);
        for name in value["security"]["runtime_secrets"]
            .as_object()
            .unwrap()
            .keys()
        {
            command.env_remove(name);
        }
        for name in [
            "CONTAINER_HOST",
            "CONTAINER_CONNECTION",
            "DOCKER_HOST",
            "WRIX_DEPLOY_KEY",
            "WRIX_SIGNING_KEY",
        ] {
            command.env_remove(name);
        }
        let mut child = Launch(
            command
                .arg("--profile-config")
                .arg(&profile)
                .args(["spawn", "--spawn-config"])
                .arg(&spawn_path)
                .arg("--stdio")
                .env("WRIX_PI_AUTH_FILE", &auth)
                .env_remove("WRIX_UNSAFE_PODMAN_SOCKET")
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
            worker.join("release"),
        );
        let deadline = Instant::now() + Duration::from_secs(600);
        while !worker.join("ready").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "Wrix failed: {}",
                fs::read_to_string(&log_path).unwrap()
            );
            assert!(
                Instant::now() < deadline,
                "Wrix readiness timed out: {}",
                log_path.display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let ids = Command::new("podman")
            .args(["ps", "--no-trunc", "--format", "{{.ID}}"])
            .env_remove("CONTAINER_HOST")
            .env_remove("CONTAINER_CONNECTION")
            .output()
            .unwrap();
        assert!(ids.status.success());
        let ids = String::from_utf8(ids.stdout).unwrap();
        let mut matching = Vec::new();
        for id in ids.lines() {
            let output = Command::new("podman")
                .args(["inspect", "--format", "{{json .Mounts}}", id])
                .env_remove("CONTAINER_HOST")
                .env_remove("CONTAINER_CONNECTION")
                .output()
                .unwrap();
            assert!(output.status.success());
            let mounts: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
            if mounts
                .iter()
                .any(|mount| mount["Source"] == namespace.target().to_str().unwrap())
            {
                matching.push(serde_json::json!({"Id": id, "Mounts": mounts}));
            }
        }
        assert_eq!(matching.len(), 1, "no unique actual target projection");
        let inspect = matching.pop().unwrap();
        fs::write(
            retained.join(format!("inspect-{shared}.json")),
            serde_json::to_vec_pretty(&inspect).unwrap(),
        )
        .unwrap();
        let mounts = inspect["Mounts"].as_array().unwrap();
        assert!(mounts.iter().any(
            |mount| mount["Source"] == namespace.target().to_str().unwrap()
                && mount["Destination"] == WORKER_TARGET
                && mount["RW"] == true
        ));
        assert_eq!(
            mounts
                .iter()
                .any(|mount| mount["Source"] == cache.to_str().unwrap()),
            shared
        );
        for mount in mounts {
            let source = Path::new(mount["Source"].as_str().unwrap());
            assert!(!source.starts_with(association.integration()));
            assert!(!source.starts_with(trusted_root().unwrap()));
            assert_ne!(source, association.operator());
            assert!(!std::env::current_exe().unwrap().starts_with(source));
        }
        assert_eq!(
            fs::read_to_string(namespace.target().join("guest-sentinel")).unwrap(),
            "guest-target-write"
        );
        let stats: serde_json::Value =
            serde_json::from_slice(&fs::read(worker.join("stats.json")).unwrap()).unwrap();
        let location = stats["cache_location"].as_str().unwrap();
        assert!(
            location.contains(if shared {
                "/sccache"
            } else {
                "/home/wrix/.cache/sccache"
            }),
            "{stats}"
        );
        assert!(
            stats["max_cache_size"]
                .as_u64()
                .is_some_and(|size| size > 0 && size <= 50 * 1024 * 1024 * 1024),
            "{stats}"
        );
        fs::copy(
            worker.join("stats.json"),
            retained.join(format!("stats-{shared}.json")),
        )
        .unwrap();
        fs::write(worker.join("release"), "release").unwrap();
        assert!(child.0.wait().unwrap().success());
        assert!(
            namespace.target().exists(),
            "runtime completion must not authorize disposal"
        );
    }
}
