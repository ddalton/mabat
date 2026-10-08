//! A GraphQL schema generated from Mabat views, with async-graphql.
//!
//! Each root field loads its views with one Mabat load of the fields the query selects:
//! only their columns are selected and only their child queries run, whatever the depth of
//! the query. Overrides of a [`mabat::Mabat`] registry apply.
//!
//! ```ignore
//! let schema = mabat_graphql::schema(&pool)
//!     .list::<TaskView>("tasks") // tasks(where: TaskViewWhere, orderBy: [TaskViewOrderBy!], limit: Int, offset: Int): [TaskView!]!
//!     .by_key::<TaskView>("task") // task(key: UUID!): TaskView
//!     .finish()?;
//!
//! let response = schema
//!     .execute(r#"{ tasks(where: { name: { ilike: "%release%" } }, limit: 10) { name assignee { name } } }"#)
//!     .await;
//! ```
//!
//! # Types
//!
//! - A view or an embedded struct is an object type named after its Rust type, with a field
//!   per Rust field, named as in Rust.
//! - An enum with data is a union of an object type per variant, named after the enum and the
//!   variant, such as `StateBlocked`, with a `_variant` field and the variant's fields. An
//!   enum whose variants have no data is a GraphQL enum.
//! - A map collection is a list of `{ key: String!, value }` objects.
//! - Columns are `Boolean`, `Int`, `Float` and `String`, or the custom scalars `BigInt`,
//!   `UUID`, `Date`, `Time`, `DateTime`, `NaiveDateTime`, `Decimal`, `JSON` and `Bytes`, in the
//!   JSON of their Rust type's `Serialize`. A column of another Rust type is a scalar named
//!   after it.
//!
//! # Arguments
//!
//! A [`SchemaBuilder::list`] field takes:
//!
//! - `where`: a filter per column, with `eq`, `ne`, `lt`, `le`, `gt`, `ge`, `in`, `notIn`,
//!   `isNull`, and `like` and `ilike` for strings, combined with `and`, `or` and `not`. Columns
//!   of booleans, numbers, strings, UUIDs, dates and times can be filtered.
//! - `orderBy`: a list of `{ column: ASC | DESC }`, applied in order.
//! - `limit` and `offset`.
//!
//! A [`SchemaBuilder::by_key`] field takes the `key` of a view, of the type of its key field.

mod args;
mod resolve;
mod types;

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub use async_graphql;
use async_graphql::dynamic::{self, Field, FieldFuture, FieldValue, InputValue, Object, TypeRef};
use mabat::filter::Condition;
use mabat::{Backend, Key, Mabat, Pooled, Selection, ViewDecoder};
use serde_json::Value as Json;
use sqlx::Pool;

/// Why a schema could not be built.
#[derive(Debug)]
pub enum Error {
    /// The views cannot be GraphQL types, such as two views with the same name.
    Types(Vec<String>),
    /// async-graphql rejected the schema.
    Schema(dynamic::SchemaError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Types(errors) => write!(f, "the views cannot be GraphQL types: {}", errors.join("; ")),
            Error::Schema(error) => write!(f, "invalid GraphQL schema: {error}"),
        }
    }
}

impl std::error::Error for Error {}

/// Start a schema whose fields load views on connections of the pool.
pub fn schema<B: Backend>(pool: &Pool<B>) -> SchemaBuilder<B> {
    SchemaBuilder {
        pool: pool.clone(),
        registry: None,
        connections: 1,
        query: Object::new("Query"),
        types: types::Types::default(),
    }
}

/// Builds a GraphQL schema of views, see the [crate documentation](crate).
pub struct SchemaBuilder<B: Backend> {
    pool: Pool<B>,
    registry: Option<Arc<Mabat<B>>>,
    connections: usize,
    query: Object,
    types: types::Types,
}

