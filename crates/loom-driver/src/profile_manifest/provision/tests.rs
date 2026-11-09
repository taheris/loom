#![cfg(test)]

use super::*;

fn entry() -> Entry {
    serde_json::from_value(serde_json::json!({
        "ref":"selected", "source_kind":"nix-descriptor",
        "realization": {
            "path":"/nix/store/00000000000000000000000000000000-selected.json",
            "derivation":"/nix/store/11111111111111111111111111111111-selected.json.drv"
        }
    }))
    .unwrap()
}

#[test]
fn recipe_output_binding_requires_exactly_the_selected_output() {
    let selected = entry();
    let correct = format!("{}\n", selected.realization.path.display());
    assert!(check_output_binding(&selected, correct.as_bytes()).is_ok());
    for invalid in [
        "",
        "/other\n",
        "/nix/store/00000000000000000000000000000000-other.json\n",
    ] {
        assert!(check_output_binding(&selected, invalid.as_bytes()).is_err());
    }
    assert!(check_output_binding(&selected, format!("{correct}{correct}").as_bytes()).is_err());
    assert!(check_output_binding(&selected, &[255]).is_err());
}

#[test]
fn factory_output_rejects_partial_results_and_mutable_manifests() {
    for document in [b"{".as_slice(), b"[]", b"[{\"outputs\":{}}]"] {
        assert!(matches!(
            store_catalogue(document),
            Err(ProvisionError::Output { .. })
        ));
    }
    let directory = tempfile::tempdir().unwrap();
    let manifest = directory.path().join("mutable.json");
    std::fs::write(&manifest, "{}").unwrap();
    let result = serde_json::to_vec(&serde_json::json!([{"outputs":{"out":manifest}}])).unwrap();
    assert!(matches!(
        store_catalogue(&result),
        Err(ProvisionError::Output { .. })
    ));
}

#[tokio::test]
async fn retired_environment_cannot_bypass_authenticated_discovery() {
    const CHILD: &str = "LOOM_TEST_RETIRED_PROFILE_ENV_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let workspace = Path::new("/not-a-profile-factory-checkout");
        let policy =
            RepoGitPolicy::resolve(workspace, "unused-wrix".into(), crate::git::KeyMode::Host)
                .unwrap();
        assert!(
            bootstrap_loop(workspace, &LoomConfig::default(), policy)
                .await
                .is_err()
        );
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let manifest = directory.path().join("images.json");
    std::fs::write(&manifest, r#"{"rust":{"pi":{"ref":"selected","source":"/selected/image","source_kind":"nix-descriptor","profile_config":"/selected/config"}}}"#).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "profile_manifest::provision::tests::retired_environment_cannot_bypass_authenticated_discovery"])
        .env(CHILD,"1")
        .env("LOOM_PROFILES_MANIFEST",&manifest)
        .env("LOOM_PROVISIONED_PROFILES_MANIFEST",&manifest)
        .env("LOOM_NATIVE_PREFLIGHT_MANIFEST",&manifest)
        .env("LOOM_PROFILE_CATALOGUE",&manifest)
        .output().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}
