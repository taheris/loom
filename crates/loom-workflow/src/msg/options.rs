//! Parser for the **Options Format Contract** on clarify beads.
//!
//! Clarify beads enumerate their options with two heading shapes:
//!
//! ```markdown
//! ## Options — <one-line summary, ≤50 chars>
//!
//! ### Option 1 — <short title>
//! <body>
//!
//! ### Option 2 — <short title>
//! <body>
//! ```
//!
//! Separators between `Options` / `Option N` and the trailing summary or
//! title may be em-dash `—`, en-dash `–`, single hyphen `-`, or double
//! hyphen `--`. The parser tolerates any of these. Headings inside fenced
//! code blocks are ignored.

use std::ops::Range;

use loom_driver::markdown::{Event, HeadingLevel, Tag, TagEnd, parser};
use pulldown_cmark::OffsetIter;

/// Result of parsing one bead description against the contract.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OptionsParse {
    /// One-line summary trailing the `## Options` heading. Empty when the
    /// header is absent or carries no summary.
    pub summary: String,

    /// `### Option N — <title>` subsections in source order. Empty when no
    /// numbered options are present.
    pub options: Vec<OptionEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionEntry {
    /// 1-based numbering as authored in `### Option N`. Used by `-a <int>`
    /// fast-reply lookup.
    pub n: u32,
    pub title: String,
    pub body: String,
}

struct SubHead {
    n: u32,
    title: String,
    body_start: usize,
    body_end: usize,
}

/// Parse an `## Options` block from a bead description per the contract.
///
/// Behaviour:
/// - Returns `OptionsParse::default()` when the description has no
///   `## Options` heading.
/// - Trailing summary is stripped of the leading separator (`—`, `–`, `-`,
///   `--`) before being returned.
/// - Each `### Option N` subsection extends from its heading until the next
///   `### Option` or the next `##` heading (whichever comes first).
///
/// Use [`parse_options_in`] when the bead may carry options in `--notes`
/// (the path the reviewer takes when promoting a `loom:blocked` bead to
/// `loom:clarify`, per `specs/gate.md` § Options Format Contract).
pub fn parse_options(description: &str) -> OptionsParse {
    let mut parse = OptionsParse::default();
    let mut iter = parser(description).into_offset_iter();

    let Some(summary_raw) = find_options_summary(&mut iter) else {
        return parse;
    };
    parse.summary = strip_separator(&summary_raw);

    let mut subheadings: Vec<SubHead> = Vec::new();
    let mut section_end = description.len();

    while let Some((event, range)) = iter.next() {
        let Event::Start(Tag::Heading { level, .. }) = event else {
            continue;
        };
        if level == HeadingLevel::H2 {
            if let Some(last) = subheadings.last_mut() {
                last.body_end = range.start;
            }
            section_end = range.start;
            break;
        }
        if level != HeadingLevel::H3 {
            consume_to_heading_end(&mut iter);
            continue;
        }
        let mut text = String::new();
        let mut heading_end = range.end;
        for (e, r) in iter.by_ref() {
            if matches!(e, Event::End(TagEnd::Heading(_))) {
                heading_end = r.end;
                break;
            }
            if let Event::Text(t) | Event::Code(t) = e {
                text.push_str(&t);
            }
        }
        if let Some((n, rest)) = parse_option_heading(&text) {
            if let Some(last) = subheadings.last_mut() {
                last.body_end = range.start;
            }
            subheadings.push(SubHead {
                n,
                title: strip_separator(rest),
                body_start: heading_end,
                body_end: section_end,
            });
        }
    }
    if let Some(last) = subheadings.last_mut()
        && last.body_end > section_end
    {
        last.body_end = section_end;
    }

    parse.options = subheadings
        .into_iter()
        .map(|s| OptionEntry {
            n: s.n,
            title: s.title,
            body: trim_blank_lines(description.get(s.body_start..s.body_end).unwrap_or("")),
        })
        .collect();
    parse
}

/// Parse an `## Options` block from a bead, preferring `notes` over
/// `description` when notes carries the canonical block.
///
/// The reviewer promotes a previously `loom:blocked` bead to
/// `loom:clarify` by writing the options into `--notes`; new clarify
/// beads carry their options in `--description`. The msg queue must
/// surface both, so [`to_clarify_bead`] / [`build_rows`] call this
/// wrapper instead of [`parse_options`] directly.
///
/// Behaviour: parse `notes` first; if it produces any summary or option
/// rows, return that parse. Otherwise fall back to parsing
/// `description`. An empty options block in either source yields the
/// default (empty) parse.
///
/// [`to_clarify_bead`]: super::context
/// [`build_rows`]: super::list::build_rows
pub fn parse_options_in(notes: Option<&str>, description: &str) -> OptionsParse {
    if let Some(n) = notes {
        let parsed = parse_options(n);
        if !parsed.summary.is_empty() || !parsed.options.is_empty() {
            return parsed;
        }
    }
    parse_options(description)
}

