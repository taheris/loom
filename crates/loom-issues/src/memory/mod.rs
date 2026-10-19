//! Isolated in-process tracking. Cloned handles share state until the last handle is dropped.
//! Synchronization is an in-process barrier, not durable publication.
use crate::{
    task::{self, Dependency, DependencyKind, Guard, Id, Record, Relation, Status},
    tracking::{self, Error, Store, Synchronization},
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
};

/// A deterministic interruption boundary for a mutation or synchronization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timing {
    BeforeWrite,
    AfterWrite,
}

/// Operations that can be interrupted independently of reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Create,
    Update,
    Close,
    Relate,
    Parent,
    Bond,
    Synchronize,
}

/// Interrupt the matching operation after skipping `skip` matching attempts.
#[derive(Debug, Clone, Copy)]
pub struct Fault {
    pub operation: Operation,
    pub timing: Timing,
    pub skip: usize,
}

/// No subprocesses, persistence on disk, revision guards, or batch transactions.
#[derive(Debug, Clone, Default)]
pub struct Backend {
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    records: HashMap<Id, Record>,
    edges: Vec<(Id, Id, DependencyKind)>,
    next: u64,
    faults: VecDeque<Fault>,
}

fn rejected(reason: impl Into<String>) -> Error {
    Error::Rejected {
        reason: reason.into(),
    }
}

impl Backend {
    /// Queue a one-shot fault. Reads and nonmatching operations do not consume it.
    ///
    /// # Errors
    /// Returns a transport error if the shared state lock is poisoned.
    pub fn inject(&self, fault: Fault) -> Result<(), Error> {
        self.lock()?.faults.push_back(fault);
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, Error> {
        self.state.lock().map_err(|error| Error::Transport {
            reason: format!("in-process state lock poisoned: {error}"),
        })
    }

    fn mutate<T>(
        &self,
        operation: Operation,
        write: impl FnOnce(&mut State) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut state = self.lock()?;
        let fault = match state.faults.front_mut() {
            Some(fault) if fault.operation == operation && fault.skip == 0 => {
                state.faults.pop_front()
            }
            Some(fault) if fault.operation == operation => {
                fault.skip -= 1;
                None
            }
            _ => None,
        };
        if fault.is_some_and(|fault| fault.timing == Timing::BeforeWrite) {
            return Err(Error::Transport {
                reason: "interrupted before write".into(),
            });
        }
        let result = write(&mut state)?;
        drop(state);
        if fault.is_some_and(|fault| fault.timing == Timing::AfterWrite) {
            return Err(Error::Uncertain {
                reason: "interrupted after write; reconcile shared state".into(),
            });
        }
        Ok(result)
    }
}

impl State {
    fn get(&self, id: &Id) -> Result<&Record, Error> {
        self.records
            .get(id)
            .ok_or_else(|| rejected(format!("unknown task {id}")))
    }

    fn reaches(&self, start: &Id, target: &Id) -> bool {
        let mut pending = vec![start.clone()];
        let mut visited = HashSet::new();
        while let Some(id) = pending.pop() {
            if &id == target {
                return true;
            }
            if visited.insert(id.clone()) {
                pending.extend(
                    self.edges
                        .iter()
                        .filter(|(source, _, kind)| {
                            source == &id && *kind != DependencyKind::DiscoveredFrom
                        })
                        .map(|(_, target, _)| target.clone()),
                );
            }
        }
        false
    }

    fn descendant(&self, id: &Id, ancestor: &Id) -> bool {
        let mut current = self
            .records
            .get(id)
            .and_then(|record| record.parent.as_ref());
        while let Some(parent) = current {
            if parent == ancestor {
                return true;
            }
            current = self
                .records
                .get(parent)
                .and_then(|record| record.parent.as_ref());
        }
        false
    }

    fn edge(&mut self, source: &Id, target: &Id, kind: DependencyKind) -> Result<(), Error> {
        self.get(source)?;
        self.get(target)?;
        if source == target {
            return Err(rejected("self relationship"));
        }
        if kind != DependencyKind::DiscoveredFrom && self.reaches(target, source) {
            return Err(rejected("relationship would create a cycle"));
        }
        if kind == DependencyKind::Blocks && self.descendant(target, source) {
            return Err(rejected("cannot depend on a descendant"));
        }
        if kind != DependencyKind::ParentChild && self.descendant(source, target) {
            return Err(rejected("cannot add an explicit dependency on an ancestor"));
        }
        let edge = (source.clone(), target.clone(), kind);
        if !self.edges.contains(&edge) {
            self.edges.push(edge);
        }
        Ok(())
    }

