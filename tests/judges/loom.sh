#!/usr/bin/env bash
# Judge rubrics for harness.md success criteria.
#
# Each function describes a rubric the judge LLM evaluates against the
# referenced source files; the spec links to the function via a
# `[judge](tests/judges/loom.sh::<name>)` annotation in its Success Criteria.

test_git_client_encapsulation() {
  judge_files \
    "crates/loom-driver/src/git/mod.rs" \
    "crates/loom-driver/src/git/client.rs" \
    "crates/loom-driver/src/git/error.rs" \
    "crates/loom-driver/src/lib.rs" \
    "crates/loom/src/main.rs" \
    "crates/loom-agent/src/lib.rs" \
    "crates/loom-workflow/src/lib.rs" \
    "crates/loom-templates/src/lib.rs"
  judge_criterion \
    "GitClient (crates/loom-driver/src/git/) is the ONLY module that imports the gix crate or invokes the git CLI (Command::new(\"git\") or shell-out). Outside the git module, no source file may 'use gix' or spawn git directly. Callers see only typed Rust methods (status, diff_head_parent, worktrees, create_worktree, remove_worktree, merge_branch). Verify by inspecting every listed file: only files under loom-driver/src/git/ may reference gix or invoke git; the other crates and lib.rs / main.rs must not."
}

test_template_context_structs() {
  judge_files \
    "crates/loom-templates/src/lib.rs" \
    "crates/loom-templates/src/plan/mod.rs" \
    "crates/loom-templates/src/plan/new.rs" \
    "crates/loom-templates/src/plan/update.rs" \
    "crates/loom-templates/src/todo/mod.rs" \
    "crates/loom-templates/src/todo/new.rs" \
    "crates/loom-templates/src/todo/update.rs" \
    "crates/loom-templates/src/run/mod.rs" \
    "crates/loom-templates/src/review/mod.rs" \
    "crates/loom-templates/src/msg/mod.rs"
  judge_criterion \
    "Each Loom workflow template has a typed Rust context struct with #[derive(askama::Template)] and the correct #[template(path = ...)] attribute. Module structure is nested per template family — no central types.rs at the crate root, no shared error.rs; lib.rs only declares pub mod for plan, todo, run, review, msg. Domain identifier fields use the loom-events newtypes (BeadId, MoleculeId, SpecLabel) — never bare String — for issue_id, molecule_id, label. Optional fields use Option<T> (spec_diff, existing_tasks, molecule_id, issue_id, title, description, beads_summary, base_commit, previous_failure). Multi-valued fields use Vec<T> (companion_paths: Vec<String>, implementation_notes: Vec<String>, clarify_beads: Vec<ClarifyBead>). PreviousFailure is its own type that enforces the 4000-char truncation cap from the spec — RunContext stores Option<PreviousFailure>, not Option<String>. ClarifyBead and ClarifyOption live alongside MsgContext in msg/mod.rs (the only template that uses them). Templates declare escape = \"none\" so markdown bodies are not HTML-escaped."
}

test_run_single_event_sink() {
  judge_files \
    "crates/loom-render/src/sink/mod.rs" \
    "crates/loom-render/src/renderer.rs" \
    "crates/loom-driver/src/logging/mod.rs"
  judge_criterion \
    "LogSink (crates/loom-render/src/sink/mod.rs) is a single tee-style sink: one Self::emit method writes the AgentEvent to BOTH the on-disk JSONL log file AND the TerminalRenderer in lockstep within the same call. There is no independent task, channel, thread, or background worker that drives the renderer or the file writer separately — both writers must observe the same event sequence by construction. Verify by inspecting sink/mod.rs: the struct holds the BufWriter<File> and the renderer (Option<Box<dyn Renderer>>) as direct fields, and emit() dispatches to both inline. The renderer must NOT pull events from a queue or be wrapped in a separate Tokio task. The on-disk format is the serialized AgentEvent (one JSON object per line), so the renderer and the file writer agree on the event sequence. loom-driver/src/logging/mod.rs is the thin driver-side re-export of LogSink, TerminalRenderer, and the path helpers from loom-render so legacy call sites keep resolving — it must contain no parallel sink implementation of its own."
}

