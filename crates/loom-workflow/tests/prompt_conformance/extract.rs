use anyhow::{Result, bail, ensure};
use loom_protocol::gate::DispatchScope;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

#[derive(Debug)]
pub struct Example {
    pub scopes: Vec<DispatchScope>,
    pub body: String,
}

pub fn examples(markdown: &str) -> Result<Vec<Example>> {
    let mut examples = Vec::new();
    let mut current = None;
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                let words: Vec<_> = info.split_whitespace().collect();
                if words.get(1) == Some(&"loom-example") {
                    ensure!(words[0] == "text", "example must be a text fence");
                    let scopes = match words.get(2).copied() {
                        None | Some("per-bead") => vec![DispatchScope::PerBead],
                        Some("tree") => vec![DispatchScope::Tree],
                        Some("any-scope") => vec![
                            DispatchScope::PerBead,
                            DispatchScope::Tree,
                            DispatchScope::PushGate,
                        ],
                        Some(other) => bail!("unknown example scope: {other}"),
                    };
                    ensure!(words.len() <= 3, "unexpected example fence metadata");
                    current = Some(Example {
                        scopes,
                        body: String::new(),
                    });
                }
            }
            Event::Text(text) => {
                if let Some(example) = &mut current {
                    example.body.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(example) = current.take() {
                    ensure!(!example.body.trim().is_empty(), "empty executable example");
                    examples.push(example);
                }
            }
            _ => {}
        }
    }
    ensure!(!examples.is_empty(), "no executable examples in prompt");
    Ok(examples)
}

pub fn table(markdown: &str, header: &[&str]) -> Result<Vec<Vec<String>>> {
    let mut tables = Vec::new();
    let mut rows = Vec::new();
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut in_cell = false;
    for event in Parser::new_ext(markdown, Options::ENABLE_TABLES) {
        match event {
            Event::Start(Tag::TableCell) => {
                cell.clear();
                in_cell = true;
            }
            Event::Text(text) | Event::Code(text) if in_cell => cell.push_str(&text),
            Event::End(TagEnd::TableCell) => {
                cells.push(cell.clone());
                in_cell = false;
            }
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                rows.push(std::mem::take(&mut cells));
            }
            Event::End(TagEnd::Table) => tables.push(std::mem::take(&mut rows)),
            _ => {}
        }
    }
    let mut matching = tables.into_iter().filter(|rows| {
        rows.first()
            .is_some_and(|row| row.iter().map(String::as_str).eq(header.iter().copied()))
    });
    let rows = matching
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing table {header:?}"))?;
    ensure!(matching.next().is_none(), "duplicate contract table");
    Ok(rows.into_iter().skip(1).collect())
}
