use loom_issues::{
    beads::{BdClient, BdError, CommandRunner, RunOutput},
    task,
    tracking::{Store, Synchronization},
};
use std::{ffi::OsString, time::Duration};

struct Transport;
impl CommandRunner for Transport {
    async fn run(&self, _: Vec<OsString>, _: Duration) -> Result<RunOutput, BdError> {
        Ok(RunOutput {
            status: 0,
            stdout: b"[]".to_vec(),
            stderr: Vec::new(),
        })
    }
}

async fn portable(store: &impl Store) -> Result<(), loom_issues::tracking::Error> {
    assert!(!store.guarantees().revision_guard);
    let _records = store.query(task::Query::default()).await?;
    store.synchronize(Synchronization::SharedState).await?;
    Ok(())
}

fn main() {
    let adapter = BdClient::with_runner(Transport);
    drop(portable(&adapter));
}
