//! The actual compiler fixture, not a caller-created catalogue.
#[path = "native_data.rs"]
mod native_data;

pub fn fixture() -> tempfile::TempDir {
    native_data::fixture("LOOM_NATIVE_RUST_FIXTURE", "native-rust-fixture")
}