/// Byte range of the originating `## Options` block in `text`.
///
/// The block starts at the byte offset of the `## Options` H2 heading and
/// ends at the byte offset of the next H2 heading, or at the end of `text`
/// when no following H2 exists. Returns `None` when no `## Options` H2 is
/// present.
///
/// Used by [`strip_options_block`] to splice the block out of a bead's
/// notes when a `loom:clarify` resolution writes its note (per `specs/gate.md`
/// § Resolution lifecycle).
pub fn find_options_block_range(text: &str) -> Option<Range<usize>> {
    let mut iter = parser(text).into_offset_iter();
    let mut start: Option<usize> = None;
    while let Some((event, range)) = iter.next() {
        let Event::Start(Tag::Heading {
            level: HeadingLevel::H2,
            ..
        }) = event
        else {
            continue;
        };
        let mut heading_text = String::new();
        for (e, _) in iter.by_ref() {
            if matches!(e, Event::End(TagEnd::Heading(_))) {
                break;
            }
            if let Event::Text(t) | Event::Code(t) = e {
                heading_text.push_str(&t);
            }
        }
        let trimmed = heading_text.trim_start();
        let Some(rest) = trimmed.strip_prefix("Options") else {
            continue;
        };
        if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
            continue;
        }
        start = Some(range.start);
        break;
    }
    let start = start?;
    for (event, range) in iter {
        if let Event::Start(Tag::Heading {
            level: HeadingLevel::H2,
            ..
        }) = event
        {
            return Some(start..range.start);
        }
    }
    Some(start..text.len())
}

/// Return `text` with the originating `## Options` block spliced out.
///
/// Returns the input unchanged when no `## Options` H2 heading is present.
/// Adjacent trailing whitespace from the spliced range is preserved; callers
/// typically `trim_end` the result before stitching it back to a resolution
/// note (see [`super::reply::compose_resolved_notes`]).
pub fn strip_options_block(text: &str) -> String {
    match find_options_block_range(text) {
        Some(range) => {
            let mut out = String::with_capacity(text.len());
            out.push_str(&text[..range.start]);
            out.push_str(&text[range.end..]);
            out
        }
        None => text.to_string(),
    }
}

fn find_options_summary(iter: &mut OffsetIter<'_>) -> Option<String> {
    while let Some((event, _)) = iter.next() {
        let Event::Start(Tag::Heading {
            level: HeadingLevel::H2,
            ..
        }) = event
        else {
            continue;
        };
        let mut text = String::new();
        for (e, _) in iter.by_ref() {
            if matches!(e, Event::End(TagEnd::Heading(_))) {
                break;
            }
            if let Event::Text(t) | Event::Code(t) = e {
                text.push_str(&t);
            }
        }
        let trimmed = text.trim_start();
        let Some(rest) = trimmed.strip_prefix("Options") else {
            continue;
        };
        if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
            continue;
        }
        return Some(rest.trim_start().to_string());
    }
    None
}

fn parse_option_heading(text: &str) -> Option<(u32, &str)> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix("Option")?;
    let rest = rest.strip_prefix(' ')?;
    let trimmed = rest.trim_start();
    let (digits, after) = take_digits(trimmed);
    if digits.is_empty() {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    if !after.is_empty() && !after.starts_with(char::is_whitespace) {
        return None;
    }
    Some((n, after.trim_start()))
}

fn take_digits(s: &str) -> (&str, &str) {
    let mut end = 0;
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    s.split_at(end)
}

fn strip_separator(rest: &str) -> String {
    let trimmed = rest.trim_start();
    let after = if let Some(s) = trimmed.strip_prefix("—") {
        s
    } else if let Some(s) = trimmed.strip_prefix("–") {
        s
    } else if let Some(s) = trimmed.strip_prefix("--") {
        s
    } else if let Some(s) = trimmed.strip_prefix('-') {
        s
    } else {
        trimmed
    };
    after.trim().to_string()
}

fn trim_blank_lines(s: &str) -> String {
    let mut out = s.to_string();
    while out.ends_with('\n') || out.ends_with('\r') {
        out.pop();
    }
    while out.starts_with('\n') || out.starts_with('\r') {
        out.remove(0);
    }
    out
}

