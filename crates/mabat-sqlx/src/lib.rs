//! The executor of Mabat views, built on SQLx, for PostgreSQL, MySQL and SQLite.
//!
//! Use the `mabat` crate, which re-exports this crate together with the derive macro.

pub mod backend;
mod check;
mod describe;
mod error;
pub mod filter;
mod graph;
mod json;
mod key;
pub mod manifest;
mod node;
mod overrides;
mod pooled;
mod registry;
mod report;
mod write;

use std::marker::PhantomData;
use std::sync::Arc;

use mabat_core::sql::RootOptions;
use mabat_core::{EmbeddedShape, OrderBy, QueryPlan, ViewShape};
pub use mabat_core::{Selection, SelectionError};

pub use backend::{Backend, Conn};
use check::Checked;
pub use describe::Description;
pub use error::Error;
pub use graph::{Graph, Ref};
pub use key::Key;
pub use node::Node;
pub use overrides::Origin;
pub use pooled::Pooled;
pub use registry::{Builder, Mabat, OnInvalid, Reloaded, ShadowSummary, scaffold};
pub use report::{Diagnostic, Report, Severity};
pub use write::{EmbeddedEncoder, RowWrite, ViewEncoder, Written};

/// A view: a type whose values are loaded from a table, together with their embedded
/// structs, to-one references and to-many collections.
///
/// Implemented by `#[derive(View)]`, together with [`ViewDecoder`] for each enabled
/// database.
pub trait View: Sized + Send + Sync + 'static {
    /// The static description of the view.
    fn shape() -> &'static ViewShape;
}

/// Decoding a view from the rows of a database. Implemented by `#[derive(View)]` for each
/// database whose feature is enabled.
pub trait ViewDecoder<B: Backend>: View {
    /// Decode a value from a row of the view's query and the rows of its child queries.
    fn decode(row: &B::Row, node: &Node<B>) -> Result<Self, Error>;

    /// Describe the Rust types of the view's columns, to check queries against them.
    fn describe(description: &mut Description<B>);

    /// Visit the rows of a query for a graph: store them in the graph as entities of this
    /// view if `entity`, and visit the rows of the child fields.
    fn decode_graph(node: &Node<B>, graph: &mut graph::GraphBuilder, entity: bool) -> Result<(), Error>;

    /// Decode the loaded fields of a value as a JSON object, see [`Load::json`].
    fn decode_json(row: &B::Row, node: &Node<B>) -> Result<serde_json::Value, Error>;
}

/// A value stored in the row of the view that contains it: a struct whose fields are
/// columns of the view's table, or an enum.
///
/// Implemented by `#[derive(View)]` for structs with `#[view(embedded)]` and for enums,
/// together with [`EmbeddedDecoder`] for each enabled database.
pub trait Embedded: Sized + Send {
    fn shape() -> &'static EmbeddedShape;
}

/// Decoding an embedded value from the rows of a database.
pub trait EmbeddedDecoder<B: Backend>: Embedded {
    /// Decode a value from the columns whose aliases start with `prefix`. `field_index` is
    /// the index of the view's field that holds the value, to find the rows of variant
    /// tables.
    fn decode_embedded(row: &B::Row, node: &Node<B>, prefix: &str, field_index: usize) -> Result<Self, Error>;

    /// Describe the Rust types of the columns, whose aliases start with `prefix`.
    fn describe_embedded(description: &mut Description<B>, prefix: &str, field_index: usize);

