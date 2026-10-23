//! Loom's Read tool consumes the requested workspace path; Wrix owns bind mounts.

use loom_agent::direct::tool::{Read, ToolContext};
use loom_llm::Tool;
use serde_json::json;

#[tokio::test]
async fn direct_tools_read_requested_workspace_file() {
    let workspace = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let nested = workspace.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let path = nested.join("probe.txt");
    let expected = "requested workspace\r\nnot the caller directory\n";
    std::fs::write(&path, expected).unwrap();
    let tool = Read::new(ToolContext::new(
        scratch.path().join("offload"),
        loom_driver::config::InlineByteLimit::default(),
    ));
    let result = tool.invoke(json!({"file_path": path})).await.unwrap();
    assert!(!result.is_error, "{result:?}");
    assert_eq!(result.content, expected);
    let absent = tool
        .invoke(json!({"file_path": workspace.path().join("absent.txt")}))
        .await
        .unwrap();
    assert!(absent.is_error);
}
