//! Conformance coverage for batched metadata updates through the external bd shim.

use std::process::Command;

#[test]
fn bd_shim_batches_typed_metadata_and_preserves_existing_keys() {
    let state = tempfile::tempdir().unwrap();
    let invoke = |args: &[String]| {
        let output = Command::new(env!("CARGO_BIN_EXE_bd-shim"))
            .args(args)
            .env("BD_STATE_DIR", state.path())
            .env("BD_CREATE_ID", "lm-shim.1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    invoke(
        &[
            "create",
            "--silent",
            "--title",
            "fixture",
            "--description",
            "fixture",
            "--metadata",
            r#"{"existing": "preserved"}"#,
        ]
        .map(str::to_owned),
    );
    let mut args = vec!["update".to_owned(), "lm-shim.1".to_owned()];
    for key in 0..20 {
        args.extend(["--set-metadata".to_owned(), format!("key{key}={key}")]);
    }
    args.extend(["--set-metadata".to_owned(), "key0=true".to_owned()]);
    args.extend([
        "--set-metadata".to_owned(),
        "nested={\"paths\":[\"src/lib.rs\"]}".to_owned(),
    ]);
    invoke(&args);
    let metadata: serde_json::Value = serde_json::from_slice(
        &std::fs::read(state.path().join("lm-shim.1/metadata.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(metadata["existing"], "preserved");
    assert_eq!(metadata["key0"], true);
    for key in 1..20 {
        assert_eq!(metadata[format!("key{key}")], key);
    }
    assert_eq!(
        metadata["nested"],
        serde_json::json!({"paths": ["src/lib.rs"]})
    );
}
