//! Architectural: the renderer and the on-disk log writer share one
//! event source via `LogSink::emit`. The method's body must reference
//! `self.file` and the renderer in the same call — a fan-out task
//! would create two independent subscribers and re-introduce drift.

use super::util::{read_to_string, rel, verdict_from, workspace_root};
use super::{Verdict, WalkInput};

const RULE: &str =
    "single_event_channel — LogSink::emit must drive renderer + on-disk log in one call";

pub fn run(_input: &WalkInput) -> Verdict {
    let root = workspace_root();
    let sink_path = root.join("crates/loom-render/src/sink/mod.rs");
    let rel_path = rel(&root, &sink_path);

    let Some(source) = read_to_string(&sink_path) else {
        return verdict_from(
            RULE,
            vec![format!(
                "{rel_path}:1 missing — LogSink source file not found"
            )],
        );
    };
    let Ok(file) = syn::parse_file(&source) else {
        return verdict_from(
            RULE,
            vec![format!(
                "{rel_path}:1 parse error — LogSink source did not parse"
            )],
        );
    };

    let mut found_emit = false;
    let mut writes_file = false;
    let mut drives_renderer = false;

    for item in &file.items {
        let syn::Item::Impl(item_impl) = item else {
            continue;
        };
        let syn::Type::Path(type_path) = &*item_impl.self_ty else {
            continue;
        };
        let Some(seg) = type_path.path.segments.last() else {
            continue;
        };
        if seg.ident != "LogSink" {
            continue;
        }
        for impl_item in &item_impl.items {
            let syn::ImplItem::Fn(method) = impl_item else {
                continue;
            };
            if method.sig.ident != "emit" {
                continue;
            }
            found_emit = true;
            let block_text = source_slice(&source, &method.block);
            if block_text.contains("self.file") {
                writes_file = true;
            }
            if block_text.contains("renderer") {
                drives_renderer = true;
            }
        }
    }

    let mut violations = Vec::new();
    if !found_emit {
        violations.push(format!(
            "{rel_path}:1 LogSink::emit method not found — tee-style sink must subscribe both renderer and on-disk log"
        ));
    }
    if found_emit && !writes_file {
        violations.push(format!(
            "{rel_path}:1 LogSink::emit must write to `self.file` in the same call as the renderer"
        ));
    }
    if found_emit && !drives_renderer {
        violations.push(format!(
            "{rel_path}:1 LogSink::emit must drive the renderer in the same call as the on-disk write"
        ));
    }
    verdict_from(RULE, violations)
}

fn source_slice<T: syn::spanned::Spanned>(source: &str, node: &T) -> String {
    let span = node.span();
    let start = span.start();
    let end = span.end();
    let mut out = String::new();
    for (idx, line) in source.lines().enumerate() {
        let lineno = idx + 1;
        if lineno < start.line || lineno > end.line {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}
