#!/usr/bin/env bash
# Judge rubrics for harness.md success criteria.
#
# Each function describes a rubric the judge LLM evaluates against the
# referenced source files; the spec links to the function via a
# `[judge](tests/judges/loom.sh::<name>)` annotation in its Success Criteria.

test_git_client_encapsulation() {
  judge_files \
    "loom/crates/loom-driver/src/git/mod.rs" \
    "loom/crates/loom-driver/src/git/client.rs" \
    "loom/crates/loom-driver/src/git/error.rs" \
    "loom/crates/loom-driver/src/lib.rs" \
    "loom/crates/loom/src/main.rs" \
    "loom/crates/loom-agent/src/lib.rs" \
    "loom/crates/loom-workflow/src/lib.rs" \
    "loom/crates/loom-templates/src/lib.rs"
  judge_criterion \
    "GitClient (loom/crates/loom-driver/src/git/) is the ONLY module that imports the gix crate or invokes the git CLI (Command::new(\"git\") or shell-out). Outside the git module, no source file may 'use gix' or spawn git directly. Callers see only typed Rust methods (status, diff_head_parent, worktrees, create_worktree, remove_worktree, merge_branch). Verify by inspecting every listed file: only files under loom-driver/src/git/ may reference gix or invoke git; the other crates and lib.rs / main.rs must not."
}

test_template_context_structs() {
  judge_files \
    "loom/crates/loom-templates/src/lib.rs" \
    "loom/crates/loom-templates/src/plan/mod.rs" \
    "loom/crates/loom-templates/src/plan/new.rs" \
    "loom/crates/loom-templates/src/plan/update.rs" \
    "loom/crates/loom-templates/src/todo/mod.rs" \
    "loom/crates/loom-templates/src/todo/new.rs" \
    "loom/crates/loom-templates/src/todo/update.rs" \
    "loom/crates/loom-templates/src/run/mod.rs" \
    "loom/crates/loom-templates/src/check/mod.rs" \
    "loom/crates/loom-templates/src/msg/mod.rs"
  judge_criterion \
    "Each Loom workflow template has a typed Rust context struct with #[derive(askama::Template)] and the correct #[template(path = ...)] attribute. Module structure is nested per template family — no central types.rs at the crate root, no shared error.rs; lib.rs only declares pub mod for plan, todo, run, check, msg. Domain identifier fields use the loom-driver newtypes (BeadId, MoleculeId, SpecLabel) — never bare String — for issue_id, molecule_id, label. Optional fields use Option<T> (spec_diff, existing_tasks, molecule_id, issue_id, title, description, beads_summary, base_commit, previous_failure). Multi-valued fields use Vec<T> (companion_paths: Vec<String>, implementation_notes: Vec<String>, clarify_beads: Vec<ClarifyBead>). PreviousFailure is its own type that enforces the 4000-char truncation cap from the spec — RunContext stores Option<PreviousFailure>, not Option<String>. ClarifyBead and ClarifyOption live alongside MsgContext in msg/mod.rs (the only template that uses them). Templates declare escape = \"none\" so markdown bodies are not HTML-escaped."
}

test_run_single_event_sink() {
  judge_files \
    "loom/crates/loom-driver/src/logging/mod.rs" \
    "loom/crates/loom-driver/src/logging/sink.rs" \
    "loom/crates/loom-driver/src/logging/renderer.rs"
  judge_criterion \
    "LogSink (loom/crates/loom-driver/src/logging/sink.rs) is a single tee-style sink: one Self::emit method writes the AgentEvent to BOTH the on-disk JSONL log file AND the TerminalRenderer in lockstep within the same call. There is no independent task, channel, thread, or background worker that drives the renderer or the file writer separately — both writers must observe the same event sequence by construction. Verify by inspecting sink.rs: the struct holds the BufWriter<File> and the TerminalRenderer as direct fields, and emit() dispatches to both inline. The renderer must NOT pull events from a queue or be wrapped in a separate Tokio task. The on-disk format is the serialized AgentEvent (one JSON object per line), so the renderer and the file writer agree on the event sequence."
}

