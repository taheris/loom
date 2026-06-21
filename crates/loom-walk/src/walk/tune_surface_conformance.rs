//! Tune-surface conformance walk for `specs/harness.md`.

use std::collections::BTreeSet;

use super::cli_surface::{self, VariantShape};
use super::util::{read_to_string, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str = "tune_surface_conformance — `loom sync` is absent and `loom tune` matches specs/harness.md Tune Modes";
const SPEC: &str = "specs/harness.md";
const MAIN_RS: &str = "crates/loom/src/main.rs";
const EXPECTED_TUNE_ACTIONS: &[&str] = &["skill", "phase", "partial", "checker", "all"];
const EXPECTED_LEVELS: &[&str] = &["fast", "run", "full"];
const EXPECTED_PROPOSAL_FLAGS: &[&str] = &["dry-run", "seed"];

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let Some(spec_body) = read_to_string(&root.join(SPEC)) else {
        return verdict_from(RULE, vec![format!("{SPEC}:1 spec not readable")]);
    };
    let Some(main_body) = read_to_string(&root.join(MAIN_RS)) else {
        return verdict_from(
            RULE,
            vec![format!("{MAIN_RS}:1 binary entry point not found")],
        );
    };
    let main_file = match cli_surface::parse_file(&main_body, MAIN_RS) {
        Ok(file) => file,
        Err(e) => return verdict_from(RULE, vec![e]),
    };

    let mut violations = Vec::new();
    check_spec_tune_modes_present(&spec_body, &mut violations);
    check_top_level_commands(&main_file, &mut violations);
    check_tune_actions(&main_file, &mut violations);
    check_tune_levels(&main_file, &mut violations);
    check_proposal_args(&main_file, &mut violations);
    verdict_from(RULE, violations)
}

fn check_spec_tune_modes_present(spec_body: &str, violations: &mut Vec<String>) {
    if !spec_body
        .lines()
        .any(|line| line.starts_with("### Tune Modes"))
    {
        violations.push(format!("{SPEC}:1 missing `### Tune Modes` section"));
    }
}

fn check_top_level_commands(main_file: &syn::File, violations: &mut Vec<String>) {
    let commands = match cli_surface::enum_variant_names(main_file, "Command", MAIN_RS) {
        Ok(commands) => commands,
        Err(e) => {
            violations.push(e);
            return;
        }
    };
    if commands.contains("sync") {
        violations.push(format!(
            "{MAIN_RS}:1 forbidden top-level `loom sync` command is declared"
        ));
    }
    if !commands.contains("tune") {
        violations.push(format!(
            "{MAIN_RS}:1 required top-level `loom tune` command is missing"
        ));
    }
}

fn check_tune_actions(main_file: &syn::File, violations: &mut Vec<String>) {
    let actions = match cli_surface::enum_variant_names(main_file, "TuneAction", MAIN_RS) {
        Ok(actions) => actions,
        Err(e) => {
            violations.push(e);
            return;
        }
    };
    compare_set(
        "TuneAction subcommand",
        expected_set(EXPECTED_TUNE_ACTIONS),
        &actions,
        violations,
    );
    for forbidden in ["skills", "phases", "partials", "template", "templates"] {
        if actions.contains(forbidden) {
            violations.push(format!(
                "{MAIN_RS}:1 forbidden plural/template tune subcommand `{forbidden}` declared"
            ));
        }
    }
    match cli_surface::enum_variant_shape(main_file, "TuneAction", "checker", MAIN_RS) {
        Ok(VariantShape::Unit) => {}
        Ok(_) => violations.push(format!(
            "{MAIN_RS}:1 `loom tune checker` must be list-only and accept no proposal args"
        )),
        Err(e) => violations.push(e),
    }
}

fn check_tune_levels(main_file: &syn::File, violations: &mut Vec<String>) {
    let levels = match cli_surface::enum_variant_names(main_file, "TuneLevelArg", MAIN_RS) {
        Ok(levels) => levels,
        Err(e) => {
            violations.push(e);
            return;
        }
    };
    compare_set(
        "TuneLevelArg",
        expected_set(EXPECTED_LEVELS),
        &levels,
        violations,
    );
}

fn check_proposal_args(main_file: &syn::File, violations: &mut Vec<String>) {
    let surface_fields =
        match cli_surface::struct_field_names(main_file, "TuneSurfaceArgs", MAIN_RS) {
            Ok(fields) => fields,
            Err(e) => {
                violations.push(e);
                return;
            }
        };
    let all_fields = match cli_surface::struct_field_names(main_file, "TuneAllArgs", MAIN_RS) {
        Ok(fields) => fields,
        Err(e) => {
            violations.push(e);
            return;
        }
    };
    let expected_surface_fields = expected_set(&["level", "targets", "dry_run", "seed"]);
    let expected_all_fields = expected_set(&["level", "dry_run", "seed"]);
    compare_set(
        "TuneSurfaceArgs field",
        expected_surface_fields,
        &surface_fields,
        violations,
    );
    compare_set(
        "TuneAllArgs field",
        expected_all_fields,
        &all_fields,
        violations,
    );
    if all_fields.contains("targets") {
        violations.push(format!(
            "{MAIN_RS}:1 `loom tune all` must not accept target names after the level"
        ));
    }

    for struct_name in ["TuneSurfaceArgs", "TuneAllArgs"] {
        let flags = match cli_surface::struct_long_flags(main_file, struct_name, MAIN_RS) {
            Ok(flags) => flags,
            Err(e) => {
                violations.push(e);
                continue;
            }
        };
        compare_set(
            &format!("{struct_name} proposal flag"),
            expected_set(EXPECTED_PROPOSAL_FLAGS),
            &flags,
            violations,
        );
        for field in ["dry_run", "seed"] {
            match cli_surface::field_requires(main_file, struct_name, field, "level", MAIN_RS) {
                Ok(true) => {}
                Ok(false) => violations.push(format!(
                    "{MAIN_RS}:1 `{struct_name}.{field}` must require `level` so list commands stay read-only"
                )),
                Err(e) => violations.push(e),
            }
        }
    }
}

fn compare_set(
    label: &str,
    expected: BTreeSet<String>,
    actual: &BTreeSet<String>,
    violations: &mut Vec<String>,
) {
    for missing in expected.difference(actual) {
        violations.push(format!(
            "{SPEC} documents {label} `{missing}` but {MAIN_RS} does not declare it"
        ));
    }
    for extra in actual.difference(&expected) {
        violations.push(format!(
            "{MAIN_RS} declares {label} `{extra}` but {SPEC} does not document it"
        ));
    }
}

fn expected_set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}