    /// Decode a value as JSON, from the columns whose aliases start with `prefix`.
    fn decode_embedded_json(
        row: &B::Row,
        node: &Node<B>,
        prefix: &str,
        field_index: usize,
    ) -> Result<serde_json::Value, Error>;
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

/// Save a value as an aggregate, in a transaction (a savepoint if the connection is in one):
/// insert its row, or update the row with its key, then make what it owns match the value.
///
/// - Columns, embedded structs and JSON fields are written to the row.
/// - A to-one reference writes its foreign key, the key of the referenced value, which is a
///   separate aggregate and is not written.
/// - An owned collection is made equal to the value's: elements whose key is gone are
///   deleted with what they own, and the others are saved, with the key of the parent and,
///   for an ordered list, their position.
/// - A many-to-many collection replaces the links of the row; the linked values are not
///   written.
/// - An enum stored in columns writes its tag and its variant's columns, and NULL to the
///   columns of the other variants. An enum stored in a table per variant saves the
///   variant's row and deletes the rows of the other variants.
///
/// Keys are assigned by the application: the view and the views of its collections need a
/// key field. Views with references into a graph (`Ref<T>`) cannot be saved yet. Writes never
/// use overrides.
///
/// A view with a `#[view(version)]` field is locked optimistically: its row is updated only
/// if it still has the value's version, which it increments, and inserted only if no row has
/// its key. Otherwise the save fails with [`Error::Conflict`]. The new versions are written
/// back into the value, and into the elements of its owned collections, so it can be saved
/// again.
pub async fn save<T, C>(value: &mut T, conn: &mut C) -> Result<(), Error>
where
    T: ViewEncoder<C::Backend>,
    C: Conn,
    <C::Backend as sqlx::Database>::Connection: Send,
{
    use sqlx::Connection;
    let row = value.write()?;
    let mut conn = conn.source().single().await?;
    let mut tx = conn.begin().await.map_err(Error::Connection)?;
    let written = write::save_row::<C::Backend>(&mut tx, row, None).await?;
    tx.commit().await.map_err(Error::Connection)?;
    value.written(&written)
}

/// Save what changed from `before`, as it was loaded, to `after`, in a transaction: only
/// the columns that differ are updated, elements of owned collections that are new are
/// saved whole and those that are gone are deleted with what they own, and links are
/// replaced only if they differ. Rows that did not change are not written.
///
/// Values are compared with `PartialEq`, and a value whose type does not implement it is
/// written as if it changed. Elements of collections are matched by key. A row that was
/// deleted since, or whose version changed, fails the save with [`Error::Conflict`]; new
/// versions are written back into `after`.
pub async fn save_changes<T, C>(before: &T, after: &mut T, conn: &mut C) -> Result<(), Error>
where
    T: ViewEncoder<C::Backend>,
    C: Conn,
    <C::Backend as sqlx::Database>::Connection: Send,
{
    use sqlx::Connection;
    if before.key() != after.key() {
        return Err(Error::Write {
            view: T::shape().name,
            message: "the values have different keys; save_changes compares two versions of one value".to_string(),
        });
    }
    let row = after.write_changes(before)?;
    let mut conn = conn.source().single().await?;
    let mut tx = conn.begin().await.map_err(Error::Connection)?;
    let written = write::save_row::<C::Backend>(&mut tx, row, None).await?;
    tx.commit().await.map_err(Error::Connection)?;
    after.written(&written)
}

/// Delete the aggregate of a view with the key, in a transaction: the row and what it owns,
/// as [`save`] writes it. Returns whether the row existed.
pub async fn delete<T, C>(key: impl Into<Key>, conn: &mut C) -> Result<bool, Error>
where
    T: View,
    C: Conn,
    <C::Backend as sqlx::Database>::Connection: Send,
{
    use sqlx::Connection;
    let mut conn = conn.source().single().await?;
    let mut tx = conn.begin().await.map_err(Error::Connection)?;
    let deleted = write::delete_tree::<C::Backend>(&mut tx, T::shape(), vec![key.into()]).await?;
    tx.commit().await.map_err(Error::Connection)?;
    Ok(deleted > 0)
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
    selection: Option<Selection>,
    nested: Vec<(String, Nested)>,
    _view: PhantomData<fn() -> T>,
}

/// Which elements of a to-many collection to load, for each parent, and in which order: a
/// filter on the collection's table, an order that replaces the collection's, and a page.
/// See [`Load::nested`].
#[derive(Debug, Clone, Default)]
pub struct Nested {
    condition: Option<filter::Condition>,
    order_by: Vec<OrderBy>,
    limit: Option<u64>,
    offset: Option<u64>,
}

impl Nested {
    pub fn new() -> Nested {
        Nested::default()
    }

