use std::path::Path;

/// Default name of the wrapix launcher binary on PATH. Tests override via
/// the `LOOM_WRAPIX_BIN` env var resolved by [`super::runner::run`].
pub const WRAPIX_BIN: &str = "wrapix";

/// Build the argv passed to `wrapix run` for an interactive `loom plan` session.
///
/// Layout:
///
/// ```text
/// wrapix run <workspace> claude --dangerously-skip-permissions <prompt>
/// ```
///
/// `wrapix run` (NOT `spawn`) keeps the TTY attached and inherits the
/// user's terminal — there is no `--spawn-config` and no `--stdio` flag,
/// matching the spec's "exception" carve-out for the interactive interview.
/// Profile selection on this path flows through the `WRAPIX_DEFAULT_IMAGE_REF`
/// and `WRAPIX_DEFAULT_IMAGE_SOURCE` env vars exported by
/// [`super::runner::run`] — `wrapix run` does not parse `--profile`; any
/// trailing tokens after the workspace are forwarded into the container as
/// the command vector (so adding `--profile <name>` here makes the
/// entrypoint exec `--profile` and exit 127).
/// Returns argv as a `Vec<String>` so callers (and tests) can inspect it
/// without paying for a real spawn.
pub fn build_wrapix_argv(workspace: &Path, prompt_body: &str) -> Vec<String> {
    vec![
        "run".to_string(),
        workspace.to_string_lossy().into_owned(),
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
        let argv = build_wrapix_argv(&PathBuf::from("/work"), "PROMPT");
        assert_eq!(argv[0], "run");
        assert_eq!(argv[1], "/work");
        assert_eq!(argv[2], "claude");
    }

    #[test]
    fn argv_passes_prompt_to_claude_with_skip_permissions() {
        let argv = build_wrapix_argv(&PathBuf::from("/work"), "PROMPT BODY");
        assert_eq!(argv[2], "claude");
        assert_eq!(argv[3], "--dangerously-skip-permissions");
        assert_eq!(argv[4], "PROMPT BODY");
    }

    #[test]
    fn argv_never_contains_profile_spawn_or_stdio_or_spawn_config() {
        let argv = build_wrapix_argv(&PathBuf::from("/work"), "PROMPT");
        assert!(
            !argv.iter().any(|a| a == "--profile"),
            "wrapix run has no --profile parser; profile flows via WRAPIX_DEFAULT_IMAGE_* env vars"
        );
        assert!(!argv.iter().any(|a| a == "spawn"));
        assert!(!argv.iter().any(|a| a == "run-bead"));
        assert!(!argv.iter().any(|a| a == "--stdio"));
        assert!(!argv.iter().any(|a| a == "--spawn-config"));
    }
}
