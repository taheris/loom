use super::super::{
    Namespace, prepare,
    record::{Lifetime, Placement},
    retention::{Bytes, Fence, HoldAction, Kind, Policy},
    tests::cases::fixture,
    trusted_command_at,
};
use super::*;
use crate::{agent::SpawnConfig, clock::MockClock, config::CargoTargetReuse, state::RecordKind};
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn policy() -> Policy {
    Policy {
        max_bytes: Bytes::new(8 * 1024 * 1024),
        reservation_bytes: Bytes::new(128 * 1024),
        min_free_bytes: Bytes::default(),
        ..Policy::default()
    }
}

fn pressure() -> Policy {
    Policy {
        max_age_secs: 0,
        max_bytes: Bytes::default(),
        ..policy()
    }
}

fn spawn(source: &Path, profile: &Path, issuer: &Path) -> SpawnConfig {
    let mut spawn: SpawnConfig = serde_json::from_value(serde_json::json!({
        "image_ref":"fixture", "workspace":source, "env":[], "initial_prompt":"fixture", "agent_args":[],
        "repin":{"orientation":"fixture", "pinned_context":"fixture", "partial_bodies":[]}
    })).unwrap();
    spawn.profile_config = Some(profile.into());
    spawn.wrix_launcher = Some(issuer.into());
    spawn
}

fn issuer(root: &Path, profile: &Path) -> PathBuf {
    fs::write(
        profile,
        r#"{"profile":{"mounts":[]},"features":{"execution_tickets":true}}"#,
    )
    .unwrap();
    fs::write(root.join("observation"), "\"protected\"\n").unwrap();
    let issuer = root.join("wrix");
    let observation = root.join("observation");
    let counter = root.join("counter");
    fs::write(&issuer, format!("#!/usr/bin/env bash\nset -euo pipefail\n[[ \"$1\" == execution ]]\ncase \"$2\" in\nreserve) count=0; if [[ -f '{}' ]]; then read -r count < '{}'; fi; count=$((count+1)); printf '%s\\n' \"$count\" > '{}'; printf '%064x\\n' \"$count\";;\nreconcile) [[ \"${{#4}}\" == 64 ]]; cat '{}';;\n*) exit 2;;\nesac\n", counter.display(), counter.display(), counter.display(), observation.display())).unwrap();
    fs::set_permissions(&issuer, fs::Permissions::from_mode(0o700)).unwrap();
    issuer
}

fn allocate(association: &super::super::Association, source: &Path, profile: &Path) -> Namespace {
    prepare(
        association,
        source,
        profile,
        CargoTargetReuse::Off,
        policy().default_request(),
        &MockClock::new(),
    )
    .unwrap()
}

#[test]
fn wrix_cargo_retention_pressure_bounds_protect_independent_work() {
    let (root, association, source, profile) = fixture();
    let issuer = issuer(root.path(), &profile);
    let namespace = allocate(&association, &source, &profile);
    let target = namespace.target();
    let mut launch = spawn(&source, &profile, &issuer);
    namespace.configure(&mut launch).unwrap();
    assert!(
        launch
            .launcher_env
            .iter()
            .any(|(name, _)| name == "WRIX_EXECUTION_TICKET")
    );
    assert!(
        !launch
            .to_wrix_json()
            .unwrap()
            .to_string()
            .contains("WRIX_EXECUTION_TICKET")
    );
    fs::write(target.join("output"), vec![7; 64 * 1024]).unwrap();
    fs::write(
        source.join("worker-completed"),
        "retired; PID missing; lease free",
    )
    .unwrap();
    drop(namespace);
    let charged = retention::sweep(&association, &pressure(), &MockClock::new())
        .unwrap()
        .charged;
    assert!(target.exists());
    let smaller = Policy {
        max_bytes: Bytes::new(charged.get().checked_add(256 * 1024).unwrap()),
        reservation_bytes: Bytes::new(32 * 1024),
        ..policy()
    };
    let independent = prepare(
        &association,
        &source,
        &profile,
        CargoTargetReuse::Off,
        smaller.default_request(),
        &MockClock::new(),
    )
    .unwrap();
    assert!(independent.target().exists());
    fs::write(root.path().join("observation"), "\"retired\"\n").unwrap();
    retention::sweep(&association, &pressure(), &MockClock::new()).unwrap();
    assert!(!target.exists());
    assert!(source.join("worker-completed").exists());
    assert!(source.join("Cargo.toml").exists());
    assert!(independent.target().exists());
}

