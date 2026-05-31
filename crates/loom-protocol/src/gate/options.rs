//! Shared validator for the [Options Format
//! Contract](https://specs/gate.md#options-format-contract): clarify-bound
//! findings and direct-emit `LOOM_CLARIFY` self-reports must persist a
//! `## Options — <summary>` heading with at least one `### Option <N> —
//! <title>` subsection to the bead under dispatch (or the molecule epic
//! for `todo_*` phases).
//!
//! The contract is enforced symmetrically by the verdict-gate direct-emit
//! path and the mint-side per-finding processing step in `loom-workflow`;
//! a single validator here gives both sites a structurally identical
//! check so a malformed block surfaces the same `clarify-without-options`
//! downgrade on either path.

/// Returns `true` iff `text` contains a well-formed `## Options —
/// <non-empty summary>` heading followed by at least one
/// `### Option <N> — <non-empty title>` subsection per `specs/gate.md` §
/// *Options Format Contract*.
///
/// The validator scans line-by-line — the contract names the heading
/// shapes, not nesting rules, so a line scan is sufficient and keeps
/// `loom-protocol`'s dependency surface a leaf. Whitespace around the
/// heading separator (`—` / `–` / `-` / `--`) is tolerated.
#[must_use]
pub fn has_well_formed_block(text: &str) -> bool {
    let mut summary_seen = false;
    let mut option_seen = false;
    for raw in text.lines() {
        let line = raw.trim_start();
        if !summary_seen {
            if let Some(rest) = strip_heading_prefix(line, "## ", "Options")
                && !strip_separator(rest).trim().is_empty()
            {
                summary_seen = true;
            }
            continue;
        }
        if let Some(rest) = strip_heading_prefix(line, "### ", "Option")
            && let Some((n, title)) = parse_option_id(rest)
            && n >= 1
            && !title.trim().is_empty()
        {
            option_seen = true;
            break;
        }
    }
    summary_seen && option_seen
}

fn strip_heading_prefix<'a>(line: &'a str, hashes: &str, keyword: &str) -> Option<&'a str> {
    let after_hashes = line.strip_prefix(hashes)?;
    let after_keyword = after_hashes.strip_prefix(keyword)?;
    if after_keyword.is_empty() || after_keyword.starts_with(char::is_whitespace) {
        Some(after_keyword)
    } else {
        None
    }
}

fn strip_separator(rest: &str) -> &str {
    let trimmed = rest.trim_start();
    if let Some(s) = trimmed.strip_prefix('\u{2014}') {
        s
    } else if let Some(s) = trimmed.strip_prefix('\u{2013}') {
        s
    } else if let Some(s) = trimmed.strip_prefix("--") {
        s
    } else if let Some(s) = trimmed.strip_prefix('-') {
        s
    } else {
        trimmed
    }
}

fn parse_option_id(rest: &str) -> Option<(u32, &str)> {
    let trimmed = rest.trim_start();
    let mut end = 0;
    for (i, c) in trimmed.char_indices() {
        if c.is_ascii_digit() {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    let (digits, after) = trimmed.split_at(end);
    let n: u32 = digits.parse().ok()?;
    if !after.is_empty() && !after.starts_with(char::is_whitespace) {
        return None;
    }
    Some((n, strip_separator(after)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_block() {
        let text = "\
## Options — pick a path

### Option 1 — Preserve the invariant
body paragraph.
";
        assert!(has_well_formed_block(text));
    }

    #[test]
    fn accepts_block_with_leading_prose_and_trailing_h2() {
        let text = "\
Intro paragraph.

## Options — summary text

### Option 1 — title
body.

## Unrelated section
ignored.
";
        assert!(has_well_formed_block(text));
    }

    #[test]
    fn accepts_each_supported_separator_variant() {
        for sep in ["—", "–", "-", "--"] {
            let text = format!("## Options {sep} summary\n\n### Option 1 {sep} title\nbody\n");
            assert!(has_well_formed_block(&text), "sep={sep}");
        }
    }

    #[test]
    fn accepts_multidigit_option_ids() {
        let text = "## Options — summary\n\n### Option 12 — twelfth\nbody\n";
        assert!(has_well_formed_block(text));
    }

    #[test]
    fn rejects_text_with_no_options_heading() {
        assert!(!has_well_formed_block("Just prose with no headings."));
    }

    #[test]
    fn rejects_options_heading_with_empty_summary() {
        let text = "## Options —\n\n### Option 1 — title\nbody\n";
        assert!(!has_well_formed_block(text));
    }

    #[test]
    fn rejects_options_heading_without_separator_or_summary() {
        let text = "## Options\n\n### Option 1 — title\nbody\n";
        assert!(!has_well_formed_block(text));
    }

    #[test]
    fn rejects_summary_only_with_no_subsections() {
        let text = "## Options — summary text\n\nbody without a subsection.\n";
        assert!(!has_well_formed_block(text));
    }

    #[test]
    fn rejects_subsection_with_no_numeric_id() {
        let text = "## Options — summary\n\n### Option N — title\nbody\n";
        assert!(!has_well_formed_block(text));
    }

    #[test]
    fn rejects_subsection_with_empty_title() {
        let text = "## Options — summary\n\n### Option 1 —\nbody\n";
        assert!(!has_well_formed_block(text));
    }

    #[test]
    fn rejects_subsection_with_zero_id() {
        let text = "## Options — summary\n\n### Option 0 — title\nbody\n";
        assert!(!has_well_formed_block(text));
    }

    #[test]
    fn rejects_when_subsection_precedes_heading() {
        let text = "### Option 1 — title\nbody\n\n## Options — summary\n";
        assert!(!has_well_formed_block(text));
    }

    #[test]
    fn tolerates_whitespace_around_separator() {
        let text = "## Options   —   summary text\n\n### Option 1   —   title\nbody\n";
        assert!(has_well_formed_block(text));
    }
}
