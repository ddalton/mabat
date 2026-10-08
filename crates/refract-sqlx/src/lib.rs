//! PostgreSQL executor for Refract views, built on SQLx.
//!
//! Use the `refract` crate, which re-exports this crate together with the derive macro.

mod check;
mod describe;
mod error;
mod key;
mod node;
mod overrides;
mod registry;
mod report;

use std::marker::PhantomData;
use std::sync::Arc;

use refract_core::sql::RootOptions;
use refract_core::{EmbeddedShape, OrderBy, QueryPlan, ViewShape};
use sqlx::PgConnection;
use sqlx::postgres::PgRow;

use check::Checked;
pub use describe::Description;
pub use error::Error;
pub use key::Key;
pub use node::Node;
pub use overrides::Origin;
pub use registry::{Builder, OnInvalid, Refract, Reloaded, ShadowSummary, scaffold};
pub use report::{Diagnostic, Report, Severity};

/// A view: a type whose values are loaded from a table, together with their embedded
/// structs, to-one references and to-many collections.
///
/// Implemented by `#[derive(View)]`.
pub trait View: Sized + Send {
    /// The static description of the view.
    fn shape() -> &'static ViewShape;

    /// Decode a value from a row of the view's query and the rows of its child queries.
    fn decode(row: &PgRow, node: &Node) -> Result<Self, Error>;

    /// Describe the Rust types of the view's columns, to check queries against them.
    fn describe(description: &mut Description);
}

/// A struct stored in columns of the table of the view that contains it.
///
/// Implemented by `#[derive(View)]` with `#[view(embedded)]`.
pub trait Embedded: Sized + Send {
    fn shape() -> &'static EmbeddedShape;

    /// Decode a value from the columns whose aliases start with `prefix`.
    fn decode_embedded(row: &PgRow, node: &Node, prefix: &str) -> Result<Self, Error>;

    /// Describe the Rust types of the columns, whose aliases start with `prefix`.
    fn describe_embedded(description: &mut Description, prefix: &str);
}

/// The query plan of a view.
pub fn plan<T: View>() -> Result<QueryPlan, Error> {
    Ok(QueryPlan::build(T::shape())?)
}

/// Start loading values of a view with the generated queries, without overrides.
///
/// ```ignore
/// let task: TaskView = refract::load::<TaskView>().by_key(id).one(&mut *tx).await?;
/// let all: Vec<TaskView> = refract::load::<TaskView>().order_by("name").limit(50).all(&mut *conn).await?;
/// ```
///
/// Use [`Refract::load`] to load with overrides.
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
    _view: PhantomData<fn() -> T>,
}

enum Source {
    /// [`load`]: plan the view and run the generated queries.
    Generated,
    /// [`Refract::load`] of a registered view.
    Registered(Arc<Checked>),
    /// [`Refract::load`] of a view that is not registered.
    NotRegistered,
}

impl<T: View> Load<T> {
    fn new(source: Source) -> Self {
        Load { source, keys: None, options: RootOptions::default(), _view: PhantomData }
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

    /// Load all matching values.
    pub async fn all(self, conn: &mut PgConnection) -> Result<Vec<T>, Error> {
        let view = T::shape().name;
        let generated;
        let (plan, overrides) = match &self.source {
            Source::Generated => {
                generated = plan::<T>()?;
                (&generated, None)
            }
            Source::Registered(checked) => (&*checked.plan, Some(&checked.overrides)),
            Source::NotRegistered => return Err(Error::NotRegistered { view }),
        };
        let keys = match self.keys {
            Some(keys) if keys.is_empty() => return Ok(Vec::new()),
            Some(keys) => Some(key::KeyArray::new(keys).map_err(|_| Error::MixedKeys { view })?),
            None => None,
        };
        let options = RootOptions { by_keys: keys.is_some(), ..self.options };
        let node = node::load(conn, plan, &options, keys, overrides).await?;
        node.rows().iter().map(|row| T::decode(row, &node)).collect()
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

/// Used by the code generated by `#[derive(View)]`.
#[doc(hidden)]
pub mod __private {
    pub use crate::describe::Description;
    pub use crate::node::{children, column, optional_column, to_one, to_one_required};
    pub use refract_core::{EmbeddedShape, Field, FieldKind, OrderBy, ViewShape};
    pub use sqlx::postgres::PgRow;
}