fn consume_to_heading_end(iter: &mut OffsetIter<'_>) {
    for (e, _) in iter.by_ref() {
        if matches!(e, Event::End(TagEnd::Heading(_))) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_options_section_returns_default() {
        let parse = parse_options("Just a body, no options.");
        assert!(parse.summary.is_empty());
        assert!(parse.options.is_empty());
    }

    #[test]
    fn options_em_dash_summary_and_three_options() {
        let desc = "\
## Options — pick a path

### Option 1 — Preserve invariant
Revert the change. Cost: churn.

### Option 2 — Keep on top
Accept the clash. Cost: debt.

### Option 3 — Change invariant
Update the spec. Cost: realignment.
";
        let parse = parse_options(desc);
        assert_eq!(parse.summary, "pick a path");
        assert_eq!(parse.options.len(), 3);
        assert_eq!(parse.options[0].n, 1);
        assert_eq!(parse.options[0].title, "Preserve invariant");
        assert!(parse.options[0].body.contains("Revert the change"));
        assert_eq!(parse.options[2].title, "Change invariant");
    }

    #[test]
    fn separator_variants_all_strip_cleanly() {
        for sep in ["—", "–", "-", "--"] {
            let desc = format!("## Options {sep} summary text\n\n### Option 1 {sep} title\nbody\n");
            let parse = parse_options(&desc);
            assert_eq!(parse.summary, "summary text", "sep={sep}");
            assert_eq!(parse.options[0].title, "title", "sep={sep}");
        }
    }

    #[test]
    fn option_body_extends_to_next_option_heading() {
        let desc = "\
## Options

### Option 1 — first
line a
line b

### Option 2 — second
line c
";
        let parse = parse_options(desc);
        assert_eq!(parse.options.len(), 2);
        assert_eq!(parse.options[0].body, "line a\nline b");
        assert_eq!(parse.options[1].body, "line c");
    }

    #[test]
    fn next_h2_terminates_options_block() {
        let desc = "\
## Options — sum

### Option 1 — t1
body1

## Other section
ignored
";
        let parse = parse_options(desc);
        assert_eq!(parse.options.len(), 1);
        assert_eq!(parse.options[0].body, "body1");
    }

    #[test]
    fn options_without_summary_have_empty_summary() {
        let desc = "## Options\n\n### Option 1 — t\nbody\n";
        let parse = parse_options(desc);
        assert_eq!(parse.summary, "");
        assert_eq!(parse.options.len(), 1);
    }

    #[test]
    fn option_without_title_has_empty_title() {
        let desc = "## Options\n\n### Option 1\nbody only\n";
        let parse = parse_options(desc);
        assert_eq!(parse.options.len(), 1);
        assert_eq!(parse.options[0].title, "");
        assert_eq!(parse.options[0].body, "body only");
    }

    #[test]
    fn parse_options_in_prefers_notes_when_notes_carry_options() {
        let notes = "## Options — promoted summary\n\n### Option 1 — promoted\nbody\n";
        let description = "## Options — original summary\n\n### Option 1 — original\nbody\n";
        let parse = parse_options_in(Some(notes), description);
        assert_eq!(parse.summary, "promoted summary");
        assert_eq!(parse.options.len(), 1);
        assert_eq!(parse.options[0].title, "promoted");
    }

    #[test]
    fn parse_options_in_falls_back_to_description_when_notes_empty() {
        let parse = parse_options_in(
            Some("agent-blocked: no options here"),
            "## Options — from desc\n\n### Option 1 — d\nbody\n",
        );
        assert_eq!(parse.summary, "from desc");
        assert_eq!(parse.options.len(), 1);
        assert_eq!(parse.options[0].title, "d");
    }

    #[test]
    fn parse_options_in_falls_back_when_notes_absent() {
        let parse = parse_options_in(None, "## Options — only desc\n\n### Option 1 — t\nb\n");
        assert_eq!(parse.summary, "only desc");
        assert_eq!(parse.options.len(), 1);
    }

    #[test]
    fn parse_options_in_returns_default_when_neither_source_has_options() {
        let parse = parse_options_in(Some("just notes"), "just a description");
        assert!(parse.summary.is_empty());
        assert!(parse.options.is_empty());
    }

    #[test]
    fn strip_options_block_returns_unchanged_when_no_options_present() {
        let text = "just notes\nwith no options block\n";
        assert_eq!(strip_options_block(text), text);
    }

    #[test]
    fn strip_options_block_removes_block_at_end_of_input() {
        let text = "intro paragraph\n\n## Options — pick\n\n### Option 1 — t\nbody\n";
        let stripped = strip_options_block(text);
        assert!(!stripped.contains("## Options"));
        assert!(!stripped.contains("### Option 1"));
        assert!(stripped.contains("intro paragraph"));
    }

    #[test]
    fn strip_options_block_preserves_content_after_terminating_h2() {
        let text = "\
## Options — pick

### Option 1 — t
body

## Other section

kept content
";
        let stripped = strip_options_block(text);
        assert!(!stripped.contains("## Options"));
        assert!(!stripped.contains("### Option 1"));
        assert!(stripped.contains("## Other section"));
        assert!(stripped.contains("kept content"));
    }

    #[test]
    fn strip_options_block_preserves_prior_notes() {
        let text = "\
prior resolution note A

## Options — current
### Option 1 — t
body

## After block

after-note
";
        let stripped = strip_options_block(text);
        assert!(stripped.contains("prior resolution note A"));
        assert!(stripped.contains("after-note"));
        assert!(!stripped.contains("## Options — current"));
    }

    #[test]
    fn fenced_options_example_inside_description_is_ignored() {
        let desc = "\
Setup paragraph.

```markdown
## Options — fake summary

### Option 1 — fake title
fake body
```

## Options — real

### Option 1 — real title
real body
";
        let parse = parse_options(desc);
        assert_eq!(parse.summary, "real");
        assert_eq!(parse.options.len(), 1);
        assert_eq!(parse.options[0].title, "real title");
        assert_eq!(parse.options[0].body, "real body");
    }
}
