#[cfg(test)]
pub(super) mod cases {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use super::super::*;
    use crate::agent::SpawnConfig;
    use crate::clock::MockClock;
    use crate::config::CargoTargetReuse;
    use crate::workspace::cargo::{Namespace, prepare, tests::cases::fixture};

    pub(in crate::workspace::cargo::retention) fn policy() -> Policy {
        Policy {
            max_bytes: Bytes::new(8 * 1024 * 1024),
            reservation_bytes: Bytes::new(128 * 1024),
            min_free_bytes: Bytes::default(),
            ..Policy::default()
        }
    }

    pub(in crate::workspace::cargo::retention) fn spawn(workspace: &Path) -> SpawnConfig {
        serde_json::from_value(serde_json::json!({
            "image_ref":"fixture", "workspace":workspace, "env":[],
            "initial_prompt":"fixture", "agent_args":[],
            "repin":{"orientation":"fixture", "pinned_context":"fixture", "partial_bodies":[]}
        }))
        .unwrap()
    }

    pub(in crate::workspace::cargo::retention) fn allocate(
        association: &Association,
        worker: &Path,
        profile: &Path,
        policy: &Policy,
    ) -> Namespace {
        prepare(
            association,
            worker,
            profile,
            CargoTargetReuse::Off,
            policy.default_request(),
            &MockClock::new(),
        )
        .unwrap()
    }

    #[test]
    fn cargo_retention_bounds_protect_independent_work() {
        let (_root, association, worker, profile) = fixture();
        let mut policy = policy();
        let first = allocate(&association, &worker, &profile, &policy);
        let first_target = first.target();
        first.configure(&mut spawn(&worker)).unwrap();
        fs::write(first_target.join("compiler-output"), vec![7; 32 * 1024]).unwrap();
        fs::create_dir(worker.join(".wrix")).unwrap();
        fs::write(
            worker.join(".wrix/completed"),
            "closed; inactive; disposable",
        )
        .unwrap();
        drop(first);
        let report = sweep(&association, &policy, &MockClock::new()).unwrap();
        assert!(report.allocated.get() >= 32 * 1024);
        assert!(report.charged >= policy.reservation_bytes);
        assert_eq!(
            report.charged,
            report.allocated.add(report.outstanding).unwrap()
        );
        policy.max_bytes = report.charged.add(Bytes::new(64 * 1024)).unwrap();
        assert!(matches!(
            prepare(
                &association,
                &worker,
                &profile,
                CargoTargetReuse::Off,
                policy.default_request(),
                &MockClock::new()
            ),
            Err(Error::CargoAdmission { .. })
        ));
        assert!(first_target.join("compiler-output").exists());
        policy.reservation_bytes = Bytes::new(32 * 1024);
        let independent = allocate(&association, &worker, &profile, &policy);
        assert!(independent.target().is_dir());
        let report = sweep(&association, &policy, &MockClock::new()).unwrap();
        assert!(report.charged <= policy.max_bytes);
        assert!(first_target.join("compiler-output").exists());
    }

