use loom_driver::agent::{ProtocolError, SpawnConfig};
use loom_skills::disclosure::DisclosureMode;
use loom_skills::registry::MaterializedRegistry;

pub(crate) trait NativeRegistrar {
    fn register(registry: &MaterializedRegistry) -> Result<(), ProtocolError>;
}

pub(crate) struct NoNativeRegistrar;

impl NativeRegistrar for NoNativeRegistrar {
    fn register(_registry: &MaterializedRegistry) -> Result<(), ProtocolError> {
        Err(ProtocolError::Unsupported)
    }
}

pub(crate) fn register_native_skills<R: NativeRegistrar>(
    config: &SpawnConfig,
) -> Result<(), ProtocolError> {
    let Some(skills) = config.skills.as_ref() else {
        return Ok(());
    };
    if skills.disclosure() != DisclosureMode::Native {
        return Ok(());
    }
    R::register(skills.registry())
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use loom_driver::agent::{ImageSourceKind, RePinContent, SpawnConfig};
    use loom_skills::registry::RegisteredSkills;

    use super::*;

    struct FailingRegistrar;

    impl NativeRegistrar for FailingRegistrar {
        fn register(_registry: &MaterializedRegistry) -> Result<(), ProtocolError> {
            Err(ProtocolError::Io(io::Error::other(
                "native registrar failed",
            )))
        }
    }

    struct CountingRegistrar;

    static CALLS: AtomicUsize = AtomicUsize::new(0);

    impl NativeRegistrar for CountingRegistrar {
        fn register(_registry: &MaterializedRegistry) -> Result<(), ProtocolError> {
            CALLS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn config(disclosure: DisclosureMode) -> SpawnConfig {
        SpawnConfig {
            image_ref: "localhost/wrix-test:pi".into(),
            image_source: PathBuf::from("/nix/store/zzz-wrix-test.tar"),
            image_source_kind: Some(ImageSourceKind::NixDescriptor),
            wrix_launcher: None,
            profile_config: None,
            workspace: PathBuf::from("/workspace"),
            env: vec![],
            mounts: vec![],
            initial_prompt: "prompt".into(),
            agent_args: vec![],
            repin: RePinContent {
                orientation: String::new(),
                pinned_context: String::new(),
                partial_bodies: vec![],
            },
            skills: Some(RegisteredSkills::new(
                MaterializedRegistry::new(vec![]),
                disclosure,
            )),
            event_metadata: None,
            scratch_dir: PathBuf::from("/workspace/.loom/scratch/test"),
            model_id: None,
            model: None,
            thinking_level: None,
            observers: Default::default(),
            output_limits: None,
            shutdown_grace: None,
            denied_tools: Vec::new(),
            handshake_timeout: None,
            stall_warn_interval: None,
            launcher_env: Vec::new(),
        }
    }

    #[test]
    fn native_skill_registration_failure_is_fatal() {
        let err = register_native_skills::<FailingRegistrar>(&config(DisclosureMode::Native))
            .expect_err("native registrar failure propagates");
        assert!(err.to_string().contains("io failure"), "{err}");
    }

    #[test]
    fn prompt_disclosure_skips_native_registrar() {
        CALLS.store(0, Ordering::SeqCst);
        register_native_skills::<CountingRegistrar>(&config(DisclosureMode::Prompt))
            .expect("prompt disclosure skips registrar");
        assert_eq!(CALLS.load(Ordering::SeqCst), 0);
    }
}
