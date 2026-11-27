#![cfg(test)]

use super::*;
use crate::provider::nix::{locate_tool, query, read_graph};

#[test]
fn cold_reporter_products_are_rooted_without_executing_the_consumer() {
    let tool = locate_tool().unwrap();
    let bash = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("bash"))
        .filter_map(|file| file.canonicalize().ok())
        .find(|file| file.is_file() && file.starts_with("/nix/store"))
        .unwrap();
    let bash_root = StorePath::root(&bash).unwrap();
    let nonce = tempfile::tempdir().unwrap();
    let expression = format!(
        r#"let
      attrs = {{ system = "{system}"; builder = builtins.appendContext {bash} {{ {bash_root} = {{ path = true; }}; }}; }};
      product = builtins.derivation (attrs // {{ name = "report-input"; nonce = {nonce}; outputs = [ "out" "dev" ]; args = [ "-c" "echo program > $out; echo fixture > $dev" ]; }});
      in (builtins.derivation (attrs // {{ name = "must-not-run"; input = product.dev; args = [ "-c" "exit 72" ]; }})).drvPath"#,
        system = crate::runner::result::current_platform(),
        bash = serde_json::to_string(&bash).unwrap(),
        bash_root = serde_json::to_string(&bash_root).unwrap(),
        nonce = serde_json::to_string(nonce.path()).unwrap()
    );
    let root: StorePath = serde_json::from_slice(
        &query(
            &tool,
            &["eval", "--offline", "--json", "--expr", &expression],
        )
        .unwrap(),
    )
    .unwrap();
    let graph = read_graph(&tool, &root).unwrap();
    let consumer = &graph[&root];
    let (producer, names) = consumer.dependencies.first_key_value().unwrap();
    assert_eq!(names.len(), 1);
    let name = names.first().unwrap();
    assert_eq!(name.as_str(), "dev");
    let product = &graph[producer].outputs[name];
    let valid = |path: &StorePath| {
        super::super::present_paths(&tool, &[path.clone()].into())
            .unwrap()
            .contains(path)
    };
    assert!(!valid(product));
    let lease = Products::acquire(&tool, &graph, consumer).unwrap();
    // Query the live store rather than assume a canonical host path or rely on
    // an in-process list of the paths we meant to protect.
    let roots = String::from_utf8(
        query(
            &tool.with_file_name("nix-store"),
            &["--query", "--roots", product.as_str()],
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        roots.contains(lease.roots.path().to_str().unwrap()),
        "{roots}"
    );
    assert!(valid(product));
    assert!(consumer.outputs.values().all(|path| !valid(path)));
    let directory = lease.roots.path().to_owned();
    drop(lease);
    assert!(!directory.exists());
}

#[test]
fn incomplete_duplicate_or_wrong_product_cannot_satisfy_acquisition() {
    let recipe = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-test.drv";
    let path = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-product";
    let expected = BTreeMap::from([(
        StorePath::try_from(recipe.to_owned()).unwrap(),
        BTreeMap::from([(
            Name::try_from("dev".to_owned()).unwrap(),
            StorePath::try_from(path.to_owned()).unwrap(),
        )]),
    )]);
    let built = serde_json::json!({"drvPath":recipe,"outputs":{"dev":path}});
    for rows in [
        serde_json::json!([]),
        serde_json::json!([built, built]),
        serde_json::json!([{"drvPath":recipe,"outputs":{"out":path}}]),
        serde_json::json!([{"drvPath":recipe,"outputs":{"dev":recipe}}]),
    ] {
        assert!(require_outputs(&serde_json::to_vec(&rows).unwrap(), expected.clone()).is_err());
    }
    require_outputs(&serde_json::to_vec(&[built]).unwrap(), expected).unwrap();
}
