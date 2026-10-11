//! Strict strategy declarations exercise the production Markdown/TOML boundary.

use std::error::Error as _;
use std::fs;
use std::path::Path;

use loom_gate::annotation::strategy::{Declaration, Deferral, Error};
use loom_gate::annotation::{ParseError, ParsedSpecs, parse_content, parse_workspace};
use loom_test_support::proptest_config;
use nextest_filtering::ParsedExpr;
use proptest::prelude::*;

fn parse(body: &str) -> Result<ParsedSpecs, ParseError> {
    parse_content(Path::new("specs/example/tests.md"), body)
}

fn package_block(body: &str) -> String {
    format!("```loom-verify\n{body}\n```\n\n## Success Criteria\n- Baseline [test](ordinary)\n")
}

fn local_block(body: &str) -> String {
    format!(
        "## Success Criteria\n\n- Requirement [test](shared)\n\n  ```loom-verify\n  {body}\n  ```\n"
    )
}

#[test]
fn quint_verify_blocks_parse_optional_strict_strategy() {
    let body = "## Success Criteria\n\n- Expensive requirement [test](shared)\n\n  ```loom-verify\n  defer_until = 'publication'\n  ```\n\n- Ordinary requirement [test](shared)\n\n- Independent requirement [test?](other)\n\n    ~~~loom-verify\n    defer_until = 'publication'\n    ~~~\n";
    let parsed = parse(body).unwrap();
    assert_eq!(parsed.criteria.len(), 3);
    assert_eq!(parsed.annotations.len(), 3);
    assert_eq!(parsed.annotations[0].target, parsed.annotations[1].target);
    assert_eq!(parsed.criteria[0].text, "Expensive requirement");
    assert_eq!(parsed.criteria[1].text, "Ordinary requirement");
    assert_eq!(parsed.criteria[2].text, "Independent requirement");
    assert_eq!(parsed.strategy.len(), 2);
    for (declaration, owner) in parsed.strategy.iter().zip([0, 2]) {
        let Declaration::Criterion {
            source,
            line,
            defer_until,
        } = declaration
        else {
            panic!("expected a criterion-local declaration");
        };
        assert_eq!(*line, parsed.criteria[owner].line);
        assert_eq!(*defer_until, Deferral::Publication);
        assert_eq!(source.document, Path::new("specs/example/tests.md"));
        assert!(body[source.bytes.clone()].contains("defer_until"));
    }
    let baseline = parse("## Success Criteria\n- Ordinary [test](shared)\n").unwrap();
    assert_eq!(baseline.criteria.len(), 1);
    assert!(baseline.strategy.is_empty());
    for invalid in [
        local_block("not TOML"),
        local_block("defer_until = 'feedback'"),
        local_block("defer_until = ['publication']"),
        local_block("defer_until = 'publication'\n  disabled = true"),
        format!("{}\n  ```loom-verify\n  defer_until = 'publication'\n  ```\n", local_block("defer_until = 'publication'")),
        "## Success Criteria\n- Detached [test](shared)\n\n```loom-verify\ndefer_until = 'publication'\n```\n".to_owned(),
        "## Success Criteria\n- Ambiguous [test](one) [test](two)\n\n  ```loom-verify\n  defer_until = 'publication'\n  ```\n".to_owned(),
    ] {
        assert!(matches!(parse(&invalid), Err(ParseError::Strategy { .. })));
    }
}