#[test]
fn cargo_retention_durable_holds_survive_cache_replay_and_independent_release() {
    let (root, association, source, profile) = fixture();
    let issuer = issuer(root.path(), &profile);
    let namespace = allocate(&association, &source, &profile);
    namespace
        .configure(&mut spawn(&source, &profile, &issuer))
        .unwrap();
    let target = namespace.target();
    let first = Hold::new(Kind::Evidence, b"retained evidence owner").unwrap();
    let second = Hold::new(Kind::Runtime, b"independent runtime owner").unwrap();
    retention::set_hold(&association, &target, first, HoldAction::Acquire).unwrap();
    retention::set_hold(&association, &target, second, HoldAction::Acquire).unwrap();
    drop(namespace);
    fs::write(root.path().join("observation"), "\"retired\"\n").unwrap();
    {
        let fence = Fence::open(&association).unwrap();
        let (key, mut record) = fence
            .database
            .records::<Record>(RecordKind::CargoTarget)
            .unwrap()
            .pop()
            .unwrap();
        record.holds.clear();
        fence
            .database
            .put_record(RecordKind::CargoTarget, &key, &record)
            .unwrap();
    }
    retention::sweep(&association, &pressure(), &MockClock::new()).unwrap();
    assert!(target.exists());
    retention::set_hold(&association, &target, first, HoldAction::Release).unwrap();
    retention::sweep(&association, &pressure(), &MockClock::new()).unwrap();
    assert!(target.exists());
    retention::set_hold(&association, &target, second, HoldAction::Release).unwrap();
    retention::sweep(&association, &pressure(), &MockClock::new()).unwrap();
    assert!(!target.exists());
}

#[test]
fn cargo_retention_unavailable_replaced_and_replayed_issuers_retain_output() {
    for response in ["invalid", "\"protected\"", "\"unknown\""] {
        let (root, association, source, profile) = fixture();
        let issuer = issuer(root.path(), &profile);
        let namespace = allocate(&association, &source, &profile);
        namespace
            .configure(&mut spawn(&source, &profile, &issuer))
            .unwrap();
        let target = namespace.target();
        drop(namespace);
        fs::write(root.path().join("observation"), response).unwrap();
        retention::sweep(&association, &pressure(), &MockClock::new()).unwrap();
        assert!(target.exists());
        fs::rename(&issuer, issuer.with_extension("old")).unwrap();
        fs::write(
            &issuer,
            "#!/usr/bin/env bash\nset -euo pipefail\nprintf '\"retired\"'\n",
        )
        .unwrap();
        fs::set_permissions(&issuer, fs::Permissions::from_mode(0o700)).unwrap();
        retention::sweep(&association, &pressure(), &MockClock::new()).unwrap();
        assert!(target.exists());
    }
}

#[test]
fn cargo_retention_replayed_preparation_cannot_erase_completed_generation_holds() {
    let (root, association, source, profile) = fixture();
    let issuer = issuer(root.path(), &profile);
    let namespace = allocate(&association, &source, &profile);
    let prepared = {
        let fence = Fence::open(&association).unwrap();
        fence
            .database
            .records::<Record>(RecordKind::CargoTarget)
            .unwrap()
            .pop()
            .unwrap()
    };
    namespace
        .configure(&mut spawn(&source, &profile, &issuer))
        .unwrap();
    let target = namespace.target();
    let hold = Hold::new(Kind::Operator, b"independent operator pin").unwrap();
    retention::set_hold(&association, &target, hold, HoldAction::Acquire).unwrap();
    drop(namespace);
    fs::write(root.path().join("observation"), "\"retired\"\n").unwrap();
    {
        let fence = Fence::open(&association).unwrap();
        fence
            .database
            .put_record(RecordKind::CargoTarget, &prepared.0, &prepared.1)
            .unwrap();
    }
    retention::sweep(&association, &pressure(), &MockClock::new()).unwrap();
    assert!(target.exists());
}

