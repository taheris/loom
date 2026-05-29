use std::collections::BTreeMap;

use serde::Deserialize;

/// Named built-in parser that extracts per-target verdicts from a runner's
/// stdout. The set is closed by loom; consumers select one by name in
/// `[runner.<tier>.<name>] parse = "..."`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Parser {
    /// Rust `cargo test`/`nextest` `--message-format` JSON.
    LibtestJson,
    /// JUnit-XML reports (pytest, others).
    JunitXml,
    /// `nix build`'s per-derivation success/failure output.
    NixBuildStatus,
    /// One `{"target":"<name>","pass":bool,"evidence":"<msg>"}` per line on
    /// stdout.
    JsonLines,
    /// Single per-runner verdict from the process exit code.
    ExitCode,
}

/// One named runner under a tier: `[runner.<tier>.<name>]`. Describes how
/// to recognise its annotations, build the batch command, and parse
/// per-target verdicts out of the runner's stdout.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RunnerEntry {
    /// Regex over the annotation's target string. Annotations whose target
    /// matches are dispatched through this runner. When omitted, this
    /// runner is the tier default.
    #[serde(rename = "match")]
    pub match_regex: Option<String>,

    /// Command-line template. `{filter}` / `{targets}` substitute the
    /// joined-target string; `{capture_N}` substitutes a regex capture from
    /// the matched target.
    pub command: Option<String>,

    /// Per-target template applied to each matched annotation before
    /// joining. References `{name}` (full target) or `{capture_N}`.
    pub target: Option<String>,

    /// String inserted between formatted targets to build the joined-target
    /// substitution for `{filter}` / `{targets}`.
    pub join: Option<String>,

    /// Built-in parser that extracts per-target verdicts from the runner's
    /// stdout.
    pub parse: Option<Parser>,

    /// Repo-relative directory to run the command from. Overrides the
    /// tier-default `cwd`.
    pub cwd: Option<String>,
}

/// One tier block: `[runner.<tier>]` carries the tier-default `cwd` plus
/// the optional implicit-default-runner fields (`command`/`target`/`join`/
/// `parse`/`match`). Named runners declared as `[runner.<tier>.<name>]`
/// subtables collect into `runners`. A bare `[runner.<tier>] cwd = "..."`
/// block (no other fields, no subtables) sets only the tier-default cwd.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct RunnerTier {
    /// Default cwd for annotations in this tier whose matched runner does
    /// not set its own `cwd`. Also the cwd of the implicit default runner
    /// when one is declared at the tier level.
    pub cwd: Option<String>,

    /// Optional implicit-default-runner regex. When present, the tier
    /// block defines an unnamed default runner; the regex narrows which
    /// targets this default covers.
    #[serde(rename = "match")]
    pub match_regex: Option<String>,

    /// Implicit default runner's command template. When set, the tier
    /// block defines a default runner alongside (or instead of) any
    /// named subtables.
    pub command: Option<String>,

    /// Implicit default runner's per-target template.
    pub target: Option<String>,

    /// Implicit default runner's batch join separator.
    pub join: Option<String>,

    /// Implicit default runner's parser.
    pub parse: Option<Parser>,

    /// Named runners declared under this tier as `[runner.<tier>.<name>]`.
    #[serde(flatten)]
    pub runners: BTreeMap<String, RunnerEntry>,
}

impl RunnerTier {
    /// Build a [`RunnerEntry`] view of the implicit default runner when
    /// the tier block carries any runner-shaped scalar field; otherwise
    /// `None`. The tier-default `cwd` flows through when the default
    /// runner does not override.
    pub fn default_runner(&self) -> Option<RunnerEntry> {
        let has_runner_field = self.match_regex.is_some()
            || self.command.is_some()
            || self.target.is_some()
            || self.join.is_some()
            || self.parse.is_some();
        if !has_runner_field {
            return None;
        }
        Some(RunnerEntry {
            match_regex: self.match_regex.clone(),
            command: self.command.clone(),
            target: self.target.clone(),
            join: self.join.clone(),
            parse: self.parse,
            cwd: self.cwd.clone(),
        })
    }
}