/// What a root field asks a load for.
struct Request {
    selection: Selection,
    keys: Vec<Key>,
    condition: Option<Condition>,
    order: Vec<(&'static str, bool)>,
    limit: Option<u64>,
    offset: Option<u64>,
}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Runs the load of a root field for a view.
type Loader = Arc<dyn Fn(Request) -> BoxFuture<Result<Vec<Json>, mabat::Error>> + Send + Sync>;

impl<B: Backend> SchemaBuilder<B>
where
    B::Connection: Send,
    B::Row: Send + Sync,
{
    /// Load the views with the overrides of a registry, which needs to register them.
    pub fn registry(mut self, mabat: Arc<Mabat<B>>) -> Self {
        self.registry = Some(mabat);
        self
    }

    /// Run the queries of a level of each load at the same time, on up to `connections`
    /// connections of the pool, each seeing what is committed when it runs (see
    /// [`Pooled::read_committed`]). One connection by default.
    pub fn connections(mut self, connections: usize) -> Self {
        self.connections = connections.max(1);
        self
    }

    /// A root field `name(where, orderBy, limit, offset): [T!]!` that loads the matching
    /// values of the view.
    pub fn list<T: ViewDecoder<B>>(mut self, name: &str) -> Self {
        let shape = T::shape();
        let type_name = self.types.view(shape);
        let where_name = self.types.where_input(shape);
        let order_name = self.types.order_input(shape);
        let loader = self.loader::<T>();
        let field = Field::new(name, TypeRef::named_nn_list_nn(type_name), move |ctx| {
            let loader = loader.clone();
            FieldFuture::new(async move {
                let mut request = request(shape, &ctx.ctx.field());
                if let Some(filter) = ctx.args.get("where") {
                    request.condition = Some(args::condition(shape, &filter.object()?)?);
                }
                if let Some(order) = ctx.args.get("orderBy") {
                    request.order = args::order(shape, &order)?;
                }
                request.limit = count(&ctx, "limit")?;
                request.offset = count(&ctx, "offset")?;
                let values = loader(request).await.map_err(|e| async_graphql::Error::new(e.to_string()))?;
                Ok(Some(FieldValue::list(values.into_iter().map(FieldValue::owned_any))))
            })
        })
        .argument(InputValue::new("where", TypeRef::named(where_name)))
        .argument(InputValue::new("orderBy", TypeRef::named_nn_list(order_name)))
        .argument(InputValue::new("limit", TypeRef::named(TypeRef::INT)))
        .argument(InputValue::new("offset", TypeRef::named(TypeRef::INT)));
        self.query = self.query.field(field);
        self
    }

    /// A root field `name(key): T` that loads the value of the view with the key, `null` if
    /// there is none.
    pub fn by_key<T: ViewDecoder<B>>(mut self, name: &str) -> Self {
        let shape = T::shape();
        let type_name = self.types.view(shape);
        let (key_type, key_scalar) = self.types.key(shape);
        let loader = self.loader::<T>();
        let field = Field::new(name, TypeRef::named(type_name), move |ctx| {
            let loader = loader.clone();
            FieldFuture::new(async move {
                let mut request = request(shape, &ctx.ctx.field());
                request.keys = vec![args::key(key_scalar, &ctx.args.try_get("key")?)?];
                let mut values = loader(request).await.map_err(|e| async_graphql::Error::new(e.to_string()))?;
                Ok(values.pop().map(FieldValue::owned_any))
            })
        })
        .argument(InputValue::new("key", key_type));
        self.query = self.query.field(field);
        self
    }

    /// The loader of a view, which erases its type.
    fn loader<T: ViewDecoder<B>>(&self) -> Loader {
        let pool = self.pool.clone();
        let registry = self.registry.clone();
        let connections = self.connections;
        Arc::new(move |request: Request| {
            let pool = pool.clone();
            let registry = registry.clone();
            Box::pin(async move {
                let mut load = match &registry {
                    Some(mabat) => mabat.load::<T>(),
                    None => mabat::load::<T>(),
                };
                if !request.keys.is_empty() {
                    load = load.by_keys(request.keys);
                }
                if let Some(condition) = request.condition {
                    load = load.filter(condition);
                }
                for (column, descending) in request.order {
                    load = if descending { load.order_by_desc(column) } else { load.order_by(column) };
                }
                if let Some(limit) = request.limit {
                    load = load.limit(limit);
                }
                if let Some(offset) = request.offset {
                    load = load.offset(offset);
                }
                let load = load.select(request.selection);
                if connections > 1 {
                    load.json(&mut Pooled::read_committed(&pool, connections)).await
                } else {
                    let mut conn = pool.acquire().await.map_err(mabat::Error::Connection)?;
                    load.json(&mut conn).await
                }
            })
        })
    }

    /// The async-graphql builder of the schema, to add data, limits or extensions before
    /// finishing it.
    pub fn into_dynamic(self) -> Result<dynamic::SchemaBuilder, Error> {
        if !self.types.errors.is_empty() {
            return Err(Error::Types(self.types.errors));
        }
        let mut builder = dynamic::Schema::build("Query", None, None).register(self.query);
        for ty in self.types.into_types() {
            builder = builder.register(ty);
        }
        Ok(builder)
    }

    /// The schema.
    pub fn finish(self) -> Result<dynamic::Schema, Error> {
        self.into_dynamic()?.finish().map_err(Error::Schema)
    }
}

/// A request for the fields a root field selects.
fn request(shape: &'static mabat::shape::ViewShape, field: &async_graphql::SelectionField<'_>) -> Request {
    Request {
        selection: resolve::selection(shape, field),
        keys: Vec::new(),
        condition: None,
        order: Vec::new(),
        limit: None,
        offset: None,
    }
}

/// A `limit` or `offset` argument, which cannot be negative.
fn count(ctx: &dynamic::ResolverContext<'_>, name: &str) -> async_graphql::Result<Option<u64>> {
    match ctx.args.get(name) {
        None => Ok(None),
        Some(value) if value.is_null() => Ok(None),
        Some(value) => {
            let count = value.i64()?;
            u64::try_from(count).map(Some).map_err(|_| format!("{name} cannot be negative").into())
        }
    }
}
