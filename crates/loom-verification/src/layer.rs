use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

type Scores = BTreeMap<String, u64>;
type Graph = BTreeMap<String, BTreeSet<String>>;

#[derive(Deserialize)]
pub struct Attributes {
    exclude_paths: Vec<String>,
    graph: Vec<Row>,
    pipeline: Value,
}

#[derive(Deserialize)]
struct Row {
    path: String,
    references: Vec<String>,
}

pub fn read_plan(path: &Path) -> Result<Vec<Vec<String>>> {
    let mut attributes: Attributes = serde_json::from_slice(&fs::read(path)?)?;
    if let Value::String(path) = &attributes.pipeline {
        attributes.pipeline = serde_json::from_slice(&fs::read(path)?)?;
    }
    plan(&attributes)
}

fn pipeline(value: &Value) -> Result<(BTreeSet<String>, usize)> {
    let Some(steps) = value.as_array() else {
        bail!("unsupported Wrix layer pipeline")
    };
    ensure!(steps.len() == 3, "unsupported Wrix layer pipeline");
    let removed = steps[0]
        .as_array()
        .context("invalid remove_paths operation")?;
    let popularity = steps[1]
        .as_array()
        .context("invalid popularity operation")?;
    let limit = steps[2]
        .as_array()
        .context("invalid layer limit operation")?;
    ensure!(
        removed.len() == 2
            && removed[0] == "remove_paths"
            && popularity.len() == 1
            && popularity[0] == "popularity_contest"
            && limit.len() == 2
            && limit[0] == "limit_layers",
        "unsupported Wrix layer pipeline"
    );
    let paths = removed[1]
        .as_array()
        .context("remove_paths must contain strings")?
        .iter()
        .map(|p| {
            p.as_str()
                .map(str::to_owned)
                .context("remove_paths must contain strings")
        })
        .collect::<Result<_>>()?;
    let budget = usize::try_from(
        limit[1]
            .as_u64()
            .context("layer budget must be a positive integer")?,
    )?;
    ensure!(budget > 0, "layer budget must be a positive integer");
    Ok((paths, budget))
}

fn descendants(
    path: &str,
    graph: &Graph,
    active: &mut BTreeSet<String>,
    cache: &mut BTreeMap<String, Scores>,
) -> Result<Scores> {
    if let Some(found) = cache.get(path) {
        return Ok(found.clone());
    }
    ensure!(
        active.insert(path.to_owned()),
        "cyclic reference graph; Wrix planner refuses it"
    );
    let mut counts = Scores::new();
    for reference in &graph[path] {
        increment(&mut counts, reference, 1)?;
        for (child, score) in descendants(reference, graph, active, cache)? {
            increment(
                &mut counts,
                &child,
                score.checked_add(1).context("popularity score overflow")?,
            )?;
        }
    }
    active.remove(path);
    cache.insert(path.to_owned(), counts.clone());
    Ok(counts)
}

fn increment(scores: &mut Scores, path: &str, amount: u64) -> Result<()> {
    let score = scores.entry(path.to_owned()).or_default();
    *score = score
        .checked_add(amount)
        .context("popularity score overflow")?;
    Ok(())
}

fn package_name(path: &str) -> String {
    match path.split_once('-') {
        Some((prefix, rest)) => format!("{rest}-{prefix}"),
        None => path.to_owned(),
    }
}

pub fn plan(attributes: &Attributes) -> Result<Vec<Vec<String>>> {
    let (mut excluded, budget) = pipeline(&attributes.pipeline)?;
    excluded.extend(attributes.exclude_paths.iter().cloned());
    let known: BTreeSet<_> = attributes.graph.iter().map(|r| r.path.clone()).collect();
    ensure!(
        known.len() == attributes.graph.len(),
        "duplicate reference graph paths"
    );
    let mut graph = Graph::new();
    for row in &attributes.graph {
        ensure!(
            row.references.iter().all(|r| known.contains(r)),
            "reference graph is not closed"
        );
        if !excluded.contains(&row.path) {
            graph.insert(
                row.path.clone(),
                row.references
                    .iter()
                    .filter(|r| **r != row.path && !excluded.contains(*r))
                    .cloned()
                    .collect(),
            );
        }
    }
    let mut cache = BTreeMap::new();
    for path in graph.keys() {
        descendants(path, &graph, &mut BTreeSet::new(), &mut cache)?;
    }
    let referenced: BTreeSet<_> = graph.values().flatten().collect();
    let mut scores = Scores::new();
    for root in graph.keys().filter(|p| !referenced.contains(p)) {
        increment(&mut scores, root, 1)?;
        for (child, score) in &cache[root] {
            increment(
                &mut scores,
                child,
                score.checked_add(1).context("popularity score overflow")?,
            )?;
        }
    }
    let mut ordered: Vec<_> = scores.keys().cloned().collect();
    ordered.sort_by(|a, b| {
        scores[b]
            .cmp(&scores[a])
            .then_with(|| package_name(a).cmp(&package_name(b)))
    });
    let split = (budget - 1).min(ordered.len());
    let mut layers: Vec<_> = ordered[..split]
        .iter()
        .cloned()
        .map(|path| vec![path])
        .collect();
    if split < ordered.len() {
        layers.push(ordered[split..].to_vec());
    }
    Ok(layers)
}