    fn reparent(&mut self, id: &Id, parent: &Id) -> Result<(), Error> {
        self.edge(id, parent, DependencyKind::ParentChild)?;
        self.edges.retain(|(source, target, kind)| {
            source != id || *kind != DependencyKind::ParentChild || target == parent
        });
        self.records
            .get_mut(id)
            .ok_or_else(|| rejected("missing task"))?
            .parent = Some(parent.clone());
        Ok(())
    }

    fn blocked(&self, id: &Id) -> bool {
        self.edges.iter().any(|(source, target, kind)| {
            source == id
                && *kind == DependencyKind::Blocks
                && self
                    .records
                    .get(target)
                    .is_some_and(|record| record.status != Status::Closed)
        })
    }

    fn blocked_in_hierarchy(&self, id: &Id) -> bool {
        self.blocked(id)
            || self
                .records
                .get(id)
                .and_then(|record| record.parent.as_ref())
                .is_some_and(|parent| self.blocked_in_hierarchy(parent))
    }

    fn query(&self, query: &task::Query) -> Vec<Record> {
        let records = self
            .records
            .values()
            .filter(|record| {
                (query.statuses.is_empty() || query.statuses.contains(&record.status))
                    && query
                        .label
                        .as_ref()
                        .is_none_or(|label| record.labels.contains(label))
                    && (query.label_any.is_empty()
                        || query
                            .label_any
                            .iter()
                            .any(|label| record.labels.contains(label)))
                    && query
                        .parent
                        .as_ref()
                        .is_none_or(|parent| record.parent.as_ref() == Some(parent))
                    && query.kind.is_none_or(|kind| record.issue_type == kind)
            })
            .cloned()
            .collect();
        limited(records, query.limit)
    }
}

fn limited(mut records: Vec<Record>, limit: Option<u32>) -> Vec<Record> {
    records.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
    if let Some(limit) = limit.filter(|limit| *limit != 0) {
        records.truncate(limit as usize);
    }
    records
}

impl Store for Backend {
    fn guarantees(&self) -> tracking::Guarantees {
        tracking::Guarantees {
            revision_guard: false,
            atomic_batch: false,
            status_predicate: true,
            visibility: tracking::Visibility::SharedAuthoritative,
            persistence: tracking::Persistence::ProcessLifetime,
        }
    }

    async fn create(&self, request: task::Create) -> Result<Id, Error> {
        self.mutate(Operation::Create, |state| {
            if let Some(parent) = &request.parent {
                state.get(parent)?;
            }
            state.next += 1;
            let id = Id::new(&format!("memory-{}", state.next))
                .map_err(|error| rejected(error.to_string()))?;
            let parent = request.parent.clone();
            state.records.insert(
                id.clone(),
                Record {
                    id: id.clone(),
                    title: request.title,
                    description: request.description,
                    status: request.status.unwrap_or(Status::Open),
                    priority: request.priority.unwrap_or(task::Priority::P2),
                    issue_type: request.kind.unwrap_or_default(),
                    labels: request.labels,
                    parent: parent.clone(),
                    metadata: request.metadata,
                    notes: request.notes,
                },
            );
            if let Some(parent) = parent {
                state
                    .edges
                    .push((id.clone(), parent, DependencyKind::ParentChild));
            }
            Ok(id)
        })
    }

    async fn get(&self, id: &Id) -> Result<Record, Error> {
        self.lock()?.get(id).cloned()
    }

    async fn query(&self, query: task::Query) -> Result<Vec<Record>, Error> {
        Ok(self.lock()?.query(&query))
    }

    async fn update(&self, id: &Id, update: task::Update, guard: Guard) -> Result<(), Error> {
        if matches!(guard, Guard::Revision(_)) {
            return Err(Error::Unsupported {
                reason: "no revision compare-and-swap".into(),
            });
        }
        if update.notes.is_some() && update.append_notes.is_some() {
            return Err(rejected("notes and append_notes are mutually exclusive"));
        }
        self.mutate(Operation::Update, |state| {
            let record = state
                .records
                .get_mut(id)
                .ok_or_else(|| rejected(format!("unknown task {id}")))?;
            if matches!(guard, Guard::Status(status) if status != record.status) {
                return Err(Error::Conflict {
                    reason: "status predicate failed".into(),
                });
            }
            if let Some(title) = update.title {
                record.title = title;
            }
            if let Some(description) = update.description {
                record.description = description;
            }
            if let Some(status) = update.status {
                record.status = status;
            }
            if let Some(priority) = update.priority {
                record.priority = priority;
            }
            record
                .labels
                .retain(|label| !update.remove_labels.contains(label));
            for label in update.add_labels {
                if !record.labels.contains(&label) {
                    record.labels.push(label);
                }
            }
            if let Some(notes) = update.notes {
                record.notes = Some(notes);
            }
            if let Some(notes) = update.append_notes {
                record.notes = Some(match record.notes.take() {
                    Some(previous) if !previous.is_empty() => format!("{previous}\n{notes}"),
                    _ => notes,
                });
            }
            record.metadata.extend(update.set_metadata);
            for key in update.unset_metadata {
                record.metadata.remove(&key);
            }
            Ok(())
        })
    }

