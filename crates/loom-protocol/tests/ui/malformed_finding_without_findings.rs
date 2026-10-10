use loom_protocol::{
    gate::{BadWalk, TerminalSurface},
    output::{Phase, decode},
};

fn main() {
    let context = Box::new(
        decode("LOOM_COMPLETE", Phase::Review)
            .unwrap()
            .into_context(),
    );
    let _ = BadWalk::MalformedFinding {
        errors: Vec::new(),
        terminal: TerminalSurface::Complete,
        context,
    };
}