test_newtypes_for_identifiers() {
  judge_files \
    "crates/loom-events/src/identifier/mod.rs" \
    "crates/loom-events/src/identifier/bead.rs" \
    "crates/loom-events/src/identifier/spec.rs" \
    "crates/loom-events/src/identifier/molecule.rs" \
    "crates/loom-events/src/identifier/profile.rs" \
    "crates/loom-events/src/identifier/session.rs" \
    "crates/loom-events/src/identifier/tool_call.rs" \
    "crates/loom-events/src/identifier/request.rs" \
    "crates/loom-driver/src/agent/kind.rs"
  judge_criterion \
    "Domain and protocol identifiers in loom-events::identifier are hand-written newtypes — there is NO shared macro (no newtype_id! or equivalent). Each id family lives in its own submodule under identifier/ — bead.rs (BeadId), spec.rs (SpecLabel), molecule.rs (MoleculeId), profile.rs (ProfileName), session.rs (SessionId), tool_call.rs (ToolCallId), request.rs (RequestId) — and identifier/mod.rs only declares the submodules and re-exports the public types. Every newtype is a tuple struct wrapping a single String, derives #[serde(transparent)] plus the standard value traits (Debug, Clone, PartialEq, Eq, Hash, Serialize), exposes as_str(&self) -> &str, and implements Display by writing the inner string. Deserialize is hand-written (not derived) so the construction path can validate input. BeadId is the strictest: BeadId::new returns Result<Self, ParseBeadIdError> and enforces the canonical <prefix>-<base32>(.<digits>)? shape; its FromStr and Deserialize both go through new() so external input (CLI args, bd --json output) cannot smuggle in a malformed wrapper. SpecLabel parses kebab-case via FromStr; its new(impl Into<String>) is permissive while Deserialize routes through the parser. The remaining ids (MoleculeId, ProfileName, SessionId, ToolCallId, RequestId) keep a permissive new(impl Into<String>). NF-8 forbids derive(From) and derive(Into) on any of these newtypes so values must enter through new() and future per-family validation cannot be bypassed. AgentKind in loom-driver/src/agent/kind.rs is a plain enum { Pi, Claude } with serde(rename_all = \"lowercase\") (NOT a newtype) — variants serialize as 'pi'/'claude' because the variants are a closed compile-time set, not parsed strings."
}

judge_live_path_coverage() {
  judge_files \
    "crates/loom-templates/templates/review.md" \
    "crates/loom-workflow/src/review/runner.rs" \
    "crates/loom-workflow/src/review/phase_verdict.rs"
  judge_criterion \
    "The review prompt (review.md) and review-gate code (review/runner.rs, review/phase_verdict.rs) treat live-path coverage as the reviewer's primary concern: at least one [verify] annotation on the bead must exercise the live path — same binary, same argv shape, same env as production. The reviewer is instructed to flag a bead whose entire [verify] set is mock-only (no live invocation), and that flag resolves to RecoveryCause::ReviewConcern with the concern named as one of the verifier-honesty tokens (verifier-bypass, fabricated-result, weak-assertion, coincidental-pass) in the flag detail (so the gate's recovery path is observable). Inspect review.md: the prompt must state this expectation explicitly and tell the reviewer what to do when an all-mock set is observed; inspect runner.rs / phase_verdict.rs: the live-path concern must be representable as one of the named flag concerns the gate emits (the ReviewConcern enum in phase_verdict.rs), not buried in free-form text."
}

judge_mock_discipline() {
  judge_files \
    "crates/loom-templates/templates/review.md" \
    "crates/loom-workflow/src/review/runner.rs" \
    "crates/loom-workflow/src/review/phase_verdict.rs"
  judge_criterion \
    "The review prompt (review.md) instructs the reviewer to flag mocks that stand in for the very thing under test — for example, mocking the agent backend in an agent-integration test, or stubbing the database in a test whose stated purpose is to exercise schema migrations. The rubric the reviewer applies is: identify what the test claims to validate (from its name, location, or [verify] criterion text), then check whether the test mocks that exact subsystem. When the answer is 'yes', the reviewer raises a flag, the gate resolves to RecoveryCause::ReviewConcern, and the flag detail names 'mock' as the triggering concern (mirrors how the verifier-honesty tokens are named). Mocks of unrelated dependencies are NOT in scope; only mocks of the system-under-test are flagged."
}

