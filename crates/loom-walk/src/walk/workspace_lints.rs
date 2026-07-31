//! RS-3: the workspace owns one concrete lint policy. The root
//! `Cargo.toml` defines the required Rust and Clippy levels, permits only
//! the reviewed Clippy allowlist, and every member crate inherits that
//! policy with `[lints] workspace = true`.

use std::path::Path;

use toml::Value;

use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "workspace_lints — root enforces the reviewed lint policy; every member uses [lints] workspace = true";

#[derive(Clone, Copy)]
struct RequiredLint {
    name: &'static str,
    level: &'static str,
    priority: Option<i64>,
}

const REQUIRED_RUST_LINTS: &[RequiredLint] = &[
    RequiredLint {
        name: "unsafe_code",
        level: "forbid",
        priority: None,
    },
    RequiredLint {
        name: "unused_must_use",
        level: "deny",
        priority: None,
    },
];

const REQUIRED_CLIPPY_LINTS: &[RequiredLint] = &[
    RequiredLint {
        name: "all",
        level: "warn",
        priority: Some(-1),
    },
    RequiredLint {
        name: "pedantic",
        level: "warn",
        priority: Some(-1),
    },
    RequiredLint {
        name: "nursery",
        level: "warn",
        priority: Some(-1),
    },
    RequiredLint {
        name: "unwrap_used",
        level: "deny",
        priority: None,
    },
    RequiredLint {
        name: "expect_used",
        level: "deny",
        priority: None,
    },
    RequiredLint {
        name: "panic",
        level: "deny",
        priority: None,
    },
    RequiredLint {
        name: "todo",
        level: "deny",
        priority: None,
    },
    RequiredLint {
        name: "unimplemented",
        level: "deny",
        priority: None,
    },
    RequiredLint {
        name: "unreachable",
        level: "deny",
        priority: None,
    },
    RequiredLint {
        name: "dbg_macro",
        level: "deny",
        priority: None,
    },
    RequiredLint {
        name: "print_stdout",
        level: "deny",
        priority: None,
    },
    RequiredLint {
        name: "print_stderr",
        level: "deny",
        priority: None,
    },
    RequiredLint {
        name: "allow_attributes",
        level: "warn",
        priority: None,
    },
    RequiredLint {
        name: "must_use_candidate",
        level: "allow",
        priority: None,
    },
    RequiredLint {
        name: "option_if_let_else",
        level: "allow",
        priority: None,
    },
    RequiredLint {
        name: "too_many_lines",
        level: "allow",
        priority: None,
    },
    RequiredLint {
        name: "use_self",
        level: "allow",
        priority: None,
    },
];

const REVIEWED_CLIPPY_ALLOWS: &[&str] = &[
    "must_use_candidate",
    "option_if_let_else",
    "too_many_lines",
    "use_self",
];

const LIBRARY_CRATES: &[&str] = &[
    "loom-driver",
    "loom-events",
    "loom-llm",
    "loom-skill",
    "loom-tune",
    "loom-render",
    "loom-agent",
    "loom-direct-runner",
    "loom-gate",
    "loom-protocol",
    "loom-workflow",
    "loom-templates",
    "loom-test-support",
    "loom-walk",
];

const BINARY_CRATE: &str = "loom";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let workspace_manifest = root.join("Cargo.toml");
    let mut violations = Vec::new();

    match read_to_string(&workspace_manifest) {
        Some(body) => check_workspace_policy(&body, &mut violations),
        None => violations.push("Cargo.toml:1 workspace manifest not readable".to_string()),
    }

    let crates_root = root.join("crates");
    for name in std::iter::once(&BINARY_CRATE).chain(LIBRARY_CRATES.iter()) {
        check_member_inherits(&crates_root, name, &mut violations);
    }

    verdict_from(RULE, violations)
}

fn check_workspace_policy(body: &str, violations: &mut Vec<String>) {
    let manifest = match toml::from_str::<Value>(body) {
        Ok(manifest) => manifest,
        Err(error) => {
            violations.push(format!("Cargo.toml:1 invalid TOML: {error}"));
            return;
        }
    };
    let Some(lints) = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("lints"))
    else {
        violations.push("Cargo.toml:1 [workspace.lints] policy missing".to_string());
        return;
    };

    check_required_section(
        lints.get("rust"),
        "workspace.lints.rust",
        REQUIRED_RUST_LINTS,
        violations,
    );
    let clippy = lints.get("clippy");
    check_required_section(
        clippy,
        "workspace.lints.clippy",
        REQUIRED_CLIPPY_LINTS,
        violations,
    );
    check_clippy_allowlist(clippy, violations);
}

fn check_required_section(
    section: Option<&Value>,
    section_name: &str,
    required: &[RequiredLint],
    violations: &mut Vec<String>,
) {
    let Some(table) = section.and_then(Value::as_table) else {
        violations.push(format!("Cargo.toml:1 [{section_name}] section missing"));
        return;
    };

    for setting in required {
        let Some(value) = table.get(setting.name) else {
            violations.push(format!(
                "Cargo.toml:1 [{section_name}] `{}` must be `{}`",
                setting.name, setting.level
            ));
            continue;
        };
        if lint_level(value) != Some(setting.level) {
            violations.push(format!(
                "Cargo.toml:1 [{section_name}] `{}` must have level `{}`",
                setting.name, setting.level
            ));
        }
        if let Some(priority) = setting.priority
            && lint_priority(value) != Some(priority)
        {
            violations.push(format!(
                "Cargo.toml:1 [{section_name}] `{}` must have priority {priority}",
                setting.name
            ));
        }
    }
}

fn check_clippy_allowlist(section: Option<&Value>, violations: &mut Vec<String>) {
    let Some(table) = section.and_then(Value::as_table) else {
        return;
    };
    for (name, value) in table {
        if lint_level(value) == Some("allow") && !REVIEWED_CLIPPY_ALLOWS.contains(&name.as_str()) {
            violations.push(format!(
                "Cargo.toml:1 [workspace.lints.clippy] `{name} = \"allow\"` is not in the reviewed allowlist"
            ));
        }
    }
}

fn lint_level(value: &Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value
            .as_table()
            .and_then(|table| table.get("level"))
            .and_then(Value::as_str)
    })
}

fn lint_priority(value: &Value) -> Option<i64> {
    value
        .as_table()
        .and_then(|table| table.get("priority"))
        .and_then(Value::as_integer)
}

fn check_member_inherits(crates_root: &Path, name: &str, violations: &mut Vec<String>) {
    let manifest = crates_root.join(name).join("Cargo.toml");
    let Some(body) = read_to_string(&manifest) else {
        violations.push(format!("crates/{name}/Cargo.toml:1 manifest not readable"));
        return;
    };
    let inherits = toml::from_str::<Value>(&body)
        .ok()
        .and_then(|manifest| manifest.get("lints").cloned())
        .and_then(|lints| lints.get("workspace").and_then(Value::as_bool))
        == Some(true);
    if !inherits {
        violations.push(format!(
            "crates/{name}/Cargo.toml:1 missing `[lints] workspace = true`",
        ));
    }
}
