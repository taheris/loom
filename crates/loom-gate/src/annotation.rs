//! `[tier](target)` annotation parser.
//!
//! Walks the consumer's `specs/*.md` tree and extracts every
//! `[check](...)` / `[test](...)` / `[system](...)` / `[judge](...)` token
//! into a typed [`Annotation`] record. Tier vocabulary is owned by
//! `docs/spec-conventions.md`; dispatch is owned by `specs/gate.md`.
//!
//! The parser is purely extractive — it does not resolve commands on
//! PATH, look up test functions in cargo metadata, or check that judge
//! files exist. Those checks belong to the integrity gate. The parser's
//! output is shaped so the integrity gate can group annotations by
//! criterion (atomic-acceptance violations) and intersect the criterion
//! set against the annotation set (criteria missing an annotation).
//!
//! Code-fence isolation is delegated to `pulldown-cmark`: it identifies
//! the byte ranges of fenced code blocks, indented code blocks, and
//! inline code spans, and any annotation token whose `[` falls inside
//! one of those ranges is dropped before construction. The structural
//! pass also records which bullet items belong to a `## Success
//! Criteria` section so the integrity gate can detect bullets that
//! carry zero annotations.

use std::collections::BTreeSet;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use displaydoc::Display;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use thiserror::Error;

/// Verifier tier for one annotation. Closed set per RS-17; the wire
/// strings line up with the `[tier]` text in spec files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Static analysis — `[check](command)` invokes a verifier subprocess.
    Check,
    /// Language-native test — `[test](path)` is batched into one runner.
    Test,
    /// Container / packaging / end-to-end — `[system](command)` is its own
    /// subprocess.
    System,
    /// LLM judgement — `[judge](path)` reads a rubric file.
    Judge,
}

impl Tier {
    /// Lowercase wire string. Matches the `[tier]` text in spec files.
    pub fn as_wire(&self) -> &'static str {
        match self {
            Tier::Check => "check",
            Tier::Test => "test",
            Tier::System => "system",
            Tier::Judge => "judge",
        }
    }

    /// Inverse of [`Tier::as_wire`]; returns `None` for any token that is
    /// not one of the four tier names. Match is case-sensitive — the
    /// convention reserves the lowercase forms for annotations and the
    /// gate keeps the boundary tight so a `[Check]` typo surfaces as
    /// "missing annotation" rather than silently dispatching.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "check" => Some(Tier::Check),
            "test" => Some(Tier::Test),
            "system" => Some(Tier::System),
            "judge" => Some(Tier::Judge),
            _ => None,
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

/// One parsed `[tier](target)` annotation extracted from a spec file.
///
/// `target` is the raw string between the parentheses; resolution
/// (whether the command exists on PATH, whether the test path matches a
/// function, whether the file exists on disk) is the integrity gate's
/// job. `criterion_line` is the 1-indexed line of the enclosing bullet
/// item, or the annotation's own line when the annotation lives in
/// prose with no enclosing list item; the integrity gate groups by
/// `(source_spec, criterion_line)` to enforce atomic acceptance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub tier: Tier,
    pub target: String,
    pub source_spec: PathBuf,
    pub line: u32,
    pub criterion_line: u32,
}

/// One acceptance criterion located in a spec's Success-Criteria region.
///
/// Used by the integrity gate to flag bullets that carry zero
/// annotations. A criterion's `line` matches the `criterion_line` of any
/// annotations attached to the same bullet, so set-difference against
/// the annotation list yields the un-annotated criteria directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Criterion {
    pub source_spec: PathBuf,
    pub line: u32,
}

/// Aggregated parser output across one or more spec files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedSpecs {
    pub annotations: Vec<Annotation>,
    pub criteria: Vec<Criterion>,
}