judge_plan_update_merges_notes() {
  judge_files \
    "crates/loom-templates/templates/plan_update.md" \
    "crates/loom-templates/src/plan/update.rs" \
    "crates/loom-workflow/src/plan/runner.rs" \
    "crates/loom-workflow/src/plan/prompt.rs"
  judge_criterion \
    "The plan_update.md prompt renders the existing implementation-notes array from the spec's notes table (the typed PlanUpdateContext.implementation_notes field) into the interview, and explicitly instructs the agent to MERGE: keep notes still relevant, drop notes a new decision invalidates, add fresh notes, rather than blind append or blind replace. The prompt names all three operations (keep / drop / add) and frames the merge as the agent's judgement during the interview. The runner in plan/runner.rs reads the existing array via StateDb::notes_list before launching the interview and passes it into the rendered context through plan/prompt.rs. The agent persists the merged array back via 'loom note set LABEL --kind implementation --json ARRAY', which atomically replaces the prior set in a single SQLite transaction (StateDb::notes_set performs DELETE plus INSERTs in one transaction). No code path silently appends or silently replaces; the merge is mediated by the interview output, and the prompt directs the agent at the exact CLI invocation."
}

test_scratchpad_partial_clarity() {
  judge_files \
    "crates/loom-templates/templates/partial/scratchpad.md"
  judge_criterion \
    "partial/scratchpad.md tells the agent that the scratchpad is agent-lifecycle-only — the file is created at session start, removed at session end on every exit path, and is a compaction-recovery aid rather than durable storage. It explicitly enumerates durable destinations for anything that must outlive the session: bead notes (bd update --notes), the spec file (specs/<label>.md), the commit message, CLAUDE.md / companion docs, or a new bead (bd create). The partial directs the agent to write to those destinations BEFORE session end if the thought is worth keeping, so a future agent reading the bead, spec, or commit history can find the durable record. Vague guidance like 'write important things down' without naming the durable destination is insufficient — the partial must enumerate them."
}

judge_tool_trait_ecosystem_compat() {
  judge_files \
    "crates/loom-llm/src/tool.rs" \
    "crates/loom-llm/src/lib.rs"
  judge_criterion \
    "The Tool trait in loom-llm/src/tool.rs exposes a shape that is reasonably convertible to other Rust agent-loop crates' tool surfaces (agent-client-protocol, rig, etc.) so re-hosting Conversation on a different crate later is feasible. Specifically: (1) the trait carries exactly the four documented members — name() -> &str, description() -> &str, input_schema() -> serde_json::Value, and an async invoke(args: serde_json::Value) -> Result<ToolOutput, LlmError> — so the three read-side accessors are sufficient to populate the Anthropic Messages-API tool definition shape { name, description, input_schema } from the trait alone; (2) ToolOutput carries a canonical-JSON content payload (serde_json::Value, not String) plus an is_error flag, so tool results compose into ecosystem crates that key on canonical JSON; (3) the trait is dyn-compatible — the async invoke returns a boxed future (the InvokeFuture alias) so handlers store as Box<dyn Tool> without per-type monomorphisation, matching the Vec<Box<dyn Tool>> registry shape used by every ecosystem agent-loop crate; (4) the trait bounds Send + Sync so handlers cross thread boundaries the same way ecosystem crates require. The forward-compat smoke test (tool_trait_generates_anthropic_schema_that_round_trips in the same file) exercises a sample Tool impl and verifies the generated Anthropic schema JSON round-trips losslessly through serde_json — judge that the test actually constructs the Anthropic-shaped JSON from the trait's read-side surface (no parallel hand-built struct) and asserts the round-tripped value matches the original, so a future refactor that drops a method or changes a return type trips the test."
}