#[test]
fn quint_verify_blocks_parse_property_filters_and_role_exceptions() {
    let expression = "package(=not-yet-discovered) & (test(/^properties::/) | binary(=shared))";
    let body = package_block(&format!(
        "[rust]\nproperty_filters = ['{expression}']\n\n[[role_exception]]\nsubject = 'fixture:invalid-rust'\nrole = 'test-fixture'\nreason = 'Consumed by parser tests.'\n\n[[role_exception]]\nsubject = 'generated:future-resource'\nrole = 'generated'\nreason = '''Produced by a build step.\nVerified by its consumer.'''"
    ));
    let parsed = parse(&body).unwrap();
    let [Declaration::Package { source, policy }] = parsed.strategy.as_slice() else {
        panic!("expected package strategy");
    };
    assert_eq!(source.line, 1);
    assert_eq!(source.document, Path::new("specs/example/tests.md"));
    assert!(body[source.bytes.clone()].starts_with("```loom-verify\n"));
    let filters = &policy.rust.as_ref().unwrap().property_filters;
    assert_eq!(filters.len(), 1);
    assert_eq!(filters[0].expression(), expression);
    assert_eq!(filters[0].parsed(), &ParsedExpr::parse(expression).unwrap());
    assert_eq!(policy.role_exceptions.len(), 2);
    assert_eq!(
        policy.role_exceptions[0].subject.as_str(),
        "fixture:invalid-rust"
    );
    assert_eq!(policy.role_exceptions[0].role.as_str(), "test-fixture");
    assert_eq!(
        policy.role_exceptions[0].reason,
        "Consumed by parser tests."
    );
    assert_eq!(
        policy.role_exceptions[1].subject.as_str(),
        "generated:future-resource"
    );
    assert!(policy.role_exceptions[1].reason.contains('\n'));
    assert_eq!(parsed.criteria[0].text, "Baseline");
    for invalid in [
        "[rust]\nproperty_filters = 'all()'",
        "[rust]\nproperty_filters = [42]",
        "[rust]\nproperty_filters = ['test(']",
        "[[role_exception]]\nsubject = 42\nrole = 'fixture'\nreason = 'cost'",
        "[[role_exception]]\nsubject = 'a'\nrole = false\nreason = 'cost'",
        "[[role_exception]]\nsubject = 'a'\nrole = 'fixture'\nreason = []",
        "[[role_exception]]\nsubject = 'a'\nrole = 'fixture'\nreason = 'cost'\nfiles = ['a.rs']",
    ] {
        assert!(matches!(
            parse(&package_block(invalid)),
            Err(ParseError::Strategy { .. })
        ));
    }
    let conventions = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/spec-conventions.md"),
    )
    .unwrap();
    assert!(parse(&conventions).unwrap().strategy.is_empty());
}

#[test]
fn absent_strategy_preserves_baseline_criteria_and_bindings() {
    let body =
        "## Success Criteria\n- Ordinary [test](shared)\n- Future [check?](future-command)\n";
    let parsed = parse(body).unwrap();
    assert!(parsed.strategy.is_empty());
    assert_eq!(parsed.criteria.len(), 2);
    assert_eq!(parsed.annotations.len(), 2);
    assert_eq!(parsed.criteria[0].text, "Ordinary");
    assert_eq!(parsed.annotations[0].target, "shared");
    assert!(parsed.annotations[1].pending);
}

#[test]
fn only_exact_fenced_info_strings_activate_strategy() {
    for info in [
        "toml",
        "Loom-verify",
        "loom-verify toml",
        "loom-verify extra",
    ] {
        let body = format!(
            "## Success Criteria\n- Example [test](real)\n\n  ```{info}\n  not even TOML [test](fake)\n  ```\n"
        );
        let parsed = parse(&body).unwrap();
        assert!(parsed.strategy.is_empty(), "{info}");
        assert_eq!(parsed.annotations.len(), 1);
        assert!(parsed.criteria[0].text.contains("not even TOML"));
    }
    let indented = "## Success Criteria\n- Real [test](real)\n\nOutside paragraph.\n\n    ```loom-verify\n    not even TOML [test](fake)\n    ```\n";
    let parsed = parse(indented).unwrap();
    assert!(parsed.strategy.is_empty());
    assert_eq!(parsed.annotations.len(), 1);
}

#[test]
fn ordinary_toml_example_is_requirement_text_not_policy() {
    let body = "## Success Criteria\n- Example [test](real)\n\n  ```toml\n  [rust]\n  property_filters = ['all()']\n  ```\n";
    let parsed = parse(body).unwrap();
    assert!(parsed.strategy.is_empty());
    assert!(
        parsed.criteria[0]
            .text
            .contains("property_filters = ['all()']")
    );
}

