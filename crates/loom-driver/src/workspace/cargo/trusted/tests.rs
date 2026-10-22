#[cfg(test)]
mod cases {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    use super::super::*;
    use crate::clock::MockClock;
    use crate::workspace::cargo::retention::{Bytes, Hold, HoldAction, Kind};

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Policy) {
        let root = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
        let output = root.path().join("trusted-cargo");
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        let policy = Policy {
            max_bytes: Bytes::new(64 * 1024 * 1024),
            reservation_bytes: Bytes::new(4 * 1024 * 1024),
            min_free_bytes: Bytes::default(),
            max_age_secs: 0,
            ..Policy::default()
        };
        configure(&output, &policy);
        (root, output, source, policy)
    }

    fn configure(output: &Path, policy: &Policy) {
        let fence = Fence::trusted(output).unwrap();
        let mut file = fence.root.append_private_file(Path::new(POLICY)).unwrap();
        file.set_len(0).unwrap();
        serde_json::to_writer(&mut file, policy).unwrap();
        file.sync_all().unwrap();
        fence.root.sync().unwrap();
    }

    #[test]
    fn trusted_cargo_retention_initializes_private_defaults_without_worker_policy() {
        let root = tempfile::tempdir_in(fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
        let output = root.path().join("trusted-cargo");
        let fence = Fence::trusted(&output).unwrap();
        assert_eq!(
            super::super::policy(&fence.root).unwrap(),
            Policy::default()
        );
        assert_eq!(
            fs::metadata(output.join(POLICY))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn trusted_cargo_retention_shares_one_budget_across_controlling_workspaces() {
        let (root, output, first, mut policy) = fixture();
        let second = root.path().join("another-operator");
        fs::create_dir(&second).unwrap();
        fs::write(
            first.join("loom.toml"),
            "[loom.cargo_retention]\nmax_bytes=0\n",
        )
        .unwrap();
        let clock = MockClock::new();
        let target = prepare(&output, &first, &clock).unwrap();
        fs::write(target.join("retained"), b"first workspace output").unwrap();
        let charged = {
            let fence = Fence::trusted(&output).unwrap();
            let records = fence
                .database
                .records::<Record>(RecordKind::CargoTarget)
                .unwrap();
            assert_eq!(records.len(), 1);
            assert!(records[0].1.lifetime == Lifetime::Dispatched);
            assert!(records[0].1.bead.is_none());
            fence.report(&policy).unwrap().charged.get()
        };
        policy.max_bytes = Bytes::new(charged + policy.reservation_bytes.get() / 2);
        configure(&output, &policy);
        assert!(matches!(
            prepare(&output, &second, &clock),
            Err(Error::CargoAdmission { .. })
        ));
        assert_eq!(prepare(&output, &first, &clock).unwrap(), target);
        assert_eq!(
            fs::read(target.join("retained")).unwrap(),
            b"first workspace output"
        );
        let fence = Fence::trusted(&output).unwrap();
        assert_eq!(
            fence
                .database
                .records::<Record>(RecordKind::CargoTarget)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn trusted_cargo_retention_charges_legacy_and_unrecorded_output_without_adopting_it() {
        let (_root, output, source, policy) = fixture();
        let clock = MockClock::new();
        let target = prepare(&output, &source, &clock).unwrap();
        let before = {
            let fence = Fence::trusted(&output).unwrap();
            let records = fence
                .database
                .records::<Record>(RecordKind::CargoTarget)
                .unwrap();
            fence
                .database
                .remove_record(RecordKind::CargoTarget, &records[0].0)
                .unwrap();
            fence.report(&policy).unwrap()
        };
        let legacy = output.join("old-context");
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("canonical-sentinel"), vec![3; 1024 * 1024]).unwrap();
        let next = prepare(&output, &source, &clock).unwrap();
        assert_ne!(next, target);
        let fence = Fence::trusted(&output).unwrap();
        let after = fence.report(&policy).unwrap();
        assert!(after.allocated.get() >= before.allocated.get() + 1024 * 1024);
        assert!(after.outstanding.get() >= policy.reservation_bytes.get());
        assert!(target.exists());
        assert_eq!(
            fs::read(legacy.join("canonical-sentinel")).unwrap(),
            vec![3; 1024 * 1024]
        );
    }

    #[test]
    fn trusted_cargo_retention_replay_cannot_erase_dispatch_or_independent_holds() {
        let (_root, output, source, policy) = fixture();
        let clock = MockClock::new();
        let target = prepare(&output, &source, &clock).unwrap();
        let first = Hold::new(Kind::Evidence, b"first retained evidence").unwrap();
        let second = Hold::new(Kind::Runtime, b"independent installed runtime").unwrap();
        {
            let fence = Fence::trusted(&output).unwrap();
            fence
                .change_hold(&target, first, HoldAction::Acquire)
                .unwrap();
            fence
                .change_hold(&target, second, HoldAction::Acquire)
                .unwrap();
            fence
                .change_hold(&target, first, HoldAction::Release)
                .unwrap();
            let (key, mut record) = fence
                .database
                .records::<Record>(RecordKind::CargoTarget)
                .unwrap()
                .pop()
                .unwrap();
            assert_eq!(record.holds, BTreeSet::from([second]));
            record.holds.clear();
            record.lifetime = Lifetime::Prepared;
            fence
                .database
                .put_record(RecordKind::CargoTarget, &key, &record)
                .unwrap();
            fence.admit(&policy, Bytes::default(), &clock).unwrap();
        }
        assert!(target.exists());
        assert!(
            target
                .parent()
                .unwrap()
                .join(retention::PROJECTION)
                .exists()
        );
        let next = prepare(&output, &source, &clock).unwrap();
        assert_ne!(next, target);
        assert!(target.exists());
    }

    #[test]
    fn trusted_cargo_retention_refuses_policy_and_target_substitution() {
        let (root, output, source, policy) = fixture();
        let clock = MockClock::new();
        let target = prepare(&output, &source, &clock).unwrap();
        let saved = root.path().join("saved-target");
        fs::rename(&target, &saved).unwrap();
        symlink(&saved, &target).unwrap();
        assert!(prepare(&output, &source, &clock).is_err());
        fs::remove_file(&target).unwrap();
        fs::rename(&saved, &target).unwrap();
        let policy_path = output.join(POLICY);
        fs::set_permissions(&policy_path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(prepare(&output, &source, &clock).is_err());
        fs::set_permissions(&policy_path, fs::Permissions::from_mode(0o600)).unwrap();
        let saved_policy = root.path().join("saved-policy");
        fs::rename(&policy_path, &saved_policy).unwrap();
        symlink(&saved_policy, &policy_path).unwrap();
        assert!(prepare(&output, &source, &clock).is_err());
        fs::remove_file(&policy_path).unwrap();
        fs::hard_link(&saved_policy, &policy_path).unwrap();
        assert!(prepare(&output, &source, &clock).is_err());
        assert!(target.exists());
        assert_eq!(
            serde_json::from_slice::<Policy>(&fs::read(saved_policy).unwrap()).unwrap(),
            policy
        );
    }

    #[test]
    fn trusted_cargo_retention_rejects_foreign_trust_domain_records() {
        let (_root, output, source, _policy) = fixture();
        let clock = MockClock::new();
        let target = prepare(&output, &source, &clock).unwrap();
        let fence = Fence::trusted(&output).unwrap();
        let (key, mut record) = fence
            .database
            .records::<Record>(RecordKind::CargoTarget)
            .unwrap()
            .pop()
            .unwrap();
        record.bead = Some("lm-foreign".parse().unwrap());
        fence
            .database
            .put_record(RecordKind::CargoTarget, &key, &record)
            .unwrap();
        drop(fence);
        assert!(prepare(&output, &source, &clock).is_err());
        assert!(target.exists());
    }

    #[test]
    fn trusted_cargo_retention_rejects_free_space_and_malformed_policy_before_allocation() {
        let (_root, output, source, mut policy) = fixture();
        let clock = MockClock::new();
        policy.min_free_bytes = Bytes::new(u64::MAX);
        configure(&output, &policy);
        assert!(prepare(&output, &source, &clock).is_err());
        assert!(!output.join("native").exists());
        let fence = Fence::trusted(&output).unwrap();
        let mut file = fence.root.read_private_file(Path::new(POLICY)).unwrap();
        file.set_len(0).unwrap();
        file.write_all(br#"{"max_byte":1}"#).unwrap();
        drop(file);
        drop(fence);
        assert!(prepare(&output, &source, &clock).is_err());
        assert!(!output.join("native").exists());
    }

    #[test]
    fn trusted_cargo_retention_collects_only_owned_never_dispatched_preparations() {
        let (_root, output, source, policy) = fixture();
        let clock = MockClock::new();
        let dispatched = prepare(&output, &source, &clock).unwrap();
        let prepared = {
            let fence = Fence::trusted(&output).unwrap();
            let (_, mut record) = fence
                .database
                .records::<Record>(RecordKind::CargoTarget)
                .unwrap()
                .pop()
                .unwrap();
            let parent = dispatched.parent().unwrap().parent().unwrap();
            let path = tempfile::Builder::new()
                .permissions(fs::Permissions::from_mode(0o700))
                .prefix("generation-")
                .tempdir_in(parent)
                .unwrap()
                .keep();
            let directory = Directory::open(&path).unwrap();
            let lease = directory.append_private_file(Path::new("lease")).unwrap();
            let target = directory.create_child(Path::new("target")).unwrap();
            record.target = path.join("target");
            record.namespace_identity = Identity::from_stat(&directory.stat().unwrap());
            record.target_identity = Some(Identity::from_stat(&target.stat().unwrap()));
            record.lease_identity = Some(Identity::from_stat(&rustix::fs::fstat(&lease).unwrap()));
            record.lifetime = Lifetime::Prepared;
            let key = path.strip_prefix(&output).unwrap().to_str().unwrap();
            fence
                .database
                .put_record(RecordKind::CargoTarget, key, &record)
                .unwrap();
            path
        };
        let target = prepare(&output, &source, &clock).unwrap();
        assert_eq!(target, dispatched);
        assert!(!prepared.exists());
        let fence = Fence::trusted(&output).unwrap();
        assert!(fence.report(&policy).unwrap().charged.get() > 0);
        assert!(dispatched.exists());
    }

    #[test]
    fn trusted_cargo_retention_read_only_source_identity_never_admits_unsafe_output() {
        let (root, output, _source, _policy) = fixture();
        let input_parent = root.path().join("input-only-parent");
        fs::create_dir(&input_parent).unwrap();
        fs::set_permissions(&input_parent, fs::Permissions::from_mode(0o777)).unwrap();
        let source = input_parent.join("source");
        fs::create_dir(&source).unwrap();
        assert!(Directory::open(&source).is_err());
        let target = prepare(&output, &source, &MockClock::new()).unwrap();
        assert!(target.starts_with(&output));
        assert!(
            prepare(
                &input_parent.join("unsafe-output"),
                &source,
                &MockClock::new()
            )
            .is_err()
        );
        assert!(!input_parent.join("unsafe-output").exists());
        assert!(target.exists());
    }

    #[test]
    fn trusted_cargo_retention_replaced_checkout_gets_a_new_context() {
        let (_root, output, source, _policy) = fixture();
        let clock = MockClock::new();
        let first = prepare(&output, &source, &clock).unwrap();
        fs::rename(&source, source.with_extension("old")).unwrap();
        fs::create_dir(&source).unwrap();
        let second = prepare(&output, &source, &clock).unwrap();
        assert_ne!(first, second);
        assert!(first.exists());
    }

    #[test]
    fn trusted_cargo_retention_serializes_parallel_admission_without_double_reservation() {
        let (_root, output, source, _policy) = fixture();
        let targets = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let output = &output;
                    let source = &source;
                    scope.spawn(move || prepare(output, source, &MockClock::new()).unwrap())
                })
                .collect();
            assert_eq!(handles.len(), 8);
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(targets.iter().all(|target| target == &targets[0]));
        let fence = Fence::trusted(&output).unwrap();
        assert_eq!(
            fence
                .database
                .records::<Record>(RecordKind::CargoTarget)
                .unwrap()
                .len(),
            1
        );
    }
}
