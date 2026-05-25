//! Structural markdown helpers backed by `pulldown-cmark`.
//!
//! Wraps `pulldown_cmark::Parser` so spec extractors share one fence-aware,
//! AST-true view of the document instead of walking `content.lines()`. The
//! helper exists because line-based heading scans were prone to locking onto
//! markdown examples *inside* fenced code blocks (see issue wx-n9jsn).

use std::ops::Range;

pub use pulldown_cmark::{Event, HeadingLevel, Tag, TagEnd};
use pulldown_cmark::{Options, Parser};

/// Construct a `pulldown_cmark::Parser` configured with the options Loom
/// extractors expect (currently: GitHub-flavored task lists).
pub fn parser(content: &str) -> Parser<'_> {
    Parser::new_ext(content, Options::ENABLE_TASKLISTS)
}

/// Locate the first heading at `level` whose plain-text content matches
/// `predicate`, then collect the events of its section. The section starts
/// after the heading's `End` event and ends at the first subsequent heading
/// whose level is ≤ `level` (or at end of input).
///
/// Heading text is the concatenated text of `Event::Text`/`Event::Code`
/// children, trimmed of surrounding whitespace.
///
/// Returns `None` when no matching heading exists. The heading's own events
/// are not included in the returned slice.
pub fn section_events<'a, F>(
    content: &'a str,
    level: HeadingLevel,
    mut predicate: F,
) -> Option<Vec<(Event<'a>, Range<usize>)>>
where
    F: FnMut(&str) -> bool,
{
    let mut iter = parser(content).into_offset_iter();

    loop {
        let (event, _) = iter.next()?;
        let Event::Start(Tag::Heading {
            level: heading_level,
            ..
        }) = event
        else {
            continue;
        };
        if heading_level != level {
            continue;
        }
        let mut heading_text = String::new();
        for (e, _) in iter.by_ref() {
            match e {
                Event::End(TagEnd::Heading(_)) => break,
                Event::Text(t) | Event::Code(t) => heading_text.push_str(&t),
                _ => {}
            }
        }
        if predicate(heading_text.trim()) {
            break;
        }
    }

    let mut events = Vec::new();
    for (event, range) in iter {
        if let Event::Start(Tag::Heading {
            level: heading_level,
            ..
        }) = &event
            && (*heading_level as usize) <= (level as usize)
        {
            break;
        }
        events.push((event, range));
    }
    Some(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_heading_returns_none() {
        let md = "# Title\n\nbody\n";
        assert!(section_events(md, HeadingLevel::H2, |t| t == "Companions").is_none());
    }

    #[test]
    fn section_excludes_heading_event_and_stops_at_same_level() {
        let md = "## Companions\n\n- a\n\n## Other\n\n- b\n";
        let events =
            section_events(md, HeadingLevel::H2, |t| t == "Companions").unwrap_or_default();
        assert!(!events.is_empty(), "section must be located");
        let starts_a_list = events
            .iter()
            .any(|(e, _)| matches!(e, Event::Start(Tag::List(_))));
        assert!(starts_a_list);
        let mentions_b = events
            .iter()
            .any(|(e, _)| matches!(e, Event::Text(t) if t.as_ref() == "b"));
        assert!(!mentions_b);
    }

    #[test]
    fn fenced_code_block_does_not_anchor_heading_match() {
        let md = "# Spec\n\n```markdown\n## Companions\n\n- `fake/path/`\n```\n";
        assert!(section_events(md, HeadingLevel::H2, |t| t == "Companions").is_none());
    }

    #[test]
    fn higher_level_subheading_stays_inside_section() {
        let md = "## Companions\n\n- a\n\n### sub\n\n- nested\n\n## Other\n\nx\n";
        let events =
            section_events(md, HeadingLevel::H2, |t| t == "Companions").unwrap_or_default();
        let saw_subheading = events.iter().any(|(e, _)| {
            matches!(
                e,
                Event::Start(Tag::Heading {
                    level: HeadingLevel::H3,
                    ..
                })
            )
        });
        assert!(saw_subheading);
    }
}
