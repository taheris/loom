//! Exercise the production selected-provisioning boundary without launching a worker.

use loom_driver::{
    agent::AgentRuntime,
    config::LoomConfig,
    git::{KeyMode, RepoGitPolicy},
    identifier::ProfileName,
    profile_manifest::bootstrap_loop,
};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let [workspace, profile, runtime] = args.as_slice() else {
        return Err("usage: profile-catalogue-probe WORKSPACE PROFILE RUNTIME".into());
    };
    let workspace = PathBuf::from(workspace).canonicalize()?;
    let profile = profile
        .to_str()
        .ok_or("non-UTF8 profile")?
        .parse::<ProfileName>()?;
    let runtime = runtime
        .to_str()
        .ok_or("non-UTF8 runtime")?
        .parse::<AgentRuntime>()?;
    let wrix =
        std::env::var_os("LOOM_WRIX_BIN").map_or_else(|| PathBuf::from("wrix"), PathBuf::from);
    let policy = RepoGitPolicy::resolve(&workspace, wrix, KeyMode::Repository)?;
    let config = LoomConfig::load(LoomConfig::resolve_path(&workspace))?;
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    executor.block_on(async {
        let manifest = bootstrap_loop(&workspace, &config, policy).await?;
        let image = manifest.resolve(&profile, runtime).await?;
        serde_json::to_writer(std::io::stdout().lock(), image)?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
