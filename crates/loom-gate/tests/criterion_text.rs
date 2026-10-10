//! Criterion text and identity must follow Markdown item boundaries, not layout.

use std::path::Path;

use loom_driver::identifier::SpecLabel;
use loom_gate::annotation::{criterion_id_for, criterion_text_for_line, parse_content};

fn texts(body: &str) -> Vec<String> {
    parse_content(Path::new("specs/example.md"), body)
        .criteria
        .into_iter()
        .map(|criterion| criterion.text)
        .collect()
}

#[test]
fn criterion_text_stops_at_markdown_item_boundaries() {
    let body = "## Success Criteria\n\n- First claim [test](first)\n\nUnrelated prose.\n\n### Another group\n\n- Final claim [test](last)\n\n## Requirements\n\nThis is not acceptance text.\n\n- Nor is this bullet.\n";
    assert_eq!(texts(body), ["First claim", "Final claim"]);
    assert_eq!(criterion_text_for_line(body, 3), "First claim");
    assert_eq!(criterion_text_for_line(body, 9), "Final claim");
}

#[test]
fn formatting_comments_do_not_change_criterion_identity() {
    let plain =
        "## Success Criteria\n\n- First claim [test](first)\n\n- Second claim [test](second)\n";
    let formatted = [
        "## Success Criteria\n\n<!-- prettier-ignore -->\n- First <!-- a presentation comment --> claim [test](first)\n\n<!-- prettier-ignore -->\n- Second claim [test](second)\n\n<!-- trailing comment -->\n",
        "## Success Criteria\n\n- First\n  <!-- format --> claim\n  [test](first)\n\n- Second\n  <!-- format --> claim [test](second)\n",
        "## Success Criteria\n\n- First\n  <!-- multiline\n  presentation comment --> claim\n  [test](first)\n\n- Second\n  <!-- format --> <!-- another comment --> claim [test](second)\n",
    ];
    let label = SpecLabel::new("example").expect("valid label");
    let before = texts(plain);
    assert_eq!(before, ["First claim", "Second claim"]);
    for body in formatted {
        let after = texts(body);
        assert_eq!(after, before);
        for (a, b) in before.iter().zip(&after) {
            assert_eq!(criterion_id_for(&label, a), criterion_id_for(&label, b));
        }
    }
}

#[test]
fn inline_and_separate_bindings_have_identical_criterion_text() {
    let inline = "## Success Criteria\n\n- Review's secondary concerns are scope appropriateness and `[judge]` rubric\n  satisfaction [test](review_renders_review_context_fields)\n";
    let separate = inline.replace("satisfaction [test]", "satisfaction\n  [test]");
    let parsed = parse_content(Path::new("specs/gate.md"), inline);
    assert_eq!(parsed.annotations[0].criterion_line, 3);
    assert_eq!(parsed.annotations[0].line, 4);
    assert_eq!(texts(inline), texts(&separate));
    assert_eq!(
        texts(inline),
        ["Review's secondary concerns are scope appropriateness and `[judge]` rubric satisfaction"]
    );
}

#[test]
fn multiline_balanced_bindings_do_not_leak_into_criterion_text() {
    let body = "## Success Criteria\n\n- A Unicode claim: λ works\n  [check?](bash -c 'printf \"%s\" \"$(date)\"\n  && echo done')\n  with this qualifier.\n";
    let parsed = parse_content(Path::new("specs/example.md"), body);
    assert_eq!(parsed.annotations.len(), 1);
    assert!(parsed.annotations[0].pending);
    assert_eq!(
        parsed.annotations[0].target,
        "bash -c 'printf \"%s\" \"$(date)\"\n  && echo done'"
    );
    assert_eq!(
        texts(body),
        ["A Unicode claim: λ works with this qualifier."]
    );
}