#[test]
fn unsupported_deferrals_and_local_unknown_fields_are_rejected() {
    for declaration in [
        "defer_until = 'feedback'",
        "defer_until = 'never'",
        "defer_until = ['publication']",
        "defer_until = true",
        "stages = ['publication']",
        "defer_until = 'publication'\n  target = 'shared'",
        "defer_until = 'publication'\n  reason = 'slow'",
        "defer_until = 'publication'\n  defer_until = 'publication'",
    ] {
        assert!(
            matches!(
                parse(&local_block(declaration)),
                Err(ParseError::Strategy { .. })
            ),
            "{declaration}"
        );
    }
}

#[test]
fn malformed_package_strategy_fails_closed() {
    for declaration in [
        "",
        "[rust",
        "unknown = true",
        "[rust]\nproperty_filters = 'all()'",
        "[rust]\nproperty_filters = [42]",
        "[rust]\nproperty_filters = []\nunknown = true",
        "[rust]",
        "[[role_exception]]\nsubject = 'a'\nrole = 'fixture'",
        "[[role_exception]]\nsubject = 42\nrole = 'fixture'\nreason = 'cost'",
        "[[role_exception]]\nsubject = 'a'\nrole = false\nreason = 'cost'",
        "[[role_exception]]\nsubject = 'a'\nrole = 'fixture'\nreason = []",
        "[[role_exception]]\nsubject = 'a'\nrole = 'fixture'\nreason = 'cost'\nfiles = ['a.rs']",
        "[[role_exception]]\nsubject = ''\nrole = 'fixture'\nreason = 'cost'",
        "[[role_exception]]\nsubject = 'a'\nrole = ' '\nreason = 'cost'",
        "[[role_exception]]\nsubject = 'a'\nrole = 'fixture'\nreason = ' '",
        "[[role_exception]]\nsubject = \"a\\nname\"\nrole = 'fixture'\nreason = 'cost'",
    ] {
        assert!(
            matches!(
                parse(&package_block(declaration)),
                Err(ParseError::Strategy { .. })
            ),
            "{declaration}"
        );
    }
}

#[test]
fn nextest_filter_grammar_errors_retain_upstream_diagnostics() {
    for expression in ["test(", "unknown(name)", "test(/[/)", "all() trailing", ""] {
        let body = package_block(&format!("[rust]\nproperty_filters = ['{expression}']"));
        let Err(ParseError::Strategy {
            source:
                Error::Filter {
                    expression: actual,
                    errors,
                },
            ..
        }) = parse(&body)
        else {
            panic!("accepted invalid nextest expression: {expression}");
        };
        assert_eq!(actual, expression);
        assert_eq!(errors, ParsedExpr::parse(expression).unwrap_err());
    }
}

#[test]
fn criterion_stage_requires_unambiguous_markdown_containment() {
    for body in [
        "## Success Criteria\n- Requirement [test](shared)\n\n```loom-verify\ndefer_until = 'publication'\n```\n",
        "## Success Criteria\n\n```loom-verify\ndefer_until = 'publication'\n```\n\n- Requirement [test](shared)\n",
        "## Architecture\n- Not acceptance [test](shared)\n\n  ```loom-verify\n  defer_until = 'publication'\n  ```\n",
        "## Success Criteria\n- No annotation\n\n  ```loom-verify\n  defer_until = 'publication'\n  ```\n",
        "## Success Criteria\n- Two [test](one) [test](two)\n\n  ```loom-verify\n  defer_until = 'publication'\n  ```\n",
        "## Success Criteria\n- Outer [test](one)\n  - Inner without a binding\n\n    ```loom-verify\n    defer_until = 'publication'\n    ```\n",
        "## Success Criteria\nProse [test](shared)\n\n```loom-verify\ndefer_until = 'publication'\n```\n",
    ] {
        assert!(
            matches!(
                parse(body),
                Err(ParseError::Strategy {
                    source: Error::Attachment,
                    ..
                })
            ),
            "{body}"
        );
    }
}

