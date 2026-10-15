use super::*;
use loom_gate::provider::TrackedSubjects;

#[test]
fn analysis_dependencies_do_not_claim_their_unrelated_native_obligations() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let mut analysis = BTreeSet::from([name("resource:README.md")]);
    let owners = inventory
        .subjects()
        .values()
        .filter(|subject| subject.role() == loom_gate::inventory::Role::RustSource)
        .map(|subject| subject.name().clone())
        .collect::<BTreeSet<_>>();
    analysis.extend(owners.iter().cloned());
    let definition = Definition::tracked_analysis(
        &snapshot,
        &inventory,
        name("heading"),
        path("src/lib.rs"),
        TrackedSubjects::new(&analysis, &owners),
        &BTreeSet::from([Duty::Lint]),
        check,
    )
    .unwrap();
    let candidate = definition.description().clone();
    assert!(
        candidate
            .responsibilities()
            .iter()
            .all(|offered| { owners.contains(offered.subject()) && offered.duty() == Duty::Lint })
    );
    assert!(
        !candidate
            .responsibilities()
            .iter()
            .any(|offered| offered.subject() == &name("resource:README.md"))
    );
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

#[test]
fn unread_subjects_cannot_offer_native_responsibility() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let analysis = BTreeSet::from([name("resource:README.md")]);
    let owners = inventory
        .subjects()
        .values()
        .filter(|subject| subject.role() == loom_gate::inventory::Role::RustSource)
        .map(|subject| subject.name().clone())
        .collect::<BTreeSet<_>>();
    assert!(matches!(
        Definition::tracked_analysis(
            &snapshot,
            &inventory,
            name("heading"),
            path("src/lib.rs"),
            TrackedSubjects::new(&analysis, &owners),
            &BTreeSet::from([Duty::Lint]),
            check,
        ),
        Err(Error::Responsibility { .. })
    ));
}

#[test]
fn empty_analysis_cannot_admit_an_annotation_only_callback() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    assert!(matches!(
        Definition::tracked_analysis(
            &snapshot,
            &inventory,
            name("heading"),
            path("src/lib.rs"),
            TrackedSubjects::new(&BTreeSet::new(), &BTreeSet::new()),
            &BTreeSet::new(),
            check,
        ),
        Err(Error::Responsibility { .. })
    ));
}

#[test]
fn annotation_only_analysis_never_claims_native_baseline_coverage() {
    let dir = fixture();
    let (snapshot, inventory) = context(&dir);
    let analysis = BTreeSet::from([name("resource:README.md")]);
    let definitions: Vec<_> = ["heading", "first", "second"]
        .into_iter()
        .map(|target| {
            let definition = Definition::tracked_analysis(
                &snapshot,
                &inventory,
                name(target),
                path("src/lib.rs"),
                TrackedSubjects::new(&analysis, &BTreeSet::new()),
                &BTreeSet::new(),
                check,
            )
            .unwrap();
            assert!(definition.description().responsibilities().is_empty());
            definition
        })
        .collect();
    let candidate = definitions[0].description().clone();
    let registry = Registry::new(&snapshot, definitions).unwrap();
    let unit = registry
        .resolve(
            &snapshot,
            &inventory,
            &candidate,
            path("specs/example/tests.md"),
        )
        .unwrap();
    assert_eq!(unit.run().unwrap().outcome(), &Outcome::Pass);
    assert!(matches!(
        registry.admit(&snapshot, &inventory),
        Err(Error::Coverage { .. })
    ));
}

#[test]
fn exact_target_alias_executes_the_same_definition_without_a_command_bridge() {
    let dir = fixture();
    let alias = "cargo run -p loom-walk -- heading";
    write(
        dir.path(),
        "specs/example/tests.md",
        &format!("# Example\n\n## Success Criteria\n\n- Heading [check]({alias})\n"),
    );
    let (snapshot, inventory) = context(&dir);
    let original = definition(&snapshot, &inventory, "heading", check);
    let aliased = original.clone().named(name(alias)).unwrap();
    assert_eq!(aliased.description().target(), &name(alias));
    assert_ne!(
        aliased.description().definition(),
        original.description().definition()
    );
    assert_eq!(
        aliased.description().inputs(),
        original.description().inputs()
    );
    let registry = Registry::new(&snapshot, vec![aliased]).unwrap();
    let query = registry.query(&[name(alias)]).unwrap();
    let units = registry
        .resolve_query(&snapshot, &inventory, &query, &contracts(&[alias]))
        .unwrap();
    assert_eq!(units[0].run().unwrap().target(), &name(alias));
    assert_eq!(units[0].run().unwrap().outcome(), &Outcome::Pass);
    assert!(matches!(
        registry.resolve(
            &snapshot,
            &inventory,
            original.description(),
            path("specs/example/tests.md")
        ),
        Err(Error::Missing { .. })
    ));
}

fn read_members(context: &mut Context<'_>) -> Result<Outcome, Error> {
    let subjects = context.subjects().cloned().collect::<Vec<_>>();
    for subject in subjects {
        let paths = context.members(&subject)?.clone();
        for path in paths {
            context.read(&path)?;
        }
    }
    Ok(Outcome::Pass)
}

#[test]
fn analysis_scope_is_bound_even_when_both_scopes_are_already_compilation_inputs() {
    let dir = fixture();
    write(dir.path(), "tests/other.rs", "#[test]\nfn other() {}\n");
    let (snapshot, inventory) = context(&dir);
    let subjects = inventory
        .subjects()
        .values()
        .filter(|subject| subject.role() == loom_gate::inventory::Role::RustSource)
        .map(|subject| subject.name().clone())
        .collect::<Vec<_>>();
    assert_eq!(subjects.len(), 2);
    let make = |subject: &Name| {
        Definition::tracked_analysis(
            &snapshot,
            &inventory,
            name("heading"),
            path("src/lib.rs"),
            TrackedSubjects::new(&BTreeSet::from([subject.clone()]), &BTreeSet::new()),
            &BTreeSet::new(),
            read_members,
        )
        .unwrap()
    };
    let first = make(&subjects[0]);
    let second = make(&subjects[1]);
    assert_ne!(
        first.description().definition(),
        second.description().definition()
    );
    let candidate = first.description().clone();
    let registry = Registry::new(&snapshot, vec![second]).unwrap();
    assert!(matches!(
        registry.resolve(
            &snapshot,
            &inventory,
            &candidate,
            path("specs/example/tests.md")
        ),
        Err(Error::Inconsistent { .. })
    ));
}

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
