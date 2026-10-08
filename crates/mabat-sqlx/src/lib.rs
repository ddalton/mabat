//! PostgreSQL executor for Mabat views, built on SQLx.
//!
//! Use the `mabat` crate, which re-exports this crate together with the derive macro.

mod check;
mod describe;
mod error;
pub mod filter;
mod graph;
mod key;
pub mod manifest;
mod node;
mod overrides;
mod registry;
mod report;

use std::marker::PhantomData;
use std::sync::Arc;

use mabat_core::sql::RootOptions;
use mabat_core::{EmbeddedShape, OrderBy, QueryPlan, ViewShape};
use sqlx::PgConnection;
use sqlx::postgres::PgRow;

use check::Checked;
pub use describe::Description;
pub use error::Error;
pub use graph::{Graph, Ref};
pub use key::Key;
pub use node::Node;
pub use overrides::Origin;
pub use registry::{Builder, Mabat, OnInvalid, Reloaded, ShadowSummary, scaffold};
pub use report::{Diagnostic, Report, Severity};

/// A view: a type whose values are loaded from a table, together with their embedded
/// structs, to-one references and to-many collections.
///
/// Implemented by `#[derive(View)]`.
pub trait View: Sized + Send + Sync + 'static {
    /// The static description of the view.
    fn shape() -> &'static ViewShape;

    /// Decode a value from a row of the view's query and the rows of its child queries.
    fn decode(row: &PgRow, node: &Node) -> Result<Self, Error>;

    /// Describe the Rust types of the view's columns, to check queries against them.
    fn describe(description: &mut Description);

    /// Visit the rows of a query for a graph: store them in the graph as entities of this
    /// view if `entity`, and visit the rows of the child fields.
    fn decode_graph(node: &Node, graph: &mut graph::GraphBuilder, entity: bool) -> Result<(), Error>;
}

/// A value stored in the row of the view that contains it: a struct whose fields are
/// columns of the view's table, or an enum.
///
/// Implemented by `#[derive(View)]` for structs with `#[view(embedded)]` and for enums.
pub trait Embedded: Sized + Send {
    fn shape() -> &'static EmbeddedShape;

    /// Decode a value from the columns whose aliases start with `prefix`. `field_index` is
    /// the index of the view's field that holds the value, to find the rows of variant
    /// tables.
    fn decode_embedded(row: &PgRow, node: &Node, prefix: &str, field_index: usize) -> Result<Self, Error>;

    /// Describe the Rust types of the columns, whose aliases start with `prefix`.
    fn describe_embedded(description: &mut Description, prefix: &str, field_index: usize);
}

/// The query plan of a view.
pub fn plan<T: View>() -> Result<QueryPlan, Error> {
    Ok(QueryPlan::build(T::shape())?)
}

/// Start loading values of a view with the generated queries, without overrides.
///
/// ```ignore
/// let task: TaskView = mabat::load::<TaskView>().by_key(id).one(&mut *tx).await?;
/// let all: Vec<TaskView> = mabat::load::<TaskView>().order_by("name").limit(50).all(&mut *conn).await?;
/// ```
///
/// Use [`Mabat::load`] to load with overrides.
pub fn load<T: View>() -> Load<T> {
    Load::new(Source::Generated)
}

/// A load of a view, configured with the builder methods and run with [`Load::all`],
/// [`Load::one`] or [`Load::optional`].
///
/// All queries run on the given connection, so they see the uncommitted changes of its
/// transaction.
#[must_use = "a load does nothing until it is run with all, one or optional"]
pub struct Load<T> {
    source: Source,
    keys: Option<Vec<Key>>,
    options: RootOptions,
    condition: Option<filter::Condition>,
    _view: PhantomData<fn() -> T>,
}

enum Source {
    /// [`load`]: plan the view and run the generated queries.
    Generated,
    /// [`Mabat::load`] of a registered view.
    Registered(Arc<Checked>),
    /// [`Mabat::load`] of a view that is not registered.
    NotRegistered,
}

impl<T: View> Load<T> {
    fn new(source: Source) -> Self {
        Load { source, keys: None, options: RootOptions::default(), condition: None, _view: PhantomData }
    }

    pub(crate) fn registered(checked: Option<Arc<Checked>>) -> Self {
        Load::new(checked.map_or(Source::NotRegistered, Source::Registered))
    }

    /// Load the value with the given key.
    pub fn by_key(mut self, key: impl Into<Key>) -> Self {
        self.keys.get_or_insert_with(Vec::new).push(key.into());
        self
    }

    /// Load the values with the given keys.
    pub fn by_keys<K: Into<Key>>(mut self, keys: impl IntoIterator<Item = K>) -> Self {
        self.keys.get_or_insert_with(Vec::new).extend(keys.into_iter().map(Into::into));
        self
    }

    /// Order the values by a column of the view's table, ascending. With an override of
    /// the root query, the column needs to be selected by the view.
    pub fn order_by(mut self, column: &'static str) -> Self {
        self.options.order_by.push(OrderBy::asc(column));
        self
    }

    /// Order the values by a column of the view's table, descending.
    pub fn order_by_desc(mut self, column: &'static str) -> Self {
        self.options.order_by.push(OrderBy::desc(column));
        self
    }

    /// Load at most `limit` values.
    pub fn limit(mut self, limit: u64) -> Self {
        self.options.limit = Some(limit);
        self
    }

    /// Skip the first `offset` values.
    pub fn offset(mut self, offset: u64) -> Self {
        self.options.offset = Some(offset);
        self
    }

