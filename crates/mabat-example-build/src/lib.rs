//! An application whose views are checked against its database schema by its build script,
//! with no database: see `build.rs`.
//!
//! - `mabat/views.json`, the manifest of the views, is written by `tests/files.rs`.
//! - `mabat/schema.json`, the snapshot of the schema, is written by `mabat schema` (here, from
//!   `mabat/schema.sql` by `tests/files.rs`, to keep the example self-contained).
//! - `build.rs` checks the one against the other, so a view that reads a column the schema does
//!   not have fails `cargo build`.

use mabat::{Mabat, View};

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "project")]
pub struct ProjectView {
    #[view(generated)]
    pub id: Option<i64>,
    pub name: String,
    pub description: Option<String>,
    #[view(child(fk = "project_id", order_by = "id"))]
    pub tasks: Vec<TaskView>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "task")]
pub struct TaskView {
    pub id: i64,
    pub title: String,
    pub done: bool,
}

/// The registry of the application's views.
pub fn views() -> mabat::Builder<sqlx::Sqlite> {
    Mabat::builder().register::<ProjectView>().overrides_dir("mabat/overrides")
}
