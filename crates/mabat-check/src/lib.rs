//! Checking Mabat views against a database schema without a database.
//!
//! `mabat schema` writes a [`Snapshot`] of a database's schema, `mabat/schema.json`, to commit
//! next to the manifest of the views, `mabat/views.json`. This crate reads both with no database
//! driver, so that a build script and the `mabat` command line tool can check the views against
//! the schema without a database, and compares a snapshot with the schema it was taken from, so
//! that CI can tell when it is out of date.
//!
//! ```no_run
//! // In the `main` of build.rs, with `mabat-check` in [build-dependencies]: `cargo build` fails
//! // when the views no longer match the schema
//! mabat_check::build("mabat/views.json", "mabat/schema.json").overrides("mabat/overrides").run();
//! ```

pub mod build;
pub mod check;
pub mod manifest;
pub mod overrides;
pub mod report;
pub mod schema;

pub use build::{Build, Outcome, build, directives};
pub use manifest::Manifest;
pub use report::{Diagnostic, Report, Severity};
pub use schema::{Column, ForeignKey, Snapshot, Table};
