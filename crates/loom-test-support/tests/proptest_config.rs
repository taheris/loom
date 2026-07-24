use std::process::Command;

use anyhow::{Context, Result, ensure};
use loom_test_support::proptest_config;

const PROBE_ENV: &str = "LOOM_PROPTEST_CONFIG_PROBE";

#[test]
fn proptest_case_configuration_honours_default_and_env_override() -> Result<()> {
    if let Ok(expected) = std::env::var(PROBE_ENV) {
        let expected = expected.parse::<u32>().context("parse probe case count")?;
        ensure!(proptest_config().cases == expected);
        return Ok(());
    }

    let current_exe = std::env::current_exe().context("locate test executable")?;
    for (value, expected) in [(None, 32_u32), (Some("77"), 77_u32)] {
        let mut child = Command::new(&current_exe);
        child
            .args([
                "--exact",
                "proptest_case_configuration_honours_default_and_env_override",
                "--nocapture",
            ])
            .env(PROBE_ENV, expected.to_string());
        match value {
            Some(cases) => {
                child.env("PROPTEST_CASES", cases);
            }
            None => {
                child.env_remove("PROPTEST_CASES");
            }
        }
        let output = child.output().context("run property configuration probe")?;
        ensure!(
            output.status.success(),
            "property configuration probe failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
    }
    Ok(())
}