    async fn close(&self, id: &Id, _reason: Option<&str>) -> Result<(), Error> {
        self.mutate(Operation::Close, |state| {
            if state.blocked(id) {
                return Err(rejected("cannot close blocked task"));
            }
            state
                .records
                .get_mut(id)
                .ok_or_else(|| rejected("missing task"))?
                .status = Status::Closed;
            Ok(())
        })
    }

    async fn graph(&self, id: &Id) -> Result<task::Graph, Error> {
        let state = self.lock()?;
        let status = state.get(id)?.status;
        let dependencies = state
            .edges
            .iter()
            .filter(|(source, _, _)| source == id)
            .map(|(_, target, kind)| {
                Ok(Dependency {
                    id: target.clone(),
                    status: state.get(target)?.status,
                    kind: *kind,
                })
            })
            .collect::<Result<_, Error>>()?;
        drop(state);
        Ok(task::Graph {
            status,
            dependencies,
        })
    }

    async fn dependents(&self, id: &Id) -> Result<Vec<Id>, Error> {
        let state = self.lock()?;
        state.get(id)?;
        Ok(state
            .edges
            .iter()
            .filter(|(_, target, kind)| target == id && *kind == DependencyKind::Blocks)
            .map(|(source, _, _)| source.clone())
            .collect())
    }

    async fn relate(&self, source: &Id, target: &Id, relation: Relation) -> Result<(), Error> {
        self.mutate(Operation::Relate, |state| {
            state.edge(
                source,
                target,
                match relation {
                    Relation::Blocks => DependencyKind::Blocks,
                    Relation::DiscoveredFrom => DependencyKind::DiscoveredFrom,
                },
            )
        })
    }

    async fn parent(&self, id: &Id, parent: &Id) -> Result<(), Error> {
        self.mutate(Operation::Parent, |state| state.reparent(id, parent))
    }

    async fn bond(&self, left: &Id, right: &Id) -> Result<(), Error> {
        self.mutate(Operation::Bond, |state| {
            state.edge(right, left, DependencyKind::Blocks)
        })
    }

    async fn ready(&self, query: task::Ready) -> Result<Vec<Record>, Error> {
        let state = self.lock()?;
        let records = state.query(&task::Query {
            statuses: vec![Status::Open],
            label: query.label,
            parent: query.parent,
            ..task::Query::default()
        });
        let ready = records
            .into_iter()
            .filter(|record| {
                !state.blocked_in_hierarchy(&record.id)
                    && !query
                        .exclude_labels
                        .iter()
                        .any(|label| record.labels.contains(label))
            })
            .collect();
        drop(state);
        Ok(limited(ready, query.limit))
    }

    async fn progress(&self, group: &task::GroupId) -> Result<task::Progress, Error> {
        let state = self.lock()?;
        let id = Id::new(group.as_str()).map_err(|error| rejected(error.to_string()))?;
        let title = state.get(&id)?.title.clone();
        let mut descendants: Vec<_> = state
            .records
            .values()
            .filter(|record| state.descendant(&record.id, &id))
            .cloned()
            .collect();
        drop(state);
        descendants.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
        let count = |status| {
            u32::try_from(
                descendants
                    .iter()
                    .filter(|record| record.status == status)
                    .count(),
            )
            .map_err(|error| rejected(error.to_string()))
        };
        let total =
            u32::try_from(descendants.len()).map_err(|error| rejected(error.to_string()))?;
        let completed = count(Status::Closed)?;
        Ok(task::Progress {
            molecule_id: group.clone(),
            molecule_title: title,
            completed,
            in_progress: count(Status::InProgress)?,
            total,
            percent: if total == 0 {
                0.0
            } else {
                f64::from(completed) * 100.0 / f64::from(total)
            },
            current_step_id: descendants
                .iter()
                .find(|record| record.status == Status::InProgress)
                .map(|record| record.id.clone()),
        })
    }

    async fn synchronize(&self, _mode: Synchronization) -> Result<(), Error> {
        self.mutate(Operation::Synchronize, |_| Ok(()))
    }
}