/// Failures the parser surfaces to its caller. Filesystem errors are
/// the only failure mode — markdown parsing itself is infallible and
/// the parser intentionally does not validate annotation targets.
#[derive(Debug, Display, Error)]
pub enum ParseError {
    /// failed to read specs directory `{path}`: {source}
    ReadDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// failed to read spec file `{path}`: {source}
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Walk `specs_dir`, parse every top-level `*.md` file in lexicographic
/// order, and return the union of their annotation / criterion records.
///
/// Lex order keeps the output deterministic across hosts so downstream
/// consumers (cache writes, integrity findings) don't shuffle on each
/// run.
pub fn parse(specs_dir: &Path) -> Result<ParsedSpecs, ParseError> {
    let entries = fs::read_dir(specs_dir).map_err(|e| ParseError::ReadDir {
        path: specs_dir.to_path_buf(),
        source: e,
    })?;

    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| ParseError::ReadDir {
            path: specs_dir.to_path_buf(),
            source: e,
        })?;
        let p = entry.path();
        if p.is_file() && p.extension().is_some_and(|e| e == "md") {
            paths.push(p);
        }
    }
    paths.sort();

    let mut out = ParsedSpecs::default();
    for path in paths {
        let content = fs::read_to_string(&path).map_err(|e| ParseError::ReadFile {
            path: path.clone(),
            source: e,
        })?;
        let parsed = parse_content(&path, &content);
        out.annotations.extend(parsed.annotations);
        out.criteria.extend(parsed.criteria);
    }
    Ok(out)
}

/// Parse a single in-memory spec body. `source_spec` is recorded as-is
/// on every emitted record so callers can produce filename-tagged
/// errors without re-resolving the path. Infallible: markdown parsing
/// never errors and the parser does not validate annotation targets.
pub fn parse_content(source_spec: &Path, content: &str) -> ParsedSpecs {
    let line_index = LineIndex::from(content);
    let structure = StructuralPass::run(content);

    let mut annotations: Vec<Annotation> = Vec::new();
    for hit in scan_tokens(content) {
        if structure.is_inside_code(hit.start) {
            continue;
        }
        let line = line_index.line_of(hit.start);
        let criterion_line = structure
            .innermost_item_containing(hit.start)
            .unwrap_or(line);
        annotations.push(Annotation {
            tier: hit.tier,
            target: hit.target,
            source_spec: source_spec.to_path_buf(),
            line,
            criterion_line,
        });
    }

    let criteria = structure
        .criterion_bullet_lines
        .iter()
        .map(|&line| Criterion {
            source_spec: source_spec.to_path_buf(),
            line,
        })
        .collect();

    ParsedSpecs {
        annotations,
        criteria,
    }
}

/// One annotation token recovered by the byte-level scanner.
struct TokenHit {
    start: usize,
    tier: Tier,
    target: String,
}

/// Scan `content` for `[tier](target)` tokens by direct byte inspection.
///
/// `pulldown-cmark` only emits `Tag::Link` events for URLs that conform
/// to CommonMark's destination grammar — destinations with spaces (the
/// common shape for `[check]` / `[system]` commands) round-trip through
/// `Text` events instead. Rather than coalescing those text fragments,
/// the parser scans the raw source and uses the structural pass to mask
/// out code-block regions. Parens inside the target are accepted as
/// long as they balance.
fn scan_tokens(content: &str) -> Vec<TokenHit> {
    let bytes = content.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'[' {
            i += 1;
            continue;
        }
        let after_lbracket = i + 1;
        let Some((tier, tier_len)) = match_tier(&bytes[after_lbracket..]) else {
            i += 1;
            continue;
        };
        let close_bracket = after_lbracket + tier_len;
        if close_bracket >= bytes.len() || bytes[close_bracket] != b']' {
            i += 1;
            continue;
        }
        let lparen = close_bracket + 1;
        if lparen >= bytes.len() || bytes[lparen] != b'(' {
            i += 1;
            continue;
        }
        let Some(rparen) = find_balanced_close(bytes, lparen) else {
            i += 1;
            continue;
        };
        let target_bytes = &bytes[lparen + 1..rparen];
        let target = String::from_utf8_lossy(target_bytes).into_owned();
        out.push(TokenHit {
            start: i,
            tier,
            target,
        });
        i = rparen + 1;
    }
    out
}

