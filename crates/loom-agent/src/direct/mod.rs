//! Direct backend — composes `loom-llm::Conversation` with Loom's six
//! sandbox-aware tools, running inside a per-bead container.
//!
//! Two submodules split the surface the wider codebase consumes from the
//! tool implementations the runner registers:
//!
//! - [`backend`] — the [`DirectBackend`] zero-sized type plus its
//!   [`AgentBackend`](loom_driver::agent::AgentBackend) impl. Spawns a
//!   container via `wrapix spawn` whose entrypoint exec's
//!   `loom-direct-runner` over JSONL on stdin/stdout.
//! - [`tools`] — net-new sandbox-aware tool implementations the runner
//!   registers with the in-process `Conversation`. See `specs/agent.md`
//!   § Direct Backend.

pub mod backend;
pub mod tools;

pub use backend::DirectBackend;
