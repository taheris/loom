//! Compile and render the actual partials independently of their callers.

use anyhow::Result;
use askama::Template;
use loom_protocol::output::Phase;

#[derive(Template)]
#[template(path = "partial/progress_markers.md", escape = "none")]
struct Progress {
    phase: Phase,
}

impl Progress {
    const fn output_phase(&self) -> Phase {
        self.phase
    }
}

#[derive(Template)]
#[template(path = "partial/self_report_markers.md", escape = "none")]
struct Direct {
    phase: Phase,
}

impl Direct {
    const fn output_phase(&self) -> Phase {
        self.phase
    }
}

#[derive(Template)]
#[template(path = "partial/review_self_report_markers.md", escape = "none")]
struct Review;

#[test]
fn progress_partial_does_not_restate_todo_or_review_payloads() -> Result<()> {
    for phase in [
        Phase::Plan,
        Phase::Todo,
        Phase::Loop,
        Phase::Review,
        Phase::Inbox,
    ] {
        let rendered = Progress { phase }.render()?;
        for forbidden in ["LOOM_TODO:", "LOOM_CONCERN:", "LOOM_FINDING:"] {
            assert!(!rendered.contains(forbidden), "{phase:?}: {forbidden}");
        }
        assert!(rendered.contains("LOOM_COMPLETE"));
    }
    Ok(())
}

#[test]
fn direct_partial_renders_worker_scope_and_distinct_retry_decision_blocked_roles() -> Result<()> {
    for phase in [Phase::Loop, Phase::Todo] {
        let rendered = Direct { phase }.render()?;
        let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
        for instruction in [
            "Loop and Todo report all currently discovered unresolved human decisions",
            "LOOM_RETRY:",
            "environmental failure or agent self-reset",
            "LOOM_CLARIFY:",
            "candidate resolutions cannot be safely enumerated",
            "why options cannot be safely framed",
            "Review is inspection-only",
            "Interactive Plan/Inbox resolve questions with the human in-turn",
            "Never use a Clarify record as a terminal or combine two terminals",
        ] {
            assert!(
                normalized.contains(instruction),
                "{phase:?}: missing {instruction}"
            );
        }
        assert!(!rendered.contains("LOOM_CONCERN:"));
        assert!(!rendered.contains("LOOM_FINDING:"));
    }
    Ok(())
}

#[test]
fn review_partial_renders_inspection_only_discriminator_without_finding_payloads() -> Result<()> {
    let rendered = Review.render()?;
    let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
    for instruction in [
        "Review is inspection-only",
        "Do not mutate `bd` state",
        "fresh dispatch is likely to succeed after an environmental failure",
        "clarify-route finding",
        "not direct decision-bead writes or Clarify records",
        "no safe options can be enumerated",
        "LOOM_RETRY:",
        "LOOM_BLOCKED:",
    ] {
        assert!(normalized.contains(instruction), "missing {instruction}");
    }
    assert!(!rendered.contains("LOOM_CONCERN:"));
    assert!(!rendered.contains("LOOM_FINDING:"));
    Ok(())
}