    /// Only load values whose root row matches the condition. Conditions of several calls
    /// all need to match. With an override of the root query, the columns need to be
    /// selected by the view.
    ///
    /// ```ignore
    /// use mabat::filter::col;
    /// let open = mabat::load::<TaskView>().filter(col("status").eq("open")).all(&mut conn).await?;
    /// ```
    pub fn filter(mut self, condition: filter::Condition) -> Self {
        self.condition = Some(match self.condition.take() {
            Some(existing) => existing.and(condition),
            None => condition,
        });
        self
    }

    /// The plan and overrides to run, the keys, the root options and the filter values.
    fn prepare(self) -> Result<Option<Prepared>, Error> {
        let view = T::shape().name;
        let (plan, overrides) = match self.source {
            Source::Generated => (Arc::new(plan::<T>()?), None),
            Source::Registered(checked) => (checked.plan.clone(), Some(checked)),
            Source::NotRegistered => return Err(Error::NotRegistered { view }),
        };
        let keys = match self.keys {
            Some(keys) if keys.is_empty() => return Ok(None),
            Some(keys) => Some(key::KeyArray::new(keys).map_err(|_| Error::MixedKeys { view })?),
            None => None,
        };
        let (filter, values) = match self.condition {
            Some(condition) => {
                let (filter, values) = condition.into_parts();
                (Some(filter), values)
            }
            None => (None, Vec::new()),
        };
        let options = RootOptions { by_keys: keys.is_some(), filter, ..self.options };
        Ok(Some(Prepared { plan, overrides, keys, options, values }))
    }

    /// Load all matching values.
    ///
    /// A view with references into a graph (`Ref<T>` fields) is loaded with
    /// [`Load::graph`] instead.
    pub async fn all(self, conn: &mut PgConnection) -> Result<Vec<T>, Error> {
        let Some(load) = self.prepare()? else { return Ok(Vec::new()) };
        if load.plan.has_graph_edges() {
            return Err(Error::GraphRequired { view: T::shape().name });
        }
        let node = load.run(conn, graph::Identity::new(false)).await?;
        node.rows().iter().map(|row| T::decode(row, &node)).collect()
    }

    /// Load the matching values and every entity they reference through `Ref<T>` fields,
    /// directly or indirectly, as a [`Graph`] whose roots are the matching values.
    ///
    /// Each entity is fetched once and each collection of an entity is loaded once, so
    /// cycles end by themselves: a graph needs no `depth`. All the entities reachable through
    /// references are loaded, so a graph view should only reference what it needs.
    pub async fn graph(self, conn: &mut PgConnection) -> Result<Graph<T>, Error> {
        let identity = graph::Identity::new(true);
        let Some(load) = self.prepare()? else {
            return graph::GraphBuilder::new(identity).finish(Vec::new());
        };
        let node = load.run(conn, identity.clone()).await?;
        let mut builder = graph::GraphBuilder::new(identity);
        T::decode_graph(&node, &mut builder, true)?;
        builder.finish(node.row_keys()?)
    }

    /// Count the matching values, ignoring `order_by`, `limit` and `offset`. Runs only the
    /// root query, as `SELECT count(*)`.
    pub async fn count(self, conn: &mut PgConnection) -> Result<i64, Error> {
        let Some(load) = self.prepare()? else { return Ok(0) };
        let overrides = load.overrides.as_ref().map(|c| &c.overrides);
        node::count(conn, &load.plan, &load.options, load.keys, overrides, load.values).await
    }

    /// Load exactly one value: [`Error::NotFound`] if there is none, [`Error::TooManyRows`]
    /// if there is more than one.
    pub async fn one(self, conn: &mut PgConnection) -> Result<T, Error> {
        let view = T::shape().name;
        self.optional(conn).await?.ok_or(Error::NotFound { view })
    }

    /// Load at most one value: [`Error::TooManyRows`] if there is more than one.
    pub async fn optional(self, conn: &mut PgConnection) -> Result<Option<T>, Error> {
        let view = T::shape().name;
        let mut values = self.all(conn).await?;
        match values.len() {
            0 => Ok(None),
            1 => Ok(values.pop()),
            count => Err(Error::TooManyRows { view, count }),
        }
    }
}

/// A load ready to run.
struct Prepared {
    plan: Arc<QueryPlan>,
    overrides: Option<Arc<Checked>>,
    keys: Option<key::KeyArray>,
    options: RootOptions,
    values: Vec<filter::Bound>,
}

impl Prepared {
    async fn run(self, conn: &mut PgConnection, identity: Arc<graph::Identity>) -> Result<Node, Error> {
        let overrides = self.overrides.as_ref().map(|c| &c.overrides);
        let plan = &*self.plan;
        let path = String::new();
        let entities = identity.graph;
        let values = &self.values;
        node::load(conn, plan, &self.options, self.keys, overrides, values, path, Vec::new(), identity, entities).await
    }
}

/// Used by the code generated by `#[derive(View)]`.
#[doc(hidden)]
pub mod __private {
    pub use crate::describe::{DescribeFn, Description};
    pub use crate::graph::GraphBuilder;
    pub use crate::node::{
        MapInsert, children, column, graph_store, graph_visit, map, optional_column, reference, reference_required,
        references, shared_children, shared_to_one, shared_to_one_required, strict, tag, to_one, to_one_required,
        unknown_tag, variant,
    };
    pub use mabat_core::{
        Child, EmbeddedKind, EmbeddedShape, Field, FieldKind, OrderBy, Recursion, SumShape, SumStrategy, Through,
        Variant, VariantData, ViewShape,
    };
    pub use sqlx::postgres::PgRow;
    pub use sqlx::types::Json;
}