judge_llm_error_mapping_honesty() {
  judge_files \
    "crates/loom-llm/src/client/mod.rs" \
    "crates/loom-llm/src/client/multi_provider.rs" \
    "crates/loom-llm/src/client/openai_compat.rs"
  judge_criterion \
    "Upstream error → LlmError mapping is exhaustive-and-honest for every Client family in loom-llm. Both surfaces — genai::Error for the three genai-backed Clients (AnthropicClient, OpenAiClient, GeminiClient) and reqwest::Error + parsed HTTP status for OpenAiCompatClient — must classify into a non-Provider LlmError variant whenever the upstream carries enough information to do so. LlmError::Provider { message } is the documented fallback ONLY for explicitly-unclassifiable cases; surfacing it for an upstream that structurally carries timeout / rate-limit / auth / HTTP-status / transport / body / TLS / DNS / decode information is a 'dishonest' mapping and must fail this rubric. (1) genai::Error arm — walk the current variant set of genai::Error (parse genai's source under the workspace Cargo.lock-pinned version, or inspect via 'cargo doc --no-deps --json -p genai'; the upstream is #[non_exhaustive] so the rubric pins the variants as-of-today, not forever) and locate the corresponding arm in the per-schema Client mapping in multi_provider.rs that lowers it into LlmError. For each genai variant whose payload identifies the failure class — timeout → LlmError::Timeout, 429 / rate-limit → LlmError::RateLimited (parsing Retry-After when available, falling back to DEFAULT_RETRY_AFTER), 401 / 403 / auth → LlmError::AuthFailed, other non-success HTTP → LlmError::ProviderHttp { status, body }, transport / DNS / connect / TLS → LlmError::Transport, response JSON parse failure → LlmError::MalformedJson, schema validation failure → LlmError::SchemaViolation — the mapping arm must yield the named LlmError variant. Falling through to LlmError::Provider { message: err.to_string() } when the genai variant structurally carries one of the above classes is a fail; name the unmapped variant in the diagnostic. (2) reqwest::Error arm — walk reqwest::Error::is_timeout, is_connect, is_request, is_body, is_decode, is_redirect, is_builder, plus err.status() in openai_compat.rs's reqwest_error_to_llm. Per specs/llm.md § LlmError classification table: is_timeout → Timeout; is_connect / DNS / TLS / is_request / is_body → Transport; is_decode (response body decode) → MalformedJson; err.status() returning a non-success code → ProviderHttp { status, body } unless the status falls under one of the explicit carve-outs (401/403 → AuthFailed; 429 → RateLimited). The arm must NOT fall through to Provider for any predicate that carries a classifiable shape. (3) Parsed HTTP-status arm — classify_status (or equivalent) must map 2xx success → Ok(()); 401 and 403 → LlmError::AuthFailed { reason: body }; 429 → LlmError::RateLimited { retry_after } with retry_after parsed from the Retry-After header via parse_retry_after, falling back to DEFAULT_RETRY_AFTER when the header is missing / unparseable; other 4xx → LlmError::ProviderHttp { status, body } (retry_advice NonRetryable); 5xx → LlmError::ProviderHttp { status, body } (retry_advice Retryable per the threshold at 500). Falling through to Provider for any of these status classes — when the wire response structurally carries the status code — is a fail. (4) Provider-fallback honesty — search every reachable mapping path (multi_provider.rs's three per-schema impls' complete / complete_structured_raw, openai_compat.rs's reqwest_error_to_llm and classify_status) for any arm that produces LlmError::Provider { message } when the upstream cause is structurally classifiable per the three arms above. Each such arm is a 'dishonest' mapping the rubric must surface — emit the file path and the arm so the diagnostic names the offender. Pass when every classifiable upstream lands on a non-Provider LlmError variant; fail with a one-line diagnostic naming the unmapped variant / arm otherwise."
}

judge_sibling_spec_editing_documents_split() {
  judge_files \
    "crates/loom-templates/templates/partial/sibling_spec_editing.md"
  judge_criterion \
    "partial/sibling_spec_editing.md establishes three things, all in one place: (1) the anchor/sibling editing model — that the -u label owns the session state row and any spec under specs/ may be edited in-place during the interview; (2) the new-sibling-spec carve-out — the planner may decide a section warrants its own spec, in which case it allocates a tracking epic via 'bd create --type=epic' and adds the row to docs/README.md, and this is the SINGLE permitted exception to the otherwise-strict 'no bead creation during planning' rule (implementation beads come later, from loom todo, not here); and (3) the commit-discipline rule — planning sessions edit specs in place but do NOT commit; soft signals like 'looks good' or 'next' or 'accept' authorize the next interview step but never authorize a commit; commits require unambiguous language such as 'commit', 'ship it', 'land the changes', 'land the plane', or 'push it'. The same discipline applies to git push, beads-push, and any shared-state mutation. The partial must name all three: the editing model, the bead-allocation carve-out (with the 'one carve-out' framing so the reader understands why it's an exception), and the commit-discipline rule (with explicit examples of soft signals vs. unambiguous triggers). Vague phrasing like 'be careful with commits' is insufficient — the partial must enumerate concrete trigger phrases."
}
