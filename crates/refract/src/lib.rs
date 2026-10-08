//! Typed aggregate reads for Rust, with SQL you can tune without changing code.
//!
//! Refract loads nested, typed data from PostgreSQL. The shape of the result is declared
//! with Rust structs deriving [`View`], and Refract plans and runs the queries that fill it:
//! one query for the root rows, plus one batched query per to-many collection and per
//! to-one reference, so loading never runs one query per row.
//!
//! ```ignore
//! use refract::View;
//! use uuid::Uuid;
//!
//! #[derive(View)]
//! #[view(table = "task")]
//! struct TaskView {
//!     id: Uuid,
//!     name: String,
//!     #[view(embed(prefix = "addr_"))]
//!     address: Address,
//!     #[view(to_one(fk = "assignee_id"))]
//!     assignee: Option<PersonView>,
//!     #[view(child(fk = "parent_id", order_by = "name"))]
//!     children: Vec<SubtaskView>,
//! }
//!
//! #[derive(View)]
//! #[view(embedded)]
//! struct Address {
//!     street: String,
//!     city: String,
//! }
//!
//! let task = refract::load::<TaskView>().by_key(id).one(&mut *tx).await?;
//! ```
//!
//! Every query of a load runs on the connection you pass, so it sees the uncommitted
//! changes of its transaction. Use `&mut *conn` for a pooled connection and `&mut *tx` for a
//! transaction.
//!
//! See the [design document](https://github.com/ddalton/refract/blob/main/docs/design.md).

pub use refract_derive::View;
pub use refract_sqlx::filter;
pub use refract_sqlx::{
    Builder, Diagnostic, Embedded, Error, Graph, Key, Load, Node, OnInvalid, Origin, Ref, Refract, Reloaded, Report,
    Severity, ShadowSummary, View, load, plan, scaffold,
};

/// The query plan of a view and its SQL.
pub mod query {
    pub use refract_core::sql::{RootOptions, select};
    pub use refract_core::{ChildPlan, Link, PlanError, QueryPlan, SelectColumn};
}

#[doc(hidden)]
pub use refract_sqlx::__private;
