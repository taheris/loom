//! Strict, fence-aware parsing of the Options Format Contract.

use std::ops::Range;

use loom_driver::markdown::{Event, HeadingLevel, Tag, TagEnd, parser};
use pulldown_cmark::OffsetIter;

/// One unique decision brief with a summary and sequential, contextual options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brief {
    summary: String,
    options: Vec<OptionEntry>,
}

impl Brief {
    pub fn summary(&self) -> &str {
        &self.summary
    }

    pub fn options(&self) -> &[OptionEntry] {
        &self.options
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionEntry {
    pub n: u32,
    pub title: String,
    pub body: String,
}

/// A bounded diagnostic for an absent, ambiguous, or malformed decision brief.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// missing active Options brief
    Missing,
    /// multiple active Options briefs across notes and description
    Ambiguous,
    /// Options summary must have a separator and 1–50 characters on one line
    Summary,
    /// Options brief requires at least one numbered option
    NoOptions,
    /// option heading must be Option N followed by a separator and title
    Heading,
    /// option numbering must be sequential starting at 1
    Sequence,
    /// option title must be nonblank
    Title,
    /// each option requires nonblank body context
    Context,
}

impl Error {
    pub fn repair_message(self) -> String {
        format!(
            "clarify-without-options: {self}. Repair this decision's notes/description to contain exactly one Options brief with a nonblank summary (at most 50 characters) and sequential numbered, titled options with body context."
        )
    }
}

struct Heading {
    level: HeadingLevel,
    text: String,
    range: Range<usize>,
}

/// Parse exactly one active Options brief. Fenced examples are inert.
///
/// # Errors
/// Returns a bounded diagnostic when the brief is missing or defective.
pub fn parse_options(description: &str) -> Result<Brief, Error> {
    parse_options_in(None, description)
}

/// Resolve one active brief across notes and description, without precedence.
///
/// # Errors
/// Returns a bounded diagnostic when either source makes the brief ambiguous
/// or the sole active brief is malformed.
pub fn parse_options_in(notes: Option<&str>, description: &str) -> Result<Brief, Error> {
    let sources = [notes.unwrap_or(""), description];
    let mut candidate = None;
    for source in sources {
        for range in options_block_ranges(source) {
            if candidate.is_some() {
                return Err(Error::Ambiguous);
            }
            candidate = Some(&source[range]);
        }
    }
    resolve(candidate.ok_or(Error::Missing)?)
}

fn resolve(block: &str) -> Result<Brief, Error> {
    let headings = headings(block);
    let first = headings.first().ok_or(Error::Missing)?;
    let summary = keyword_rest(&first.text, "Options")
        .and_then(separated_text)
        .filter(|summary| {
            !summary.is_empty() && summary.chars().count() <= 50 && !summary.contains(['\n', '\r'])
        })
        .ok_or(Error::Summary)?;
    let mut options = Vec::new();
    let mut current: Option<&Heading> = None;
    for heading in headings.iter().skip(1) {
        if heading.level != HeadingLevel::H3 || keyword_rest(&heading.text, "Option").is_none() {
            continue;
        }
        if let Some(previous) = current {
            options.push(resolve_option(
                block,
                previous,
                heading.range.start,
                options.len(),
            )?);
        }
        current = Some(heading);
    }
    if let Some(previous) = current {
        options.push(resolve_option(block, previous, block.len(), options.len())?);
    }
    if options.is_empty() {
        return Err(Error::NoOptions);
    }
    Ok(Brief {
        summary: summary.to_owned(),
        options,
    })
}

fn resolve_option(
    block: &str,
    heading: &Heading,
    body_end: usize,
    preceding: usize,
) -> Result<OptionEntry, Error> {
    let rest = keyword_rest(&heading.text, "Option").ok_or(Error::Heading)?;
    let digit_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (digits, after) = rest.split_at(digit_end);
    let n = digits.parse::<u32>().map_err(|_| Error::Heading)?;
    let expected = u32::try_from(preceding + 1).map_err(|_| Error::Sequence)?;
    if n != expected {
        return Err(Error::Sequence);
    }
    let title = separated_text(after).ok_or(Error::Heading)?;
    if title.is_empty() {
        return Err(Error::Title);
    }
    let body = block[heading.range.end..body_end].trim();
    if body.is_empty() {
        return Err(Error::Context);
    }
    Ok(OptionEntry {
        n,
        title: title.to_owned(),
        body: body.to_owned(),
    })
}

fn headings(text: &str) -> Vec<Heading> {
    let mut iter = parser(text).into_offset_iter();
    let mut headings = Vec::new();
    while let Some((event, range)) = iter.next() {
        if let Event::Start(Tag::Heading { level, .. }) = event {
            let (text, end) = heading_text(&mut iter, range.end);
            headings.push(Heading {
                level,
                text,
                range: range.start..end,
            });
        }
    }
    headings
}

fn heading_text(iter: &mut OffsetIter<'_>, mut end: usize) -> (String, usize) {
    let mut text = String::new();
    for (event, range) in iter.by_ref() {
        match event {
            Event::End(TagEnd::Heading(_)) => {
                end = range.end;
                break;
            }
            Event::Text(value) | Event::Code(value) => text.push_str(&value),
            Event::SoftBreak | Event::HardBreak => text.push('\n'),
            _ => {}
        }
    }
    (text, end)
}

fn keyword_rest<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = text.trim_start().strip_prefix(keyword)?;
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim_start())
    } else {
        None
    }
}

