use super::*;

fn document() -> serde_json::Value {
    serde_json::json!({"schema":1,"profiles":{"rust":{"pi":{
        "ref":"selected","source_kind":"nix-descriptor","realization":{
            "path":"/nix/store/00000000000000000000000000000000-selected.json",
            "derivation":"/nix/store/11111111111111111111111111111111-selected.json.drv"
        }
    }}}})
}

fn parse(value: &serde_json::Value) -> Result<Catalogue, ProfileError> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalogue.json");
    std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    Catalogue::from_path(&path)
}

#[test]
fn catalogue_discovery_has_no_ready_image_or_implicit_realization() {
    let catalogue = parse(&document()).unwrap();
    let manifest =
        super::super::ProfileImageManifest::from_catalogue(std::sync::Arc::new(catalogue));
    let profile = ProfileName::new("rust").unwrap();
    assert_eq!(manifest.len(), 1);
    assert_eq!(manifest.declared_profiles().collect::<Vec<_>>(), [&profile]);
    assert_eq!(
        manifest
            .declared_runtimes(&profile)
            .unwrap()
            .collect::<Vec<_>>(),
        [AgentRuntime::Pi]
    );
    assert!(matches!(
        manifest.lookup(&profile, AgentRuntime::Pi),
        Err(ProfileError::NotProvisioned)
    ));
    assert!(matches!(
        manifest.lookup(&profile, AgentRuntime::Claude),
        Err(ProfileError::UnknownRuntimeForProfile { .. })
    ));
}

#[test]
fn catalogue_rejects_unsupported_schema_output_bearing_fields_and_unsafe_paths() {
    assert!(parse(&document()).is_ok());
    for (pointer, value) in [
        ("/schema", serde_json::json!(2)),
        ("/profiles", serde_json::json!({})),
        ("/profiles/rust", serde_json::json!({})),
        ("/profiles/rust/pi/ref", serde_json::json!("")),
        (
            "/profiles/rust/pi/source_kind",
            serde_json::json!("unsupported"),
        ),
        (
            "/profiles/rust/pi/realization/path",
            serde_json::json!("/tmp/manifest.json"),
        ),
        (
            "/profiles/rust/pi/realization/derivation",
            serde_json::json!("/nix/store/11111111111111111111111111111111-output"),
        ),
    ] {
        let mut invalid = document();
        *invalid.pointer_mut(pointer).unwrap() = value;
        assert!(parse(&invalid).is_err(), "{pointer}");
    }
    let mut invalid = document();
    invalid["profiles"]["rust"]["pi"]["source"] = serde_json::json!("/nix/store/image");
    assert!(parse(&invalid).is_err());
}

#[tokio::test]
async fn catalogue_cannot_materialize_without_explicit_caller_authority() {
    let manifest = super::super::ProfileImageManifest::from_catalogue(std::sync::Arc::new(
        parse(&document()).unwrap(),
    ));
    assert!(matches!(
        manifest
            .resolve(&ProfileName::new("rust").unwrap(), AgentRuntime::Pi)
            .await,
        Err(ProfileError::NotProvisioned)
    ));
}

#[test]
fn catalogue_schema_is_detected_without_environment_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalogue.json");
    std::fs::write(&path, serde_json::to_vec(&document()).unwrap()).unwrap();
    let manifest = super::super::ProfileImageManifest::from_path(&path).unwrap();
    assert_eq!(manifest.len(), 1);
    assert!(matches!(
        manifest.lookup(&ProfileName::new("rust").unwrap(), AgentRuntime::Pi),
        Err(ProfileError::NotProvisioned)
    ));
}