pub fn conformance(upstream: &Path) -> Result<()> {
    use sha2::{Digest, Sha256};
    let fixture: Value = serde_json::from_str(include_str!("../tests/fixtures/layer-plan.json"))?;
    let digest = format!("{:x}", Sha256::digest(fs::read(upstream)?));
    ensure!(
        fixture["upstream_sha256"] == digest,
        "pinned upstream planner changed; regenerate and review its differential fixtures"
    );
    check_fixtures(&fixture)
}

fn check_fixtures(fixture: &Value) -> Result<()> {
    let fixtures = fixture["fixtures"]
        .as_array()
        .context("missing layer oracle fixtures")?;
    ensure!(
        fixtures.len() == 35 && fixture["seed"] == 714,
        "layer oracle campaign changed"
    );
    for row in fixtures {
        for budget in [1, 2, 8, 15, 40] {
            let mut data = row["attributes"].clone();
            data["pipeline"] = serde_json::json!([
                ["remove_paths", row["removed"]],
                ["popularity_contest"],
                ["limit_layers", budget]
            ]);
            let attributes: Attributes = serde_json::from_value(data.clone())?;
            let actual = plan(&attributes)?;
            ensure!(
                serde_json::to_value(&actual)? == row["expected"][budget.to_string()],
                "layer planner differs from pinned upstream oracle"
            );
            let flat: Vec<_> = actual.iter().flatten().collect();
            let unique: BTreeSet<_> = flat.iter().copied().collect();
            let mut excluded: BTreeSet<_> = attributes.exclude_paths.iter().cloned().collect();
            for removed in row["removed"]
                .as_array()
                .context("missing excluded paths")?
            {
                excluded.insert(
                    removed
                        .as_str()
                        .context("invalid excluded path")?
                        .to_owned(),
                );
            }
            let expected: BTreeSet<_> = attributes
                .graph
                .iter()
                .map(|r| &r.path)
                .filter(|p| !excluded.contains(*p))
                .collect();
            ensure!(
                unique == expected && flat.len() == unique.len() && actual.len() <= budget,
                "layer planner lost paths, duplicated paths or exceeded budget"
            );
            data["graph"]
                .as_array_mut()
                .context("missing graph")?
                .reverse();
            ensure!(
                actual == plan(&serde_json::from_value(data)?)?,
                "layer ordering depends on graph input order"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input(graph: Value, budget: Value) -> Attributes {
        let mut attributes = json!({"exclude_paths":[],"graph":[],"pipeline":[["remove_paths",[]],["popularity_contest"],["limit_layers",1]]});
        attributes["graph"] = graph;
        attributes["pipeline"][2][1] = budget;
        serde_json::from_value(attributes).unwrap()
    }

    #[test]
    fn exact_seeded_upstream_campaign_preserves_all_175_cases() {
        check_fixtures(
            &serde_json::from_str(include_str!("../tests/fixtures/layer-plan.json")).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn pipeline_file_is_read_by_real_cli_boundary() {
        let root = tempfile::tempdir().unwrap();
        let pipeline = root.path().join("pipeline.json");
        fs::write(
            &pipeline,
            serde_json::to_vec(&input(json!([]), json!(3)).pipeline).unwrap(),
        )
        .unwrap();
        let path = root.path().join("attributes.json");
        fs::write(&path, serde_json::to_vec(&json!({"exclude_paths": [], "graph": [{"path":"a", "references":[]}], "pipeline":pipeline})).unwrap()).unwrap();
        assert_eq!(read_plan(&path).unwrap(), vec![vec!["a"]]);
    }

    #[test]
    fn diamond_counts_shared_descendants_per_path() {
        let attributes = input(
            json!([
                {"path":"h-root","references":["h-a","h-b"]},
                {"path":"h-a","references":["h-leaf"]},
                {"path":"h-b","references":["h-leaf"]},
                {"path":"h-leaf","references":[]}
            ]),
            json!(3),
        );
        assert_eq!(
            plan(&attributes).unwrap(),
            vec![vec!["h-leaf"], vec!["h-a"], vec!["h-b", "h-root"]]
        );
    }

    #[test]
    fn malformed_pipeline_and_graph_fail_closed() {
        let mut attributes = input(json!([]), json!(1));
        assert_eq!(plan(&attributes).unwrap(), Vec::<Vec<String>>::new());
        for budget in [json!(0), json!(-1), json!(true), json!(1.5)] {
            assert!(plan(&input(json!([]), budget)).is_err());
        }
        attributes.pipeline = json!([["unknown"]]);
        assert!(plan(&attributes).is_err());
        for rows in [
            json!([{"path":"a","references":["missing"]}]),
            json!([{"path":"a","references":[]},{"path":"a","references":[]}]),
            json!([{"path":"a","references":["b"]},{"path":"b","references":["a"]}]),
        ] {
            assert!(plan(&input(rows, json!(2))).is_err());
        }
    }

    #[test]
    fn excludes_and_self_references_do_not_create_cycles() {
        let mut attributes = input(
            json!([{"path":"h-a","references":["h-a","h-b"]},{"path":"h-b","references":[]}]),
            json!(1),
        );
        attributes.exclude_paths.push("h-b".into());
        assert_eq!(plan(&attributes).unwrap(), vec![vec!["h-a"]]);
    }
}
