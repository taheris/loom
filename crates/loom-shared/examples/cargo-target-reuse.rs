//! Measure a disposable native Cargo fixture without touching existing targets or caches.

#[path = "../tests/support/mod.rs"]
pub mod support;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let parent = args
        .next()
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
    anyhow::ensure!(
        args.next().is_none(),
        "usage: cargo-target-reuse [fixture-parent-directory]"
    );
    let fixture = support::Fixture::new(&parent).await?;
    let report = fixture.measure().await?;
    let stdout = std::io::stdout();
    serde_json::to_writer_pretty(stdout.lock(), &report)?;
    Ok(())
}
