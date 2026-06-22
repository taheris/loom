use std::process::Command as StdCommand;

use tokio::process::Command as TokioCommand;

const LOCAL_ENV_VARS: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

pub(super) fn std_git_command() -> StdCommand {
    let mut command = StdCommand::new("git");
    scrub_std_command(&mut command);
    command
}

pub(super) fn tokio_git_command() -> TokioCommand {
    let mut command = TokioCommand::new("git");
    scrub_tokio_command(&mut command);
    command
}

fn scrub_std_command(command: &mut StdCommand) {
    for &name in LOCAL_ENV_VARS {
        command.env_remove(name);
    }
}

fn scrub_tokio_command(command: &mut TokioCommand) {
    for &name in LOCAL_ENV_VARS {
        command.env_remove(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_scrubbed(stdout: &[u8]) {
        let stdout = String::from_utf8_lossy(stdout);
        for &name in LOCAL_ENV_VARS {
            let prefix = format!("{name}=");
            assert!(
                !stdout.lines().any(|line| line.starts_with(&prefix)),
                "{name} leaked through scrubbed command: {stdout}",
            );
        }
    }

    #[test]
    fn std_command_scrubs_explicit_git_local_env() {
        let mut command = StdCommand::new("env");
        for &name in LOCAL_ENV_VARS {
            command.env(name, "leaked");
        }
        scrub_std_command(&mut command);
        let output = command.output().expect("spawn env");
        assert!(output.status.success(), "env exited with {}", output.status);
        assert_scrubbed(&output.stdout);
    }

    #[tokio::test]
    async fn tokio_command_scrubs_explicit_git_local_env() {
        let mut command = TokioCommand::new("env");
        for &name in LOCAL_ENV_VARS {
            command.env(name, "leaked");
        }
        scrub_tokio_command(&mut command);
        let output = command.output().await.expect("spawn env");
        assert!(output.status.success(), "env exited with {}", output.status);
        assert_scrubbed(&output.stdout);
    }
}
