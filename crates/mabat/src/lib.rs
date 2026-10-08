//! Typed aggregate reads for Rust, with SQL you can tune without changing code.
//!
//! Mabat loads nested, typed data from PostgreSQL, MySQL or SQLite. The shape of the result
//! is declared with Rust types deriving [`View`], and Mabat plans and runs the queries that
//! fill it: one query for the root rows, plus one batched query (`WHERE fk = ANY($1)` on
//! PostgreSQL, `WHERE fk IN (?, …)` elsewhere) per collection and per reference, so loading
//! never runs one query per row. Any of those queries can be replaced with hand-tuned SQL
//! from a file, checked against the views and the database at startup.
//!
//! # Views
//!
//! ```no_run
//! use mabat::View;
//! use uuid::Uuid;
//!
//! #[derive(View, Debug)]
//! #[view(table = "task")]
//! struct TaskView {
//!     id: Uuid,
//!     name: String,
//!     description: Option<String>,
//!     #[view(embed(prefix = "addr_"))]
//!     address: Address,                     // columns addr_street, addr_city
//!     #[view(to_one(fk = "assignee_id"))]
//!     assignee: Option<PersonView>,         // one batched query for all tasks
//!     #[view(child(fk = "parent_id", order_by = "position, name"))]
//!     children: Vec<SubtaskView>,           // one batched query for all tasks
//! }
//!
//! #[derive(View, Debug)]
//! #[view(embedded)]
//! struct Address {
//!     street: String,
//!     city: String,
//! }
//!
//! #[derive(View, Debug)]
//! #[view(table = "person")]
//! struct PersonView {
//!     id: i64,
//!     #[view(column = "full_name")]
//!     name: String,
//! }
//!
//! #[derive(View, Debug)]
//! #[view(table = "task")]
//! struct SubtaskView {
//!     name: String,
//! }
//!
//! # #[cfg(feature = "postgres")] type Conn = sqlx::PgConnection;
//! # #[cfg(all(feature = "mysql", not(feature = "postgres")))] type Conn = sqlx::MySqlConnection;
//! # #[cfg(not(any(feature = "postgres", feature = "mysql")))] type Conn = sqlx::SqliteConnection;
//! # async fn example(conn: &mut Conn, id: Uuid) -> Result<(), mabat::Error> {
//! use mabat::filter::col;
//!
//! let task = mabat::load::<TaskView>().by_key(id).one(conn).await?;
//!
//! let page = mabat::load::<TaskView>()
//!     .filter(col("parent_id").is_null() & col("name").ilike("%release%"))
//!     .order_by("name")
//!     .limit(20)
//!     .all(conn)
//!     .await?;
//! let total = mabat::load::<TaskView>().filter(col("parent_id").is_null()).count(conn).await?;
//! # Ok(())
//! # }
//! ```
//!
//! Every query of a load runs on the connection you pass, so it sees the uncommitted
//! changes of its transaction: pass `&mut *tx` for a transaction and `&mut *conn` for a
//! pooled connection.
//!
//! # Databases
//!
//! The features `postgres` (the default), `mysql` and `sqlite` enable the databases. A view is
//! decoded on each enabled database, and a load runs on the database of the connection it
//! is given. When a view has field types that not every enabled database decodes, such as
//! a PostgreSQL array or `rust_decimal::Decimal` on SQLite, `#[view(databases = "postgres")]`
//! limits it to the databases listed.
//!
//! In override SQL, `:keys` stands for the keys of a batched query on any database: it
//! becomes `$1` on PostgreSQL, to use as `= ANY(:keys)`, and a list of parameters on MySQL
//! and SQLite, to use as `IN (:keys)`.
//!
//! # Collections, recursion and enums
//!
//! ```no_run
//! use std::collections::BTreeMap;
//! use mabat::View;
//!
//! #[derive(View)]
//! #[view(table = "playlist")]
//! struct PlaylistView {
//!     // many-to-many, placed by an index column whatever the order of the rows
//!     #[view(child(through = "playlist_song", fk = "playlist_id", target = "song_id", index = "seq"))]
//!     songs: Vec<SongView>,
//!     // a map keyed by a column
//!     #[view(child(fk = "playlist_id", key = "name"))]
//!     settings: BTreeMap<String, SettingView>,
//!     // an enum with data, stored in columns of the row
//!     #[view(embed)]
//!     state: State,
//! }
//!
//! #[derive(View)]
//! #[view(table = "song")]
//! struct SongView {
//!     title: String,
//! }
//!
//! #[derive(View)]
//! #[view(table = "setting")]
//! struct SettingView {
//!     value: String,
//! }
//!
//! #[derive(View)]
//! #[view(tag = "state")]
//! enum State {
//!     #[view(tag_value = "draft")]
//!     Draft,
//!     #[view(tag_value = "published")]
//!     Published { published_at: chrono::DateTime<chrono::Utc> },
//! }
//!
//! // A tree of any depth, loaded with one WITH RECURSIVE query
//! #[derive(View)]
//! #[view(table = "category")]
//! struct CategoryTree {
//!     name: String,
//!     #[view(child(fk = "parent_id", order_by = "name", recursive = "cte"))]
//!     children: Vec<CategoryTree>,
//! }
//! ```
//!
//! # Saving aggregates
//!
//! [`save`] writes a value with what it owns, and [`delete`] deletes it, in a transaction:
//! rows are upserted by key, owned collections are made equal to the value's, and
//! references and many-to-many links write foreign keys only. [`save_changes`] writes only
//! what changed between two values, and a `#[view(version)]` field locks rows
//! optimistically.
//!
//! # JSON and selections
//!
//! [`Load::json`] loads views as JSON objects, and [`Load::select`] with a [`Selection`]
//! loads only some of their fields, as a GraphQL query does: only their columns are selected
//! and only their child queries run.
//!
//! # Concurrent loads
//!
//! With a [`Pooled`] pool in place of the connection, the queries of each level of a load
//! run at the same time on connections of the pool: in one shared snapshot with
//! [`Pooled::snapshot`] on PostgreSQL, or each seeing what is committed with
//! [`Pooled::read_committed`] on any database.
//!
//! # Shared values and graphs
//!
//! `Arc<T>` fields are decoded once per entity and shared. `Ref<T>` fields make a view a
//! graph: entities in arenas with typed references, so cycles need no `Rc` or `RefCell`.
//!
//! ```no_run
//! use mabat::{Ref, View};
//!
//! #[derive(View)]
//! #[view(table = "employee")]
//! pub struct Employee {
//!     pub name: String,
//!     #[view(to_one(fk = "manager_id"))]
//!     pub manager: Option<Ref<Employee>>,
//!     #[view(child(fk = "manager_id", order_by = "name"))]
//!     pub reports: Vec<Ref<Employee>>,
//! }
//!
//! # #[cfg(feature = "postgres")] type Conn = sqlx::PgConnection;
//! # #[cfg(all(feature = "mysql", not(feature = "postgres")))] type Conn = sqlx::MySqlConnection;
//! # #[cfg(not(any(feature = "postgres", feature = "mysql")))] type Conn = sqlx::SqliteConnection;
//! # async fn example(conn: &mut Conn) -> Result<(), mabat::Error> {
//! let graph = mabat::load::<Employee>().by_key(4_i64).graph(conn).await?;
//! let me = graph.root().unwrap();
//! if let Some(manager) = me.manager(&graph) {
//!     for colleague in manager.reports(&graph) {
//!         println!("{}", colleague.name);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Tuning without code changes
//!
//! A [`Mabat`] registry runs the views with the override files of a directory, after
//! checking every query, generated or overridden, against the database:
//!
//! ```no_run
//! # use mabat::View;
//! # #[derive(View)]
//! # #[view(table = "task")]
//! # struct TaskView { name: String }
//! # #[cfg(feature = "postgres")] type Conn = sqlx::PgConnection;
//! # #[cfg(all(feature = "mysql", not(feature = "postgres")))] type Conn = sqlx::MySqlConnection;
//! # #[cfg(not(any(feature = "postgres", feature = "mysql")))] type Conn = sqlx::SqliteConnection;
//! # async fn example(conn: &mut Conn) -> Result<(), mabat::Error> {
//! use mabat::Mabat;
//!
//! let mabat = Mabat::builder()
//!     .register::<TaskView>()
//!     .overrides_dir("mabat/overrides")
//!     .build(conn) // fails with a report if an override does not match the view
//!     .await?;
//!
//! let tasks = mabat.load::<TaskView>().all(conn).await?;
//! println!("{}", mabat.explain::<TaskView>().unwrap());
//! # Ok(())
//! # }
//! ```
//!
//! The application can also write a [`manifest::Manifest`] of its views, which the
//! `mabat` command line tool (crate `mabat-cli`) checks override files against, with no
//! Rust toolchain.
//!
//! See the [README](https://github.com/ddalton/mabat#readme) and the
//! [design document](https://github.com/ddalton/mabat/blob/main/docs/design.md) for the
//! attributes, the override format and the decoding rules.

