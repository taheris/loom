use loom_protocol::gate::{TerminalSurface, WalkOutput};

fn main() {
    let _walk = WalkOutput {
        terminal: TerminalSurface::Complete,
        findings: Vec::new(),
        finding_errors: Vec::new(),
    };
}