    /// Only load elements whose row matches the condition. Conditions of several calls all
    /// need to match.
    pub fn filter(mut self, condition: filter::Condition) -> Self {
        self.condition = Some(match self.condition.take() {
            Some(existing) => existing.and(condition),
            None => condition,
        });
        self
    }

    /// Order the elements of each parent by a column of the collection's table, ascending,
    /// instead of the collection's order.
    pub fn order_by(mut self, column: &'static str) -> Self {
        self.order_by.push(OrderBy::asc(column));
        self
    }

    /// Order the elements of each parent by a column, descending.
    pub fn order_by_desc(mut self, column: &'static str) -> Self {
        self.order_by.push(OrderBy::desc(column));
        self
    }

    /// Load at most `limit` elements for each parent.
    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Skip the first `offset` elements of each parent.
    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }
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
        Load {
            source,
            keys: None,
            options: RootOptions::default(),
            condition: None,
            selection: None,
            nested: Vec::new(),
            _view: PhantomData,
        }
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

    /// Load only the selected fields of the view, as JSON with [`Load::json`]: only their
    /// columns are selected and only their child queries run. Overrides of the view's
    /// queries still apply.
    ///
    /// ```ignore
    /// use mabat::Selection;
    /// let selection = Selection::parse("name assignee { name } children { name }")?;
    /// let tasks = mabat::load::<TaskView>().select(selection).json(&mut conn).await?;
    /// ```
    pub fn select(mut self, selection: Selection) -> Self {
        self.selection = Some(selection);
        self
    }

    /// Load only some of the elements of the to-many collection at `path`, the name of its
    /// query, such as `children` or `children.notes`, and in another order. The paging
    /// applies to the elements of each parent, with one query for all parents.
    ///
    /// ```ignore
    /// use mabat::Nested;
    /// use mabat::filter::col;
    /// // Every task with its three most recent open subtasks
    /// let tasks = mabat::load::<TaskView>()
    ///     .nested("children", Nested::new().filter(col("done").eq(false)).order_by_desc("created_at").limit(3))
    ///     .all(&mut conn)
    ///     .await?;
    /// ```
    ///
    /// The arguments of a collection at every level of a recursive view apply to each level.
    /// With an override of the collection's query, the columns need to be selected by its view.
    pub fn nested(mut self, path: impl Into<String>, nested: Nested) -> Self {
        let path = path.into();
        self.nested.retain(|(p, _)| *p != path);
        self.nested.push((path, nested));
        self
    }

