//! Test-only `bd` CLI shim. Drives integration tests that exercise the
//! production code's real subprocess path through `BdClient` —
//! `tokio::process::Command::new("bd").args(...).spawn()` — without
//! requiring a live beads-dolt sidecar.
//!
//! State lives under `$BD_STATE_DIR`, one directory per bead. Inside
//! each bead directory:
//!
//! - `title`        — single-line title
//! - `description`  — full markdown body
//! - `status`       — `open` / `in_progress` / `closed`
//! - `priority`     — integer 0–4
//! - `issue_type`   — `task` / `bug` / `feature` / `epic`
//! - `labels`       — one label per line (no trailing comma)
//! - `notes`        — free-form text (absent = empty)
//! - `metadata.json` — free-form JSON object (absent = empty)
//!
//! Every invocation appends a debug line to `$BD_STATE_DIR/.invocations.log`
//! so failing tests can show which calls landed where.
//!
//! Supported subcommands (the subset `loom loop` / `loom gate` /
//! `loom inbox` actually invoke):
//!
//! - `bd list --json [--label-any=<L> …] [--parent=<id>]`
//! - `bd ready --json [--limit=N] [--label=<L>] [--exclude-label=<L> …]`
//! - `bd show <id> --json`
//! - `bd create --silent --title <t> --description <t> [--type <t>] [--priority <n>] [--labels <csv>] [--metadata <json>]`
//! - `bd update <id> [--title <t>] [--description <t>] [--notes <t>] [--remove-label <l>] [--add-label <l>] [--status <s>] [--priority <n>] [--claim] [--set-metadata <k=v>]`
//! - `bd close <id>` — sets status to closed; recorded in the invocation log
//!   so the verdict-gate "no driver-side bd close" assertion can find it
//!
//! Unsupported subcommands and flags exit non-zero with a diagnostic
//! rather than silently succeeding — silent success would hide test
//! drift from production.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helper: panicking on filesystem failures is the desired loud-fail behaviour"
)]

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let Some(state_raw) = env::var_os("BD_STATE_DIR") else {
        eprintln!("bd-shim: BD_STATE_DIR must be set");
        return ExitCode::from(2);
    };
    let state_dir = PathBuf::from(state_raw);
    if !state_dir.is_dir() {
        eprintln!(
            "bd-shim: BD_STATE_DIR {} is not a directory",
            state_dir.display(),
        );
        return ExitCode::from(2);
    }

    log_invocation(&state_dir, &args);

    let Some(sub) = args.get(1) else {
        eprintln!("bd-shim: subcommand required");
        return ExitCode::from(2);
    };
    let rest = &args[2..];
    match sub.as_str() {
        "list" => cmd_list(&state_dir, rest),
        "ready" => cmd_ready(&state_dir, rest),
        "show" => cmd_show(&state_dir, rest),
        "create" => cmd_create(&state_dir, rest),
        "update" => cmd_update(&state_dir, rest),
        "close" => cmd_close(&state_dir, rest),
        other => {
            eprintln!("bd-shim: unsupported subcommand {other}");
            ExitCode::from(2)
        }
    }
}

fn log_invocation(state_dir: &Path, args: &[String]) {
    let mut line = String::new();
    for a in args.iter().skip(1) {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&shell_quote(a));
    }
    line.push('\n');
    let path = state_dir.join(".invocations.log");
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .expect("open invocation log");
    f.write_all(line.as_bytes()).expect("write invocation log");
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.' | '=' | ':'))
        && !s.is_empty()
    {
        return s.to_string();
    }
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

fn read_field(state_dir: &Path, id: &str, field: &str) -> String {
    let path = state_dir.join(id).join(field);
    fs::read_to_string(&path).unwrap_or_default()
}

fn read_labels(state_dir: &Path, id: &str) -> Vec<String> {
    read_field(state_dir, id, "labels")
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

fn bead_to_json(state_dir: &Path, id: &str) -> serde_json::Value {
    let priority: u8 = read_field(state_dir, id, "priority")
        .trim()
        .parse()
        .unwrap_or(0);
    serde_json::json!({
        "id": id,
        "title": read_field(state_dir, id, "title"),
        "description": read_field(state_dir, id, "description"),
        "status": read_field(state_dir, id, "status"),
        "priority": priority,
        "issue_type": read_field(state_dir, id, "issue_type"),
        "notes": read_field(state_dir, id, "notes"),
        "labels": read_labels(state_dir, id),
        "parent": parent_json(state_dir, id),
        "metadata": metadata_json(state_dir, id),
    })
}

fn parent_json(state_dir: &Path, id: &str) -> serde_json::Value {
    let parent = read_field(state_dir, id, "parent");
    let trimmed = parent.trim();
    if trimmed.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(trimmed.to_owned())
    }
}

