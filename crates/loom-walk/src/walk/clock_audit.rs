//! Clock-use audit shared by the determinism walks.

use std::collections::HashMap;

use syn::visit::Visit;
use syn::{Attribute, Expr, ExprCall, ImplItemFn, ItemFn, Meta};

use super::util::{
    all_rs_files_including_verifiers, line_of, narrow_to_loom_files, parse_rs, rel, verdict_from,
    workspace_root,
};
use super::{Verdict, WalkInput};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Operation {
    ThreadSleep,
    TokioSleep,
    TokioTimeout,
    RealClockRead,
}

impl Operation {
    const fn label(self) -> &'static str {
        match self {
            Self::ThreadSleep => "std::thread::sleep",
            Self::TokioSleep => "tokio::time::sleep",
            Self::TokioTimeout => "tokio::time::timeout",
            Self::RealClockRead => "Instant::now/SystemTime::now",
        }
    }

    const fn permits_paused_time(self) -> bool {
        matches!(self, Self::TokioSleep | Self::TokioTimeout)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    ProcessLifecycle,
    ElapsedPerformance,
}

struct Permit {
    path: &'static str,
    function: &'static str,
    operation: Operation,
    occurrences: usize,
}

struct Exception {
    path: &'static str,
    function: &'static str,
    operation: Operation,
    occurrences: usize,
    boundary: Boundary,
    justification: &'static str,
    upper_deadline: &'static str,
    cleanup: &'static str,
    deterministic_companion: &'static str,
}

const CLOCK_BOUNDARIES: &[Permit] = &[
    Permit {
        path: "crates/loom-driver/src/clock/system.rs",
        function: "now",
        operation: Operation::RealClockRead,
        occurrences: 1,
    },
    Permit {
        path: "crates/loom-driver/src/clock/system.rs",
        function: "wall_now",
        operation: Operation::RealClockRead,
        occurrences: 1,
    },
    Permit {
        path: "crates/loom-driver/src/clock/system.rs",
        function: "sleep",
        operation: Operation::TokioSleep,
        occurrences: 1,
    },
    Permit {
        path: "crates/loom-driver/src/clock/system.rs",
        function: "timeout",
        operation: Operation::TokioTimeout,
        occurrences: 1,
    },
    Permit {
        path: "crates/loom-driver/src/clock/mock.rs",
        function: "sleep",
        operation: Operation::TokioSleep,
        occurrences: 1,
    },
    Permit {
        path: "crates/loom-render/src/clock.rs",
        function: "now",
        operation: Operation::RealClockRead,
        occurrences: 1,
    },
];