test_newtypes_for_identifiers() {
  judge_files \
    "loom/crates/loom-driver/src/identifier/mod.rs" \
    "loom/crates/loom-driver/src/identifier/bead.rs" \
    "loom/crates/loom-driver/src/identifier/spec.rs" \
    "loom/crates/loom-driver/src/identifier/molecule.rs" \
    "loom/crates/loom-driver/src/identifier/profile.rs" \
    "loom/crates/loom-driver/src/identifier/session.rs" \
    "loom/crates/loom-driver/src/identifier/tool_call.rs" \
    "loom/crates/loom-driver/src/identifier/request.rs" \
    "loom/crates/loom-driver/src/agent/mod.rs"
  judge_criterion \
    "Domain and protocol identifiers are newtype wrappers, not bare strings. The newtype_id! macro in identifier/mod.rs produces a #[serde(transparent)] tuple struct around String with new(impl Into<String>) -> Self, as_str(&self) -> &str, and a Display impl that writes the inner string. Each id family lives in its own submodule under identifier/ — bead.rs (BeadId), spec.rs (SpecLabel), molecule.rs (MoleculeId), profile.rs (ProfileName), session.rs (SessionId), tool_call.rs (ToolCallId), request.rs (RequestId) — and invokes newtype_id! exactly once. AgentKind in agent/mod.rs is a plain enum { Pi, Claude } with serde derive (NOT a newtype) — variants serialize as 'pi'/'claude'. The macro must NOT emit derive(From) or derive(Into) (NF-8 forbids them); values must enter via new() so future per-family validation can be added without bypass paths."
}

judge_live_path_coverage() {
  judge_files \
    "loom/crates/loom-templates/templates/check.md" \
    "loom/crates/loom-workflow/src/check/runner.rs" \
    "loom/crates/loom-workflow/src/check/phase_verdict.rs"
  judge_criterion \
    "The review prompt (check.md) and review-gate code (check/runner.rs, check/phase_verdict.rs) treat live-path coverage as the reviewer's primary concern: at least one [verify] annotation on the bead must exercise the live path — same binary, same argv shape, same env as production. The reviewer is instructed to flag a bead whose entire [verify] set is mock-only (no live invocation), and that flag resolves to RecoveryCause::ReviewConcern with the concern named 'live-path' in the flag detail (so the gate's recovery path is observable). Inspect check.md: the prompt must state this expectation explicitly and tell the reviewer what to do when an all-mock set is observed; inspect runner.rs / phase_verdict.rs: the live-path concern must be representable as one of the named flag concerns the gate emits, not buried in free-form text."
}

judge_mock_discipline() {
  judge_files \
    "loom/crates/loom-templates/templates/check.md" \
    "loom/crates/loom-workflow/src/check/runner.rs" \
    "loom/crates/loom-workflow/src/check/phase_verdict.rs"
  judge_criterion \
    "The review prompt (check.md) instructs the reviewer to flag mocks that stand in for the very thing under test — for example, mocking the agent backend in an agent-integration test, or stubbing the database in a test whose stated purpose is to exercise schema migrations. The rubric the reviewer applies is: identify what the test claims to validate (from its name, location, or [verify] criterion text), then check whether the test mocks that exact subsystem. When the answer is 'yes', the reviewer raises a flag, the gate resolves to RecoveryCause::ReviewConcern, and the flag detail names 'mock' as the triggering concern (mirrors how 'live-path' is named). Mocks of unrelated dependencies are NOT in scope; only mocks of the system-under-test are flagged."
}

judge_plan_update_merges_notes() {
  judge_files \
    "loom/crates/loom-templates/templates/plan_update.md" \
    "loom/crates/loom-templates/src/plan/update.rs" \
    "loom/crates/loom-workflow/src/plan/runner.rs" \
    "loom/crates/loom-workflow/src/plan/prompt.rs"
  judge_criterion \
    "The plan_update.md prompt renders the existing implementation-notes array from the spec's notes table (the typed PlanUpdateContext.implementation_notes field) into the interview, and explicitly instructs the agent to MERGE: keep notes still relevant, drop notes a new decision invalidates, add fresh notes, rather than blind append or blind replace. The prompt names all three operations (keep / drop / add) and frames the merge as the agent's judgement during the interview. The runner in plan/runner.rs reads the existing array via StateDb::notes_list before launching the interview and passes it into the rendered context through plan/prompt.rs. The agent persists the merged array back via 'loom note set LABEL --kind implementation --json ARRAY', which atomically replaces the prior set in a single SQLite transaction (StateDb::notes_set performs DELETE plus INSERTs in one transaction). No code path silently appends or silently replaces; the merge is mediated by the interview output, and the prompt directs the agent at the exact CLI invocation."
}

