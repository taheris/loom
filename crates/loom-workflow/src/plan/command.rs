use std::path::Path;

use loom_driver::identifier::ProfileName;

/// Default name of the wrapix launcher binary on PATH. Tests override via
/// the `LOOM_WRAPIX_BIN` env var resolved by [`super::runner::run`].
pub const WRAPIX_BIN: &str = "wrapix";

/// Build the argv passed to `wrapix run` for an interactive `loom plan` session.
///
/// Layout:
///
/// ```text
/// wrapix run <workspace> --profile <name> claude --dangerously-skip-permissions <prompt>
/// ```
///
/// `wrapix run` (NOT `spawn`) keeps the TTY attached and inherits the
/// user's terminal — there is no `--spawn-config` and no `--stdio` flag,
/// matching the spec's "exception" carve-out for the interactive interview.
/// The `--profile <name>` pair is the spec-defined contract for profile
/// selection on this path; the resolved name flows in from
/// `LoomConfig::agent_for(Phase::Plan)` (or the CLI override).
/// Returns argv as a `Vec<String>` so callers (and tests) can inspect it
/// without paying for a real spawn.
pub fn build_wrapix_argv(
    workspace: &Path,
    profile: &ProfileName,
    prompt_body: &str,
) -> Vec<String> {
    vec![
        "run".to_string(),
        workspace.to_string_lossy().into_owned(),
        "--profile".to_string(),
        profile.to_string(),
        "claude".to_string(),
        "--dangerously-skip-permissions".to_string(),
        prompt_body.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn argv_starts_with_run_subcommand() {
        let argv = build_wrapix_argv(&PathBuf::from("/work"), &ProfileName::new("base"), "PROMPT");
        assert_eq!(argv[0], "run");
        assert_eq!(argv[1], "/work");
        assert_eq!(argv[2], "--profile");
        assert_eq!(argv[3], "base");
    }

    #[test]
    fn argv_passes_prompt_to_claude_with_skip_permissions() {
        let argv = build_wrapix_argv(
            &PathBuf::from("/work"),
            &ProfileName::new("base"),
            "PROMPT BODY",
        );
        assert_eq!(argv[4], "claude");
        assert_eq!(argv[5], "--dangerously-skip-permissions");
        assert_eq!(argv[6], "PROMPT BODY");
    }

    #[test]
    fn argv_never_contains_spawn_or_stdio_or_spawn_config() {
        let argv = build_wrapix_argv(&PathBuf::from("/work"), &ProfileName::new("base"), "PROMPT");
        assert!(!argv.iter().any(|a| a == "spawn"));
        assert!(!argv.iter().any(|a| a == "run-bead"));
        assert!(!argv.iter().any(|a| a == "--stdio"));
        assert!(!argv.iter().any(|a| a == "--spawn-config"));
    }
}