const EXCEPTIONS: &[Exception] = &[
    Exception {
        path: "crates/loom-driver/tests/lock_manager.rs",
        function: "second_acquire_times_out_with_work_root_busy",
        operation: Operation::RealClockRead,
        occurrences: 1,
        boundary: Boundary::ElapsedPerformance,
        justification: "The synchronous kernel-lock timeout is the behavior under test.",
        upper_deadline: "The 250 ms request has a two-second asserted ceiling.",
        cleanup: "LockGuard drop releases both lock handles.",
        deterministic_companion: "acquire_work_root_async_times_out_via_mock_clock",
    },
    Exception {
        path: "crates/loom-driver/tests/lock_manager.rs",
        function: "times_out_with_default_timeout",
        operation: Operation::RealClockRead,
        occurrences: 1,
        boundary: Boundary::ElapsedPerformance,
        justification: "The public synchronous default timeout is the behavior under test.",
        upper_deadline: "The test asserts a seven-second ceiling.",
        cleanup: "LockGuard drop releases the held lock.",
        deterministic_companion: "acquire_work_root_async_times_out_via_mock_clock",
    },
    Exception {
        path: "crates/loom-driver/tests/lock_manager.rs",
        function: "different_work_root_locks_do_not_block",
        operation: Operation::RealClockRead,
        occurrences: 1,
        boundary: Boundary::ElapsedPerformance,
        justification: "The assertion measures that independent kernel locks do not block.",
        upper_deadline: "The test asserts a 250 ms ceiling.",
        cleanup: "LockGuard drop releases both locks.",
        deterministic_companion: "drop_releases_so_reacquire_succeeds",
    },
    Exception {
        path: "crates/loom-driver/tests/lock_manager.rs",
        function: "readonly_paths_unaffected_by_work_root_lock",
        operation: Operation::RealClockRead,
        occurrences: 1,
        boundary: Boundary::ElapsedPerformance,
        justification: "The assertion measures that read-only work stays non-blocking.",
        upper_deadline: "The test asserts a 100 ms ceiling.",
        cleanup: "LockGuard drop releases the held lock.",
        deterministic_companion: "with_state_home_creates_locks_directory_outside_workspace",
    },
    Exception {
        path: "crates/loom-driver/tests/lock_manager.rs",
        function: "crash_releases_work_root_lock",
        operation: Operation::RealClockRead,
        occurrences: 3,
        boundary: Boundary::ProcessLifecycle,
        justification: "Kernel flock release on child death requires a re-executed process.",
        upper_deadline: "The helper has a five-second kill deadline and reacquire has a 250 ms ceiling.",
        cleanup: "Timeout kills and reaps the child; normal completion is also reaped.",
        deterministic_companion: "drop_releases_so_reacquire_succeeds",
    },
    Exception {
        path: "crates/loom-driver/tests/lock_manager.rs",
        function: "crash_releases_work_root_lock",
        operation: Operation::ThreadSleep,
        occurrences: 1,
        boundary: Boundary::ProcessLifecycle,
        justification: "A short poll observes the re-executed lock holder without busy-spinning.",
        upper_deadline: "The child poll has a five-second deadline.",
        cleanup: "Timeout kills and reaps the child; normal completion is also reaped.",
        deterministic_companion: "drop_releases_so_reacquire_succeeds",
    },
    Exception {
        path: "crates/loom-driver/tests/lock_manager.rs",
        function: "second_thread_unblocks_when_holder_drops",
        operation: Operation::RealClockRead,
        occurrences: 1,
        boundary: Boundary::ElapsedPerformance,
        justification: "Kernel-lock handoff latency is the behavior under test.",
        upper_deadline: "The channel has a five-second deadline and handoff an 800 ms ceiling.",
        cleanup: "The waiter thread is joined after the lock is released.",
        deterministic_companion: "acquire_work_root_async_times_out_via_mock_clock",
    },
    Exception {
        path: "crates/loom-driver/tests/lock_manager.rs",
        function: "second_thread_unblocks_when_holder_drops",
        operation: Operation::ThreadSleep,
        occurrences: 1,
        boundary: Boundary::ElapsedPerformance,
        justification: "The real thread must enter kernel-lock contention before handoff.",
        upper_deadline: "The channel has a five-second deadline.",
        cleanup: "The waiter thread is joined after the lock is released.",
        deterministic_companion: "acquire_work_root_async_times_out_via_mock_clock",
    },
    Exception {
        path: "crates/loom-driver/tests/git_client.rs",
        function: "rebase_onto_integration_retries_through_index_lock_contention",
        operation: Operation::TokioSleep,
        occurrences: 1,
        boundary: Boundary::ProcessLifecycle,
        justification: "A real git process must observe an index lock disappear between retries.",
        upper_deadline: "Git commands have a 60-second timeout and lock retries a two-second budget.",
        cleanup: "The releaser task is awaited and every git child is reaped by the typed runner.",
        deterministic_companion: "MockClock sleep and timeout tests",
    },
    Exception {
        path: "crates/loom-driver/tests/git_client.rs",
        function: "ff_merge_integration_retries_through_index_lock_contention",
        operation: Operation::TokioSleep,
        occurrences: 1,
        boundary: Boundary::ProcessLifecycle,
        justification: "A real git process must observe an index lock disappear between retries.",
        upper_deadline: "Git commands have a 60-second timeout and lock retries a two-second budget.",
        cleanup: "The releaser task is awaited and every git child is reaped by the typed runner.",
        deterministic_companion: "MockClock sleep and timeout tests",
    },
    Exception {
        path: "crates/loom-gate/tests/cache.rs",
        function: "render_under_500ms_on_2000_row_corpus",
        operation: Operation::RealClockRead,
        occurrences: 1,
        boundary: Boundary::ElapsedPerformance,
        justification: "Actual report latency is the hard behavior under test.",
        upper_deadline: "The test asserts the 500 ms contract ceiling.",
        cleanup: "No child process or background task is created.",
        deterministic_companion: "cache report shape assertions in the same test",
    },
    Exception {
        path: "crates/loom-gate/tests/cache.rs",
        function: "render_from_rows_under_500ms_on_2000_row_corpus",
        operation: Operation::RealClockRead,
        occurrences: 1,
        boundary: Boundary::ElapsedPerformance,
        justification: "Actual pure-render latency is the hard behavior under test.",
        upper_deadline: "The test asserts the 500 ms contract ceiling.",
        cleanup: "No child process or background task is created.",
        deterministic_companion: "cache report shape tests",
    },
    Exception {
        path: "crates/loom/tests/spawn_dispatch.rs",
        function: "bounded_output",
        operation: Operation::RealClockRead,
        occurrences: 1,
        boundary: Boundary::ProcessLifecycle,
        justification: "The helper enforces host deadlines around lifecycle fixtures.",
        upper_deadline: "Each caller supplies a finite deadline.",
        cleanup: "Timeout kills the process group, reaps the child, and joins pipe readers.",
        deterministic_companion: "MockClock backend timeout tests",
    },
    Exception {
        path: "crates/loom/tests/spawn_dispatch.rs",
        function: "bounded_output",
        operation: Operation::ThreadSleep,
        occurrences: 1,
        boundary: Boundary::ProcessLifecycle,
        justification: "The helper polls child exit without busy-spinning.",
        upper_deadline: "Each caller supplies a finite deadline.",
        cleanup: "Timeout kills the process group, reaps the child, and joins pipe readers.",
        deterministic_companion: "MockClock backend timeout tests",
    },
    Exception {
        path: "crates/loom/tests/spawn_dispatch.rs",
        function: "loom_todo_claude_runs_shutdown_watchdog_through_run_agent",
        operation: Operation::RealClockRead,
        occurrences: 1,
        boundary: Boundary::ElapsedPerformance,
        justification: "Elapsed escalation proves the production watchdog is wired.",
        upper_deadline: "bounded_output enforces a ten-second deadline.",
        cleanup: "bounded_output reaps the process tree on every path.",
        deterministic_companion: "wait_with_timeout_returns_none_via_mock_clock",
    },
    Exception {
        path: "crates/loom/tests/spawn_dispatch.rs",
        function: "loom_todo_pi_hang_probe_surfaces_handshake_timeout",
        operation: Operation::RealClockRead,
        occurrences: 1,
        boundary: Boundary::ProcessLifecycle,
        justification: "The assembled handshake timeout needs a pending child pipe.",
        upper_deadline: "bounded_output enforces a ten-second deadline.",
        cleanup: "bounded_output reaps the process tree on every path.",
        deterministic_companion: "MockClock timeout_fires_when_future_does_not_complete",
    },
    Exception {
        path: "crates/loom/tests/spawn_dispatch.rs",
        function: "loom_todo_pi_stall_mid_session_emits_stall_warning",
        operation: Operation::RealClockRead,
        occurrences: 2,
        boundary: Boundary::ProcessLifecycle,
        justification: "The assembled stall warning needs a silent live child pipe.",
        upper_deadline: "The polling loop has a ten-second deadline.",
        cleanup: "The process group is killed, the child reaped, and the reader joined.",
        deterministic_companion: "stall_window_fires_warning_after_five_minutes_without_aborting",
    },
    Exception {
        path: "crates/loom/tests/spawn_dispatch.rs",
        function: "loom_todo_pi_stall_mid_session_emits_stall_warning",
        operation: Operation::ThreadSleep,
        occurrences: 1,
        boundary: Boundary::ProcessLifecycle,
        justification: "A short host poll observes warning output from a live child.",
        upper_deadline: "The polling loop has a ten-second deadline.",
        cleanup: "The process group is killed, the child reaped, and the reader joined.",
        deterministic_companion: "stall_window_fires_warning_after_five_minutes_without_aborting",
    },
];