/// The full `[runner.*]` table from `<workspace>/loom.toml`. Keys are
/// tier names (`test`, `check`, `system`, `judge`) matching the
/// `loom-gate::annotation::Tier` enum at the consumer end. Empty by
/// default, so configs without a `[runner]` block parse cleanly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct RunnerConfig(pub BTreeMap<String, RunnerTier>);

impl RunnerConfig {
    /// Look up a tier block by name.
    pub fn tier(&self, name: &str) -> Option<&RunnerTier> {
        self.0.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LoomConfig;

    /// The example reproduced verbatim from `specs/harness.md`
    /// § Configuration so drift between the parser and the spec example
    /// surfaces here.
    const SPEC_RUNNER_TOML: &str = r#"
[runner.test]
command = "cargo nextest run --manifest-path loom/Cargo.toml -E '{filter}' --message-format=libtest-json"
target  = "test({name})"
join    = " + "
parse   = "libtest-json"
cwd     = "."

[runner.check]
cwd = "loom"

[runner.system.nix]
match   = '^nix (build|run) \.#(\S+)$'
command = "nix build {targets}"
target  = ".#{capture_2}"
join    = " "
parse   = "nix-build-status"
cwd     = "."
"#;

    #[test]
    fn spec_example_parses_into_runner_config() {
        let cfg = LoomConfig::from_toml_str(SPEC_RUNNER_TOML).unwrap();

        let test = cfg.runner.tier("test").unwrap();
        let check = cfg.runner.tier("check").unwrap();
        let system = cfg.runner.tier("system").unwrap();

        assert_eq!(test.cwd.as_deref(), Some("."));
        assert_eq!(
            test.command.as_deref(),
            Some(
                "cargo nextest run --manifest-path loom/Cargo.toml -E '{filter}' --message-format=libtest-json"
            )
        );
        assert_eq!(test.target.as_deref(), Some("test({name})"));
        assert_eq!(test.join.as_deref(), Some(" + "));
        assert_eq!(test.parse, Some(Parser::LibtestJson));
        assert!(
            test.runners.is_empty(),
            "no [runner.test.<name>] subtables in the spec example"
        );

        let test_default = test.default_runner().unwrap();
        assert_eq!(test_default.cwd.as_deref(), Some("."));
        assert_eq!(test_default.parse, Some(Parser::LibtestJson));

        assert_eq!(check.cwd.as_deref(), Some("loom"));
        assert!(check.runners.is_empty());
        assert!(
            check.default_runner().is_none(),
            "[runner.check] sets only cwd; no implicit default runner"
        );

        let nix = system.runners.get("nix").unwrap();
        assert_eq!(
            nix.match_regex.as_deref(),
            Some(r"^nix (build|run) \.#(\S+)$")
        );
        assert_eq!(nix.command.as_deref(), Some("nix build {targets}"));
        assert_eq!(nix.target.as_deref(), Some(".#{capture_2}"));
        assert_eq!(nix.join.as_deref(), Some(" "));
        assert_eq!(nix.parse, Some(Parser::NixBuildStatus));
        assert_eq!(nix.cwd.as_deref(), Some("."));
    }

    #[test]
    fn empty_runner_block_parses_as_default() {
        let cfg = LoomConfig::from_toml_str("").unwrap();
        assert!(cfg.runner.0.is_empty());
        assert!(cfg.runner.tier("test").is_none());
    }

    #[test]
    fn parser_names_round_trip_kebab_case() {
        let toml = r#"
[runner.test.nextest]
command = "x"
parse = "libtest-json"

[runner.test.pytest]
command = "y"
parse = "junit-xml"

[runner.system.nix]
command = "z"
parse = "nix-build-status"

[runner.check.lines]
command = "w"
parse = "json-lines"

[runner.system.opaque]
command = "v"
parse = "exit-code"
"#;
        let cfg = LoomConfig::from_toml_str(toml).unwrap();
        let test = cfg.runner.tier("test").unwrap();
        let system = cfg.runner.tier("system").unwrap();
        let check = cfg.runner.tier("check").unwrap();
        assert_eq!(test.runners["nextest"].parse, Some(Parser::LibtestJson));
        assert_eq!(test.runners["pytest"].parse, Some(Parser::JunitXml));
        assert_eq!(system.runners["nix"].parse, Some(Parser::NixBuildStatus));
        assert_eq!(check.runners["lines"].parse, Some(Parser::JsonLines));
        assert_eq!(system.runners["opaque"].parse, Some(Parser::ExitCode));
    }

    #[test]
    fn named_runner_collects_under_tier() {
        let toml = r#"
[runner.test.nextest]
command = "cargo nextest run"
target = "test({name})"
join = " + "
parse = "libtest-json"
"#;
        let cfg = LoomConfig::from_toml_str(toml).unwrap();
        let test = cfg.runner.tier("test").unwrap();
        assert!(test.cwd.is_none());
        assert!(test.default_runner().is_none());
        let entry = test.runners.get("nextest").unwrap();
        assert_eq!(entry.command.as_deref(), Some("cargo nextest run"));
        assert_eq!(entry.target.as_deref(), Some("test({name})"));
        assert_eq!(entry.join.as_deref(), Some(" + "));
        assert_eq!(entry.parse, Some(Parser::LibtestJson));
        assert!(entry.cwd.is_none());
        assert!(entry.match_regex.is_none());
    }

    #[test]
    fn tier_default_cwd_with_named_runner() {
        let toml = r#"
[runner.check]
cwd = "loom"

[runner.check.lint]
command = "cargo clippy"
"#;
        let cfg = LoomConfig::from_toml_str(toml).unwrap();
        let check = cfg.runner.tier("check").unwrap();
        assert_eq!(check.cwd.as_deref(), Some("loom"));
        assert!(check.default_runner().is_none());
        let lint = check.runners.get("lint").unwrap();
        assert_eq!(lint.command.as_deref(), Some("cargo clippy"));
        assert!(lint.cwd.is_none());
    }

    #[test]
    fn tier_implicit_default_runner_coexists_with_named_runners() {
        let toml = r#"
[runner.test]
command = "default-runner {filter}"
target = "test({name})"
join = " + "
parse = "libtest-json"
cwd = "."

[runner.test.custom]
match = '^custom::'
command = "custom-runner {targets}"
target = "{name}"
join = " "
parse = "json-lines"
"#;
        let cfg = LoomConfig::from_toml_str(toml).unwrap();
        let test = cfg.runner.tier("test").unwrap();
        let default = test
            .default_runner()
            .expect("implicit default runner present");
        assert_eq!(default.command.as_deref(), Some("default-runner {filter}"));
        assert_eq!(default.parse, Some(Parser::LibtestJson));
        assert_eq!(default.cwd.as_deref(), Some("."));

        let custom = test.runners.get("custom").unwrap();
        assert_eq!(custom.command.as_deref(), Some("custom-runner {targets}"));
        assert_eq!(custom.match_regex.as_deref(), Some("^custom::"));
        assert_eq!(custom.parse, Some(Parser::JsonLines));
    }

    #[test]
    fn unknown_parser_value_fails_parse() {
        let toml = r#"
[runner.test.nextest]
command = "x"
parse = "not-a-parser"
"#;
        let err = LoomConfig::from_toml_str(toml).expect_err("unknown parser must error");
        let msg = err.to_string();
        assert!(
            msg.contains("parse") || msg.contains("not-a-parser") || msg.contains("variant"),
            "error must surface the bad parser name: {msg}"
        );
    }
}