fn metadata_json(state_dir: &Path, id: &str) -> serde_json::Value {
    let raw = read_field(state_dir, id, "metadata.json");
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(trimmed)
            .unwrap_or_else(|err| serde_json::json!({"bd_shim.metadata_error": err.to_string()}))
    }
}

fn list_bead_ids(state_dir: &Path) -> Vec<String> {
    let entries = match fs::read_dir(state_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    ids.sort();
    ids
}

fn cmd_list(state_dir: &Path, args: &[String]) -> ExitCode {
    let mut label_any: Vec<String> = Vec::new();
    let mut status_filter: Option<String> = None;
    let mut label_eq: Option<String> = None;
    let mut type_filter: Option<String> = None;
    let mut parent_filter: Option<String> = None;
    let mut want_json = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(v) = a.strip_prefix("--label-any=") {
            label_any.push(v.to_string());
            i += 1;
        } else if let Some(v) = a.strip_prefix("--label=") {
            label_eq = Some(v.to_string());
            i += 1;
        } else if let Some(v) = a.strip_prefix("--status=") {
            status_filter = Some(v.to_string());
            i += 1;
        } else if let Some(v) = a.strip_prefix("--type=") {
            type_filter = Some(v.to_string());
            i += 1;
        } else if let Some(v) = a.strip_prefix("--parent=") {
            parent_filter = Some(v.to_string());
            i += 1;
        } else if a == "--json" {
            want_json = true;
            i += 1;
        } else if a == "--label-any" {
            label_any.push(args.get(i + 1).cloned().unwrap_or_default());
            i += 2;
        } else if a == "--label" {
            label_eq = Some(args.get(i + 1).cloned().unwrap_or_default());
            i += 2;
        } else if a == "--status" {
            status_filter = Some(args.get(i + 1).cloned().unwrap_or_default());
            i += 2;
        } else if a == "--type" {
            type_filter = Some(args.get(i + 1).cloned().unwrap_or_default());
            i += 2;
        } else if a == "--parent" {
            parent_filter = Some(args.get(i + 1).cloned().unwrap_or_default());
            i += 2;
        } else {
            eprintln!("bd-shim: list: unsupported flag {a}");
            return ExitCode::from(2);
        }
    }
    if !want_json {
        eprintln!("bd-shim: list: --json required (production code always passes it)");
        return ExitCode::from(2);
    }
    let mut out = Vec::new();
    for id in list_bead_ids(state_dir) {
        let labels = read_labels(state_dir, &id);
        if !label_any.is_empty() && !labels.iter().any(|l| label_any.contains(l)) {
            continue;
        }
        if let Some(want) = &label_eq
            && !labels.iter().any(|l| l == want)
        {
            continue;
        }
        if let Some(want) = &status_filter
            && read_field(state_dir, &id, "status").trim() != want
        {
            continue;
        }
        if let Some(want) = &type_filter
            && read_field(state_dir, &id, "issue_type").trim() != want
        {
            continue;
        }
        if let Some(want) = &parent_filter
            && read_field(state_dir, &id, "parent").trim() != want
        {
            continue;
        }
        out.push(bead_to_json(state_dir, &id));
    }
    println!("{}", serde_json::Value::Array(out));
    ExitCode::SUCCESS
}

