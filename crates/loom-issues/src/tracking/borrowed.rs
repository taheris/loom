//! Borrowing a store preserves its identity, operations, and advertised guarantees.

use super::{Error, Guarantees, Observation, Store, Synchronization};
use crate::task::{
    Create, Graph, GroupId, Guard, Id, Progress, Query, Ready, Record, Relation, Update,
};

impl<T: Store + ?Sized> Store for &T {
    fn guarantees(&self) -> Guarantees {
        (**self).guarantees()
    }
    async fn create(&self, request: Create) -> Result<Id, Error> {
        (**self).create(request).await
    }
    async fn get(&self, id: &Id) -> Result<Record, Error> {
        (**self).get(id).await
    }
    async fn observe(&self, id: &Id) -> Result<Observation, Error> {
        (**self).observe(id).await
    }
    async fn query(&self, query: Query) -> Result<Vec<Record>, Error> {
        (**self).query(query).await
    }
    async fn update(&self, id: &Id, update: Update, guard: Guard) -> Result<(), Error> {
        (**self).update(id, update, guard).await
    }
    async fn close(&self, id: &Id, reason: Option<&str>) -> Result<(), Error> {
        (**self).close(id, reason).await
    }
    async fn graph(&self, id: &Id) -> Result<Graph, Error> {
        (**self).graph(id).await
    }
    async fn dependents(&self, id: &Id) -> Result<Vec<Id>, Error> {
        (**self).dependents(id).await
    }
    async fn relate(&self, source: &Id, target: &Id, relation: Relation) -> Result<(), Error> {
        (**self).relate(source, target, relation).await
    }
    async fn parent(&self, id: &Id, parent: &Id) -> Result<(), Error> {
        (**self).parent(id, parent).await
    }
    async fn bond(&self, left: &Id, right: &Id) -> Result<(), Error> {
        (**self).bond(left, right).await
    }
    async fn ready(&self, query: Ready) -> Result<Vec<Record>, Error> {
        (**self).ready(query).await
    }
    async fn progress(&self, group: &GroupId) -> Result<Progress, Error> {
        (**self).progress(group).await
    }
    async fn synchronize(&self, mode: Synchronization) -> Result<(), Error> {
        (**self).synchronize(mode).await
    }
}