#[test]
fn annotation_examples_remain_in_requirement_text() {
    let body = "## Success Criteria\n\n- Preserve `[test](example)` and `<!-- literal -->`.\n\n  ```markdown\n  [check](example --arg)\n  ```\n\n  [test](real_verifier)\n";
    let parsed = parse_content(Path::new("specs/example.md"), body);
    assert_eq!(parsed.annotations.len(), 1);
    assert_eq!(parsed.annotations[0].target, "real_verifier");
    assert_eq!(
        texts(body),
        [
            "Preserve `[test](example)` and `<!-- literal -->`. ```markdown [check](example --arg) ```"
        ]
    );
}

#[test]
fn commented_out_annotations_are_not_bindings() {
    let body = "## Success Criteria\n\n- Real claim <!-- [test](not_a_binding) --> [test](real)\n\n<!--\n- Hidden claim [check](not_a_command)\n-->\n";
    let parsed = parse_content(Path::new("specs/example.md"), body);
    assert_eq!(parsed.annotations.len(), 1);
    assert_eq!(parsed.annotations[0].target, "real");
    assert_eq!(texts(body), ["Real claim"]);
}

#[test]
fn bindings_after_block_comments_remain_attached_to_the_criterion() {
    let body = "## Success Criteria\n\n- Exact\n  <!-- [test](hidden)\n  --> requirement <!-- [judge](also_hidden) --> qualifier [system?](bash -c 'printf \"(bytes)\\n\"')\n";
    let parsed = parse_content(Path::new("specs/example.md"), body);
    assert_eq!(parsed.annotations.len(), 1);
    let annotation = &parsed.annotations[0];
    assert_eq!(annotation.tier, loom_gate::annotation::Tier::System);
    assert_eq!(annotation.target, "bash -c 'printf \"(bytes)\\n\"'");
    assert!(annotation.pending);
    assert_eq!(annotation.line, 5);
    assert_eq!(annotation.criterion_line, 3);
    assert_eq!(texts(body), ["Exact requirement qualifier"]);
    assert_eq!(
        criterion_text_for_line(body, 3),
        "Exact requirement qualifier"
    );
}

#[test]
fn unterminated_block_comments_hide_bindings_to_the_end_of_the_block() {
    let body =
        "## Success Criteria\n\n- Exact requirement\n  <!-- unterminated\n  [test](hidden)\n";
    let parsed = parse_content(Path::new("specs/example.md"), body);
    assert_eq!(parsed.annotations.len(), 0);
    assert_eq!(texts(body), ["Exact requirement"]);
}

#[test]
fn loose_item_paragraphs_remain_part_of_the_criterion() {
    let body = "## Success Criteria\r\n\r\n+ First paragraph.\r\n\r\n  Second paragraph with **emphasis**.\r\n\r\n  [test](real)\r\n\r\nOutside the item.\r\n";
    assert_eq!(
        texts(body),
        ["First paragraph. Second paragraph with **emphasis**."]
    );
}

#[test]
fn prose_annotation_identity_is_bounded_by_its_paragraph() {
    let body = "## Success Criteria\n\nA prose claim\nwith continuation [test](real).\n\n## Requirements\n\nNot part of that claim.\n";
    assert_eq!(
        criterion_text_for_line(body, 4),
        "A prose claim with continuation ."
    );
}

#[test]
fn workspace_criterion_text_is_independent_of_formatter_comments_and_trailing_sections() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../specs");
    let mut inspected = 0;
    for entry in std::fs::read_dir(root).expect("workspace specs") {
        let path = entry.expect("directory entry").path();
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("spec body");
        let clean = body.replace("<!-- prettier-ignore -->\n", "");
        let extended =
            format!("{clean}\n## Unrelated appendix\n\nMust not become acceptance text.\n");
        let parsed = texts(&body);
        assert!(!parsed.is_empty(), "{}", path.display());
        assert_eq!(parsed, texts(&extended), "{}", path.display());
        inspected += 1;
    }
    assert!(inspected > 0);
}
