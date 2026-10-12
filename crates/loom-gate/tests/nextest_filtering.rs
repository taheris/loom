//! Regression coverage for the syntax-only upstream filterset boundary.

use nextest_filtering::{ParsedExpr, errors::ParseSingleError};

#[test]
fn nextest_filter_grammar_errors_retain_upstream_diagnostics() {
    let cases = [
        (
            "test(",
            vec![
                ParseSingleError::InvalidString((5, 0).into()),
                ParseSingleError::ExpectedCloseParenthesis((5, 0).into()),
            ],
        ),
        (
            "test()",
            vec![ParseSingleError::InvalidString((5, 0).into())],
        ),
        (
            "test(name",
            vec![ParseSingleError::ExpectedCloseParenthesis((9, 0).into())],
        ),
        (
            "(test(name)",
            vec![ParseSingleError::ExpectedCloseParenthesis((11, 0).into())],
        ),
        (
            "all() trailing",
            vec![ParseSingleError::ExpectedEndOfExpression((5, 9).into())],
        ),
        (
            "all",
            vec![
                ParseSingleError::ExpectedOpenParenthesis((3, 0).into()),
                ParseSingleError::ExpectedCloseParenthesis((3, 0).into()),
            ],
        ),
    ];

    for (input, expected) in cases {
        let errors = ParsedExpr::parse(input).expect_err(input);
        assert_eq!(errors, expected, "input: {input}");
    }
}

#[test]
fn nextest_filter_invalid_regex_retains_upstream_message_and_span() {
    for (input, span, message) in [
        ("package(/foo(ab/)", (12, 1), "unclosed group"),
        ("test(/[/)", (6, 1), "unclosed character class"),
    ] {
        let errors = ParsedExpr::parse(input).expect_err(input);
        assert_eq!(
            errors,
            vec![ParseSingleError::InvalidRegex {
                span: span.into(),
                message: message.to_owned(),
            }],
            "input: {input}"
        );
    }
}

#[test]
fn nextest_filter_accepts_empty_regex_glob_and_escaped_closing_parentheses() {
    for (input, canonical) in [
        (" test(//) ", "test(//)"),
        ("test(#*)", "test(#*)"),
        (r"test(some(thing\))", r"test(some(thing\))"),
    ] {
        let parsed = ParsedExpr::parse(input).expect(input);
        assert_eq!(parsed.to_string(), canonical, "input: {input}");
    }
}

#[test]
fn nextest_filter_syntax_parses_without_resolving_names() {
    for input in [
        "package(=not-yet-created)",
        "deps(=not-yet-created)",
        "rdeps(=not-yet-created)",
        "binary(=not-yet-created)",
        "binary_id(=not-yet-created::integration)",
        "test(=not-yet-created::property)",
        "test(/^not_yet_created::/)",
        "(package(=future) | deps(=future)) & binary(=future) - test(/^later::/)",
    ] {
        let parsed = ParsedExpr::parse(input).expect(input);
        assert_eq!(parsed.to_string(), input);
    }
}
