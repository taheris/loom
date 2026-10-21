mod support;

use std::fs;
use std::os::unix::fs::MetadataExt;

use anyhow::Result;
use loom_shared::artifact::{Outcome, Reuse};
use support::{Fixture, external_libraries};

#[tokio::test]
async fn target_reuse_fixture_initializes_without_ambient_hooks() -> Result<()> {
    const CHILD: &str = "LOOM_TARGET_FIXTURE_HOOKS_CHILD";
    if std::env::var_os(CHILD).is_some() {
        assert!(std::env::var_os("WRIX_PREK_HOOKS").is_none());
        let fixture = Fixture::new(&std::env::temp_dir()).await?;
        assert!(fixture.donor.join("Cargo.toml").is_file());
        assert!(
            fixture
                .workspace("lm-bbbb")
                .await?
                .join("Cargo.toml")
                .is_file()
        );
        return Ok(());
    }
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "target_reuse_fixture_initializes_without_ambient_hooks",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env_remove("WRIX_PREK_HOOKS");
    let output = loom_driver::process::output(
        &mut command,
        &loom_driver::clock::SystemClock,
        std::time::Duration::from_secs(60),
    )
    .await?;
    anyhow::ensure!(
        output.status.success(),
        "fixture subprocess failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// Real independent Git clones and Cargo are needed to detect stale cross-checkout artifact reuse.
#[tokio::test]
async fn target_seeding_real_cargo_rebuilds_divergent_checkouts() -> Result<()> {
    let fixture = Fixture::new(&std::env::temp_dir()).await?;
    let destination = fixture.workspace("lm-bbbb").await?;
    let donor_build = fixture.build(&fixture.donor).await?;
    assert!(!donor_build.external_fresh);
    let before = external_libraries(&fixture.donor)?
        .into_iter()
        .map(|path| Ok((path.clone(), fs::read(&path)?)))
        .collect::<Result<Vec<_>>>()?;
    assert_ne!(before.len(), 0);
    let Outcome::Seeded(report) = fixture.prepare(&destination, Reuse::AllowHardlinks)? else {
        panic!("compatible dependency was not seeded");
    };
    assert!(report.linked_bytes + report.cloned_bytes > 0);
    fs::write(
        destination.join("src/main.rs"),
        "fn main() { println!(\"{}:{}\", external::value() + 1, env!(\"CARGO_MANIFEST_DIR\")); }\n",
    )?;
    let (a, b) = tokio::try_join!(fixture.build(&fixture.donor), fixture.build(&destination))?;
    assert!(a.external_fresh && a.application_fresh);
    assert!(b.external_fresh && !b.application_fresh);
    assert_eq!(
        fixture.run(&fixture.donor).await?.trim(),
        format!("1:{}", fixture.donor.display())
    );
    assert_eq!(
        fixture.run(&destination).await?.trim(),
        format!("2:{}", destination.display())
    );
    for (path, bytes) in &before {
        let seeded = destination
            .join("target/debug/deps")
            .join(path.file_name().unwrap());
        assert_eq!(fs::read(&seeded)?, *bytes);
        assert_eq!(
            fs::metadata(path)?.modified()?,
            fs::metadata(&seeded)?.modified()?
        );
        if report.linked_bytes > 0 {
            assert_eq!(fs::metadata(path)?.ino(), fs::metadata(&seeded)?.ino());
        } else {
            assert_ne!(fs::metadata(path)?.ino(), fs::metadata(&seeded)?.ino());
        }
    }
    let fingerprint = fs::read_dir(destination.join("target/debug/.fingerprint"))?
        .map(Result::unwrap)
        .find(|entry| entry.file_name().to_string_lossy().starts_with("external-"))
        .unwrap();
    fs::remove_dir_all(fingerprint.path())?;
    assert!(!fixture.build(&destination).await?.external_fresh);
    fixture.change_dependency(&destination).await?;
    assert!(!fixture.build(&destination).await?.external_fresh);
    for (path, bytes) in before {
        assert_eq!(fs::read(path)?, bytes);
    }
    fs::remove_dir_all(fixture.donor.join("target"))?;
    assert!(fixture.build(&destination).await?.external_fresh);
    assert_eq!(
        fixture.run(&destination).await?.trim(),
        format!("6:{}", destination.display())
    );
    Ok(())
}

/// A bounded tiny native fixture verifies measurement accounting and actual freshness, not timing quotas.
#[tokio::test]
async fn target_reuse_measurement_reports_freshness_and_inode_aware_storage() -> Result<()> {
    let fixture = Fixture::new(&std::env::temp_dir()).await?;
    let report = fixture.measure().await?;
    assert_eq!(report.trials.len(), 3);
    assert!(!report.donor_build.external_fresh);
    for trial in &report.trials {
        assert_eq!(trial.seeded, trial.build.external_fresh);
        assert!(!trial.build.application_fresh);
        if trial.policy != Reuse::AllowHardlinks {
            assert_eq!(trial.linked_library_bytes, 0);
        }
        for storage in [&trial.libraries_before_build, &trial.libraries_after_build] {
            assert_eq!(
                storage.allocated,
                storage.unique_inode_bytes + storage.shared_inode_bytes
            );
        }
        if trial.linked_library_bytes > 0 {
            assert!(trial.libraries_before_build.shared_inode_bytes > 0);
            assert!(trial.libraries_after_build.shared_inode_bytes > 0);
        } else {
            assert_eq!(trial.libraries_before_build.shared_inode_bytes, 0);
        }
    }
    Ok(())
}