fn cmd_ready(state_dir: &Path, args: &[String]) -> ExitCode {
    // `bd ready --json [--limit=N] [--label=<L>] [--exclude-label=<L> …]` —
    // beads with status=open, carrying the named label (when given), and
    // **not** carrying any of the excluded labels. The shim doesn't model
    // blocker dependencies; status + label match is sufficient for the
    // run-gate tests.
    let mut limit: Option<usize> = None;
    let mut label_eq: Option<String> = None;
    let mut exclude_labels: Vec<String> = Vec::new();
    let mut want_json = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(v) = a.strip_prefix("--limit=") {
            limit = v.parse().ok();
            i += 1;
        } else if let Some(v) = a.strip_prefix("--label=") {
            label_eq = Some(v.to_string());
            i += 1;
        } else if let Some(v) = a.strip_prefix("--exclude-label=") {
            exclude_labels.push(v.to_string());
            i += 1;
        } else if a == "--json" {
            want_json = true;
            i += 1;
        } else if a == "--limit" {
            limit = args.get(i + 1).and_then(|s| s.parse().ok());
            i += 2;
        } else if a == "--label" {
            label_eq = Some(args.get(i + 1).cloned().unwrap_or_default());
            i += 2;
        } else if a == "--exclude-label" {
            exclude_labels.push(args.get(i + 1).cloned().unwrap_or_default());
            i += 2;
        } else {
            eprintln!("bd-shim: ready: unsupported flag {a}");
            return ExitCode::from(2);
        }
    }
    if !want_json {
        eprintln!("bd-shim: ready: --json required (production code always passes it)");
        return ExitCode::from(2);
    }
    let mut out = Vec::new();
    for id in list_bead_ids(state_dir) {
        if read_field(state_dir, &id, "status").trim() != "open" {
            continue;
        }
        let labels = read_labels(state_dir, &id);
        if let Some(want) = &label_eq
            && !labels.iter().any(|l| l == want)
        {
            continue;
        }
        if labels.iter().any(|l| exclude_labels.contains(l)) {
            continue;
        }
        out.push(bead_to_json(state_dir, &id));
        if let Some(n) = limit
            && out.len() >= n
        {
            break;
        }
    }
    println!("{}", serde_json::Value::Array(out));
    ExitCode::SUCCESS
}

fn cmd_show(state_dir: &Path, args: &[String]) -> ExitCode {
    let Some(id) = args.first() else {
        eprintln!("bd-shim: show: bead id required");
        return ExitCode::from(2);
    };
    if !state_dir.join(id).is_dir() {
        eprintln!("bd-shim: bead {id} not found");
        return ExitCode::from(1);
    }
    let mut want_json = false;
    for a in &args[1..] {
        if a == "--json" {
            want_json = true;
        } else {
            eprintln!("bd-shim: show: unsupported flag {a}");
            return ExitCode::from(2);
        }
    }
    if !want_json {
        eprintln!("bd-shim: show: --json required");
        return ExitCode::from(2);
    }
    let arr = serde_json::Value::Array(vec![bead_to_json(state_dir, id)]);
    println!("{arr}");
    ExitCode::SUCCESS
}