fn match_tier(s: &[u8]) -> Option<(Tier, usize)> {
    if s.starts_with(b"check") {
        Some((Tier::Check, 5))
    } else if s.starts_with(b"test") {
        Some((Tier::Test, 4))
    } else if s.starts_with(b"system") {
        Some((Tier::System, 6))
    } else if s.starts_with(b"judge") {
        Some((Tier::Judge, 5))
    } else {
        None
    }
}

/// Walk forward from an opening `(` and return the index of the
/// matching `)`. Tracks paren depth so balanced parens inside the
/// target — e.g. `cargo run --foo "$(date)"` — round-trip intact.
/// Returns `None` when the parens never balance before a hard break
/// (newline-newline) or end-of-input; this matches the markdown
/// convention that link destinations don't span paragraphs.
fn find_balanced_close(bytes: &[u8], lparen: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut j = lparen + 1;
    let mut blank_line_count = 0usize;
    while j < bytes.len() {
        match bytes[j] {
            b'(' => {
                depth += 1;
                blank_line_count = 0;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
                blank_line_count = 0;
            }
            b'\n' => {
                blank_line_count += 1;
                if blank_line_count >= 2 {
                    return None;
                }
            }
            b' ' | b'\t' | b'\r' => {}
            _ => {
                blank_line_count = 0;
            }
        }
        j += 1;
    }
    None
}

/// Structural metadata derived from one pulldown-cmark pass.
struct StructuralPass {
    code_ranges: Vec<Range<usize>>,
    item_ranges: Vec<(Range<usize>, u32)>,
    criterion_bullet_lines: BTreeSet<u32>,
}

impl StructuralPass {
    fn run(content: &str) -> Self {
        let line_index = LineIndex::from(content);
        let mut code_ranges: Vec<Range<usize>> = Vec::new();
        let mut item_ranges: Vec<(Range<usize>, u32)> = Vec::new();
        let mut criterion_bullet_lines: BTreeSet<u32> = BTreeSet::new();

        let mut sc_level: Option<HeadingLevel> = None;
        let mut in_heading: Option<HeadingLevel> = None;
        let mut heading_text = String::new();

        for (event, range) in Parser::new_ext(content, Options::ENABLE_TASKLISTS).into_offset_iter()
        {
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    if let Some(active) = sc_level
                        && (level as usize) <= (active as usize)
                    {
                        sc_level = None;
                    }
                    in_heading = Some(level);
                    heading_text.clear();
                }
                Event::End(TagEnd::Heading(_)) => {
                    if let Some(level) = in_heading.take()
                        && heading_text.trim().eq_ignore_ascii_case("success criteria")
                    {
                        sc_level = Some(level);
                    }
                }
                Event::Text(ref t) | Event::Code(ref t) if in_heading.is_some() => {
                    heading_text.push_str(t);
                    if matches!(event, Event::Code(_)) {
                        code_ranges.push(range);
                    }
                }
                Event::Start(Tag::CodeBlock(_)) => {
                    code_ranges.push(range);
                }
                Event::Code(_) => {
                    code_ranges.push(range);
                }
                Event::Start(Tag::Item) => {
                    let bullet_line = line_index.line_of(range.start);
                    item_ranges.push((range, bullet_line));
                    if sc_level.is_some() {
                        criterion_bullet_lines.insert(bullet_line);
                    }
                }
                _ => {}
            }
        }

        Self {
            code_ranges,
            item_ranges,
            criterion_bullet_lines,
        }
    }

    fn is_inside_code(&self, offset: usize) -> bool {
        self.code_ranges.iter().any(|r| r.contains(&offset))
    }

    /// Returns the line of the innermost (smallest, most recently
    /// started) bullet whose range contains `offset`. Items in the
    /// vec are appended in document order, so among containing items
    /// the deepest is the one with the latest `range.start` — exactly
    /// the iterator's last hit.
    fn innermost_item_containing(&self, offset: usize) -> Option<u32> {
        self.item_ranges
            .iter()
            .rfind(|(r, _)| r.contains(&offset))
            .map(|(_, line)| *line)
    }
}

