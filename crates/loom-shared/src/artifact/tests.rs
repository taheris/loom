#[cfg(test)]
mod cases {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, symlink};
    use std::path::{Path, PathBuf};
    use std::process::Command;

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
            let dir = tempfile::tempdir().unwrap();
            let beads = dir.path().join(".loom/beads");
            fs::create_dir_all(&beads).unwrap();
            let donor = beads.join("lm-donor");
            fs::create_dir(&donor).unwrap();
            project(&donor);
            assert_eq!(
                prepare(&beads, &donor, Path::new(IMAGE)).unwrap(),
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
            prepare(&self.beads, workspace, Path::new(IMAGE)).unwrap()
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
                assert!(prepare(&fixture.beads, &workspace, Path::new(IMAGE)).is_err());
                assert!(!workspace.join("target").exists());
            } else {
                assert_eq!(fixture.seed(&workspace), Outcome::Cold);
            }
        }
        let workspace = fixture.destination("lm-image");
        assert_eq!(
            prepare(&fixture.beads, &workspace, Path::new("/different/profile")).unwrap(),
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
        assert!(prepare(&fixture.beads, &outside, Path::new(IMAGE)).is_err());
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

    fn run(command: &mut Command) -> String {
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{command:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn cargo(workspace: &Path, home: &Path, args: &[&str]) -> Command {
        let mut command = Command::new("cargo");
        command
            .current_dir(workspace)
            .args(args)
            .env("CARGO_HOME", home)
            .env("CARGO_INCREMENTAL", "0");
        for name in [
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_BUILD_RUSTC_WRAPPER",
            "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_BUILD_BUILD_DIR",
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_MAKEFLAGS",
        ] {
            command.env_remove(name);
        }
        command
    }

    #[test]
    fn target_seeding_real_cargo_rebuilds_divergent_checkouts() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("cargo-home");
        let beads = dir.path().join(".loom/beads");
        fs::create_dir_all(&beads).unwrap();
        let dependency = dir.path().join("dependency");
        fs::create_dir_all(dependency.join("src")).unwrap();
        fs::write(
            dependency.join("Cargo.toml"),
            "[package]\nname='external'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        fs::write(
            dependency.join("src/lib.rs"),
            "pub fn value() -> u8 { 1 }\n",
        )
        .unwrap();
        loom_driver::git::init_test_repo(&dependency).unwrap();
        loom_driver::git::commit_all_in(&dependency, "Dependency").unwrap();
        let manifest = format!(
            "[package]\nname='application'\nversion='0.1.0'\nedition='2024'\n[dependencies]\nexternal={{git='file://{}'}}\n",
            dependency.display()
        );
        let donor = beads.join("lm-real-a");
        let destination = beads.join("lm-real-b");
        for workspace in [&donor, &destination] {
            fs::create_dir_all(workspace.join("src")).unwrap();
            fs::write(workspace.join("Cargo.toml"), &manifest).unwrap();
            fs::write(
                workspace.join("src/main.rs"),
                "fn main() { println!(\"{}:{}\", external::value(), env!(\"CARGO_MANIFEST_DIR\")); }\n",
            )
            .unwrap();
        }
        run(&mut cargo(&donor, &home, &["generate-lockfile"]));
        fs::copy(donor.join("Cargo.lock"), destination.join("Cargo.lock")).unwrap();
        assert_eq!(
            prepare(&beads, &donor, Path::new(IMAGE)).unwrap(),
            Outcome::Cold
        );
        run(&mut cargo(&donor, &home, &["build", "--offline"]));
        let before = fs::read_dir(donor.join("target/debug/deps"))
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".rlib"))
            .map(|entry| (entry.path(), fs::read(entry.path()).unwrap()))
            .collect::<Vec<_>>();
        assert_ne!(before.len(), 0);
        let Outcome::Seeded(report) = prepare(&beads, &destination, Path::new(IMAGE)).unwrap()
        else {
            panic!("compatible dependency was not seeded");
        };
        assert_ne!(report.linked_bytes + report.cloned_bytes, 0);
        fs::write(
            destination.join("src/main.rs"),
            "fn main() { println!(\"{}:{}\", external::value() + 1, env!(\"CARGO_MANIFEST_DIR\")); }\n",
        )
        .unwrap();
        let a = cargo(&donor, &home, &["build", "--offline"])
            .spawn()
            .unwrap();
        let b = cargo(&destination, &home, &["build", "--offline"])
            .spawn()
            .unwrap();
        assert!(a.wait_with_output().unwrap().status.success());
        assert!(b.wait_with_output().unwrap().status.success());
        assert_eq!(
            run(&mut Command::new(donor.join("target/debug/application"))).trim(),
            format!("1:{}", donor.display())
        );
        assert_eq!(
            run(&mut Command::new(
                destination.join("target/debug/application")
            ))
            .trim(),
            format!("2:{}", destination.display())
        );
        for (path, bytes) in &before {
            let seeded = destination
                .join("target/debug/deps")
                .join(path.file_name().unwrap());
            assert_eq!(fs::read(&seeded).unwrap(), *bytes);
            assert_eq!(
                filetime::FileTime::from_last_modification_time(&fs::metadata(path).unwrap()),
                filetime::FileTime::from_last_modification_time(&fs::metadata(&seeded).unwrap())
            );
            if report.linked_bytes > 0 {
                assert_eq!(
                    fs::metadata(path).unwrap().ino(),
                    fs::metadata(seeded).unwrap().ino()
                );
            }
        }
        let fingerprint = fs::read_dir(destination.join("target/debug/.fingerprint"))
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.file_name().to_string_lossy().starts_with("external-"))
            .unwrap();
        fs::remove_dir_all(fingerprint.path()).unwrap();
        run(&mut cargo(&destination, &home, &["build", "--offline"]));
        fs::write(
            dependency.join("src/lib.rs"),
            "pub fn value() -> u8 { 5 }\n",
        )
        .unwrap();
        loom_driver::git::commit_all_in(&dependency, "Change dependency").unwrap();
        run(&mut cargo(
            &destination,
            &home,
            &["update", "-p", "external"],
        ));
        run(&mut cargo(&destination, &home, &["build", "--offline"]));
        for (path, bytes) in before {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
        fs::remove_dir_all(donor.join("target")).unwrap();
        run(&mut cargo(&destination, &home, &["build", "--offline"]));
        assert_eq!(
            run(&mut Command::new(
                destination.join("target/debug/application")
            ))
            .trim(),
            format!("6:{}", destination.display())
        );
    }
}
