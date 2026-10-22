#[cfg(test)]
mod cases {
    use super::super::*;

    /// Real Cargo metadata must not acquire a compiling verifier's allocation.
    #[test]
    fn inventory_metadata_uses_no_generated_cargo_target() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("src")).unwrap();
        std::fs::write(fixture.path().join("src/lib.rs"), "").unwrap();
        let manifest = fixture.path().join("Cargo.toml");
        std::fs::write(
            &manifest,
            "[package]\nname='inventory-discovery'\nversion='0.1.0'\n",
        )
        .unwrap();
        for _ in 0..2 {
            let bytes = command(
                fixture.path(),
                Tool::Cargo,
                &["metadata", "--offline", "--format-version=1"],
            )
            .unwrap();
            let metadata: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                metadata["target_directory"],
                "/dev/null/loom-cargo-discovery"
            );
            assert!(!fixture.path().join("target").exists());
        }
        std::fs::write(&manifest, "invalid manifest").unwrap();
        assert!(matches!(
            command(fixture.path(), Tool::Cargo, &["metadata", "--offline"]),
            Err(Error::Discovery { .. })
        ));
        assert!(!fixture.path().join("target").exists());
    }
}
