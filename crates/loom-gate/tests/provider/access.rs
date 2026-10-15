use super::*;

fn hidden_build_read<const SOURCE: bool>(context: &mut Context<'_>) -> Result<Outcome, Error> {
    let resource = if SOURCE { "src/lib.rs" } else { "Cargo.toml" };
    match context.text(&path(resource)) {
        Err(Error::Outside { .. }) => Ok(Outcome::Pass),
        Err(error) => Err(error),
        Ok(_) => Ok(Outcome::Fail {
            detail: "build input leaked into analysis access".into(),
        }),
    }
}

fn reject<F>(check: F, resource: &str)
where
    F: for<'a> Fn(&mut Context<'a>) -> Result<Outcome, Error> + Send + Sync + 'static,
{
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", check);
    assert!(
        definition
            .description()
            .inputs()
            .iter()
            .any(|input| input.path() == &path(resource))
    );
    let candidate = definition.description().clone();
    let registry = Registry::new(&snapshot, vec![definition]).unwrap();
    let unit = registry
        .resolve(
            &snapshot,
            &inventory,
            &candidate,
            path("specs/example/tests.md"),
        )
        .unwrap();
    assert!(
        matches!(unit.run(), Err(Error::Outside { path: outside }) if outside == path(resource))
    );
}

#[test]
fn checker_identity_does_not_authorize_an_undeclared_analysis_read_of_checker_source() {
    reject(hidden_build_read::<true>, "src/lib.rs");
}

#[test]
fn build_configuration_does_not_authorize_an_undeclared_analysis_read() {
    reject(hidden_build_read::<false>, "Cargo.toml");
}

fn ignored_outside_scan(context: &mut Context<'_>) -> Result<Outcome, Error> {
    match context.scan(Some(&path("src")), Some("rs")) {
        Err(Error::Outside { .. }) => Ok(Outcome::Pass),
        Err(error) => Err(error),
        Ok(_) => Ok(Outcome::Fail {
            detail: "an undeclared source scope was admitted".into(),
        }),
    }
}

#[test]
fn a_scan_cannot_hide_undeclared_existing_sources_as_an_empty_scope() {
    reject(ignored_outside_scan, "src/lib.rs");
}

fn enumerate(context: &mut Context<'_>) -> Result<Outcome, Error> {
    let subjects = context.subjects().cloned().collect::<BTreeSet<_>>();
    let paths = context.paths().cloned().collect::<BTreeSet<_>>();
    if subjects != BTreeSet::from([name("resource:README.md")])
        || paths != BTreeSet::from([path("README.md")])
    {
        return Ok(Outcome::Fail {
            detail: "analysis enumeration included build-only inputs".into(),
        });
    }
    if context.scan(Some(&path("README.md")), Some("md"))? != BTreeSet::from([path("README.md")])
        || !context.directory(None).contains(&path("src"))
        || context.directory(Some(&path("src"))) != BTreeSet::from([path("src/lib.rs")])
        || !context.directory_exists(&path("src"))
        || context.directory_exists(&path("missing"))
    {
        return Ok(Outcome::Fail {
            detail: "captured namespace discovery was incomplete".into(),
        });
    }
    check(context)
}

#[test]
fn captured_path_enumeration_is_complete_and_excludes_build_only_inputs() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let definition = definition(&snapshot, &inventory, "heading", enumerate);
    let candidate = definition.description().clone();
    let registry = Registry::new(&snapshot, vec![definition]).unwrap();
    let unit = registry
        .resolve(
            &snapshot,
            &inventory,
            &candidate,
            path("specs/example/tests.md"),
        )
        .unwrap();
    assert_eq!(unit.run().unwrap().outcome(), &Outcome::Pass);
}
