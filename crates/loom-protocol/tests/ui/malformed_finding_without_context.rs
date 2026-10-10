use loom_protocol::gate::{BadWalk, TerminalSurface};

fn main() {
    let _ = BadWalk::MalformedFinding {
        errors: Vec::new(),
        terminal: TerminalSurface::Complete,
        parsed_findings: Vec::new(),
    };
}