struct CallSite {
    function: String,
    operation: Operation,
    line: usize,
    paused_time: bool,
}

#[derive(Default)]
struct CallVisitor {
    function: Option<String>,
    paused_time: bool,
    sites: Vec<CallSite>,
}

impl CallVisitor {
    fn enter_function(&mut self, name: String, attrs: &[Attribute], visit: impl FnOnce(&mut Self)) {
        let previous_function = self.function.replace(name);
        let previous_paused = self.paused_time;
        self.paused_time = has_paused_tokio_time(attrs);
        visit(self);
        self.function = previous_function;
        self.paused_time = previous_paused;
    }
}

impl<'ast> Visit<'ast> for CallVisitor {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.enter_function(node.sig.ident.to_string(), &node.attrs, |visitor| {
            syn::visit::visit_item_fn(visitor, node);
        });
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.enter_function(node.sig.ident.to_string(), &node.attrs, |visitor| {
            syn::visit::visit_impl_item_fn(visitor, node);
        });
    }

    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Some(operation) = operation_for_call(node) {
            self.sites.push(CallSite {
                function: self
                    .function
                    .clone()
                    .unwrap_or_else(|| "<outside-function>".to_string()),
                operation,
                line: line_of(node),
                paused_time: self.paused_time,
            });
        }
        syn::visit::visit_expr_call(self, node);
    }
}