fn separated_text(rest: &str) -> Option<&str> {
    let trimmed = rest.trim_start();
    ["—", "–", "--", "-"]
        .into_iter()
        .find_map(|separator| trimmed.strip_prefix(separator))
        .map(str::trim)
}

fn options_block_ranges(text: &str) -> Vec<Range<usize>> {
    let headings = headings(text);
    headings
        .iter()
        .enumerate()
        .filter(|(_, heading)| {
            heading.level == HeadingLevel::H2 && keyword_rest(&heading.text, "Options").is_some()
        })
        .map(|(index, heading)| {
            let end = headings[index + 1..]
                .iter()
                .find(|next| next.level <= HeadingLevel::H2)
                .map_or(text.len(), |next| next.range.start);
            heading.range.start..end
        })
        .collect()
}

/// Byte range of the first active Options section; fenced examples are ignored.
pub fn find_options_block_range(text: &str) -> Option<Range<usize>> {
    options_block_ranges(text).into_iter().next()
}

/// Remove active Options sections without touching fenced examples or other prose.
pub fn strip_options_block(text: &str) -> String {
    let mut out = text.to_owned();
    for range in options_block_ranges(text).into_iter().rev() {
        out.replace_range(range, "");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRIEF: &str = "## Options — pick a path\n\n### Option 1 — Preserve invariant\nRevert. Cost: churn.\n\n### Option 2 — Keep on top\nAccept. Cost: debt.\n";

    #[test]
    fn canonical_brief_supplies_summary_and_contextual_options() {
        let brief = parse_options(BRIEF).expect("brief");
        assert_eq!(brief.summary(), "pick a path");
        assert_eq!(brief.options().len(), 2);
        assert_eq!(brief.options()[0].n, 1);
        assert_eq!(brief.options()[0].title, "Preserve invariant");
        assert_eq!(brief.options()[0].body, "Revert. Cost: churn.");
        assert_eq!(brief.options()[1].n, 2);
        assert_eq!(brief.options()[1].body, "Accept. Cost: debt.");
    }

    #[test]
    fn separator_variants_all_resolve() {
        for sep in ["—", "–", "-", "--"] {
            let text = format!("## Options {sep} summary\n\n### Option 1 {sep} title\nbody\n");
            let brief = parse_options(&text).expect("brief");
            assert_eq!(brief.summary(), "summary", "sep={sep}");
            assert_eq!(brief.options()[0].title, "title", "sep={sep}");
        }
    }

    #[test]
    fn notes_and_description_are_equal_brief_sources() {
        assert_eq!(parse_options_in(Some(BRIEF), "prose"), parse_options(BRIEF));
        assert_eq!(parse_options_in(Some("notes"), BRIEF), parse_options(BRIEF));
    }

    #[test]
    fn every_active_block_counts_toward_ambiguity() {
        for (notes, description) in [
            (Some(BRIEF), BRIEF.to_owned()),
            (None, format!("{BRIEF}\n{BRIEF}")),
            (Some("## Options\n"), BRIEF.to_owned()),
            (Some(BRIEF), "## Options\n".to_owned()),
        ] {
            assert_eq!(parse_options_in(notes, &description), Err(Error::Ambiguous));
        }
        let notes = format!("{BRIEF}\n{BRIEF}");
        assert_eq!(parse_options_in(Some(&notes), ""), Err(Error::Ambiguous));
    }

    #[test]
    fn defective_briefs_return_typed_errors() {
        for (text, error) in [
            ("prose", Error::Missing),
            ("## Options\n### Option 1 — t\nb\n", Error::Summary),
            ("## Options — \n### Option 1 — t\nb\n", Error::Summary),
            ("## Options summary\n### Option 1 — t\nb\n", Error::Summary),
            ("## Options — sum\nprose\n", Error::NoOptions),
            ("## Options — sum\n### Option N — t\nb\n", Error::Heading),
            ("## Options — sum\n### Option 0 — t\nb\n", Error::Sequence),
            ("## Options — sum\n### Option 2 — t\nb\n", Error::Sequence),
            ("## Options — sum\n### Option 1 title\nb\n", Error::Heading),
            ("## Options — sum\n### Option 1 — \nb\n", Error::Title),
            ("## Options — sum\n### Option 1 — t\n\n", Error::Context),
            (
                "## Options — sum\n## Other\n### Option 1 — t\nb\n",
                Error::NoOptions,
            ),
            (
                "## Options — sum\n### Option 1 — t\nb\n### Option broken\nb\n",
                Error::Heading,
            ),
            (
                "## Options — sum\n### Option 1 — t\nb\n### Option 1 — t\nb\n",
                Error::Sequence,
            ),
            (
                "## Options — sum\n### Option 1 — t\nb\n### Option 3 — t\nb\n",
                Error::Sequence,
            ),
        ] {
            assert_eq!(parse_options(text), Err(error), "{text}");
        }
    }

    #[test]
    fn summary_limit_counts_unicode_characters() {
        for (len, valid) in [(50, true), (51, false)] {
            let text = format!(
                "## Options — {}\n### Option 1 — title\nbody",
                "é".repeat(len)
            );
            assert_eq!(parse_options(&text).is_ok(), valid);
        }
    }

    #[test]
    fn fenced_examples_are_inert_in_each_source() {
        for fence in ["```markdown", "~~~markdown"] {
            let close = &fence[..3];
            let example = format!("{fence}\n{BRIEF}\n{close}\n");
            assert_eq!(parse_options(&example), Err(Error::Missing));
            assert_eq!(
                parse_options_in(Some(&example), BRIEF),
                parse_options(BRIEF)
            );
            assert_eq!(
                parse_options_in(Some(BRIEF), &example),
                parse_options(BRIEF)
            );
            assert_eq!(
                parse_options(&format!("{example}\n{BRIEF}")),
                parse_options(BRIEF)
            );
        }
    }

    #[test]
    fn source_boundaries_do_not_complete_partial_briefs() {
        assert_eq!(
            parse_options_in(Some("## Options — summary\n"), "### Option 1 — title\nbody"),
            Err(Error::NoOptions)
        );
        assert_eq!(
            parse_options_in(Some("```markdown\n"), BRIEF),
            parse_options(BRIEF)
        );
    }

    #[test]
    fn option_context_stops_at_next_option_or_section() {
        let text = "## Options — sum\n### Option 1 — first\nline a\nline b\n\n### Option 2 — second\nline c\n\n## Other\nignored\n";
        let brief = parse_options(text).expect("brief");
        assert_eq!(brief.options()[0].body, "line a\nline b");
        assert_eq!(brief.options()[1].body, "line c");
    }

    #[test]
    fn strip_preserves_other_sections_and_fenced_examples() {
        let example = format!("```markdown\n{BRIEF}\n```\n");
        let text = format!("prior\n\n{example}\n{BRIEF}\n## After\nkept\n");
        let stripped = strip_options_block(&text);
        assert_eq!(stripped, format!("prior\n\n{example}\n## After\nkept\n"));
        assert_eq!(strip_options_block("just notes"), "just notes");
    }
}
