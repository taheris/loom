use std::path::PathBuf;

use loom_driver::agent::SessionOutcome;
use loom_driver::git::GitOid;

/// Result of one agent invocation against a bead. The driver translates
/// session-level signals (JSONL `result/success`, non-zero process exit,
/// `LOOM_BLOCKED` / `LOOM_CLARIFY` markers) into one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentOutcome {
    /// Agent finished cleanly with reviewable work (`LOOM_COMPLETE`, exit 0).
    Success,

    /// Agent finished cleanly with an intentional empty-diff no-op.
    Noop,

    /// Agent exited with a non-zero `SessionComplete` code or surfaced a
    /// recoverable failure body. The string carries the body the driver
    /// should inject into the next retry's prompt as `previous_failure`.
    Failure { error: String },

    /// Terminal block: the bead is parked under `loom:blocked` and the
    /// loop continues with the next ready bead. Two paths reach this:
    /// the agent self-reports `LOOM_BLOCKED`, or the driver detects a
    /// post-session condition that retry cannot recover. In both cases
    /// re-running the agent is the wrong response; the human resolves via
    /// `loom inbox` or by inspecting the preserved worktree.
    Blocked { reason: String },

    /// The agent emitted success, but the fetched bead branch did not
    /// advance the integration line. This usually means the work happened
    /// outside the mounted bead clone or was never committed.
    ZeroProgress { detail: String },

    /// Agent emitted `LOOM_CLARIFY` — self-reported it needs a human answer.
    /// Routes straight to `loom:clarify` without retry.
    Clarify { question: String },

    /// Agent emitted `LOOM_RETRY` — self-reported that this attempt cannot
    /// finish but a fresh dispatch is likely to succeed (environmental or
    /// agent self-reset per `specs/harness.md` § Marker definitions).
    /// Consumes one `[loop] max_retries` slot via the same counter as
    /// `Failure`; the verbatim `reason` rides through to the next attempt's
    /// `PreviousFailure::AgentRetry { reason }`. Exhaustion routes to
    /// `loom:blocked` cause `retry-exhausted`, matching other
    /// driver-detected retry exhaustion.
    Retry { reason: String },

    /// The driver-side rebase of the bead branch onto the integration
    /// branch hit a textual conflict `git rerere` could not replay; the
    /// rebase was aborted and the loom workspace returned to its
    /// pre-rebase state. `files` are the unmerged paths and
    /// `new_base_sha` the integration tip the rebase targeted. Routed by
    /// [`super::runner::process_one_bead`] through a **single**
    /// integration-conflict retry (distinct from `[loop] max_retries`):
    /// the agent's next attempt rebases its bead-workspace branch onto
    /// `new_base_sha`, resolves, and re-commits. A second conflict
    /// escalates to `loom:clarify` with a synthesized Options block. Per
    /// `specs/harness.md` § Verdict Gate.
    IntegrationConflict {
        files: Vec<PathBuf>,
        new_base_sha: GitOid,
    },

    /// `git verify-commit` rejected a fetched (pass 1, worker-side) or
    /// rebased (pass 2, driver-side) commit during the per-bead
    /// integration step. Routes straight to `loom:blocked` with cause
    /// `signature-verification-failed` — agent-retry cannot re-sign
    /// existing commits, so this is operator-investigation territory
    /// (wrix container signing setup for pass 1; loom-workspace
    /// gitconfig + key resolution for pass 2). `detail` names the side
    /// and the offending commit. Per `specs/harness.md` § Verdict Gate.
    SignatureVerificationFailed { detail: String },

    /// Pre-stream infra failure: spawn/setup failed, or the stream ended with
    /// EOF/framing/IO failure before any canonical `source = agent` event was
    /// observed.
    InfraPreflight { error: String },

    /// Interrupted infra failure: EOF/framing/IO/process failure after at
    /// least one canonical `source = agent` event but before
    /// `session_complete`.
    InfraMidSession { error: String },

    /// Static infrastructure diagnostic that cannot be repaired by
    /// re-running the same transport attempt. Routes straight to
    /// `loom:infra` with the supplied stable `cause`.
    StaticInfra { cause: String, error: String },

    /// The bead's requested `profile:X` label (or the CLI `--profile`
    /// override) is not declared in the profile-image manifest. Routes
    /// straight to `loom:infra` cause `unknown-profile` — no retry, and
    /// the loop continues with the next ready bead so a stray label on one
    /// bead does not stall the molecule. `error` carries the requested
    /// profile name and the manifest's declared set so the operator can
    /// relabel from the bead's notes.
    UnknownProfile { error: String },

    /// The selected agent runtime is missing under an existing profile in
    /// the profile-image manifest. Routes straight to `loom:infra` cause
    /// `unknown-agent-runtime-for-profile` — no retry.
    UnknownRuntimeForProfile { error: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfraDiagnostic {
    pub cause: String,
    pub infra_class: String,
    pub error: String,
    pub attempt: Option<u32>,
    pub max_attempts: Option<u32>,
    pub first_event_seen: Option<bool>,
}

impl InfraDiagnostic {
    pub fn retryable(
        cause: &str,
        infra_class: &str,
        error: String,
        attempt: u32,
        max_attempts: u32,
        first_event_seen: bool,
    ) -> Self {
        Self {
            cause: cause.to_string(),
            infra_class: infra_class.to_string(),
            error,
            attempt: Some(attempt),
            max_attempts: Some(max_attempts),
            first_event_seen: Some(first_event_seen),
        }
    }

    pub fn static_diagnostic(cause: &str, error: String) -> Self {
        Self {
            cause: cause.to_string(),
            infra_class: "static".to_string(),
            error,
            attempt: None,
            max_attempts: None,
            first_event_seen: Some(false),
        }
    }
}

/// Final state of one bead after retries have been exhausted (or the agent
/// succeeded on first try). Drives the bd-side cleanup: success → driver
/// observes the agent's own `bd close` (no driver-side close), clarified →
/// `bd update --add-label loom:clarify --notes <question>`, blocked →
/// `bd update --add-label loom:blocked --notes <cause>`, infra →
/// `bd update --add-label loom:infra --status blocked --notes <cause>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeadResult {
    /// Bead succeeded with integration work ready for the molecule-level
    /// handoff. The driver does **not** call `bd close` — closure is the
    /// agent's responsibility per `specs/harness.md`'s verdict-gate
    /// table, where `bd-closed` is an observable rather than a driver
    /// action. If the agent forgot to close on `LOOM_COMPLETE`, the next
    /// `loom review` invocation routes that to `incomplete-signaling`
    /// recovery.
    Done,

    /// Agent intentionally contributed no integration commit (`LOOM_NOOP`).
    /// The bead is terminal for this pass, but there is no new molecule diff
    /// for the durable push gate to review.
    Noop,

    /// Agent self-reported `LOOM_CLARIFY` or retries exhausted — caller
    /// flags the bead with `loom:clarify` and writes `note` as
    /// `bd update --notes`. For self-reports `note` is the question; for
    /// retry-exhaustion it is the last failure body.
    Clarified { note: String },

    /// Routed to `loom:blocked`. `cause` is the stable identifier the
    /// driver writes into `bd update --notes`; `error` carries the raw
    /// failure body or agent reason for human triage.
    Blocked { cause: String, error: String },

    /// Routed to `loom:infra`, never `loom:blocked`.
    Infra { diagnostic: InfraDiagnostic },
}

