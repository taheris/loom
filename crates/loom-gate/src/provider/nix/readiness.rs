//! A readiness pass shares availability queries, never execution facts or approval.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::{Error, Graph, Platforms, StorePath, frontier, present_paths, query};

#[derive(Default)]
pub(in crate::provider) struct Readiness {
    tools: BTreeMap<PathBuf, (Platforms, frontier::Presence)>,
}

impl Readiness {
    pub(in crate::provider) fn cold_inputs(
        &mut self,
        graph: &Graph,
    ) -> Result<BTreeSet<StorePath>, Error> {
        let (platform, presence) = match self.tools.entry(graph.tool.clone()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => entry.insert((
                serde_json::from_slice(&query(&graph.tool, &["config", "show", "--json"])?)?,
                frontier::Presence::default(),
            )),
        };
        graph.cold_inputs(platform, |paths| {
            presence.query(paths, |paths| present_paths(&graph.tool, paths))
        })
    }
}