/// Map byte offsets into 1-indexed line numbers. Built once per file so
/// each link / item lookup is a `O(log n)` binary search.
struct LineIndex {
    line_starts: Vec<usize>,
}

impl LineIndex {
    fn from(content: &str) -> Self {
        let mut line_starts = vec![0usize];
        for (i, b) in content.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self { line_starts }
    }

    fn line_of(&self, offset: usize) -> u32 {
        let idx = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        (idx + 1) as u32
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn spec_path() -> PathBuf {
        PathBuf::from("specs/example.md")
    }

    #[test]
    fn tier_round_trips_through_wire() {
        for tier in [Tier::Check, Tier::Test, Tier::System, Tier::Judge] {
            assert_eq!(Tier::from_wire(tier.as_wire()), Some(tier));
        }
        assert_eq!(Tier::from_wire("verify"), None);
        assert_eq!(Tier::from_wire("Check"), None);
    }

    #[test]
    fn standard_annotation_in_bullet_records_tier_target_and_lines() {
        let md = "\
# Spec

## Success Criteria

- A thing must hold
  [test](crate::module::test_name)
";
        let parsed = parse_content(&spec_path(), md);
        assert_eq!(parsed.annotations.len(), 1);
        let a = &parsed.annotations[0];
        assert_eq!(a.tier, Tier::Test);
        assert_eq!(a.target, "crate::module::test_name");
        assert_eq!(a.source_spec, spec_path());
        assert_eq!(a.line, 6);
        assert_eq!(a.criterion_line, 5);
    }

    #[test]
    fn all_four_tiers_parse_with_their_targets_including_spaces() {
        let md = "\
## Success Criteria

- a [check](cargo run -p loom-walk -- foo)
- b [test](crate::t::it)
- c [system](nix run .#test-loom)
- d [judge](rubrics/api.md)
";
        let parsed = parse_content(&spec_path(), md);
        let tiers: Vec<Tier> = parsed.annotations.iter().map(|a| a.tier).collect();
        assert_eq!(
            tiers,
            vec![Tier::Check, Tier::Test, Tier::System, Tier::Judge]
        );
        let targets: Vec<&str> = parsed
            .annotations
            .iter()
            .map(|a| a.target.as_str())
            .collect();
        assert_eq!(
            targets,
            vec![
                "cargo run -p loom-walk -- foo",
                "crate::t::it",
                "nix run .#test-loom",
                "rubrics/api.md",
            ]
        );
    }

    #[test]
    fn multiple_annotations_on_one_criterion_share_a_criterion_line() {
        let md = "\
## Success Criteria

- Thing claim
  [test](crate::a::t)
  [check](cargo run -p w -- a)
";
        let parsed = parse_content(&spec_path(), md);
        assert_eq!(parsed.annotations.len(), 2);
        let lines: Vec<u32> = parsed
            .annotations
            .iter()
            .map(|a| a.criterion_line)
            .collect();
        assert_eq!(lines[0], lines[1], "shared criterion line");
        assert_eq!(parsed.criteria.len(), 1);
        assert_eq!(parsed.criteria[0].line, lines[0]);
    }

    #[test]
    fn criterion_without_annotation_appears_in_criteria_list() {
        let md = "\
## Success Criteria

- Annotated [test](crate::a::ok)
- Un-annotated bullet
- Another [check](cargo run -p w -- b)
";
        let parsed = parse_content(&spec_path(), md);
        assert_eq!(parsed.annotations.len(), 2);
        assert_eq!(parsed.criteria.len(), 3);
        let annotated: std::collections::HashSet<u32> = parsed
            .annotations
            .iter()
            .map(|a| a.criterion_line)
            .collect();
        let unannotated: Vec<u32> = parsed
            .criteria
            .iter()
            .map(|c| c.line)
            .filter(|l| !annotated.contains(l))
            .collect();
        assert_eq!(unannotated.len(), 1, "exactly one un-annotated bullet");
    }

    #[test]
    fn annotation_in_fenced_code_block_is_ignored() {
        let md = "\
## Success Criteria

- Real one [test](crate::t::real)

```markdown
- Example shown to authors:
  [test](crate::not::real)
```
";
        let parsed = parse_content(&spec_path(), md);
        assert_eq!(parsed.annotations.len(), 1);
        assert_eq!(parsed.annotations[0].target, "crate::t::real");
    }

    #[test]
    fn annotation_inside_inline_code_is_ignored() {
        let md = "\
## Success Criteria

- Example syntax: `[test](crate::not::real)` lives inline
";
        let parsed = parse_content(&spec_path(), md);
        assert!(parsed.annotations.is_empty());
    }

    #[test]
    fn annotation_in_prose_paragraph_outside_bullets_is_extracted() {
        let md = "\
## Success Criteria

This claim is enforced by [test](crate::prose::p) inline in prose,
which spans
multiple lines but still parses as one paragraph.
";
        let parsed = parse_content(&spec_path(), md);
        assert_eq!(parsed.annotations.len(), 1);
        let a = &parsed.annotations[0];
        assert_eq!(a.target, "crate::prose::p");
        assert_eq!(
            a.criterion_line, a.line,
            "prose annotation anchors on itself"
        );
    }

    #[test]
    fn target_with_balanced_inner_parens_round_trips() {
        let md = "\
## Success Criteria

- one [check](cargo run --foo \"$(date)\" -- x)
";
        let parsed = parse_content(&spec_path(), md);
        assert_eq!(parsed.annotations.len(), 1);
        assert_eq!(
            parsed.annotations[0].target,
            "cargo run --foo \"$(date)\" -- x"
        );
    }

    #[test]
    fn malformed_tokens_are_dropped_silently() {
        let md = "\
## Success Criteria

- not annotation: [test] without parens
- broken: [test](unbalanced
  paragraph keeps going
- ok: [test](crate::t::ok)
";
        let parsed = parse_content(&spec_path(), md);
        assert_eq!(parsed.annotations.len(), 1);
        assert_eq!(parsed.annotations[0].target, "crate::t::ok");
    }

    #[test]
    fn indented_code_block_masks_annotations() {
        let md = "\
## Success Criteria

- one [test](crate::t::real)

paragraph then indented block follows:

    [test](crate::not::real)
    [check](cargo run)
";
        let parsed = parse_content(&spec_path(), md);
        assert_eq!(parsed.annotations.len(), 1);
        assert_eq!(parsed.annotations[0].target, "crate::t::real");
    }

    #[test]
    fn links_outside_tier_vocabulary_are_skipped() {
        let md = "\
## Success Criteria

- See [docs](https://example.com/) and [more](other-page.md)
- Real [test](crate::t::ok)
";
        let parsed = parse_content(&spec_path(), md);
        assert_eq!(parsed.annotations.len(), 1);
        assert_eq!(parsed.annotations[0].target, "crate::t::ok");
    }

    #[test]
    fn criteria_only_collected_under_success_criteria_heading() {
        let md = "\
## Architecture

- This bullet is not a criterion

## Success Criteria

- This one is [test](crate::sc::ok)

## Out of Scope

- Not a criterion either
";
        let parsed = parse_content(&spec_path(), md);
        assert_eq!(parsed.criteria.len(), 1);
        assert_eq!(parsed.annotations.len(), 1);
    }

    #[test]
    fn nested_subsections_under_success_criteria_still_collect_criteria() {
        let md = "\
## Success Criteria

### Unit tests

- Bullet A [test](crate::a::ok)

### Integration tests

- Bullet B [test](crate::b::ok)

## Requirements

- Should not count
";
        let parsed = parse_content(&spec_path(), md);
        assert_eq!(parsed.criteria.len(), 2);
        assert_eq!(parsed.annotations.len(), 2);
    }
}
