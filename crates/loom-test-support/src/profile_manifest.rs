//! Complete launch manifests for tests that mock Wrix rather than manifest ingestion.

use std::io;
use std::path::Path;

use serde_json::{Value, json};

/// Write a fixture runtime matrix with a raw launcher and matching profile config for every image.
/// Explicit launcher/config fields in the image fixture take precedence.
///
/// # Errors
/// Returns an error for malformed fixture JSON or failed writes.
pub fn write(path: &Path, images: impl AsRef<str>, launcher: &Path) -> io::Result<()> {
    let mut matrix: Value = serde_json::from_str(images.as_ref())?;
    let profiles = matrix
        .as_object_mut()
        .ok_or_else(|| io::Error::other("expected profile matrix"))?;
    for (profile, runtimes) in profiles {
        let runtimes = runtimes
            .as_object_mut()
            .ok_or_else(|| io::Error::other("expected runtime map"))?;
        for (runtime, image) in runtimes {
            let image = image
                .as_object_mut()
                .ok_or_else(|| io::Error::other("expected image entry"))?;
            image.entry("launcher").or_insert_with(|| json!(launcher));
            if !image.contains_key("profile_config") {
                let config = path.with_file_name(format!("{profile}-{runtime}-profile.json"));
                std::fs::write(
                    &config,
                    json!({"image": image, "agent": {"kind": runtime}}).to_string(),
                )?;
                image.insert("profile_config".into(), json!(config));
            }
        }
    }
    std::fs::write(path, matrix.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_pairs_each_runtime_with_its_launcher_and_profile_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        write(
            &path,
            r#"{"base":{"claude":{"ref":"claude-image"},"pi":{"ref":"pi-image"}}}"#,
            Path::new("/raw/wrix"),
        )
        .unwrap();
        let matrix: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for runtime in ["claude", "pi"] {
            let image = &matrix["base"][runtime];
            assert_eq!(image["launcher"], "/raw/wrix");
            let config: Value = serde_json::from_slice(
                &std::fs::read(image["profile_config"].as_str().unwrap()).unwrap(),
            )
            .unwrap();
            assert_eq!(config["agent"]["kind"], runtime);
            assert_eq!(config["image"]["ref"], format!("{runtime}-image"));
        }
    }
}