    /// The plan and overrides to run, the keys, the root options and the filter values.
    fn prepare<B: Backend>(self) -> Result<Option<Prepared>, Error> {
        let view = T::shape().name;
        let (mut plan, overrides) = match self.source {
            Source::Generated => (None, None),
            Source::Registered(checked) if checked.backend != B::NAME => {
                return Err(Error::WrongBackend { view, registry: checked.backend, connection: B::NAME });
            }
            Source::Registered(checked) => (Some(checked.plan.clone()), Some(checked)),
            Source::NotRegistered => return Err(Error::NotRegistered { view }),
        };
        if let Some(selection) = &self.selection {
            plan = Some(Arc::new(QueryPlan::build_selected(T::shape(), selection)?));
        }
        let plan = match plan {
            Some(plan) => plan,
            None => Arc::new(crate::plan::<T>()?),
        };
        let keys = match self.keys {
            Some(keys) if keys.is_empty() => return Ok(None),
            Some(keys) => Some(key::KeyList::new(keys).map_err(|_| Error::MixedKeys { view })?),
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

        // The arguments of collections, for their queries
        let mut nested = Vec::new();
        for (path, args) in self.nested {
            let mut query = None;
            plan.walk(&mut |p| {
                if p.query_name() == path {
                    query = Some((matches!(p.link, mabat_core::Link::Child { .. }), p.cte.is_some()));
                }
            });
            let reason = match query {
                None => Some("no query of the view has this name"),
                Some((false, _)) => Some("only to-many collections take arguments"),
                Some((true, true)) => Some("a collection loaded with `recursive = \"cte\"` takes no arguments"),
                Some((true, false)) => None,
            };
            if let Some(reason) = reason {
                return Err(Error::NestedArguments { view, path, reason });
            }
            let (filter, values) = match args.condition {
                Some(condition) => {
                    let (filter, values) = condition.into_parts();
                    (Some(filter), values)
                }
                None => (None, Vec::new()),
            };
            let options = RootOptions {
                filter,
                order_by: args.order_by,
                limit: args.limit,
                offset: args.offset,
                ..RootOptions::default()
            };
            nested.push((path, options, values));
        }
        Ok(Some(Prepared { plan, overrides, keys, options, values, nested }))
    }

    /// Load all matching values.
    ///
    /// A view with references into a graph (`Ref<T>` fields) is loaded with
    /// [`Load::graph`] instead.
    pub async fn all<C: Conn>(self, conn: &mut C) -> Result<Vec<T>, Error>
    where
        T: ViewDecoder<C::Backend>,
    {
        self.typed()?;
        let Some(load) = self.prepare::<C::Backend>()? else { return Ok(Vec::new()) };
        if load.plan.has_graph_edges() {
            return Err(Error::GraphRequired { view: T::shape().name });
        }
        let node = load.run::<C::Backend>(conn.source(), graph::Identity::new(false)).await?;
        node.rows().iter().map(|row| T::decode(row, &node)).collect()
    }

    /// Load the matching values and every entity they reference through `Ref<T>` fields,
    /// directly or indirectly, as a [`Graph`] whose roots are the matching values.
    ///
    /// Each entity is fetched once and each collection of an entity is loaded once, so
    /// cycles end by themselves: a graph needs no `depth`. All the entities reachable through
    /// references are loaded, so a graph view should only reference what it needs.
    pub async fn graph<C: Conn>(self, conn: &mut C) -> Result<Graph<T>, Error>
    where
        T: ViewDecoder<C::Backend>,
    {
        self.typed()?;
        let identity = graph::Identity::new(true);
        let Some(load) = self.prepare::<C::Backend>()? else {
            return graph::GraphBuilder::new(identity).finish(Vec::new());
        };
        let node = load.run::<C::Backend>(conn.source(), identity.clone()).await?;
        let mut builder = graph::GraphBuilder::new(identity);
        T::decode_graph(&node, &mut builder, true)?;
        builder.finish(node.row_keys()?)
    }

    /// Load the matching values as JSON objects, with the fields of [`Load::select`], or all
    /// fields without a selection.
    ///
    /// Columns are written with the `Serialize` implementation of their Rust type: loading a
    /// column whose type has none is an error. `#[view(json)]` columns are written as the
    /// JSON they hold, collections as arrays, maps as objects, references as objects or
    /// `null`, and enums as objects whose `__typename` field names the variant. Tuple
    /// fields are named `_0`, `_1`, …
    ///
    /// A view with references into a graph (`Ref<T>` fields) needs a selection, which says
    /// how deep to follow them.
    pub async fn json<C: Conn>(self, conn: &mut C) -> Result<Vec<serde_json::Value>, Error>
    where
        T: ViewDecoder<C::Backend>,
    {
        let selected = self.selection.is_some();
        let Some(load) = self.prepare::<C::Backend>()? else { return Ok(Vec::new()) };
        if !selected && load.plan.has_graph_edges() {
            return Err(Error::GraphRequired { view: T::shape().name });
        }
        let node = load.run::<C::Backend>(conn.source(), graph::Identity::new(false)).await?;
        node.rows().iter().map(|row| T::decode_json(row, &node)).collect()
    }

    /// A selection is only loaded as JSON.
    fn typed(&self) -> Result<(), Error> {
        match self.selection {
            Some(_) => Err(Error::SelectionWithoutJson { view: T::shape().name }),
            None => Ok(()),
        }
    }

    /// Count the matching values, ignoring `order_by`, `limit` and `offset`. Runs only the
    /// root query, as `SELECT count(*)`.
    pub async fn count<C: Conn>(self, conn: &mut C) -> Result<i64, Error> {
        let Some(load) = self.prepare::<C::Backend>()? else { return Ok(0) };
        let overrides = load.overrides.as_ref().map(|c| &c.overrides);
        let mut conn = conn.source().single().await?;
        node::count::<C::Backend>(&mut conn, &load.plan, &load.options, load.keys, overrides, load.values).await
    }

    /// Load exactly one value: [`Error::NotFound`] if there is none, [`Error::TooManyRows`]
    /// if there is more than one.
    pub async fn one<C: Conn>(self, conn: &mut C) -> Result<T, Error>
    where
        T: ViewDecoder<C::Backend>,
    {
        let view = T::shape().name;
        self.optional(conn).await?.ok_or(Error::NotFound { view })
    }

    /// Load at most one value: [`Error::TooManyRows`] if there is more than one.
    pub async fn optional<C: Conn>(self, conn: &mut C) -> Result<Option<T>, Error>
    where
        T: ViewDecoder<C::Backend>,
    {
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
    keys: Option<key::KeyList>,
    options: RootOptions,
    values: Vec<filter::Bound>,
    /// The arguments of collections: the name of their query, their options and values.
    nested: Vec<(String, RootOptions, Vec<filter::Bound>)>,
}

impl Prepared {
    async fn run<B: Backend>(
        self,
        source: pooled::Source<'_, B>,
        identity: Arc<graph::Identity>,
    ) -> Result<Node<B>, Error> {
        let runner = pooled::Runner::new(source, identity.graph).await?;
        let overrides = self.overrides.as_ref().map(|c| &c.overrides);
        let plan = &*self.plan;
        let path = String::new();
        let entities = identity.graph;
        let mut args = node::QueryArgs::new(self.options, self.values);
        for (name, options, values) in self.nested {
            args.nest(name, options, values);
        }
        let node =
            node::load::<B>(&runner, plan, &args, self.keys, overrides, path, Vec::new(), identity, entities).await?;
        runner.finish().await?;
        Ok(node)
    }
}

/// Used by the code generated by `#[derive(View)]`.
#[doc(hidden)]
pub mod __private {
    pub use crate::Key;
    pub use crate::describe::{DescribeFn, Description};
    pub use crate::graph::GraphBuilder;
    pub use crate::json::{
        JsonFallback, JsonProbe, JsonViaSerialize, field_name, json_children, json_map, json_merge, json_object,
        json_raw, json_to_one,
    };
    pub use crate::node::{
        MapInsert, children, column, graph_store, graph_visit, map, optional_column, reference, reference_required,
        references, shared_children, shared_to_one, shared_to_one_required, strict, tag, to_one, to_one_required,
        unknown_tag, variant,
    };
    pub use crate::write::{
        ChangeFallback, ChangeProbe, ChangeViaEq, JsonWriteFallback, JsonWriteProbe, JsonWriteViaSerialize,
        KeyFallback, KeyProbe, KeyViaInto, WriteFallback, WriteProbe, WriteViaEncode, Written, changes, target_key,
        variant_table,
    };
    pub use crate::write::{EmbeddedEncoder, RowWrite, ViewEncoder};
    pub use mabat_core::{
        Child, EmbeddedKind, EmbeddedShape, Field, FieldKind, OrderBy, Recursion, Scalar, SumShape, SumStrategy,
        Through, ValueType, Variant, VariantData, ViewShape,
    };
    pub use serde_json::Value as JsonValue;
    pub use sqlx::Database;
    #[cfg(feature = "mysql")]
    pub use sqlx::MySql;
    #[cfg(feature = "postgres")]
    pub use sqlx::Postgres;
    #[cfg(feature = "sqlite")]
    pub use sqlx::Sqlite;
    pub use sqlx::types::Json;
}
