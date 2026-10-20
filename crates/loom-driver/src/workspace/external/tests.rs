//! Process lifecycle components use real children and native checkout observations.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::*;
use crate::process::tests::{Fixture, bounded};

#[cfg(test)]
fn selected(operator: &Path, program: &Path) -> Membership {
    let manager =
        crate::lock::LockManager::with_state_home(operator, operator.join("test-state")).unwrap();
    let guard = manager.acquire_workspace().unwrap();
    Membership::configure(operator, Some(program), &guard).unwrap();
    Membership::load(operator).unwrap().unwrap()
}

#[cfg(test)]
#[tokio::test]
async fn external_membership_timeout_reaps_group_preserves_committed_effect_and_allows_retry() {
    bounded(async {
        let fixture = Fixture::new().await;
        let root = tempfile::tempdir().unwrap();
        let operator = root.path().join("operator");
        crate::git::init_test_repo_with_integration(&operator).unwrap();
        let program = root.path().join("adapter");
        let args = fixture.args();
        fs::write(&program, loom_test_support::bash_script(&format!(
            "set -euo pipefail\nif [[ -f \"$2/.git/committed-membership\" ]]; then exit 0; fi\nprintf committed > \"$2/.git/committed-membership\"\nexec bash '{}' '{}' '{}'\n",
            args[0], args[1], args[2],
        ))).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let membership = selected(&operator, &program);
        let (result, peers) = tokio::join!(
            membership.admit_with_clock(&operator, fixture.clock.as_ref()),
            async { let peers = fixture.ready(2).await; fixture.clock.expire(); peers },
        );
        assert!(matches!(result, Err(Error::ExternalProcess(crate::process::Error::Timeout))));
        fixture.assert_stopped(peers).await;
        assert_eq!(fs::read_to_string(operator.join(".git/committed-membership")).unwrap(), "committed");
        membership.admit(&operator).await.unwrap();
    }).await;
}

#[cfg(test)]
#[tokio::test]
async fn external_membership_bounds_both_output_pipes_and_closes_interactive_stdin() {
    bounded(async {
        let root = tempfile::tempdir().unwrap();
        let operator = root.path().join("operator");
        crate::git::init_test_repo_with_integration(&operator).unwrap();
        let program = root.path().join("adapter");
        for pipe in ["", ">&2"] {
            fs::write(
                &program,
                loom_test_support::bash_script(&format!(
                    "set -euo pipefail\nprintf '%20000s' x {pipe}\n"
                )),
            )
            .unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
            let membership = selected(&operator, &program);
            assert!(matches!(
                membership.admit(&operator).await,
                Err(Error::ExternalProcess(_))
            ));
        }
        fs::write(
            &program,
            loom_test_support::bash_script(
                "set -euo pipefail\nif IFS= read -r line; then exit 23; fi\n",
            ),
        )
        .unwrap();
        selected(&operator, &program)
            .admit(&operator)
            .await
            .unwrap();
    })
    .await;
}

#[cfg(test)]
#[tokio::test]
async fn external_membership_detects_selection_replacement_during_admission() {
    let root = tempfile::tempdir().unwrap();
    let operator = root.path().join("operator");
    crate::git::init_test_repo_with_integration(&operator).unwrap();
    let program = root.path().join("adapter");
    fs::write(&program, loom_test_support::bash_script("set -euo pipefail\nprintf '%s' '{\"version\":1,\"program\":null}' > \"$2/.git/loom-workspace-associator.json\"\n")).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let membership = selected(&operator, &program);
    assert!(matches!(
        membership.admit(&operator).await,
        Err(Error::Association { .. })
    ));
}