pub use mabat_derive::View;
pub use mabat_sqlx::{
    Backend, Builder, Conn, Diagnostic, Embedded, EmbeddedDecoder, EmbeddedEncoder, Error, Graph, Key, Load, Mabat,
    Nested, Node, OnInvalid, Origin, Pooled, Ref, Reloaded, Report, RowWrite, Selection, SelectionError, Severity,
    ShadowSummary, View, ViewDecoder, ViewEncoder, Written, delete, load, plan, save, save_changes, scaffold,
};
pub use mabat_sqlx::{filter, manifest};

/// The static description of views, generated by `#[derive(View)]`, for tools that work
/// on views generically, such as `mabat-graphql`.
pub mod shape {
    pub use mabat_core::shape::*;
}

/// The query plan of a view and its SQL.
pub mod query {
    pub use mabat_core::sql::{RootOptions, select};
    pub use mabat_core::{ChildPlan, ChildQuery, Link, PlanError, QueryPlan, SelectColumn};
}

#[doc(hidden)]
pub use mabat_sqlx::__private;

// The derive generates a decoder for every database; these keep the decoders of the databases
// whose feature is enabled. They are defined here so that the features of this crate decide,
// not those of the crate using the derive.

#[cfg(feature = "postgres")]
#[doc(hidden)]
#[macro_export]
macro_rules! __if_postgres {
    ($($item:item)*) => { $($item)* };
}

#[cfg(not(feature = "postgres"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __if_postgres {
    ($($item:item)*) => {};
}

#[cfg(feature = "mysql")]
#[doc(hidden)]
#[macro_export]
macro_rules! __if_mysql {
    ($($item:item)*) => { $($item)* };
}

#[cfg(not(feature = "mysql"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __if_mysql {
    ($($item:item)*) => {};
}

#[cfg(feature = "sqlite")]
#[doc(hidden)]
#[macro_export]
macro_rules! __if_sqlite {
    ($($item:item)*) => { $($item)* };
}

#[cfg(not(feature = "sqlite"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __if_sqlite {
    ($($item:item)*) => {};
}
