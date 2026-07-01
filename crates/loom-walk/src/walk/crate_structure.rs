//! Crate-structure sentinel: the target v1 workspace member set from
//! the loom-harness spec must match the root manifest exactly, and each
//! member must expose its canonical entry source.

use std::collections::BTreeSet;
use std::path::Path;

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "crate_structure_includes_loom_tune — the target v1 workspace members must match exactly and expose canonical entries";

struct CrateSpec {
    name: &'static str,
    entries: &'static [&'static str],
}

const CRATES: &[CrateSpec] = &[
    CrateSpec {
        name: "loom",
        entries: &["src/main.rs"],
    },
    CrateSpec {
        name: "loom-driver",
        entries: &["src/lib.rs"],
    },
    CrateSpec {
        name: "loom-events",
        entries: &["src/lib.rs"],
    },
    CrateSpec {
        name: "loom-llm",
        entries: &["src/lib.rs"],
    },
    CrateSpec {
        name: "loom-skills",
        entries: &["src/lib.rs"],
    },
    CrateSpec {
        name: "loom-tune",
        entries: &["src/lib.rs"],
    },
    CrateSpec {
        name: "loom-render",
        entries: &["src/lib.rs"],
    },
    CrateSpec {
        name: "loom-agent",
        entries: &["src/lib.rs"],
    },
    CrateSpec {
        name: "loom-direct-runner",
        entries: &["src/lib.rs", "src/main.rs"],
    },
    CrateSpec {
        name: "loom-gate",
        entries: &["src/lib.rs"],
    },
    CrateSpec {
        name: "loom-protocol",
        entries: &["src/lib.rs"],
    },
    CrateSpec {
        name: "loom-workflow",
        entries: &["src/lib.rs"],
    },
    CrateSpec {
        name: "loom-templates",
        entries: &["src/lib.rs"],
    },
    CrateSpec {
        name: "loom-test-support",
        entries: &["src/lib.rs"],
    },
    CrateSpec {
        name: "loom-walk",
        entries: &["src/main.rs"],
    },
];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let crates = root.join("crates");
    let mut violations = Vec::new();

    check_workspace_members(&root, &mut violations);
    for spec in CRATES {
        check_crate(&crates, spec, &mut violations);
    }

    verdict_from(RULE, violations)
}

fn check_workspace_members(root: &Path, violations: &mut Vec<String>) {
    let manifest = root.join("Cargo.toml");
    let Some(body) = read_to_string(&manifest) else {
        violations.push("Cargo.toml:1 workspace manifest not readable".to_string());
        return;
    };
    let value = match toml::from_str::<toml::Value>(&body) {
        Ok(value) => value,
        Err(err) => {
            violations.push(format!(
                "Cargo.toml:1 workspace manifest is not TOML: {err}"
            ));
            return;
        }
    };
    let Some(members) = value
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
    else {
        violations.push("Cargo.toml:1 missing [workspace].members array".to_string());
        return;
    };

    let expected = expected_member_paths();
    let mut actual = BTreeSet::new();
    for member in members {
        if let Some(member) = member.as_str() {
            actual.insert(member.to_string());
        } else {
            violations
                .push("Cargo.toml:1 [workspace].members contains a non-string entry".to_string());
        }
    }
    for missing in expected.difference(&actual) {
        violations.push(format!(
            "Cargo.toml:1 workspace member `{missing}` missing from fixed crate set",
        ));
    }
    for extra in actual.difference(&expected) {
        violations.push(format!(
            "Cargo.toml:1 workspace member `{extra}` is not in the fixed crate set",
        ));
    }
}

fn expected_member_paths() -> BTreeSet<String> {
    CRATES
        .iter()
        .map(|spec| format!("crates/{}", spec.name))
        .collect()
}

fn check_crate(crates: &Path, spec: &CrateSpec, violations: &mut Vec<String>) {
    let dir = crates.join(spec.name);
    if !dir.is_dir() {
        violations.push(format!("crates/{}:1 missing crate directory", spec.name));
        return;
    }
    let manifest = dir.join("Cargo.toml");
    if !manifest.is_file() {
        violations.push(format!(
            "crates/{}/Cargo.toml:1 missing manifest",
            spec.name
        ));
    }
    for entry in spec.entries {
        let entry_path = dir.join(entry);
        if !entry_path.is_file() {
            violations.push(format!(
                "crates/{}/{}:1 missing entry source",
                spec.name, entry
            ));
        }
    }
}
