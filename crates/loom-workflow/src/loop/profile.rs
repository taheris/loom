use loom_driver::bd::Label;
use loom_driver::identifier::ProfileName;
use loom_driver::profile_manifest::{ImageEntry, ProfileError, ProfileImageManifest};

/// Built-in profile name used when neither `[phase.run]` nor
/// `[phase.default]` declares a profile in `<workspace>/loom.toml`.
/// Matches `lib/sandbox/profiles.nix`.
pub const DEFAULT_PROFILE: &str = "base";

/// Resolve the [`ProfileName`] for a bead.
///
/// Order of precedence (highest first):
/// 1. CLI `--profile` override (`override_`).
/// 2. The first `profile:X` label on the bead.
/// 3. `phase_default` — the per-phase fallback resolved by
///    [`loom_driver::config::LoomConfig::agent_for`] for [`Phase::Run`]
///    (which itself walks `[phase.run]` → `[phase.default]` → built-in
///    `base`).
///
/// Pure function — the driver hands in the labels it already pulled from
/// `bd show`.
///
/// [`Phase::Run`]: loom_driver::config::Phase::Run
pub fn resolve_profile(
    bead_labels: &[Label],
    override_: Option<&ProfileName>,
    phase_default: &ProfileName,
) -> ProfileName {
    if let Some(p) = override_ {
        return p.clone();
    }
    bead_labels
        .iter()
        .find_map(Label::profile_name)
        .unwrap_or_else(|| phase_default.clone())
}

/// Resolve a bead to its [`ImageEntry`] via the parsed
/// [`ProfileImageManifest`].
///
/// Combines [`resolve_profile`] (precedence: CLI override → bead label →
/// phase default) with [`ProfileImageManifest::lookup`]. A missing
/// manifest entry surfaces as [`ProfileError::UnknownProfile`] — there is
/// no silent fallback to `base` once the resolved name lands in the
/// manifest, per `specs/harness.md` § Profile-Image Manifest.
pub fn resolve_profile_image<'a>(
    manifest: &'a ProfileImageManifest,
    bead_labels: &[Label],
    override_: Option<&ProfileName>,
    phase_default: &ProfileName,
) -> Result<&'a ImageEntry, ProfileError> {
    let name = resolve_profile(bead_labels, override_, phase_default);
    manifest.lookup(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn labels(items: &[&str]) -> Vec<Label> {
        items.iter().map(|s| Label::new(*s)).collect()
    }

    fn base() -> ProfileName {
        ProfileName::new(DEFAULT_PROFILE)
    }

    fn write_manifest(dir: &std::path::Path, body: &str) -> PathBuf {
        let path = dir.join("profile-images.json");
        std::fs::write(&path, body).expect("write manifest");
        path
    }

    fn three_profile_manifest(dir: &std::path::Path) -> ProfileImageManifest {
        let body = r#"{
          "base":   { "ref": "localhost/wrix-base:abc",   "source": "/nix/store/aaa-image-base" },
          "rust":   { "ref": "localhost/wrix-rust:def",   "source": "/nix/store/bbb-image-rust" },
          "python": { "ref": "localhost/wrix-python:ghi", "source": "/nix/store/ccc-image-python" }
        }"#;
        let path = write_manifest(dir, body);
        ProfileImageManifest::from_path(&path).expect("parse manifest")
    }

    #[test]
    fn resolve_profile_reads_label() {
        let labels = labels(&["spec:harness", "profile:rust"]);
        let p = resolve_profile(&labels, None, &base());
        assert_eq!(p, ProfileName::new("rust"));
    }

    #[test]
    fn resolve_profile_falls_back_to_phase_default_without_label() {
        let labels = labels(&["spec:harness"]);
        let p = resolve_profile(&labels, None, &ProfileName::new("python"));
        assert_eq!(p, ProfileName::new("python"));
    }

    #[test]
    fn resolve_profile_uses_override() {
        let labels = labels(&["profile:rust"]);
        let override_ = ProfileName::new("python");
        let p = resolve_profile(&labels, Some(&override_), &base());
        assert_eq!(p, ProfileName::new("python"));
    }

    #[test]
    fn resolve_profile_first_matching_label_wins() {
        let labels = labels(&["profile:rust", "profile:python"]);
        let p = resolve_profile(&labels, None, &base());
        assert_eq!(p, ProfileName::new("rust"));
    }

    #[test]
    fn resolve_profile_image_uses_manifest_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = three_profile_manifest(dir.path());
        let labels = labels(&["spec:harness", "profile:rust"]);
        let entry = resolve_profile_image(&manifest, &labels, None, &base()).expect("resolve");
        assert_eq!(entry.r#ref, "localhost/wrix-rust:def");
        assert_eq!(entry.source, PathBuf::from("/nix/store/bbb-image-rust"));
    }

    /// FR5: `--profile` beats both `profile:X` labels and the phase default.
    /// Two beads with the same labels, dispatched once with no override and
    /// once with an override, must resolve to two different manifest entries.
    #[test]
    fn resolve_profile_image_cli_override_wins_over_label() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = three_profile_manifest(dir.path());
        let labels = labels(&["spec:harness", "profile:rust"]);

        let no_override = resolve_profile_image(&manifest, &labels, None, &base()).expect("rust");
        let with_override = resolve_profile_image(
            &manifest,
            &labels,
            Some(&ProfileName::new("python")),
            &base(),
        )
        .expect("python");
        assert_eq!(no_override.r#ref, "localhost/wrix-rust:def");
        assert_eq!(with_override.r#ref, "localhost/wrix-python:ghi");
        assert_ne!(no_override.r#ref, with_override.r#ref);
        assert_ne!(no_override.source, with_override.source);
    }

    /// Missing manifest entry surfaces as `UnknownProfile` carrying the
    /// resolved name and the manifest path — no silent fallback. Matches
    /// the spec contract for per-bead dispatch (§ Profile-Image Manifest).
    #[test]
    fn resolve_profile_image_missing_entry_returns_unknown_profile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let body = r#"{ "base": { "ref": "r", "source": "/s" } }"#;
        let path = write_manifest(dir.path(), body);
        let manifest = ProfileImageManifest::from_path(&path).expect("parse");
        let labels = labels(&["profile:rust"]);
        let err = resolve_profile_image(&manifest, &labels, None, &base())
            .expect_err("expected unknown profile");
        match err {
            ProfileError::UnknownProfile {
                name,
                manifest_path,
            } => {
                assert_eq!(name, ProfileName::new("rust"));
                assert_eq!(manifest_path, path);
            }
            other => panic!("expected UnknownProfile, got {other:?}"),
        }
    }
}