pub(super) fn run(input: &WalkInput, operation: Operation, rule: &str) -> Verdict {
    let root = workspace_root();
    let scope = narrow_to_loom_files(all_rs_files_including_verifiers(&root), input, &root);
    let mut boundary_hits = HashMap::<usize, usize>::new();
    let mut exception_hits = HashMap::<usize, usize>::new();
    let mut violations = Vec::new();

    for path in scope {
        let rel_path = rel(&root, &path);
        let Some(file) = parse_rs(&path) else {
            violations.push(format!(
                "{rel_path}:1 clock audit could not parse Rust source"
            ));
            continue;
        };
        let mut visitor = CallVisitor::default();
        visitor.visit_file(&file);
        for site in visitor
            .sites
            .into_iter()
            .filter(|site| site.operation == operation)
        {
            if operation.permits_paused_time() && site.paused_time {
                continue;
            }
            if let Some((index, permit)) = matching_permit(&rel_path, &site, CLOCK_BOUNDARIES) {
                let hits = boundary_hits.entry(index).or_default();
                *hits += 1;
                if *hits <= permit.occurrences {
                    continue;
                }
            }
            if let Some((index, exception)) = matching_exception(&rel_path, &site) {
                if !exception.is_auditable() {
                    violations.push(format!(
                        "{rel_path}:{} incomplete real-time exception metadata for `{}`",
                        site.line, site.function,
                    ));
                    continue;
                }
                let hits = exception_hits.entry(index).or_default();
                *hits += 1;
                if *hits <= exception.occurrences {
                    continue;
                }
            }
            violations.push(format!(
                "{rel_path}:{} `{}` in `{}` is not an exact audited clock boundary or bounded test exception",
                site.line,
                operation.label(),
                site.function,
            ));
        }
    }

    if input.files.is_none() {
        audit_expected_counts(operation, CLOCK_BOUNDARIES, &boundary_hits, &mut violations);
        audit_exception_counts(operation, &exception_hits, &mut violations);
    }

    verdict_from(rule, violations)
}

