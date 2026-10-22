#[cfg(test)]
mod cases {
    use std::os::unix::fs::PermissionsExt as _;

    use super::super::*;

    fn child(name: &str) -> bool {
        if std::env::var_os("LOOM_PRIVATE_CARGO_CHILD").is_some() {
            return true;
        }
        for mask in ["022", "077"] {
            let output = Command::new("bash")
                .args([
                    "-c",
                    "umask \"$1\"; exec \"$2\" \"$3\" --nocapture",
                    "bash",
                    mask,
                ])
                .arg(std::env::current_exe().unwrap())
                .arg(name)
                .env("LOOM_PRIVATE_CARGO_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "umask {mask}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        false
    }

    fn target(source: &Path, output: &Path) -> std::io::Result<PathBuf> {
        let mut command = Command::new("sh");
        command.args(["-c", "printf '%s' \"$CARGO_TARGET_DIR\""]);
        let _execution = trusted_command_at(&mut command, source, output)?;
        let result = command.output()?;
        assert!(result.status.success());
        Ok(PathBuf::from(String::from_utf8(result.stdout).unwrap()))
    }

    #[test]
    fn private_cargo_native_creation_and_reuse_under_child_umasks() {
        if !child("private_cargo_native_creation_and_reuse_under_child_umasks") {
            return;
        }
        let (root, association, _, _) = tests::cases::fixture();
        let output = root.path().join("trusted");
        let first = target(association.operator(), &output).unwrap();
        for path in [&first, first.parent().unwrap()] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o7777,
                0o700
            );
        }
        fs::write(first.join("sentinel"), b"retained").unwrap();
        let inode = fs::metadata(&first).unwrap().ino();
        assert_eq!(target(association.operator(), &output).unwrap(), first);
        assert_eq!(fs::metadata(&first).unwrap().ino(), inode);
        assert_eq!(fs::read(first.join("sentinel")).unwrap(), b"retained");
    }

    #[test]
    fn private_cargo_worker_creation_under_child_umasks() {
        if !child("private_cargo_worker_creation_under_child_umasks") {
            return;
        }
        let (_root, association, worker, profile) = tests::cases::fixture();
        let namespace = prepare(
            &association,
            &worker,
            &profile,
            crate::config::CargoTargetReuse::Off,
            retention::Policy::default().default_request(),
            &crate::clock::MockClock::new(),
        )
        .unwrap();
        for path in [namespace.target(), namespace.path.clone()] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o7777,
                0o700
            );
        }
        fs::write(namespace.target().join("sentinel"), b"retained").unwrap();
        let next = prepare(
            &association,
            &worker,
            &profile,
            crate::config::CargoTargetReuse::Off,
            retention::Policy::default().default_request(),
            &crate::clock::MockClock::new(),
        )
        .unwrap();
        assert_ne!(namespace.target(), next.target());
        fs::set_permissions(&namespace.path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            prepare(
                &association,
                &worker,
                &profile,
                crate::config::CargoTargetReuse::Off,
                retention::Policy::default().default_request(),
                &crate::clock::MockClock::new(),
            )
            .is_err()
        );
        assert_eq!(
            fs::read(namespace.target().join("sentinel")).unwrap(),
            b"retained"
        );
    }

    #[test]
    fn private_cargo_unsafe_recorded_boundaries_remain_retained_under_child_umasks() {
        if !child("private_cargo_unsafe_recorded_boundaries_remain_retained_under_child_umasks") {
            return;
        }
        for boundary in ["generation", "target", "other-context"] {
            let (root, association, _, _) = tests::cases::fixture();
            let output = root.path().join("trusted");
            let first = target(association.operator(), &output).unwrap();
            let path = if boundary == "target" {
                first.as_path()
            } else {
                first.parent().unwrap()
            };
            fs::write(first.join("sentinel"), b"retained").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            let inode = fs::metadata(path).unwrap().ino();
            let fence = retention::Fence::trusted(&output).unwrap();
            let before = fence
                .database
                .records::<Record>(RecordKind::CargoTarget)
                .unwrap();
            let before = serde_json::to_vec(&before).unwrap();
            drop(fence);
            if boundary == "other-context" {
                fs::write(association.operator().join("Cargo.toml"), "changed context").unwrap();
            }
            let error = target(association.operator(), &output).unwrap_err();
            let message = format!("{:#}", anyhow::Error::new(error));
            assert!(message.contains(&path.display().to_string()), "{message}");
            assert!(message.contains("0755"), "{message}");
            assert!(message.contains("0700"), "{message}");
            assert_eq!(fs::metadata(path).unwrap().ino(), inode);
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o7777,
                0o755
            );
            assert_eq!(fs::read(first.join("sentinel")).unwrap(), b"retained");
            let fence = retention::Fence::trusted(&output).unwrap();
            let after = fence
                .database
                .records::<Record>(RecordKind::CargoTarget)
                .unwrap();
            assert_eq!(serde_json::to_vec(&after).unwrap(), before);
            assert_eq!(
                fs::read_dir(first.parent().unwrap().parent().unwrap())
                    .unwrap()
                    .count(),
                1
            );
        }
    }

    #[test]
    fn private_cargo_abandoned_unrecorded_attempt_is_not_adopted_or_repaired() {
        if !child("private_cargo_abandoned_unrecorded_attempt_is_not_adopted_or_repaired") {
            return;
        }
        let (root, association, _, _) = tests::cases::fixture();
        let output = root.path().join("trusted");
        let first = target(association.operator(), &output).unwrap();
        let abandoned = first
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("generation-abandoned");
        fs::create_dir(&abandoned).unwrap();
        fs::set_permissions(&abandoned, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(abandoned.join("sentinel"), b"unknown output").unwrap();
        let inode = fs::metadata(&abandoned).unwrap().ino();
        assert_eq!(target(association.operator(), &output).unwrap(), first);
        assert_eq!(fs::metadata(&abandoned).unwrap().ino(), inode);
        assert_eq!(
            fs::metadata(&abandoned).unwrap().permissions().mode() & 0o7777,
            0o755
        );
        assert_eq!(
            fs::read(abandoned.join("sentinel")).unwrap(),
            b"unknown output"
        );
        let fence = retention::Fence::trusted(&output).unwrap();
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
