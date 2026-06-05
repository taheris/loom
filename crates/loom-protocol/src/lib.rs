//! Typed wire-format contracts loom emits or consumes.
//!
//! `loom-protocol` is single-purpose: cross-crate wire protocols loom
//! produces on stdout or accepts from external consumers. It carries
//! the [`gate`] module (the findings / concern surface defined in
//! `specs/gate.md`) and the [`oid`] module ([`oid::GitOid`], the
//! validated git-object-id newtype shared by `loom-driver`,
//! `loom-gate`, and the typed retry-context surface in
//! `loom-templates`). Future protocols (agent stream-json, pi-mono
//! RPC, run-phase exit markers) may land as sibling modules without
//! re-litigating crate-extraction overhead.
//!
//! # Public contract
//!
//! `loom-protocol` is a public-contract leaf crate. The crate's MAJOR
//! version is the wire-format protocol version: a breaking wire change
//! (renamed token, retyped target shape, removed enum variant) requires
//! a major bump; additive changes (new `ConcernToken` variant, new
//! `FindingTarget` variant, new fields with `#[serde(default)]`) are
//! minor bumps. No `"protocol": <n>` field appears on the wire — the
//! typed parse errors give loud structural breakage on version skew.
//!
//! External Rust consumers (e.g. wrix) depend on this crate directly
//! to call [`gate::parse_walk_output`] against captured agent stdout.
//! Compile-time type safety + the leaf-crate dependency shape gives
//! consumers the same guarantees loom's own internal pipeline has.

pub mod gate;
pub mod oid;
