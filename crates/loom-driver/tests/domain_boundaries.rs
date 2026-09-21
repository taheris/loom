//! Exercise the public configuration and Beads ingestion boundaries.
#[cfg(test)]
mod tests {
    use loom_driver::agent::AgentKind;
    use loom_driver::bd::{Bead, DependencySnapshot, IssueType, Molecule, Priority, Status};
    use loom_driver::config::{BackendSettings, LoomConfig, Phase};
    use serde_json::{Value, json};

    fn bead_wire() -> Value {
        json!({"id": "lm-domain", "title": "boundary", "status": "open",
            "priority": 2, "issue_type": "task", "future_field": true})
    }

    #[test]
    fn beads_domains_preserve_recognized_wire_values() {
        for status in [
            "open",
            "in_progress",
            "blocked",
            "deferred",
            "closed",
            "pinned",
            "tombstone",
        ] {
            let mut wire = bead_wire();
            wire["status"] = status.into();
            let bead: Bead = serde_json::from_value(wire).unwrap();
            assert_eq!(bead.status, status.parse::<Status>().unwrap());
            assert_eq!(bead.status.to_string(), status);
            assert_eq!(serde_json::to_value(&bead).unwrap()["status"], status);
        }
        for issue_type in ["task", "bug", "feature", "epic", "chore"] {
            let mut wire = bead_wire();
            wire["issue_type"] = issue_type.into();
            let bead: Bead = serde_json::from_value(wire).unwrap();
            assert_eq!(bead.issue_type, issue_type.parse::<IssueType>().unwrap());
            assert_eq!(
                serde_json::to_value(&bead).unwrap()["issue_type"],
                issue_type
            );
        }
    }

    #[test]
    fn unknown_beads_statuses_and_types_are_rejected_not_collapsed() {
        for unknown in ["", "future", "OPEN", "in-progress"] {
            let mut wire = bead_wire();
            wire["status"] = unknown.into();
            assert!(serde_json::from_value::<Bead>(wire.clone()).is_err());
            assert!(serde_json::from_value::<Molecule>(wire).is_err());
            assert!(
                serde_json::from_value::<DependencySnapshot>(json!({"status": unknown})).is_err()
            );
            assert!(
                serde_json::from_value::<DependencySnapshot>(
                    json!({"status": "open", "dependencies": [
                        {"id": "lm-dep", "status": unknown, "dependency_type": "blocks"}
                    ]})
                )
                .is_err()
            );
            assert!(unknown.parse::<Status>().is_err());
            let mut wire = bead_wire();
            wire["issue_type"] = unknown.into();
            assert!(serde_json::from_value::<Bead>(wire).is_err());
            assert!(unknown.parse::<IssueType>().is_err());
        }
    }

    #[test]
    fn beads_and_configuration_priorities_share_the_checked_boundary() {
        for value in [-1, 5, 255, 256] {
            let mut wire = bead_wire();
            wire["priority"] = value.into();
            assert!(serde_json::from_value::<Bead>(wire).is_err());
            assert!(
                serde_json::from_value::<LoomConfig>(json!({"beads": {"priority": value}}))
                    .is_err()
            );
            assert!(LoomConfig::from_toml_str(&format!("[beads]\npriority = {value}\n")).is_err());
        }
        let config = LoomConfig::default();
        assert_eq!(config.beads.priority, Priority::P2);
        assert_eq!(config.beads.default_type, IssueType::Task);
        assert_eq!(
            serde_json::from_value::<LoomConfig>(json!({})).unwrap(),
            config
        );
        assert!(
            serde_json::from_value::<LoomConfig>(json!({"beads": {"default_type": "unknown"}}))
                .is_err()
        );
        for value in 0..=4 {
            let config = LoomConfig::from_toml_str(&format!(
                "[beads]\npriority = {value}\ndefault_type = 'bug'\n"
            ))
            .unwrap();
            assert_eq!(config.beads.priority.get(), value);
            assert_eq!(config.beads.default_type, IssueType::Bug);
        }
    }

    #[test]
    fn backend_override_carries_only_its_own_settings() {
        let config = LoomConfig::from_toml_str(
            "[phase.default]\nagent.backend='pi'\n[claude]\npost_result_grace_secs=7\n",
        )
        .unwrap();
        let mut selection = config.agent_for(Phase::Loop);
        assert_eq!(selection.kind(), AgentKind::Pi);
        assert!(selection.claude_settings().is_none());
        selection.backend = config.backend_settings(AgentKind::Claude);
        assert_eq!(selection.kind(), AgentKind::Claude);
        assert!(
            matches!(&selection.backend, BackendSettings::Claude(settings) if settings.post_result_grace_secs == 7)
        );
        selection.backend = config.backend_settings(AgentKind::Direct);
        assert_eq!(selection.kind(), AgentKind::Direct);
        assert!(selection.claude_settings().is_none());
    }
}
