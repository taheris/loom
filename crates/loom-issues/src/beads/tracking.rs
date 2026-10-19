use super::{BdClient, BdError, CommandRunner, CreateOpts, ListOpts, ReadyOpts, UpdateOpts};
use crate::{task, tracking};

fn translate(error: &BdError, mutation: bool) -> tracking::Error {
    let mut reason = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        reason.push_str(": ");
        reason.push_str(&cause.to_string());
        source = cause.source();
    }
    match error {
        BdError::Unsupported { .. } => tracking::Error::Unsupported { reason },

        BdError::Spawn(_) if !mutation => tracking::Error::Transport { reason },
        BdError::ShowEmpty => tracking::Error::Rejected { reason },
        _ if mutation => tracking::Error::Uncertain { reason },
        BdError::Decode { .. } | BdError::Utf8(_) => tracking::Error::InvalidResponse { reason },
        _ => tracking::Error::Transport { reason },
    }
}

fn check_metadata_numbers(value: &serde_json::Value) -> Result<(), tracking::Error> {
    match value {
        serde_json::Value::Number(number)
            if number
                .as_i64()
                .is_some_and(|value| value.unsigned_abs() > (1 << 53))
                || number.as_u64().is_some_and(|value| value > (1 << 53)) =>
        {
            Err(tracking::Error::Unsupported {
                reason: "Beads JSON numbers cannot guarantee exact large-integer preservation"
                    .into(),
            })
        }
        serde_json::Value::Array(values) => values.iter().try_for_each(check_metadata_numbers),
        serde_json::Value::Object(values) => values.values().try_for_each(check_metadata_numbers),
        _ => Ok(()),
    }
}

fn coerces_string(value: &str) -> bool {
    let unsigned = value.trim_start_matches(['+', '-']);
    matches!(
        value,
        "t" | "T" | "TRUE" | "True" | "true" | "f" | "F" | "FALSE" | "False" | "false"
    ) || value.parse::<i64>().is_ok()
        || value.replace('_', "").parse::<f64>().is_ok()
        || unsigned.starts_with("0x")
        || unsigned.starts_with("0X")
}

fn encode_metadata(value: serde_json::Value) -> Result<String, tracking::Error> {
    check_metadata_numbers(&value)?;
    match value {
        serde_json::Value::String(value) if !coerces_string(&value) => Ok(value),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => Ok(value.to_string()),
        _ => Err(tracking::Error::Unsupported {
            reason:
                "Beads targeted metadata writes cannot preserve this JSON value without coercion"
                    .into(),
        }),
    }
}

fn metadata_key(key: &str) -> Result<(), tracking::Error> {
    if key.is_empty() || key.contains('=') || key.chars().any(char::is_control) {
        return Err(tracking::Error::Rejected {
            reason: "invalid targeted metadata key".into(),
        });
    }
    Ok(())
}

impl<R: CommandRunner> tracking::Store for BdClient<R> {
    fn guarantees(&self) -> tracking::Guarantees {
        tracking::Guarantees {
            revision_guard: false,
            atomic_batch: false,
            status_predicate: true,
            visibility: tracking::Visibility::SharedAuthoritative,
            persistence: tracking::Persistence::BackendManaged,
        }
    }

    async fn create(&self, request: task::Create) -> Result<task::Id, tracking::Error> {
        for value in request.metadata.values() {
            check_metadata_numbers(value)?;
        }
        let metadata = serde_json::to_string(&request.metadata).map_err(|error| {
            tracking::Error::Rejected {
                reason: error.to_string(),
            }
        })?;
        self.create(CreateOpts {
            title: request.title,
            description: request.description,
            status: request.status,
            issue_type: request.kind,
            priority: request.priority,
            labels: request
                .labels
                .into_iter()
                .map(|label| label.to_string())
                .collect(),
            parent: request.parent,
            metadata: Some(metadata),
            notes: request.notes,
        })
        .await
        .map_err(|error| translate(&error, true))
    }

    async fn get(&self, id: &task::Id) -> Result<task::Record, tracking::Error> {
        self.show(id)
            .await
            .map_err(|error| translate(&error, false))
    }

    async fn observe(&self, id: &task::Id) -> Result<tracking::Observation, tracking::Error> {
        let (record, graph) = self
            .show_with_dependencies(id)
            .await
            .map_err(|error| translate(&error, false))?;
        if record.id != *id || record.status != graph.status {
            return Err(tracking::Error::InvalidResponse {
                reason: "observation source identity or status mismatch".into(),
            });
        }
        Ok(tracking::Observation { record, graph })
    }

