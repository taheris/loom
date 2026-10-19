//! Backend-neutral operations and explicitly limited guarantees.
use crate::task::{
    Create, Graph, GroupId, Guard, Id, Progress, Query, Ready, Record, Relation, Update,
};
use displaydoc::Display;
use std::future::Future;
use thiserror::Error;

#[derive(Debug, Display, Error)]
pub enum Error {
    /// operation rejected: {reason}
    Rejected { reason: String },
    /// conditional update conflicted: {reason}
    Conflict { reason: String },
    /// required guarantee is unsupported: {reason}
    Unsupported { reason: String },
    /// transport failed: {reason}
    Transport { reason: String },
    /// mutation may have taken effect; reconcile authoritative state: {reason}
    Uncertain { reason: String },
    /// backend response is unsupported or malformed: {reason}
    InvalidResponse { reason: String },
}

/// One coherent source record and its direct graph, including dependency statuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub record: Record,
    /// Direct graph of `record.id`, with source status equal to `record.status`.
    pub graph: Graph,
}

/// Required safety conditions must be checked, not silently downgraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guarantees {
    pub revision_guard: bool,
    pub atomic_batch: bool,
    pub status_predicate: bool,
    pub visibility: Visibility,
    pub persistence: Persistence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    SharedAuthoritative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Persistence {
    BackendManaged,
    /// State survives handle replacement, but not the hosting process.
    ProcessLifetime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Synchronization {
    /// Workers already share the authoritative store; no task-local push/pull occurs.
    SharedState,
    /// Publish backend state using the consumer-supplied publication transport.
    Publish,
}

/// Dropping a mutation future does not imply rollback. Multi-call changes are not transactions.
pub trait Store: Send + Sync {
    fn guarantees(&self) -> Guarantees;
    fn create(&self, request: Create) -> impl Future<Output = Result<Id, Error>> + Send;
    fn get(&self, id: &Id) -> impl Future<Output = Result<Record, Error>> + Send;
    /// Observe record and graph together, or refuse if coherence cannot be provided.
    /// No repeatable reads or cross-call consistency; separate reads cannot substitute.
    fn observe(&self, _id: &Id) -> impl Future<Output = Result<Observation, Error>> + Send {
        async {
            Err(Error::Unsupported {
                reason: "coherent record and graph observation is unavailable".into(),
            })
        }
    }
    fn query(&self, query: Query) -> impl Future<Output = Result<Vec<Record>, Error>> + Send;
    fn update(
        &self,
        id: &Id,
        update: Update,
        guard: Guard,
    ) -> impl Future<Output = Result<(), Error>> + Send;
    fn close(
        &self,
        id: &Id,
        reason: Option<&str>,
    ) -> impl Future<Output = Result<(), Error>> + Send;
    fn graph(&self, id: &Id) -> impl Future<Output = Result<Graph, Error>> + Send;
    fn dependents(&self, id: &Id) -> impl Future<Output = Result<Vec<Id>, Error>> + Send;
    fn relate(
        &self,
        source: &Id,
        target: &Id,
        relation: Relation,
    ) -> impl Future<Output = Result<(), Error>> + Send;
    fn parent(&self, id: &Id, parent: &Id) -> impl Future<Output = Result<(), Error>> + Send;
    fn bond(&self, left: &Id, right: &Id) -> impl Future<Output = Result<(), Error>> + Send;
    fn ready(&self, query: Ready) -> impl Future<Output = Result<Vec<Record>, Error>> + Send;
    fn progress(&self, group: &GroupId) -> impl Future<Output = Result<Progress, Error>> + Send;
    fn synchronize(&self, mode: Synchronization) -> impl Future<Output = Result<(), Error>> + Send;
}