test_scratchpad_partial_clarity() {
  judge_files \
    "loom/crates/loom-templates/templates/partial/scratchpad.md"
  judge_criterion \
    "partial/scratchpad.md tells the agent that the scratchpad is agent-lifecycle-only — the file is created at session start, removed at session end on every exit path, and is a compaction-recovery aid rather than durable storage. It explicitly enumerates durable destinations for anything that must outlive the session: bead notes (bd update --notes), the spec file (specs/<label>.md), the commit message, CLAUDE.md / companion docs, or a new bead (bd create). The partial directs the agent to write to those destinations BEFORE session end if the thought is worth keeping, so a future agent reading the bead, spec, or commit history can find the durable record. Vague guidance like 'write important things down' without naming the durable destination is insufficient — the partial must enumerate them."
}

judge_tool_trait_ecosystem_compat() {
  judge_files \
    "loom/crates/loom-llm/src/tool.rs" \
    "loom/crates/loom-llm/src/lib.rs"
  judge_criterion \
    "The Tool trait in loom-llm/src/tool.rs exposes a shape that is reasonably convertible to other Rust agent-loop crates' tool surfaces (agent-client-protocol, rig, etc.) so re-hosting Conversation on a different crate later is feasible. Specifically: (1) the trait carries exactly the four documented members — name() -> &str, description() -> &str, input_schema() -> serde_json::Value, and an async invoke(args: serde_json::Value) -> Result<ToolOutput, LlmError> — so the three read-side accessors are sufficient to populate the Anthropic Messages-API tool definition shape { name, description, input_schema } from the trait alone; (2) ToolOutput carries a canonical-JSON content payload (serde_json::Value, not String) plus an is_error flag, so tool results compose into ecosystem crates that key on canonical JSON; (3) the trait is dyn-compatible — the async invoke returns a boxed future (the InvokeFuture alias) so handlers store as Box<dyn Tool> without per-type monomorphisation, matching the Vec<Box<dyn Tool>> registry shape used by every ecosystem agent-loop crate; (4) the trait bounds Send + Sync so handlers cross thread boundaries the same way ecosystem crates require. The forward-compat smoke test (tool_trait_generates_anthropic_schema_that_round_trips in the same file) exercises a sample Tool impl and verifies the generated Anthropic schema JSON round-trips losslessly through serde_json — judge that the test actually constructs the Anthropic-shaped JSON from the trait's read-side surface (no parallel hand-built struct) and asserts the round-tripped value matches the original, so a future refactor that drops a method or changes a return type trips the test."
}

judge_sibling_spec_editing_documents_split() {
  judge_files \
    "loom/crates/loom-templates/templates/partial/sibling_spec_editing.md"
  judge_criterion \
    "partial/sibling_spec_editing.md establishes three things, all in one place: (1) the anchor/sibling editing model — that the -u label owns the session state row and any spec under specs/ may be edited in-place during the interview; (2) the new-sibling-spec carve-out — the planner may decide a section warrants its own spec, in which case it allocates a tracking epic via 'bd create --type=epic' and adds the row to docs/README.md, and this is the SINGLE permitted exception to the otherwise-strict 'no bead creation during planning' rule (implementation beads come later, from loom todo, not here); and (3) the commit-discipline rule — planning sessions edit specs in place but do NOT commit; soft signals like 'looks good' or 'next' or 'accept' authorize the next interview step but never authorize a commit; commits require unambiguous language such as 'commit', 'ship it', 'land the changes', 'land the plane', or 'push it'. The same discipline applies to git push, beads-push, and any shared-state mutation. The partial must name all three: the editing model, the bead-allocation carve-out (with the 'one carve-out' framing so the reader understands why it's an exception), and the commit-discipline rule (with explicit examples of soft signals vs. unambiguous triggers). Vague phrasing like 'be careful with commits' is insufficient — the partial must enumerate concrete trigger phrases."
}
