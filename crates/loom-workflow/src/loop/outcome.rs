use loom_driver::agent::SessionOutcome;

/// Result of one agent invocation against a bead. The driver translates
/// session-level signals (JSONL `result/success`, non-zero process exit,
/// `LOOM_BLOCKED` / `LOOM_CLARIFY` markers) into one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentOutcome {
    /// Agent finished cleanly (`LOOM_COMPLETE` or `LOOM_NOOP`, exit 0).
    Success,

    /// Agent exited with a non-zero `SessionComplete` code or surfaced a
    /// recoverable failure body. The string carries the body the driver
    /// should inject into the next retry's prompt as `previous_failure`.
    Failure { error: String },

    /// Terminal block: the bead is parked under `loom:blocked` and the
    /// loop continues with the next ready bead. Two paths reach this:
    /// the agent self-reports `LOOM_BLOCKED`, or the driver detects a
    /// post-session condition that retry cannot recover (merge conflict
    /// against the driver branch, post-merge push failure). In both
    /// cases re-running the agent is the wrong response; the human
    /// resolves via `loom msg` (agent-side) or by inspecting the
    /// preserved worktree (driver-side).
    Blocked { reason: String },

    /// Agent emitted `LOOM_CLARIFY` — self-reported it needs a human answer.
    /// Routes straight to `loom:clarify` without retry.
    Clarify { question: String },

    /// Agent emitted `LOOM_RETRY` — self-reported that this attempt cannot
    /// finish but a fresh dispatch is likely to succeed (environmental or
    /// agent self-reset per `specs/harness.md` § Marker definitions).
    /// Consumes one `[loop] max_retries` slot via the same counter as
    /// `Failure`; the verbatim `reason` rides through to the next attempt's
    /// `PreviousFailure::AgentRetry { reason }`. Exhaustion routes to
    /// `loom:blocked` cause `retry-exhausted` (distinct from the
    /// `Failure`-exhaustion path which routes to `loom:clarify`).
    Retry { reason: String },

    /// Pre-flight infra failure (image load, container start) — `B::spawn`
    /// returned an error before the agent process produced any output.
    /// Bypasses retry and routes straight to `loom:blocked` per
    /// `specs/harness.md` § "Verdict Gate · Infra failures bypass the
    /// gate".
    InfraPreflight { error: String },

    /// Mid-session infra failure (agent process exit non-zero, container
    /// OOM, IO errors). Eligible for one driver-memory retry per `loom loop`
    /// invocation. A second mid-session failure inside the same
    /// `run_loop` invocation routes to `loom:blocked`.
    InfraMidSession { error: String },

    /// The bead's requested `profile:X` label (or the CLI `--profile`
    /// override) is not declared in the profile-image manifest. Routes
    /// straight to `loom:blocked` cause `unknown-profile` — no retry, and
    /// the loop continues with the next ready bead so a stray label on one
    /// bead does not stall the molecule. `error` carries the requested
    /// profile name and the manifest's declared set so the operator can
    /// relabel from the bead's notes.
    UnknownProfile { error: String },
}

/// Final state of one bead after retries have been exhausted (or the agent
/// succeeded on first try). Drives the bd-side cleanup: success → driver
/// observes the agent's own `bd close` (no driver-side close), clarified →
/// `bd update --add-label loom:clarify --notes <question>`, blocked →
/// `bd update --add-label loom:blocked --notes <cause>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeadResult {
    /// Bead succeeded. The driver does **not** call `bd close` — closure is
    /// the agent's responsibility per `specs/harness.md`'s verdict-gate
    /// table, where `bd-closed` is an observable rather than a driver
    /// action. If the agent forgot to close on `LOOM_COMPLETE`, the next
    /// `loom review` invocation routes that to `incomplete-signaling`
    /// recovery.
    Done,

    /// Agent self-reported `LOOM_CLARIFY` or retries exhausted — caller
    /// flags the bead with `loom:clarify` and writes `note` as
    /// `bd update --notes`. For self-reports `note` is the question; for
    /// retry-exhaustion it is the last failure body.
    Clarified { note: String },

    /// Routed to `loom:blocked`. `cause` is the stable identifier
    /// (`infra-preflight`, `infra-repeated`, `agent-blocked`) the driver
    /// writes into `bd update --notes`; `error` carries the raw failure
    /// body or agent reason for human triage.
    Blocked { cause: String, error: String },
}

/// Output of one classified agent dispatch. The run-loop closure produces
/// this so [`super::runner::process_one_bead`] can route preflight vs
/// mid-session failures to the right verdict-gate path.
#[derive(Debug, Clone)]
pub enum SessionResult {
    /// `B::spawn` succeeded and the session reached `SessionComplete`.
    /// `exit_code` may still be non-zero (the agent decided to fail) — the
    /// caller distinguishes that from infra failures via the variant.
    Complete(SessionOutcome),

    /// `B::spawn` itself failed (image load, container start). No agent
    /// output exists; routes to `loom:blocked` cause `infra-preflight`.
    PreflightFailed { error: String },

    /// Spawn succeeded but the session terminated before
    /// `SessionComplete` — process EOF, IO error, OOM kill, etc. Eligible
    /// for one driver-memory retry per `loom loop`.
    MidSessionFailed { error: String },

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
