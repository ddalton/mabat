//! Error messages of `#[derive(View)]`.
//!
//! The messages list the SQLx types of every compiled driver, so the snapshots are of the
//! workspace's features, all three databases, as `cargo test --workspace` builds them.
#![cfg(all(feature = "mysql", feature = "postgres", feature = "sqlite"))]

#[test]
fn derive_errors() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
