//! Explicit fixture contracts and bindings, resolved through the real package loader.

#[cfg(test)]
pub use fixtures::{bind, candidates, resolved, write_contract};

#[cfg(test)]
mod fixtures {
    use loom_driver::bd::Bead;
    use loom_driver::identifier::SpecLabel;
    use loom_protocol::acceptance::{Candidates, METADATA_KEY, References};
    use std::fmt::Write as _;
    use std::path::Path;

    pub fn write_contract(workspace: &Path, label: &SpecLabel, requirement: &str) {
        std::fs::create_dir_all(workspace.join("specs")).unwrap();
        std::fs::create_dir_all(workspace.join("docs")).unwrap();
        std::fs::write(
        workspace.join(format!("specs/{label}.md")),
        format!(
            "# {label}\n\n## Success Criteria\n\n- {requirement} [test?](fixture_worker_contract)\n"
        ),
    )
    .unwrap();
        let index = workspace.join("docs/README.md");
        let mut text = match std::fs::read_to_string(&index) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => panic!("read fixture index: {error}"),
        };
        let link = format!("../specs/{label}.md");
        if !text.contains(&link) {
            writeln!(text, "\n- [{label}]({link})").unwrap();
            std::fs::write(index, text).unwrap();
        }
    }

    pub fn candidates(workspace: &Path, label: &SpecLabel) -> Candidates {
        let snapshot = loom_gate::acceptance::load(workspace).unwrap();
        let references = snapshot
            .obligations()
            .filter(|obligation| obligation.reference().spec_label() == label)
            .map(|obligation| obligation.reference().clone())
            .collect();
        Candidates::Implementation {
            criteria: References::new(references).unwrap(),
        }
    }

    pub fn bind(workspace: &Path, label: &SpecLabel, bead: &mut Bead) {
        bead.metadata.insert(
            METADATA_KEY.to_owned(),
            serde_json::Value::String(
                serde_json::to_string(&candidates(workspace, label)).unwrap(),
            ),
        );
    }

    pub fn resolved(
        workspace: &Path,
        label: &SpecLabel,
    ) -> loom_protocol::acceptance::dispatch::Acceptance {
        let Candidates::Implementation { criteria } = candidates(workspace, label) else {
            panic!("fixture contract must produce implementation candidates")
        };
        loom_protocol::acceptance::dispatch::Acceptance::Implementation(
            loom_gate::acceptance::load(workspace)
                .unwrap()
                .resolve(&criteria)
                .unwrap(),
        )
    }
}