    #[test]
    fn cargo_retention_rechecks_materialized_seed_growth_before_worker_projection() {
        let (_root, association, worker, profile) = fixture();
        fs::write(
            worker.join("loom.toml"),
            "[loom]\ncargo_target_reuse='allow-hardlinks'\n",
        )
        .unwrap();
        fs::write(worker.join("Cargo.lock"), "version=4\n[[package]]\nname='fixture'\nversion='0.1.0'\n[[package]]\nname='dep'\nversion='1.0.0'\nsource='registry+https://example.invalid/index'\n").unwrap();
        let mut policy = Policy {
            reservation_bytes: Bytes::new(64 * 1024),
            ..policy()
        };
        let donor = prepare(
            &association,
            &worker,
            &profile,
            CargoTargetReuse::AllowHardlinks,
            policy.default_request(),
            &MockClock::new(),
        )
        .unwrap();
        fs::create_dir_all(donor.target().join("debug/deps")).unwrap();
        fs::create_dir_all(
            donor
                .target()
                .join("debug/.fingerprint/dep-aaaaaaaaaaaaaaaa"),
        )
        .unwrap();
        fs::write(
            donor
                .target()
                .join("debug/.fingerprint/dep-aaaaaaaaaaaaaaaa/lib-dep.json"),
            "{}",
        )
        .unwrap();
        fs::write(
            donor
                .target()
                .join("debug/deps/libdep-aaaaaaaaaaaaaaaa.rlib"),
            vec![5; 512 * 1024],
        )
        .unwrap();
        fs::File::create_new(donor.target().join("debug/.cargo-lock")).unwrap();
        let before = sweep(&association, &policy, &MockClock::new()).unwrap();
        policy.max_bytes = before.charged.add(Bytes::new(128 * 1024)).unwrap();
        let rejected = prepare(
            &association,
            &worker,
            &profile,
            CargoTargetReuse::AllowHardlinks,
            policy.default_request(),
            &MockClock::new(),
        );
        match rejected {
            Err(error) => assert!(matches!(error, Error::CargoAdmission { .. }), "{error:#?}"),
            Ok(namespace) => panic!(
                "oversized dependency seed admitted at {:?}",
                namespace.target()
            ),
        }
        let database = CacheDb::open(crate::state::path(association.operator())).unwrap();
        let records = database.records::<Record>(RecordKind::CargoTarget).unwrap();
        let (_, failed) = records
            .iter()
            .find(|(_, record)| record.target != donor.target())
            .unwrap();
        assert!(failed.lifetime == Lifetime::Prepared);
        assert_eq!(
            fs::metadata(
                failed
                    .target
                    .join("debug/deps/libdep-aaaaaaaaaaaaaaaa.rlib")
            )
            .unwrap()
            .len(),
            512 * 1024
        );
        let failed_target = failed.target.clone();
        drop(database);
        let independent = allocate(&association, &worker, &profile, &policy);
        assert!(independent.target().is_dir());
        assert!(!failed_target.exists());
        assert!(
            donor
                .target()
                .join("debug/deps/libdep-aaaaaaaaaaaaaaaa.rlib")
                .is_file()
        );
    }

    #[test]
    fn cargo_retention_age_pressure_and_native_holds_never_override_lifetimes() {
        let (_root, association, worker, profile) = fixture();
        let policy = Policy {
            max_age_secs: 0,
            ..policy()
        };
        fs::create_dir(worker.join("target")).unwrap();
        let protected = [
            worker.join("target/legacy"),
            worker.join(".git/HEAD"),
            association.operator().join("canonical-runtime"),
            association.operator().join(".loom/evidence"),
        ];
        for path in &protected {
            fs::write(path, "preserved").unwrap();
        }
        let active = allocate(&association, &worker, &profile, &policy);
        let active_target = active.target();
        let held = allocate(&association, &worker, &profile, &policy);
        let held_target = held.target();
        for hold in [
            hold(Kind::Operator),
            hold(Kind::Recovery),
            hold(Kind::Evidence),
            hold(Kind::Runtime),
        ] {
            set_hold(&association, &held_target, hold, HoldAction::Acquire).unwrap();
        }
        drop(held);
        let dispatched = allocate(&association, &worker, &profile, &policy);
        let dispatched_target = dispatched.target();
        dispatched.configure(&mut spawn(&worker)).unwrap();
        drop(dispatched);
        let abandoned = allocate(&association, &worker, &profile, &policy);
        let abandoned_target = abandoned.target();
        drop(abandoned);
        sweep(&association, &policy, &MockClock::new()).unwrap();
        assert!(!abandoned_target.exists());
        for path in [&active_target, &held_target, &dispatched_target] {
            assert!(path.is_dir());
        }
        for hold in [
            hold(Kind::Operator),
            hold(Kind::Recovery),
            hold(Kind::Evidence),
        ] {
            set_hold(&association, &held_target, hold, HoldAction::Release).unwrap();
        }
        sweep(&association, &policy, &MockClock::new()).unwrap();
        assert!(held_target.is_dir());
        set_hold(
            &association,
            &held_target,
            hold(Kind::Runtime),
            HoldAction::Release,
        )
        .unwrap();
        sweep(&association, &policy, &MockClock::new()).unwrap();
        assert!(!held_target.exists());
        drop(active);
        sweep(&association, &policy, &MockClock::new()).unwrap();
        assert!(!active_target.exists());
        assert!(dispatched_target.is_dir());
        for path in &protected {
            assert_eq!(fs::read_to_string(path).unwrap(), "preserved");
        }
    }

