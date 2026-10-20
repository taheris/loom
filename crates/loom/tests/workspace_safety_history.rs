#[path = "workspace_safety_history/mod.rs"]
mod scenario;

use anyhow::Result;
use loom_driver::config::LoomTopConfig;
use loom_driver::profile_manifest::ProfileImageManifest;
use loom_driver::scratch::runtime;
use scenario::Fixture;

fn manifest(fixture: &Fixture) -> Result<ProfileImageManifest> {
    let path = fixture.root.path().join("manifest.json");
    loom_test_support::profile_manifest::write(
        &path,
        r#"{"base":{"pi":{"ref":"fixture-image","source":"/fixture/image","source_kind":"nix-descriptor"}}}"#,
        std::path::Path::new("/fixture/wrix"),
    )?;
    Ok(ProfileImageManifest::from_path(&path)?)
}

#[tokio::test]
async fn worker_preparation_denies_sccache_projections_of_canonical_authority() -> Result<()> {
    let fixture = Fixture::new().await?;
    let manifest = manifest(&fixture)?;
    let config = LoomTopConfig {
        sccache_dir: Some(fixture.association.integration().join("new-cache")),
        ..LoomTopConfig::default()
    };
    assert!(fixture.prepare(&manifest, &config).await.is_err());
    assert!(!config.sccache_dir.as_ref().unwrap().exists());
    fixture.check_host_surfaces()?;
    Ok(())
}

#[tokio::test]
async fn deterministic_feedback_in_an_isolated_worker_does_not_write_shared_authority() -> Result<()>
{
    let fixture = Fixture::new().await?;
    let manifest = manifest(&fixture)?;
    let _worker = fixture
        .prepare(&manifest, &LoomTopConfig::default())
        .await?;
    let verify = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_loom"))
            .arg("--workspace")
            .arg(&fixture.worker_path)
            .args(["gate", "verify", "--target", "test -f feedback-ok"])
            .env("LOOM_INSIDE", "1")
            .output()
    };
    let output = verify()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.check_host_surfaces()?;
    // Exercise the accepted feedback binding again, not a cached passing result.
    // Neither failure nor recovery may write the canonical host authorities.
    std::fs::remove_file(fixture.worker_path.join("feedback-ok"))?;
    let rejected = verify()?;
    assert!(!rejected.status.success(), "{rejected:?}");
    fixture.check_host_surfaces()?;
    std::fs::write(fixture.worker_path.join("feedback-ok"), "available\n")?;
    let recovered = verify()?;
    assert!(recovered.status.success(), "{recovered:?}");
    fixture.check_host_surfaces()?;
    Ok(())
}

#[tokio::test]
async fn worker_preparation_and_config_writer_preserve_host_surfaces_without_marker_transfer()
-> Result<()> {
    let fixture = Fixture::new().await?;
    let manifest = manifest(&fixture)?;
    let worker = fixture
        .prepare(&manifest, &LoomTopConfig::default())
        .await?;
    runtime::Config::write(worker.scratch.path(), &worker.spawn)?;
    assert!(
        worker
            .spawn
            .env
            .iter()
            .any(|(key, value)| key == "LOOM_INSIDE" && value == "1")
    );
    assert!(!fixture.worker_path.join(".loom/integration").exists());
    assert!(!fixture.worker_path.join(".loom/marker.json").exists());
    fixture.check_host_surfaces()?;
    Ok(())
}