/// Output of one classified agent dispatch. The run-loop closure produces
/// this so [`super::runner::process_one_bead`] can route pre-stream vs
/// interrupted infra failures to the right verdict-gate path.
#[derive(Debug, Clone)]
pub enum SessionResult {
    /// `B::spawn` succeeded and the session reached `SessionComplete`.
    /// `exit_code` may still be non-zero (the agent decided to fail) — the
    /// caller distinguishes that from infra failures via the variant.
    Complete(SessionOutcome),

    /// No canonical `source = agent` event was observed before the infra
    /// failure: spawn/setup, prompt write, pre-stream EOF, framing, process,
    /// or sink failure.
    PreflightFailed { error: String },

    /// At least one canonical `source = agent` event was observed before the
    /// infra failure, but the session did not reach `SessionComplete`.
    MidSessionFailed { error: String },

    /// Static infrastructure diagnostic detected before the worker reached a
    /// semantic marker. The loop parks the bead as `loom:infra` without
    /// consuming the retryable transport budget.
    StaticInfra { cause: String, error: String },

    /// An `EventSink::react()` returned `SessionCommand::Abort` and the
    /// driver cancelled the session. Per `specs/harness.md`
    /// §"Disambiguating no marker" this is classified as
    /// `RecoveryCause::ObserverAbort`, not `swallowed-marker`, so the
    /// human triage surface names it as a driver-detected failure rather
    /// than agent sloppiness. `reason` is the verbatim payload the
    /// observer emitted; observers that want to identify themselves
    /// prefix their name into the reason.
    ObserverAbort { reason: String },
}