impl Exception {
    fn is_auditable(&self) -> bool {
        self.occurrences > 0
            && !self.justification.is_empty()
            && !self.upper_deadline.is_empty()
            && !self.cleanup.is_empty()
            && !self.deterministic_companion.is_empty()
            && matches!(
                self.boundary,
                Boundary::ProcessLifecycle | Boundary::ElapsedPerformance
            )
    }
}

fn matching_permit<'a>(
    rel_path: &str,
    site: &CallSite,
    permits: &'a [Permit],
) -> Option<(usize, &'a Permit)> {
    permits.iter().enumerate().find(|(_, permit)| {
        permit.path == rel_path
            && permit.function == site.function
            && permit.operation == site.operation
    })
}

fn matching_exception(rel_path: &str, site: &CallSite) -> Option<(usize, &'static Exception)> {
    EXCEPTIONS.iter().enumerate().find(|(_, exception)| {
        exception.path == rel_path
            && exception.function == site.function
            && exception.operation == site.operation
    })
}

fn audit_expected_counts(
    operation: Operation,
    permits: &[Permit],
    hits: &HashMap<usize, usize>,
    violations: &mut Vec<String>,
) {
    for (index, permit) in permits.iter().enumerate() {
        if permit.operation != operation {
            continue;
        }
        let actual = hits.get(&index).copied().unwrap_or(0);
        if actual != permit.occurrences {
            violations.push(format!(
                "{} clock boundary `{}` has {actual} `{}` call sites; expected {}",
                permit.path,
                permit.function,
                operation.label(),
                permit.occurrences,
            ));
        }
    }
}

fn audit_exception_counts(
    operation: Operation,
    hits: &HashMap<usize, usize>,
    violations: &mut Vec<String>,
) {
    for (index, exception) in EXCEPTIONS.iter().enumerate() {
        if exception.operation != operation {
            continue;
        }
        let actual = hits.get(&index).copied().unwrap_or(0);
        if actual != exception.occurrences {
            violations.push(format!(
                "{} audited exception `{}` has {actual} `{}` call sites; expected {}",
                exception.path,
                exception.function,
                operation.label(),
                exception.occurrences,
            ));
        }
    }
}

fn operation_for_call(call: &ExprCall) -> Option<Operation> {
    let Expr::Path(function) = call.func.as_ref() else {
        return None;
    };
    let segments: Vec<String> = function
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();

    if ends_with(&segments, &["thread", "sleep"]) {
        return Some(Operation::ThreadSleep);
    }
    if ends_with(&segments, &["tokio", "time", "sleep"]) {
        return Some(Operation::TokioSleep);
    }
    if ends_with(&segments, &["tokio", "time", "timeout"]) {
        return Some(Operation::TokioTimeout);
    }
    let real_clock =
        ends_with(&segments, &["Instant", "now"]) || ends_with(&segments, &["SystemTime", "now"]);
    if real_clock && segments.first().map(String::as_str) != Some("tokio") {
        return Some(Operation::RealClockRead);
    }
    None
}

fn ends_with(actual: &[String], expected: &[&str]) -> bool {
    actual.len() >= expected.len()
        && actual
            .iter()
            .rev()
            .zip(expected.iter().rev())
            .all(|(actual, expected)| actual == expected)
}

fn has_paused_tokio_time(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let segments: Vec<String> = attr
            .path()
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        if segments.len() != 2 || segments[0] != "tokio" || segments[1] != "test" {
            return false;
        }
        let Meta::List(list) = &attr.meta else {
            return false;
        };
        list.tokens
            .to_string()
            .replace(' ', "")
            .contains("start_paused=true")
    })
}
