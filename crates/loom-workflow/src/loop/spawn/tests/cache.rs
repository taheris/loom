#![cfg(test)]

use std::fs;

use anyhow::Result;

use super::*;

#[test]
fn worker_sccache_configuration_serializes_one_shared_writable_cache() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let operator = root.join("operator");
    fs::create_dir_all(operator.join(".git"))?;
    fs::create_dir_all(operator.join(".loom/integration/.git"))?;
    let manifest = three_profile_manifest(root);
    let config = LoomTopConfig {
        sccache_dir: Some(root.join("shared-worker-cache")),
        ..LoomTopConfig::default()
    };
    let mount = sccache_mount(&config)?.expect("configured worker cache mount");
    let mut spawns = Vec::new();
    for id in ["lm-first", "lm-second"] {
        let workspace = operator.join(".loom/beads").join(id);
        fs::create_dir_all(workspace.join(".git"))?;
        let spawn = build_spawn_config_from_manifest(
            &manifest,
            &bead_with_labels(id, &["profile:rust"]),
            None,
            &base(),
            AgentRuntime::Pi,
            workspace,
            "Fixture".into(),
            root.join(id),
            config.container_sccache_env(),
            vec![],
            vec![mount.clone()],
            vec![],
        )?;
        let wire = spawn.to_wrix_json()?;
        let decoded = SpawnConfig::from_wrix_slice(&serde_json::to_vec(&wire)?)?;
        assert_eq!(decoded.mounts, vec![mount.clone()]);
        assert!(!decoded.mounts[0].read_only);
        for key in ["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER"] {
            let values = decoded
                .env
                .iter()
                .filter(|(name, _)| name == key)
                .collect::<Vec<_>>();
            assert_eq!(values.len(), 1);
            assert_eq!(values[0].1, "sccache");
        }
        let locations = decoded
            .env
            .iter()
            .filter(|(name, _)| name == "SCCACHE_DIR")
            .collect::<Vec<_>>();
        assert_eq!(locations.len(), 1);
        assert_eq!(std::path::Path::new(&locations[0].1), mount.container_path);
        spawns.push(decoded);
    }
    assert_ne!(spawns[0].workspace, spawns[1].workspace);
    assert_eq!(spawns[0].mounts, spawns[1].mounts);
    assert!(mount.host_path.is_dir());
    Ok(())
}
