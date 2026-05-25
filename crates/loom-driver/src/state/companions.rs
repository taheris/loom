use tracing::warn;

use crate::markdown::{Event, HeadingLevel, Tag, TagEnd, section_events};

/// Parse a spec's `## Companions` section per the rules in
/// `specs/harness.md`:
///
/// - Heading must be exactly `## Companions` (case-sensitive, level 2).
/// - Body is the first flat bullet list inside the section.
/// - Each path is the single inline-code span on that bullet line.
/// - Paths normalized to repo-relative POSIX (leading `/` stripped).
/// - Missing section yields zero rows.
/// - Malformed bullets are skipped with a `warn!` rather than aborting.
///
/// The structural parser (pulldown-cmark) ignores fenced code blocks,
/// blockquotes, and indented code, so the spec's own ```markdown ...```
/// example does not anchor the section.
pub fn parse_companions(content: &str) -> Vec<String> {
    let Some(events) = section_events(content, HeadingLevel::H2, |t| t == "Companions") else {
        return Vec::new();
    };

    let mut paths = Vec::new();
    let mut list_depth: usize = 0;
    let mut first_list_finished = false;
    let mut item_codes: Vec<String> = Vec::new();
    let mut in_top_item = false;

    for (event, _) in events {
        match event {
            Event::Start(Tag::List(_)) => {
                if list_depth == 0 && first_list_finished {
                    break;
                }
                list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
                if list_depth == 0 {
                    first_list_finished = true;
                }
            }
            Event::Start(Tag::Item) if list_depth == 1 => {
                in_top_item = true;
                item_codes.clear();
            }
            Event::End(TagEnd::Item) if list_depth == 1 && in_top_item => {
                in_top_item = false;
                match item_codes.len() {
                    1 => {
                        let raw = &item_codes[0];
                        if raw.is_empty() {
                            warn!(reason = "empty path", "skipping malformed companion entry");
                        } else {
                            paths.push(normalize(raw));
                        }
                    }
                    0 => warn!(
                        reason = "no backticks",
                        "skipping malformed companion entry"
                    ),
                    _ => warn!(
                        reason = "expected exactly one backticked path",
                        "skipping malformed companion entry"
                    ),
                }
            }
            Event::Code(text) if list_depth == 1 && in_top_item => {
                item_codes.push(text.into_string());
            }
            _ => {}
        }
    }
    paths
}

fn normalize(raw: &str) -> String {
    raw.trim_start_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_section_yields_zero_paths() {
        let md = "# Spec\n\nSome body.\n\n## Architecture\n\n- nothing here\n";
        assert!(parse_companions(md).is_empty());
    }

    #[test]
    fn parses_flat_bullet_list_with_backticked_paths() {
        let md = "\
# Spec

## Companions

- `lib/sandbox/`
- `crates/loom-templates/templates/`

## Next Section

- `not/companion/`
";
        assert_eq!(
            parse_companions(md),
            vec!["lib/sandbox/", "crates/loom-templates/templates/"]
        );
    }

    #[test]
    fn strips_leading_slash_keeps_trailing_slash() {
        let md = "## Companions\n\n- `/abs/path/`\n- `relative`\n";
        assert_eq!(parse_companions(md), vec!["abs/path/", "relative"]);
    }

    #[test]
    fn skips_malformed_lines_without_aborting() {
        let md = "\
## Companions

- `ok/path/`
- no backticks here
- `multiple` `paths`
- `another/ok`
";
        assert_eq!(parse_companions(md), vec!["ok/path/", "another/ok"]);
    }

    #[test]
    fn case_sensitive_heading_does_not_match_companions_lowercase() {
        let md = "## companions\n\n- `nope/`\n";
        assert!(parse_companions(md).is_empty());
    }

    #[test]
    fn ignores_text_outside_backticks_on_bullet_lines() {
        let md = "## Companions\n\n- the path is `lib/foo/` (used by foo)\n";
        assert_eq!(parse_companions(md), vec!["lib/foo/"]);
    }

    #[test]
    fn stops_at_next_heading_of_any_level() {
        let md = "\
## Companions

- `a/`

### sub heading

- `should/not/parse/`
";
        assert_eq!(parse_companions(md), vec!["a/"]);
    }

    #[test]
    fn fenced_companions_example_inside_spec_is_ignored() {
        let md = "\
# Loom Harness

```markdown
## Companions

- `lib/sandbox/`
- `crates/loom-templates/templates/`
```

Body text.

## Companions

- `real/path/`
";
        assert_eq!(parse_companions(md), vec!["real/path/"]);
    }

    #[test]
    fn fenced_only_with_no_real_section_yields_zero() {
        let md = "\
# Spec

```markdown
## Companions

- `not/real/`
```
";
        assert!(parse_companions(md).is_empty());
    }
}
