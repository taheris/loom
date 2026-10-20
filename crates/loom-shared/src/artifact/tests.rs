#[cfg(test)]
mod cases {
    use std::fs;
    use std::io;
    use std::os::unix::fs::{MetadataExt, symlink};
    use std::path::{Path, PathBuf};

    use rustix::fs::{FlockOperation, flock};

    use super::super::*;

    const HASH: &str = "0123456789abcdef";
    const IMAGE: &str = "/nix/store/worker-linux-profile.json";

    struct Fixture {
        dir: tempfile::TempDir,
        beads: PathBuf,
        donor: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_reuse(Reuse::AllowHardlinks)
        }

        fn with_reuse(reuse: Reuse) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let beads = dir.path().join(".loom/beads");
            fs::create_dir_all(&beads).unwrap();
            let donor = beads.join("lm-donor");
            fs::create_dir(&donor).unwrap();
            project(&donor);
            assert_eq!(
                prepare(&beads, &donor, Path::new(IMAGE), reuse).unwrap(),
                Outcome::Cold
            );
            let profile = donor.join("target/debug");
            fs::create_dir_all(profile.join("deps")).unwrap();
            fs::File::create_new(profile.join(".cargo-lock")).unwrap();
            unit(&profile, "external", "renamed", HASH);
            unit(&profile, "local", "local", "fedcba9876543210");
            fs::create_dir_all(profile.join("build/external-0123456789abcdef/out")).unwrap();
            fs::write(
                profile.join("build/external-0123456789abcdef/out/generated"),
                "private",
            )
            .unwrap();
            fs::create_dir_all(profile.join("incremental")).unwrap();
            fs::write(profile.join("incremental/omit"), "omit").unwrap();
            fs::write(profile.join("top-level-binary"), "omit").unwrap();
            Self { dir, beads, donor }
        }

        fn destination(&self, name: &str) -> PathBuf {
            let workspace = self.beads.join(name);
            fs::create_dir(&workspace).unwrap();
            project(&workspace);
            workspace
        }

        fn seed(&self, workspace: &Path) -> Outcome {
            prepare(
                &self.beads,
                workspace,
                Path::new(IMAGE),
                Reuse::AllowHardlinks,
            )
            .unwrap()
        }
    }

    fn project(workspace: &Path) {
        fs::write(
            workspace.join("Cargo.toml"),
            "[package]\nname='local'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::write(workspace.join("Cargo.lock"), "version = 4\n[[package]]\nname='local'\nversion='0.1.0'\n[[package]]\nname='external'\nversion='0.1.0'\nsource='registry+https://example.invalid/index'\n").unwrap();
    }

    fn unit(profile: &Path, package: &str, library: &str, hash: &str) {
        let fingerprint = profile.join(format!(".fingerprint/{package}-{hash}"));
        fs::create_dir_all(&fingerprint).unwrap();
        fs::write(fingerprint.join(format!("lib-{library}.json")), "{}").unwrap();
        fs::write(fingerprint.join(format!("lib-{library}")), "fingerprint").unwrap();
        fs::write(
            profile.join(format!("deps/lib{library}-{hash}.rlib")),
            vec![42; 4096],
        )
        .unwrap();
        fs::write(profile.join(format!("deps/{library}-{hash}.d")), "depinfo").unwrap();
    }

    #[test]
    fn target_seeding_shares_only_external_libraries() {
        let fixture = Fixture::new();
        let destination = fixture.destination("lm-dest");
        let Outcome::Seeded(report) = fixture.seed(&destination) else {
            panic!("not seeded")
        };
        assert_eq!(report.cloned_bytes + report.linked_bytes, 4096);
        assert!(report.copied_bytes > 0);
        let source = fixture.donor.join("target/debug");
        let target = destination.join("target/debug");
        let library = format!("deps/librenamed-{HASH}.rlib");
        let a = fs::metadata(source.join(&library)).unwrap();
        let b = fs::metadata(target.join(&library)).unwrap();
        if report.linked_bytes > 0 {
            assert_eq!((a.dev(), a.ino()), (b.dev(), b.ino()));
        } else {
            assert_ne!(a.ino(), b.ino());
        }
        for private in [
            format!("deps/renamed-{HASH}.d"),
            format!(".fingerprint/external-{HASH}/lib-renamed.json"),
            format!("build/external-{HASH}/out/generated"),
        ] {
            assert_ne!(
                fs::metadata(source.join(&private)).unwrap().ino(),
                fs::metadata(target.join(&private)).unwrap().ino()
            );
            assert_eq!(
                fs::metadata(source.join(&private))
                    .unwrap()
                    .modified()
                    .unwrap(),
                fs::metadata(target.join(&private))
                    .unwrap()
                    .modified()
                    .unwrap()
            );
            fs::write(target.join(&private), "changed").unwrap();
            assert_ne!(fs::read(source.join(&private)).unwrap(), b"changed");
        }
        assert!(!target.join("deps/liblocal-fedcba9876543210.rlib").exists());
        assert!(!target.join("incremental").exists());
        assert!(!target.join("top-level-binary").exists());
    }

    #[test]
    fn target_reuse_off_does_not_initialize_or_register_a_target() {
        let fixture = Fixture::new();
        let destination = fixture.destination("lm-off");
        assert_eq!(
            prepare(&fixture.beads, &destination, Path::new(IMAGE), Reuse::Off).unwrap(),
            Outcome::Disabled
        );
        assert!(!destination.join("target").exists());
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 2);
    }

    fn assert_independent_or_cold(fixture: &Fixture, destination: &Path, outcome: Outcome) {
        let library = format!("target/debug/deps/librenamed-{HASH}.rlib");
        let source = fixture.donor.join(&library);
        let target = destination.join(&library);
        match outcome {
            Outcome::Seeded(report) => {
                assert_eq!(report.linked_bytes, 0);
                assert_eq!(report.cloned_bytes, 4096);
                assert_ne!(
                    fs::metadata(&source).unwrap().ino(),
                    fs::metadata(&target).unwrap().ino()
                );
                fs::write(&target, "private modification").unwrap();
                assert_eq!(fs::read(&source).unwrap(), vec![42; 4096]);
            }
            Outcome::Cold => {
                assert!(!target.exists());
                assert!(!destination.join("target/debug").exists());
            }
            other => panic!("unexpected reuse outcome: {other:?}"),
        }
        assert!(fs::read_dir(destination).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".loom-shared-")
        }));
    }

    #[test]
    fn target_reuse_defaults_to_independent_clones_or_cold_builds() {
        let fixture = Fixture::new();
        let destination = fixture.destination("lm-cow");
        assert_eq!(Reuse::default(), Reuse::CopyOnWrite);
        let outcome = prepare(
            &fixture.beads,
            &destination,
            Path::new(IMAGE),
            Reuse::default(),
        )
        .unwrap();
        assert_independent_or_cold(&fixture, &destination, outcome);
    }

    #[test]
    fn target_reuse_hardlinks_require_donor_permission() {
        let fixture = Fixture::with_reuse(Reuse::CopyOnWrite);
        let destination = fixture.destination("lm-hardlinks");
        let outcome = fixture.seed(&destination);
        assert_independent_or_cold(&fixture, &destination, outcome);
    }

    #[test]
    fn target_seeding_preserves_existing_and_rejects_incompatible_donors() {
        let fixture = Fixture::new();
        for (index, filename) in [
            "Cargo.lock",
            "Cargo.toml",
            "rust-toolchain.toml",
            ".cargo/config.toml",
        ]
        .into_iter()
        .enumerate()
        {
            let workspace = fixture.destination(&format!("lm-config-{index}"));
            if filename.starts_with(".cargo/") {
                fs::create_dir(workspace.join(".cargo")).unwrap();
            }
            fs::write(workspace.join(filename), "changed").unwrap();
            if filename == "Cargo.lock" {
                assert!(
                    prepare(
                        &fixture.beads,
                        &workspace,
                        Path::new(IMAGE),
                        Reuse::AllowHardlinks
                    )
                    .is_err()
                );
                assert!(!workspace.join("target").exists());
            } else {
                assert_eq!(fixture.seed(&workspace), Outcome::Cold);
            }
        }
        let workspace = fixture.destination("lm-image");
        assert_eq!(
            prepare(
                &fixture.beads,
                &workspace,
                Path::new("/different/profile"),
                Reuse::AllowHardlinks
            )
            .unwrap(),
            Outcome::Cold
        );
        let existing = fixture.destination("lm-existing");
        fs::create_dir(existing.join("target")).unwrap();
        assert_eq!(fixture.seed(&existing), Outcome::Preserved);
        assert!(
            fs::read_dir(existing.join("target"))
                .unwrap()
                .next()
                .is_none()
        );
        fs::write(existing.join("target/sentinel"), "keep").unwrap();
        assert_eq!(fixture.seed(&existing), Outcome::Preserved);
        assert_eq!(fs::read(existing.join("target/sentinel")).unwrap(), b"keep");
        let linked = fixture.destination("lm-linked");
        symlink(fixture.donor.join("target"), linked.join("target")).unwrap();
        assert_eq!(fixture.seed(&linked), Outcome::Preserved);
        let outside = fixture.dir.path().join("operator");
        fs::create_dir(&outside).unwrap();
        project(&outside);
        assert!(
            prepare(
                &fixture.beads,
                &outside,
                Path::new(IMAGE),
                Reuse::AllowHardlinks
            )
            .is_err()
        );
        assert!(!outside.join("target").exists());
    }

    #[test]
    fn target_seeding_refuses_unsafe_donors_and_preserves_publication_races() {
        let fixture = Fixture::new();
        let lock = fs::File::open(fixture.donor.join("target/debug/.cargo-lock")).unwrap();
        flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();
        let busy = fixture.destination("lm-busy");
        assert_eq!(fixture.seed(&busy), Outcome::Cold);
        drop(lock);
        fs::remove_file(fixture.donor.join("target/debug/.cargo-lock")).unwrap();
        let unknown = fixture.destination("lm-unknown");
        assert_eq!(fixture.seed(&unknown), Outcome::Cold);
        fs::File::create_new(fixture.donor.join("target/debug/.cargo-lock")).unwrap();
        let library = fixture
            .donor
            .join(format!("target/debug/deps/librenamed-{HASH}.rlib"));
        fs::remove_file(&library).unwrap();
        symlink("/etc/passwd", &library).unwrap();
        let unsafe_destination = fixture.destination("lm-unsafe");
        assert_eq!(fixture.seed(&unsafe_destination), Outcome::Cold);
        assert!(
            !unsafe_destination
                .join(format!("target/debug/deps/librenamed-{HASH}.rlib"))
                .exists()
        );
        let race = fixture.destination("lm-race");
        let staged = staging(&race).unwrap();
        fs::write(staged.path().join("partial"), "partial").unwrap();
        fs::create_dir(race.join("target")).unwrap();
        assert!(!publish(staged.path(), &race.join("target")).unwrap());
        assert!(fs::read_dir(race.join("target")).unwrap().next().is_none());
        let orphan = race.join(".loom-shared-interrupted");
        fs::create_dir(&orphan).unwrap();
        fs::write(orphan.join("partial"), "partial").unwrap();
        assert_eq!(fixture.seed(&race), Outcome::Preserved);
        assert!(orphan.join("partial").exists());
        assert!(!race.join("target/partial").exists());
    }

    #[test]
    fn target_seeding_private_policy_and_ambiguous_names_do_not_share() {
        let fixture = Fixture::new();
        let private = fixture.destination("lm-private");
        fs::write(private.join("wrix.toml"), "[tool_caches]\nshared=false\n").unwrap();
        assert_eq!(fixture.seed(&private), Outcome::Private);
        assert!(!private.join("target").exists());
        let ambiguous = fixture.destination("lm-ambiguous");
        let lock = "version=4\n[[package]]\nname='external'\nversion='0.1.0'\n[[package]]\nname='external'\nversion='0.2.0'\nsource='git+file:///external'\n";
        fs::write(ambiguous.join("Cargo.lock"), lock).unwrap();
        let packages = catalog::Packages::read(&ambiguous.join("Cargo.lock")).unwrap();
        assert!(packages.unit(&format!("external-{HASH}")).is_none());
    }

    struct IndependentClone;

    impl transfer::Cloner for IndependentClone {
        fn clone_file(&self, source: &Path, destination: &Path) -> io::Result<()> {
            fs::copy(source, destination)?;
            Ok(())
        }
    }

    struct RejectedClone(io::ErrorKind);

    impl transfer::Cloner for RejectedClone {
        fn clone_file(&self, _: &Path, _: &Path) -> io::Result<()> {
            Err(io::Error::from(self.0))
        }
    }

    #[test]
    fn target_reuse_prefers_independent_clones_even_with_hardlink_permission() {
        for (name, reuse) in [
            ("lm-cow", Reuse::CopyOnWrite),
            ("lm-links", Reuse::AllowHardlinks),
        ] {
            let fixture = Fixture::new();
            let destination = fixture.destination(name);
            let outcome = prepare_with(
                &fixture.beads,
                &destination,
                Path::new(IMAGE),
                reuse,
                &IndependentClone,
            )
            .unwrap();
            assert!(matches!(outcome, Outcome::Seeded(_)));
            assert_independent_or_cold(&fixture, &destination, outcome);
        }
    }

    #[test]
    fn target_reuse_unsupported_clone_respects_both_workers_permissions() {
        for donor in [Reuse::CopyOnWrite, Reuse::AllowHardlinks] {
            for recipient in [Reuse::CopyOnWrite, Reuse::AllowHardlinks] {
                let fixture = Fixture::with_reuse(donor);
                let destination = fixture.destination("lm-recipient");
                let outcome = prepare_with(
                    &fixture.beads,
                    &destination,
                    Path::new(IMAGE),
                    recipient,
                    &RejectedClone(io::ErrorKind::Unsupported),
                )
                .unwrap();
                if donor == Reuse::AllowHardlinks && recipient == Reuse::AllowHardlinks {
                    let Outcome::Seeded(report) = outcome else {
                        panic!("hardlink opt-in was ignored")
                    };
                    assert_eq!(report.cloned_bytes, 0);
                    assert_eq!(report.linked_bytes, 4096);
                    let library = format!("target/debug/deps/librenamed-{HASH}.rlib");
                    assert_eq!(
                        fs::metadata(fixture.donor.join(&library)).unwrap().ino(),
                        fs::metadata(destination.join(&library)).unwrap().ino()
                    );
                } else {
                    assert_eq!(outcome, Outcome::Cold);
                    assert_independent_or_cold(&fixture, &destination, outcome);
                }
            }
        }
    }

    #[test]
    fn target_reuse_clone_io_errors_never_authorize_hardlink_fallback() {
        for error in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::Interrupted,
            io::ErrorKind::Other,
        ] {
            let fixture = Fixture::new();
            let destination = fixture.destination("lm-error");
            let outcome = prepare_with(
                &fixture.beads,
                &destination,
                Path::new(IMAGE),
                Reuse::AllowHardlinks,
                &RejectedClone(error),
            )
            .unwrap();
            assert_eq!(outcome, Outcome::Cold);
            assert_independent_or_cold(&fixture, &destination, outcome);
        }
    }

    #[test]
    fn clone_adapter_contract_keeps_successful_files_independently_writable() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::write(&source, vec![42; 4096]).unwrap();
        for (name, cloner) in [
            ("fixture", &IndependentClone as &dyn transfer::Cloner),
            ("native", &transfer::Native as &dyn transfer::Cloner),
        ] {
            let destination = dir.path().join(name);
            match cloner.clone_file(&source, &destination) {
                Ok(()) => {
                    assert_eq!(fs::read(&destination).unwrap(), fs::read(&source).unwrap());
                    assert_ne!(
                        fs::metadata(&source).unwrap().ino(),
                        fs::metadata(&destination).unwrap().ino()
                    );
                    fs::write(&destination, "changed").unwrap();
                    assert_eq!(fs::read(&source).unwrap(), vec![42; 4096]);
                }
                Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                    assert_eq!(name, "native");
                    assert!(!destination.exists());
                }
                Err(error) => panic!("unexpected clone adapter error: {error}"),
            }
        }
    }

    struct RacingClone(PathBuf);

    impl transfer::Cloner for RacingClone {
        fn clone_file(&self, source: &Path, destination: &Path) -> io::Result<()> {
            fs::copy(source, destination)?;
            fs::create_dir(&self.0)?;
            fs::write(self.0.join("sentinel"), "keep")
        }
    }

    #[test]
    fn target_reuse_concurrent_destination_wins_without_partial_merge() {
        let fixture = Fixture::new();
        let destination = fixture.destination("lm-racing");
        let outcome = prepare_with(
            &fixture.beads,
            &destination,
            Path::new(IMAGE),
            Reuse::AllowHardlinks,
            &RacingClone(destination.join("target")),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Preserved);
        assert_eq!(
            fs::read(destination.join("target/sentinel")).unwrap(),
            b"keep"
        );
        assert_eq!(fs::read_dir(destination.join("target")).unwrap().count(), 1);
        assert_eq!(fs::read_dir(destination).unwrap().count(), 3);
    }

    #[test]
    fn target_reuse_private_donors_are_not_used_by_either_enabled_policy() {
        let fixture = Fixture::new();
        fs::write(
            fixture.donor.join("wrix.toml"),
            "[tool_caches]\nshared=false\n",
        )
        .unwrap();
        for (name, reuse) in [
            ("lm-cow", Reuse::CopyOnWrite),
            ("lm-links", Reuse::AllowHardlinks),
        ] {
            let destination = fixture.destination(name);
            let outcome = prepare_with(
                &fixture.beads,
                &destination,
                Path::new(IMAGE),
                reuse,
                &IndependentClone,
            )
            .unwrap();
            assert_eq!(outcome, Outcome::Cold);
            assert_independent_or_cold(&fixture, &destination, outcome);
        }
    }

    struct BlockedClone;

    impl transfer::Cloner for BlockedClone {
        fn clone_file(&self, _: &Path, destination: &Path) -> io::Result<()> {
            use std::io::{Read, Write};
            fs::write(destination, "partial")?;
            writeln!(io::stdout(), "target-staging-ready")?;
            io::stdout().flush()?;
            io::stdin().read_exact(&mut [0])
        }
    }

    /// Kernel process death must leave only inert staging; the owned child group is killed and reaped.
    #[tokio::test]
    async fn target_reuse_killed_preparation_leaves_inert_staging_and_retry_works() {
        use loom_driver::clock::{Clock, SystemClock};
        use loom_driver::process::OwnedChild;
        use std::process::Stdio;
        use std::time::Duration;
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::process::Command;

        if let Some(beads) = std::env::var_os("LOOM_TARGET_KILL_BEADS") {
            let destination = std::env::var_os("LOOM_TARGET_KILL_DESTINATION").unwrap();
            prepare_with(
                Path::new(&beads),
                Path::new(&destination),
                Path::new(IMAGE),
                Reuse::AllowHardlinks,
                &BlockedClone,
            )
            .unwrap();
            panic!("blocked clone unexpectedly completed");
        }
        let fixture = Fixture::new();
        let destination = fixture.destination("lm-killed");
        let mut child = OwnedChild::spawn(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "artifact::tests::cases::target_reuse_killed_preparation_leaves_inert_staging_and_retry_works", "--nocapture"])
                .env("LOOM_TARGET_KILL_BEADS", &fixture.beads)
                .env("LOOM_TARGET_KILL_DESTINATION", &destination)
                .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit())
        ).unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        SystemClock
            .timeout(Duration::from_secs(30), async {
                while let Some(line) = lines.next_line().await.unwrap() {
                    if line.ends_with("target-staging-ready") {
                        return;
                    }
                }
                panic!("child exited before staging readiness");
            })
            .await
            .unwrap();
        child.terminate().await.unwrap();
        assert!(!destination.join("target").exists());
        let orphans = fs::read_dir(&destination)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".loom-shared-")
            })
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        assert_eq!(orphans.len(), 1);
        let outcome = prepare_with(
            &fixture.beads,
            &destination,
            Path::new(IMAGE),
            Reuse::CopyOnWrite,
            &RejectedClone(io::ErrorKind::Unsupported),
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Cold);
        assert!(
            orphans[0]
                .join(format!("debug/deps/librenamed-{HASH}.rlib"))
                .exists()
        );
        assert!(!destination.join("target/debug").exists());
    }

    struct InterruptedClone;

    impl transfer::Cloner for InterruptedClone {
        fn clone_file(&self, _: &Path, destination: &Path) -> io::Result<()> {
            fs::write(destination, "partial")?;
            Err(io::Error::from(io::ErrorKind::Interrupted))
        }
    }

    #[test]
    fn target_reuse_interrupted_clone_discards_staging_without_partial_publication() {
        let fixture = Fixture::new();
        let destination = fixture.destination("lm-partial");
        let outcome = prepare_with(
            &fixture.beads,
            &destination,
            Path::new(IMAGE),
            Reuse::AllowHardlinks,
            &InterruptedClone,
        )
        .unwrap();
        assert_eq!(outcome, Outcome::Cold);
        assert_independent_or_cold(&fixture, &destination, outcome);
    }
}
