use std::path::Path;

use loom_driver::agent::AgentKind;

/// Build the argv passed to `wrix run` for an interactive `loom plan`
/// session.
///
/// Layout:
///
/// ```text
/// wrix run <workspace> claude --settings <claude-settings.json> --dangerously-skip-permissions <prompt>
/// wrix run <workspace> <non-claude-agent> <prompt>
/// ```
///
/// `wrix run` (NOT `spawn`) keeps the TTY attached and inherits the
/// user's terminal — there is no `--spawn-config` and no `--stdio` flag,
/// matching the spec's "exception" carve-out for the interactive interview.
/// Pi's native-TUI production path adds session and extension flags in the
/// shared `pi_tui` launcher helper rather than through this Claude helper.
/// The caller prefixes these subcommand arguments with the selected manifest's
/// `--profile-config` on its raw launcher. No profile options belong after the workspace.
/// Returns argv as a `Vec<String>` so callers (and tests) can inspect it
/// without paying for a real spawn.
pub fn build_wrix_argv(
    workspace: &Path,
    prompt_body: &str,
    agent_kind: AgentKind,
    claude_settings: Option<&Path>,
) -> Vec<String> {
    let mut argv = vec![
        "run".to_string(),
        workspace.to_string_lossy().into_owned(),
        agent_command(agent_kind).to_string(),
    ];
    if matches!(agent_kind, AgentKind::Claude) {
        if let Some(settings) = claude_settings {
            argv.push("--settings".to_string());
            argv.push(settings.to_string_lossy().into_owned());
        }
        argv.push("--dangerously-skip-permissions".to_string());
    }
    argv.push(prompt_body.to_string());
    argv
}

const fn agent_command(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "claude",
        AgentKind::Pi => "pi",
        AgentKind::Direct => "loom-direct-runner",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn settings_path() -> PathBuf {
        PathBuf::from("/workspace/.loom/scratch/plan/claude-settings.json")
    }

    #[test]
    fn argv_starts_with_run_subcommand() {
        let settings = settings_path();
        let argv = build_wrix_argv(
            &PathBuf::from("/work"),
            "PROMPT",
            AgentKind::Claude,
            Some(&settings),
        );
        assert_eq!(argv[0], "run");
        assert_eq!(argv[1], "/work");
        assert_eq!(argv[2], "claude");
    }

    #[test]
    fn argv_passes_prompt_to_claude_with_skip_permissions() {
        let settings = settings_path();
        let argv = build_wrix_argv(
            &PathBuf::from("/work"),
            "PROMPT BODY",
            AgentKind::Claude,
            Some(&settings),
        );
        assert_eq!(argv[2], "claude");
        assert_eq!(argv[3], "--settings");
        assert_eq!(argv[4], settings.to_string_lossy());
        assert_eq!(argv[5], "--dangerously-skip-permissions");
        assert_eq!(argv[6], "PROMPT BODY");
    }

    #[test]
    fn argv_passes_prompt_to_pi_without_claude_flags() {
        let argv = build_wrix_argv(&PathBuf::from("/work"), "PROMPT BODY", AgentKind::Pi, None);
        assert_eq!(argv[2], "pi");
        assert_eq!(argv[3], "PROMPT BODY");
        assert!(!argv.iter().any(|a| a == "--dangerously-skip-permissions"));
        assert!(!argv.iter().any(|a| a == "--settings"));
    }

    #[test]
    fn argv_never_contains_profile_spawn_or_stdio_or_spawn_config() {
        let settings = settings_path();
        let argv = build_wrix_argv(
            &PathBuf::from("/work"),
            "PROMPT",
            AgentKind::Claude,
            Some(&settings),
        );
        assert!(
            !argv.iter().any(|a| a == "--profile"),
            "profile configuration belongs before run, not in the container command"
        );
        assert!(!argv.iter().any(|a| a == "spawn"));
        assert!(!argv.iter().any(|a| a == "run-bead"));
        assert!(!argv.iter().any(|a| a == "--stdio"));
        assert!(!argv.iter().any(|a| a == "--spawn-config"));
    }
}