fn cmd_create(state_dir: &Path, args: &[String]) -> ExitCode {
    let mut title = String::new();
    let mut description = String::new();
    let mut issue_type = "task".to_owned();
    let mut priority = "2".to_owned();
    let mut parent: Option<String> = None;
    let mut labels: Vec<String> = Vec::new();
    let mut metadata = serde_json::json!({});
    let mut silent = false;
    let mut i = 0;
    while i < args.len() {
        let flag = &args[i];
        match flag.as_str() {
            "--silent" => {
                silent = true;
                i += 1;
            }
            "--title" => {
                title = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--description" => {
                description = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--type" => {
                issue_type = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--priority" => {
                priority = args.get(i + 1).cloned().unwrap_or_default();
                i += 2;
            }
            "--parent" => {
                parent = Some(args.get(i + 1).cloned().unwrap_or_default());
                i += 2;
            }
            "--labels" => {
                labels = args
                    .get(i + 1)
                    .map(|raw| {
                        raw.split(',')
                            .filter(|label| !label.is_empty())
                            .map(ToOwned::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                i += 2;
            }
            "--metadata" => {
                let raw = args.get(i + 1).cloned().unwrap_or_default();
                metadata = serde_json::from_str(&raw).unwrap_or_else(
                    |err| serde_json::json!({"bd_shim.metadata_error": err.to_string()}),
                );
                i += 2;
            }
            other => {
                eprintln!("bd-shim: create: unsupported flag {other}");
                return ExitCode::from(2);
            }
        }
    }
    if !silent {
        eprintln!("bd-shim: create: --silent required (production code always passes it)");
        return ExitCode::from(2);
    }
    let id = next_create_id(state_dir);
    let bead_dir = state_dir.join(&id);
    fs::create_dir_all(&bead_dir).expect("mkdir bead dir");
    fs::write(bead_dir.join("title"), title).expect("write title");
    fs::write(bead_dir.join("description"), description).expect("write description");
    fs::write(bead_dir.join("status"), "open").expect("write status");
    fs::write(bead_dir.join("priority"), priority).expect("write priority");
    fs::write(bead_dir.join("issue_type"), issue_type).expect("write issue_type");
    fs::write(bead_dir.join("labels"), labels.join("\n")).expect("write labels");
    if let Some(parent) = parent {
        fs::write(bead_dir.join("parent"), parent).expect("write parent");
    }
    fs::write(
        bead_dir.join("metadata.json"),
        serde_json::to_string(&metadata).expect("metadata json"),
    )
    .expect("write metadata");
    println!("{id}");
    ExitCode::SUCCESS
}

fn next_create_id(state_dir: &Path) -> String {
    if let Ok(id) = env::var("BD_CREATE_ID")
        && !id.trim().is_empty()
    {
        return id;
    }
    let mut index = list_bead_ids(state_dir).len() + 1;
    loop {
        let id = format!("lm-tune{index}");
        if !state_dir.join(&id).exists() {
            return id;
        }
        index += 1;
    }
}

fn cmd_update(state_dir: &Path, args: &[String]) -> ExitCode {
    let Some(id) = args.first() else {
        eprintln!("bd-shim: update: bead id required");
        return ExitCode::from(2);
    };
    let bead_dir = state_dir.join(id);
    if !bead_dir.is_dir() {
        eprintln!("bd-shim: bead {id} not found");
        return ExitCode::from(1);
    }
    let mut i = 1;
    while i < args.len() {
        let flag = &args[i];
        match flag.as_str() {
            "--description" => {
                let val = args.get(i + 1).cloned().unwrap_or_default();
                fs::write(bead_dir.join("description"), val).expect("write description");
                i += 2;
            }
            "--title" => {
                let val = args.get(i + 1).cloned().unwrap_or_default();
                fs::write(bead_dir.join("title"), val).expect("write title");
                i += 2;
            }
            "--notes" => {
                let val = args.get(i + 1).cloned().unwrap_or_default();
                fs::write(bead_dir.join("notes"), val).expect("write notes");
                i += 2;
            }
            "--remove-label" => {
                let val = args.get(i + 1).cloned().unwrap_or_default();
                let labels: Vec<String> = read_labels(state_dir, id)
                    .into_iter()
                    .filter(|l| l != &val)
                    .collect();
                fs::write(bead_dir.join("labels"), labels.join("\n")).expect("write labels");
                i += 2;
            }
            "--add-label" => {
                let val = args.get(i + 1).cloned().unwrap_or_default();
                let mut labels = read_labels(state_dir, id);
                if !labels.contains(&val) {
                    labels.push(val);
                }
                fs::write(bead_dir.join("labels"), labels.join("\n")).expect("write labels");
                i += 2;
            }
            "--status" => {
                let val = args.get(i + 1).cloned().unwrap_or_default();
                fs::write(bead_dir.join("status"), val).expect("write status");
                i += 2;
            }
            "--priority" => {
                let val = args.get(i + 1).cloned().unwrap_or_default();
                fs::write(bead_dir.join("priority"), val).expect("write priority");
                i += 2;
            }
            "--claim" => {
                fs::write(bead_dir.join("status"), "in_progress").expect("claim status");
                i += 1;
            }
            "--set-metadata" => {
                let val = args.get(i + 1).cloned().unwrap_or_default();
                set_metadata_value(&bead_dir, &val);
                i += 2;
            }
            other => {
                eprintln!("bd-shim: update: unsupported flag {other}");
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::SUCCESS
}

fn set_metadata_value(bead_dir: &Path, assignment: &str) {
    let (key, value) = assignment.split_once('=').unwrap_or((assignment, ""));
    let path = bead_dir.join("metadata.json");
    let current = fs::read_to_string(&path).unwrap_or_default();
    let mut metadata = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&current)
        .unwrap_or_default();
    metadata.insert(key.to_owned(), parse_metadata_value(value));
    fs::write(
        path,
        serde_json::to_string(&serde_json::Value::Object(metadata)).expect("metadata json"),
    )
    .expect("write metadata");
}

fn parse_metadata_value(value: &str) -> serde_json::Value {
    serde_json::from_str(value).unwrap_or_else(|_| serde_json::Value::String(value.to_owned()))
}

fn cmd_close(state_dir: &Path, args: &[String]) -> ExitCode {
    let Some(id) = args.first() else {
        eprintln!("bd-shim: close: bead id required");
        return ExitCode::from(2);
    };
    let bead_dir = state_dir.join(id);
    if !bead_dir.is_dir() {
        eprintln!("bd-shim: bead {id} not found");
        return ExitCode::from(1);
    }
    fs::write(bead_dir.join("status"), "closed").expect("close status");
    ExitCode::SUCCESS
}