    async fn query(&self, query: task::Query) -> Result<Vec<task::Record>, tracking::Error> {
        self.list(ListOpts {
            all: true,
            limit: Some(query.limit.unwrap_or(0)),
            statuses: query.statuses,
            label: query.label.map(|label| label.to_string()),
            label_any: query
                .label_any
                .into_iter()
                .map(|label| label.to_string())
                .collect(),
            parent: query.parent,
            issue_type: query.kind,
        })
        .await
        .map_err(|error| translate(&error, false))
    }

    async fn update(
        &self,
        id: &task::Id,
        update: task::Update,
        guard: task::Guard,
    ) -> Result<(), tracking::Error> {
        let if_status = match guard {
            task::Guard::None => None,
            task::Guard::Status(status) => Some(status),
            task::Guard::Revision(_) => {
                return Err(tracking::Error::Unsupported {
                    reason: "Beads has no revision compare-and-swap".into(),
                });
            }
        };
        let set_metadata = update
            .set_metadata
            .into_iter()
            .map(|(key, value)| {
                metadata_key(&key)?;
                Ok((key, encode_metadata(value)?))
            })
            .collect::<Result<Vec<_>, tracking::Error>>()?;
        for key in &update.unset_metadata {
            metadata_key(key)?;
        }
        let guarded = if_status.is_some();
        self.update(
            id,
            UpdateOpts {
                claim: false,
                if_status,
                title: update.title,
                description: update.description,
                status: update.status,
                priority: update.priority,
                add_labels: update
                    .add_labels
                    .into_iter()
                    .map(|label| label.to_string())
                    .collect(),
                remove_labels: update
                    .remove_labels
                    .into_iter()
                    .map(|label| label.to_string())
                    .collect(),
                notes: update.notes,
                append_notes: update.append_notes,
                set_metadata,
                unset_metadata: update.unset_metadata,
            },
        )
        .await
        .map_err(|error| {
            if guarded && matches!(error, BdError::Cli { status: 13, .. }) {
                tracking::Error::Conflict {
                    reason: error.to_string(),
                }
            } else {
                translate(&error, true)
            }
        })
    }

    async fn close(&self, id: &task::Id, reason: Option<&str>) -> Result<(), tracking::Error> {
        self.close(id, reason)
            .await
            .map_err(|error| translate(&error, true))
    }

    async fn graph(&self, id: &task::Id) -> Result<task::Graph, tracking::Error> {
        self.dependency_snapshot(id)
            .await
            .map_err(|error| translate(&error, false))
    }

    async fn dependents(&self, id: &task::Id) -> Result<Vec<task::Id>, tracking::Error> {
        self.blocking_dependents(id)
            .await
            .map_err(|error| translate(&error, false))
    }

    async fn relate(
        &self,
        source: &task::Id,
        target: &task::Id,
        relation: task::Relation,
    ) -> Result<(), tracking::Error> {
        match relation {
            task::Relation::Blocks => self.dep_add(source, target).await,
            task::Relation::DiscoveredFrom => self.discovered_from(source, target).await,
        }
        .map_err(|error| translate(&error, true))
    }

    async fn parent(&self, id: &task::Id, parent: &task::Id) -> Result<(), tracking::Error> {
        self.reparent(id, parent)
            .await
            .map_err(|error| translate(&error, true))
    }

    async fn bond(&self, left: &task::Id, right: &task::Id) -> Result<(), tracking::Error> {
        self.mol_bond(left.as_str(), right.as_str())
            .await
            .map_err(|error| translate(&error, true))
    }

    async fn ready(&self, query: task::Ready) -> Result<Vec<task::Record>, tracking::Error> {
        self.ready(ReadyOpts {
            limit: Some(query.limit.unwrap_or(0)),
            label: query.label.map(|label| label.to_string()),
            parent: query.parent,
            exclude_label: query
                .exclude_labels
                .into_iter()
                .map(|label| label.to_string())
                .collect(),
        })
        .await
        .map_err(|error| translate(&error, false))
    }

    async fn progress(&self, group: &task::GroupId) -> Result<task::Progress, tracking::Error> {
        self.mol_progress(group)
            .await
            .map_err(|error| translate(&error, false))
    }

    async fn synchronize(&self, mode: tracking::Synchronization) -> Result<(), tracking::Error> {
        match mode {
            tracking::Synchronization::SharedState => Ok(()),
            tracking::Synchronization::Publish => self
                .publish()
                .await
                .map_err(|error| translate(&error, true)),
        }
    }
}
