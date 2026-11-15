//! Public CLI seams share one linked executable, not one copy per source file.
//! Nextest retains process-per-case isolation and executes every module below.

#[path = "common/cargo.rs"]
mod cargo;
#[path = "common/checked.rs"]
#[expect(clippy::unwrap_used, reason = "Fixture setup fails by panicking.")]
mod checked;
#[path = "common/reported.rs"]
mod reported;

mod agent_flag;
mod bd_shim;
mod bootstrap;
mod envelope_stamp;
mod external_membership;
mod gate_checked_loading;
mod gate_commands_surface;
mod gate_inputs;
mod gate_inspection_output;
mod gate_integrity;
mod gate_reported_loading;
mod gate_sandbox_policy;
mod gate_skip_evidence;
mod gate_system_sharing;
mod gate_test_scope;
mod inbox_chat;
mod inbox_persist;
mod marker_cli;
mod marker_gate;
mod nested_loom_guard;
mod pending_readiness;
mod plan_cli;
mod platform_scope;
mod pre_push_config;
mod push_gate;
mod render_flags;
mod run_gate_mint_walker_wiring;
mod run_smoke;
mod smoke_result;
mod smoke_startup;
mod spec_cli;
#[path = "workspace_safety_history.rs"]
mod workspace_safety_history;