    #[test]
    fn cargo_retention_free_space_rejection_and_accounting_errors_are_not_zero_usage() {
        let (_root, association, worker, profile) = fixture();
        let unavailable = Policy {
            min_free_bytes: Bytes::new(u64::MAX - 128 * 1024),
            ..policy()
        };
        assert!(matches!(
            prepare(
                &association,
                &worker,
                &profile,
                CargoTargetReuse::Off,
                unavailable.default_request(),
                &MockClock::new()
            ),
            Err(Error::CargoAdmission { .. })
        ));
        let policy = policy();
        let namespace = allocate(&association, &worker, &profile, &policy);
        let original = namespace.target().parent().unwrap().to_owned();
        drop(namespace);
        let displaced = original.with_extension("displaced");
        fs::rename(&original, &displaced).unwrap();
        fs::create_dir(&original).unwrap();
        assert!(sweep(&association, &policy, &MockClock::new()).is_err());
        assert!(
            prepare(
                &association,
                &worker,
                &profile,
                CargoTargetReuse::Off,
                policy.default_request(),
                &MockClock::new()
            )
            .is_err()
        );
        assert!(original.is_dir());
        assert!(displaced.join("target").is_dir());
    }

    #[test]
    fn cargo_retention_symlink_substitution_and_unrecorded_generations_are_retained() {
        use std::os::unix::fs::symlink;
        let (root, association, worker, profile) = fixture();
        let policy = Policy {
            max_age_secs: 0,
            ..policy()
        };
        let namespace = allocate(&association, &worker, &profile, &policy);
        let target = namespace.target();
        let external = root.path().join("protected");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("source"), "protected source").unwrap();
        fs::rename(&target, target.with_file_name("preserved-target")).unwrap();
        symlink(&external, &target).unwrap();
        drop(namespace);
        assert!(sweep(&association, &policy, &MockClock::new()).is_err());
        assert_eq!(
            fs::read_to_string(external.join("source")).unwrap(),
            "protected source"
        );
        fs::remove_file(&target).unwrap();
        fs::rename(target.with_file_name("preserved-target"), &target).unwrap();
        let fence = Fence::open(&association).unwrap();
        let (key, _) = fence
            .database
            .records::<Record>(RecordKind::CargoTarget)
            .unwrap()
            .pop()
            .unwrap();
        fence
            .database
            .remove_record(RecordKind::CargoTarget, &key)
            .unwrap();
        drop(fence);
        let report = sweep(&association, &policy, &MockClock::new()).unwrap();
        assert!(report.allocated.get() > 0);
        assert!(report.charged >= policy.reservation_bytes);
        assert!(target.is_dir());
    }

    #[test]
    fn cargo_retention_legacy_and_conflicting_records_cannot_authorize_disposal() {
        let (_root, association, worker, profile) = fixture();
        let policy = Policy {
            max_age_secs: 0,
            ..policy()
        };
        let namespace = allocate(&association, &worker, &profile, &policy);
        let target = namespace.target();
        let database = CacheDb::open(crate::state::path(association.operator())).unwrap();
        let (key, prepared) = database
            .records::<Record>(RecordKind::CargoTarget)
            .unwrap()
            .pop()
            .unwrap();
        namespace.configure(&mut spawn(&worker)).unwrap();
        drop(namespace);
        let mut legacy = serde_json::to_value(&prepared).unwrap();
        legacy["version"] = serde_json::json!(1);
        for field in [
            "lifetime",
            "placement",
            "created",
            "reservation",
            "lease_identity",
            "target_identity",
            "holds",
        ] {
            legacy.as_object_mut().unwrap().remove(field);
        }
        database
            .put_record(RecordKind::CargoTarget, &key, &legacy)
            .unwrap();
        sweep(&association, &policy, &MockClock::new()).unwrap();
        assert!(target.is_dir());
        let mut dispatched = prepared;
        dispatched.lifetime = Lifetime::Dispatched;
        database
            .put_record(RecordKind::CargoTarget, &key, &dispatched)
            .unwrap();
        let mut stale = serde_json::to_value(&dispatched).unwrap();
        stale["lifetime"] = serde_json::json!("prepared");
        database
            .put_record(RecordKind::CargoTarget, &format!("./{key}"), &stale)
            .unwrap();
        assert!(sweep(&association, &policy, &MockClock::new()).is_err());
        assert!(target.is_dir());
    }

    #[test]
    fn cargo_retention_untrusted_controller_files_never_grant_disposal_authority() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let (_root, association, worker, profile) = fixture();
        let policy = Policy {
            max_age_secs: 0,
            ..policy()
        };
        let namespace = allocate(&association, &worker, &profile, &policy);
        let target = namespace.target();
        drop(namespace);
        let database = crate::state::path(association.operator());
        let saved = database.with_file_name("preserved-state.db");
        fs::rename(&database, &saved).unwrap();
        symlink(&saved, &database).unwrap();
        assert!(sweep(&association, &policy, &MockClock::new()).is_err());
        assert!(target.is_dir());
        fs::remove_file(&database).unwrap();
        fs::rename(&saved, &database).unwrap();
        fs::set_permissions(&database, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(sweep(&association, &policy, &MockClock::new()).is_err());
        assert!(target.is_dir());
        fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
        let alias = worker.join("state-alias");
        fs::hard_link(&database, &alias).unwrap();
        assert!(sweep(&association, &policy, &MockClock::new()).is_err());
        assert!(target.is_dir());
        fs::remove_file(alias).unwrap();
        sweep(&association, &policy, &MockClock::new()).unwrap();
        assert!(!target.exists());
    }

    #[test]
    fn cargo_retention_projection_rejects_another_worker_or_profile_before_dispatch() {
        let (root, association, worker, profile) = fixture();
        let namespace = allocate(&association, &worker, &profile, &policy());
        let mut other = spawn(&root.path().join("other-worker"));
        assert!(namespace.configure(&mut other).is_err());
        assert_eq!(other.mounts, Vec::<crate::agent::MountSpec>::new());
        let mut wrong_profile = spawn(&worker);
        wrong_profile.profile_config = Some(root.path().join("other-profile.json"));
        assert!(namespace.configure(&mut wrong_profile).is_err());
        assert_eq!(wrong_profile.mounts, Vec::<crate::agent::MountSpec>::new());
        namespace.configure(&mut spawn(&worker)).unwrap();
    }

    #[test]
    fn cargo_retention_cache_replay_cannot_erase_projection_tombstones() {
        let (_root, association, worker, profile) = fixture();
        let namespace = allocate(&association, &worker, &profile, &policy());
        let database_path = crate::state::path(association.operator());
        let database = CacheDb::open(&database_path).unwrap();
        let (key, prepared) = database
            .records::<Record>(RecordKind::CargoTarget)
            .unwrap()
            .pop()
            .unwrap();
        drop(database);
        namespace.configure(&mut spawn(&worker)).unwrap();
        let target = namespace.target();
        Directory::open(&namespace.path)
            .unwrap()
            .read_private_file(Path::new(PROJECTION))
            .unwrap();
        drop(namespace);
        let database = CacheDb::open(&database_path).unwrap();
        database
            .put_record(RecordKind::CargoTarget, &key, &prepared)
            .unwrap();
        drop(database);
        let pressure = Policy {
            max_bytes: Bytes::default(),
            max_age_secs: 0,
            ..policy()
        };
        sweep(&association, &pressure, &MockClock::new()).unwrap();
        assert!(target.is_dir());
        CacheDb::recreate(&database_path).unwrap();
        sweep(&association, &pressure, &MockClock::new()).unwrap();
        assert!(target.is_dir());
    }

    #[test]
    fn cargo_retention_allocation_is_fenced_and_reservations_survive_reopening() {
        let (_root, association, worker, profile) = fixture();
        let policy = policy();
        let first = allocate(&association, &worker, &profile, &policy);
        let fence = Fence::open(&association).unwrap();
        assert!(
            prepare(
                &association,
                &worker,
                &profile,
                CargoTargetReuse::Off,
                policy.default_request(),
                &MockClock::new()
            )
            .is_err()
        );
        drop(fence);
        first.configure(&mut spawn(&worker)).unwrap();
        drop(first);
        let reopened = Association::resolve(association.operator()).unwrap();
        let report = sweep(&reopened, &policy, &MockClock::new()).unwrap();
        assert!(report.outstanding.get() > 0);
        assert!(report.charged >= policy.reservation_bytes);
    }

    #[test]
    fn cargo_retention_hardlinked_files_are_counted_once_within_a_namespace() {
        use std::os::unix::fs::MetadataExt as _;
        let (_root, association, worker, profile) = fixture();
        let namespace = allocate(&association, &worker, &profile, &policy());
        fs::write(namespace.target().join("original"), vec![3; 8192]).unwrap();
        fs::hard_link(
            namespace.target().join("original"),
            namespace.target().join("link"),
        )
        .unwrap();
        let directory = Directory::open(&namespace.target()).unwrap();
        let used = directory.allocated_bytes().unwrap();
        let metadata = fs::metadata(namespace.target()).unwrap().blocks() * 512;
        let file = fs::metadata(namespace.target().join("original"))
            .unwrap()
            .blocks()
            * 512;
        assert_eq!(used, metadata + file);
    }

    #[test]
    fn cargo_retention_nested_symlinks_are_not_followed_or_charged_as_external_data() {
        use std::os::unix::fs::symlink;
        let (root, association, worker, profile) = fixture();
        let policy = Policy {
            max_age_secs: 0,
            ..policy()
        };
        let namespace = allocate(&association, &worker, &profile, &policy);
        let external = root.path().join("external");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("source"), vec![5; 2 * 1024 * 1024]).unwrap();
        symlink(&external, namespace.target().join("link")).unwrap();
        let used = Directory::open(&namespace.target())
            .unwrap()
            .allocated_bytes()
            .unwrap();
        assert!(used < 2 * 1024 * 1024);
        drop(namespace);
        sweep(&association, &policy, &MockClock::new()).unwrap();
        assert_eq!(
            fs::metadata(external.join("source")).unwrap().len(),
            2 * 1024 * 1024
        );
    }

    fn hold(kind: Kind) -> Hold {
        Hold::new(kind, b"native-fixture-owner").unwrap()
    }

    #[test]
    fn cargo_retention_independent_pin_owners_cannot_release_each_other() {
        let (_root, association, worker, profile) = fixture();
        let policy = Policy {
            max_age_secs: 0,
            ..policy()
        };
        let namespace = allocate(&association, &worker, &profile, &policy);
        let target = namespace.target();
        let first = Hold::new(Kind::Evidence, b"certificate-one").unwrap();
        let second = Hold::new(Kind::Evidence, b"certificate-two").unwrap();
        set_hold(&association, &target, first, HoldAction::Acquire).unwrap();
        set_hold(&association, &target, second, HoldAction::Acquire).unwrap();
        drop(namespace);
        set_hold(&association, &target, first, HoldAction::Release).unwrap();
        sweep(&association, &policy, &MockClock::new()).unwrap();
        assert!(target.is_dir());
        set_hold(&association, &target, second, HoldAction::Release).unwrap();
        sweep(&association, &policy, &MockClock::new()).unwrap();
        assert!(!target.exists());
        assert!(Hold::new(Kind::Evidence, b"").is_err());
    }

    #[test]
    fn cargo_retention_configuration_rejects_unknown_and_negative_bounds() {
        let config: crate::config::LoomTopConfig = toml::from_str("[cargo_retention]\nmax_bytes=4096\nreservation_bytes=1024\nmin_free_bytes=0\nmax_age_secs=0\n").unwrap();
        assert_eq!(config.cargo_retention.max_bytes, Bytes::new(4096));
        assert!(toml::from_str::<Policy>("max_bytes=-1").is_err());
        assert!(toml::from_str::<Policy>("max_btyes=4").is_err());
        let per_profile: Policy = toml::from_str("[profile_reservations]\nrust=8192\n").unwrap();
        assert_eq!(
            per_profile
                .request(&crate::identifier::ProfileName::new("rust").unwrap())
                .reservation,
            Bytes::new(8192)
        );
        assert_eq!(
            per_profile
                .request(&crate::identifier::ProfileName::new("base").unwrap())
                .reservation,
            Policy::default().reservation_bytes
        );
        assert!(toml::from_str::<Policy>("[profile_reservations]\n'bad name'=4\n").is_err());
        assert!(Bytes::new(u64::MAX).add(Bytes::new(1)).is_err());
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(in crate::workspace::cargo::retention) enum Checkpoint {
        Intent,
        Renamed,
        Removing,
        Unlinked,
    }

    pub(in crate::workspace::cargo::retention) fn checkpoint(at: Checkpoint) -> Result<(), Error> {
        let Some(requested) = std::env::var_os("LOOM_RETENTION_TEST_STOP_AT") else {
            return Ok(());
        };
        if requested == format!("{at:?}").as_str() {
            rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::STOP)
                .map_err(std::io::Error::from)?;
            // Group-stop delivery can lag on another libtest thread; never advance past the seam.
            loop {
                std::thread::park();
            }
        }
        Ok(())
    }

    #[test]
    fn cargo_retention_fixture_child() {
        let Some(operator) = std::env::var_os("LOOM_RETENTION_TEST_OPERATOR") else {
            return;
        };
        let association = Association::resolve(Path::new(&operator)).unwrap();
        let policy = Policy {
            max_age_secs: 0,
            ..policy()
        };
        sweep(&association, &policy, &MockClock::new()).unwrap();
    }

    #[test]
    fn cargo_retention_quarantine_rechecks_ownership_and_resumes_sigkill() {
        use rustix::process::{Pid, Signal, WaitOptions, kill_process, waitpid};
        for at in [
            Checkpoint::Intent,
            Checkpoint::Renamed,
            Checkpoint::Removing,
            Checkpoint::Unlinked,
        ] {
            let (_root, association, worker, profile) = fixture();
            let policy = Policy {
                max_age_secs: 0,
                ..policy()
            };
            let namespace = allocate(&association, &worker, &profile, &policy);
            let original = namespace.target().parent().unwrap().to_owned();
            fs::write(namespace.target().join("output"), vec![1; 8192]).unwrap();
            fs::write(worker.join(".git/HEAD"), "preserved").unwrap();
            drop(namespace);
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "workspace::cargo::retention::tests::cases::cargo_retention_fixture_child",
                    "--nocapture",
                ])
                .env("LOOM_RETENTION_TEST_OPERATOR", association.operator())
                .env("LOOM_RETENTION_TEST_STOP_AT", format!("{at:?}"))
                .spawn()
                .unwrap();
            let pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
            let (observed, status) = waitpid(Some(pid), WaitOptions::UNTRACED).unwrap().unwrap();
            assert_eq!(observed, pid);
            assert_eq!(status.stopping_signal(), Some(Signal::STOP.as_raw()));
            kill_process(pid, Signal::KILL).unwrap();
            assert!(!child.wait().unwrap().success());
            let fence = Fence::open(&association).unwrap();
            let (key, record) = fence
                .database
                .records::<Record>(RecordKind::CargoTarget)
                .unwrap()
                .pop()
                .unwrap();
            assert!(record.placement != Placement::Live);
            let (_, quarantine) = fence.paths(&key, &record).unwrap();
            drop(fence);
            if at != Checkpoint::Unlinked {
                set_hold(
                    &association,
                    &original.join("target"),
                    hold(Kind::Evidence),
                    HoldAction::Acquire,
                )
                .unwrap();
                sweep(&association, &policy, &MockClock::new()).unwrap();
                assert!(original.exists() || quarantine.exists());
                set_hold(
                    &association,
                    &original.join("target"),
                    hold(Kind::Evidence),
                    HoldAction::Release,
                )
                .unwrap();
            }
            sweep(&association, &policy, &MockClock::new()).unwrap();
            assert!(!original.exists());
            assert!(!quarantine.exists());
            assert_eq!(
                fs::read_to_string(worker.join(".git/HEAD")).unwrap(),
                "preserved"
            );
            let fence = Fence::open(&association).unwrap();
            assert!(
                fence
                    .database
                    .records::<Record>(RecordKind::CargoTarget)
                    .unwrap()
                    .is_empty()
            );
        }
    }
}