#[test]
fn criterion_stage_must_follow_annotation_directly() {
    for body in [
        "## Success Criteria\n- Requirement\n\n  ```loom-verify\n  defer_until = 'publication'\n  ```\n\n  [test](shared)\n",
        "## Success Criteria\n- Requirement [test](shared) qualifier\n\n  ```loom-verify\n  defer_until = 'publication'\n  ```\n",
        "## Success Criteria\n- Requirement [test](shared)\n\n  ```toml\n  example = true\n  ```\n\n  ```loom-verify\n  defer_until = 'publication'\n  ```\n",
        "## Success Criteria\n- Requirement [test](shared)\n\n  ### Heading\n\n  ```loom-verify\n  defer_until = 'publication'\n  ```\n",
    ] {
        assert!(
            matches!(
                parse(body),
                Err(ParseError::Strategy {
                    source: Error::Attachment,
                    ..
                })
            ),
            "{body}"
        );
    }
}

#[test]
fn formatting_comments_do_not_detach_local_strategy() {
    let body = "## Success Criteria\n- Requirement [check](bash -c 'printf \"(test)\"')\n  <!-- prettier-ignore -->\n\n  ```loom-verify\n  defer_until = 'publication'\n  ```\n";
    let parsed = parse(body).unwrap();
    assert_eq!(parsed.strategy.len(), 1);
    assert_eq!(parsed.criteria[0].text, "Requirement");
}

#[test]
fn package_policy_and_criterion_deferral_cannot_be_mixed() {
    let mixed = "defer_until = 'publication'\n[rust]\nproperty_filters = ['all()']";
    assert!(matches!(
        parse(&package_block(mixed)),
        Err(ParseError::Strategy {
            source: Error::Mixed,
            ..
        })
    ));
    assert!(matches!(
        parse(&local_block(
            "role_exception = []\n  defer_until = 'publication'"
        )),
        Err(ParseError::Strategy {
            source: Error::Mixed,
            ..
        })
    ));
    let local = local_block("[rust]\n  property_filters = ['all()']");
    assert!(matches!(
        parse(&local),
        Err(ParseError::Strategy {
            source: Error::Attachment,
            ..
        })
    ));
}

proptest! {
    #![proptest_config(proptest_config())]

    #[test]
    fn local_strategy_identity_is_independent_of_metadata_and_fence_layout(
        words in prop::collection::vec("[a-zA-Z]{1,15}", 1..8),
        rationale in "[a-zA-Z0-9 ]{0,60}",
        indent in 2_usize..5,
        tilde in any::<bool>(),
        crlf in any::<bool>(),
    ) {
        let requirement = words.join(" ");
        let baseline = format!("## Success Criteria\n- {requirement} [test](shared)\n");
        let marker = if tilde { "~~~" } else { "```" };
        let pad = " ".repeat(indent);
        let attached = format!(
            "{baseline}\n{pad}{marker}loom-verify\n{pad}# {rationale}\n{pad}defer_until = 'publication'\n{pad}{marker}\n"
        );
        let attached = if crlf { attached.replace('\n', "\r\n") } else { attached };
        let before = parse(&baseline).unwrap();
        let after = parse(&attached).unwrap();
        prop_assert_eq!(&after.criteria[0].text, &before.criteria[0].text);
        prop_assert_eq!(&after.annotations, &before.annotations);
        prop_assert_eq!(after.strategy.len(), 1);
        prop_assert_eq!(
            loom_gate::annotation::criterion_text_for_line(&attached, after.criteria[0].line),
            requirement
        );
    }
}

