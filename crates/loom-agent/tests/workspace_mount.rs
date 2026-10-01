//! Executed only by the real-container workspace verifier.

use loom_agent::direct::tool::{Read, ToolContext};
use loom_llm::Tool;
use serde_json::json;

#[tokio::test]
#[ignore = "requires the fixture bind mount installed by scripts/test-direct-workspace.sh"]
async fn direct_tools_read_against_container_workspace_mount() {
    let expected = std::env::var("LOOM_MOUNT_CANARY").expect("container canary");
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
    assert!(
        mountinfo
            .lines()
            .any(|line| line.split_whitespace().nth(4) == Some("/workspace"))
    );
    let scratch = tempfile::tempdir().unwrap();
    let tool = Read::new(ToolContext::new(
        scratch.path().join("offload"),
        loom_driver::config::InlineByteLimit::default(),
    ));
    let result = tool
        .invoke(json!({"file_path": "/workspace/nested/probe.txt"}))
        .await
        .unwrap();
    assert!(!result.is_error, "{result:?}");
    assert_eq!(result.content, format!("{expected}\r\nmounted workspace\n"));
    let absent = tool
        .invoke(json!({"file_path": "/workspace/not-mounted.txt"}))
        .await
        .unwrap();
    assert!(absent.is_error);
}