#[test]
fn cargo_retention_interrupted_unlink_preserves_independent_hold_authority() {
    let (root, association, worker, profile) = fixture();
    let issuer = issuer(root.path(), &profile);
    let namespace = allocate(&association, &worker, &profile);
    namespace
        .configure(&mut spawn(&worker, &profile, &issuer))
        .unwrap();
    let target = namespace.target();
    let hold = Hold::new(Kind::Runtime, b"canonical runtime").unwrap();
    let original = namespace.path.clone();
    fs::write(namespace.target().join("evidence"), "keep").unwrap();
    drop(namespace);
    fs::write(root.path().join("observation"), "\"retired\"\n").unwrap();
    let fence = Fence::open(&association).unwrap();
    let (key, mut record) = fence
        .database
        .records::<Record>(RecordKind::CargoTarget)
        .unwrap()
        .remove(0);
    let directory = Directory::open(&original).unwrap();
    assert!(retire(&directory, &record).unwrap());
    preserve(&directory, &record).unwrap();
    let (_, quarantine) = fence.paths(&key, &record).unwrap();
    fs::rename(&original, &quarantine).unwrap();
    record.placement = Placement::Removing;
    fence
        .database
        .put_record(RecordKind::CargoTarget, &key, &record)
        .unwrap();
    fs::remove_file(quarantine.join(retention::PROJECTION)).unwrap();
    drop(fence);
    retention::set_hold(&association, &target, hold, HoldAction::Acquire).unwrap();
    let fence = Fence::open(&association).unwrap();
    record.version = 2;
    record.lifetime = Lifetime::Prepared;
    record.holds.clear();
    fence
        .database
        .put_record(RecordKind::CargoTarget, &key, &record)
        .unwrap();
    drop(fence);
    assert!(
        retention::set_hold(
            &association,
            &target,
            Hold::new(Kind::Evidence, b"new owner").unwrap(),
            HoldAction::Acquire
        )
        .is_err(),
        "a cache downgrade must not acknowledge an undurable new hold"
    );
    retention::sweep(&association, &pressure(), &MockClock::new()).unwrap();
    assert!(
        quarantine.join("target/evidence").exists(),
        "cache downgrade cannot bypass the surviving private receipt"
    );
    let fence = Fence::open(&association).unwrap();
    record.version = 3;
    record.lifetime = Lifetime::Dispatched;
    fence
        .database
        .put_record(RecordKind::CargoTarget, &key, &record)
        .unwrap();
    drop(fence);
    retention::sweep(&association, &pressure(), &MockClock::new()).unwrap();
    assert!(quarantine.join("target/evidence").exists());
    retention::set_hold(&association, &target, hold, HoldAction::Release).unwrap();
    retention::sweep(&association, &pressure(), &MockClock::new()).unwrap();
    assert!(!quarantine.exists());
    assert!(worker.join("Cargo.toml").exists());
    assert!(!journal_exists(&record).unwrap());
}

#[test]
#[ignore = "requires Linux delegated cgroup2 and pinned native entry helpers"]
fn native_cargo_lifetime_seals_empty_generation_and_cancels_stale_entry() {
    if std::env::var_os("LOOM_NATIVE_LIFETIME_FIXTURE_CHILD").is_none() {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "workspace::cargo::lifetime::tests::native_cargo_lifetime_seals_empty_generation_and_cancels_stale_entry", "--ignored", "--nocapture"])
            .env("LOOM_NATIVE_LIFETIME_FIXTURE_CHILD", "1");
        for (key, _) in
            std::env::vars_os().filter(|(key, _)| key.as_encoded_bytes().starts_with(b"LD_"))
        {
            command.env_remove(key);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    assert!(
        native_root().unwrap().is_some(),
        "native containment unavailable: flock={} shell={} stat={} nix={} worker={} group={}",
        option_env!("LOOM_NATIVE_FLOCK_BIN").is_some(),
        option_env!("LOOM_NATIVE_SHELL_BIN").is_some(),
        option_env!("LOOM_NATIVE_STAT_BIN").is_some(),
        std::env::var_os("NIX_BUILD_TOP").is_some(),
        std::env::var_os(crate::agent::LOOM_INSIDE_ENV).is_some(),
        std::fs::read_to_string("/proc/self/cgroup").unwrap()
    );
    let (root, association, source, _) = fixture();
    let output = root.path().join("native");
    let mut command = Command::new("sh");
    command.args(["-c", "printf completed > \"$CARGO_TARGET_DIR/output\""]);
    let guard = trusted_command_at(&mut command, &source, &output).unwrap();
    let target = PathBuf::from(
        command
            .get_envs()
            .find(|(name, _)| *name == "CARGO_TARGET_DIR")
            .unwrap()
            .1
            .unwrap(),
    );
    assert!(command.status().unwrap().success());
    assert!(target.join("output").exists());
    Fence::trusted(&output)
        .unwrap()
        .collect(&pressure(), Bytes::default(), &MockClock::new())
        .unwrap();
    assert!(
        target.exists(),
        "pending execution lease is independently protective"
    );
    drop(guard);
    let mut next = Command::new("sh");
    next.args(["-c", "printf next > \"$CARGO_TARGET_DIR/next\""]);
    let next_guard = trusted_command_at(&mut next, &source, &output).unwrap();
    assert_eq!(
        next.get_envs()
            .find(|(name, _)| *name == "CARGO_TARGET_DIR")
            .unwrap()
            .1
            .unwrap(),
        target.as_os_str()
    );
    assert!(
        !command.status().unwrap().success(),
        "old immutable entry must fail even after the same target is rearmed"
    );
    assert!(next.status().unwrap().success());
    assert!(target.join("next").exists());
    drop(next_guard);
    Fence::trusted(&output)
        .unwrap()
        .collect(&pressure(), Bytes::default(), &MockClock::new())
        .unwrap();
    assert!(!target.exists());
    assert!(source.join("Cargo.toml").exists());
    assert!(
        !command.status().unwrap().success(),
        "sealed stale commands must not start or recreate output"
    );
    assert!(!target.exists());
    assert!(association.integration().exists());
}