#[test]
fn ordinary_example_edits_change_requirement_identity() {
    let baseline =
        "## Success Criteria\n- Example [test](real)\n\n  ```toml\n  value = 'first'\n  ```\n";
    let changed = baseline.replace("'first'", "'second'");
    let label = loom_driver::identifier::SpecLabel::new("example").unwrap();
    let before = parse(baseline).unwrap();
    let after = parse(&changed).unwrap();
    assert_ne!(
        loom_gate::annotation::criterion_id_for(&label, &before.criteria[0].text),
        loom_gate::annotation::criterion_id_for(&label, &after.criteria[0].text)
    );
    assert!(before.strategy.is_empty());
    assert!(after.strategy.is_empty());
}

#[test]
fn repeated_package_declarations_are_conflicts() {
    let rust = "```loom-verify\n[rust]\nproperty_filters = []\n```\n";
    let role = "```loom-verify\n[[role_exception]]\nsubject = 'named'\nrole = 'test-fixture'\nreason = 'Consumed by tests.'\n```\n";
    for body in [
        format!("{rust}\n{rust}"),
        format!("{role}\n{role}"),
        package_block("[rust]\nproperty_filters = ['all()', 'all()']"),
        package_block(
            "[[role_exception]]\nsubject = 'named'\nrole = 'test-fixture'\nreason = 'one'\n[[role_exception]]\nsubject = 'named'\nrole = 'generated'\nreason = 'two'",
        ),
    ] {
        assert!(
            matches!(
                parse(&body),
                Err(ParseError::Strategy {
                    source: Error::Conflict { .. },
                    ..
                })
            ),
            "{body}"
        );
    }
}

#[test]
fn repeated_local_deferrals_are_rejected() {
    let body = format!(
        "{}\n  ```loom-verify\n  defer_until = 'publication'\n  ```\n",
        local_block("defer_until = 'publication'")
    );
    assert!(matches!(parse(&body), Err(ParseError::Strategy { .. })));
}

#[test]
fn strategy_errors_preserve_document_fence_line_and_toml_cause() {
    let body = "## Success Criteria\n\n- Requirement [test](shared)\n\n  ```loom-verify\n  defer_until = 123\n  ```\n";
    let error = parse(body).unwrap_err();
    let ParseError::Strategy {
        path,
        line,
        source: Error::Toml(ref cause),
    } = error
    else {
        panic!("expected contextual TOML error");
    };
    assert_eq!(path, Path::new("specs/example/tests.md"));
    assert_eq!(line, 5);
    assert!(cause.span().is_some());
    assert!(cause.source().is_some() || !cause.message().is_empty());
}

#[test]
fn canonical_package_strategy_is_aggregated_without_supporting_examples() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("docs")).unwrap();
    fs::write(dir.path().join("docs/README.md"), "## Specs\n| Spec | Purpose |\n| --- | --- |\n| [alpha](../specs/alpha/spec.md) | owner |\n| [beta](../specs/beta/spec.md) | owner |\n").unwrap();
    for label in ["beta", "alpha"] {
        let package = dir.path().join("specs").join(label);
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join("spec.md"),
            "[Acceptance](tests.md#success-criteria)\n",
        )
        .unwrap();
        fs::write(
            package.join("tests.md"),
            local_block("defer_until = 'publication'"),
        )
        .unwrap();
        fs::write(package.join("support.md"), package_block("invalid = true")).unwrap();
    }
    let parsed = parse_workspace(dir.path()).unwrap();
    assert_eq!(parsed.criteria.len(), 2);
    assert_eq!(parsed.strategy.len(), 2);
    for (declaration, label) in parsed.strategy.iter().zip(["alpha", "beta"]) {
        let Declaration::Criterion { source, line, .. } = declaration else {
            panic!("expected local strategy");
        };
        assert_eq!(*line, 3);
        assert_eq!(
            source.document,
            dir.path().join("specs").join(label).join("tests.md")
        );
    }
    fs::write(
        dir.path().join("specs/beta/tests.md"),
        package_block("invalid = true"),
    )
    .unwrap();
    assert!(
        matches!(parse_workspace(dir.path()), Err(ParseError::Strategy { path, .. }) if path.ends_with("beta/tests.md"))
    );
}
