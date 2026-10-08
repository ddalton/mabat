use refract_core::PlanError;

/// Errors of loading a view. Each error names the view and the path involved.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Plan(#[from] PlanError),

    #[error("query for {view} at `{path}` failed: {source}\n  sql: {sql}")]
    Query {
        view: &'static str,
        path: String,
        sql: String,
        #[source]
        source: sqlx::Error,
    },

    #[error("could not decode `{path}` of {view}: {source}")]
    Decode {
        view: &'static str,
        path: String,
        #[source]
        source: sqlx::Error,
    },

    #[error("{view}: the to-one reference `{path}` points to a row that was not found")]
    MissingReference { view: &'static str, path: String },

    #[error("{view}: the tag `{path}` is NULL")]
    NullTag { view: &'static str, path: String },

    #[error("{view}: the tag `{path}` is {tag:?}, which is not one of {expected:?}")]
    UnknownTag { view: &'static str, path: String, tag: String, expected: Vec<&'static str> },

    #[error("{view}: `{path}` belongs to another variant than {tag:?}, but is not NULL")]
    OtherVariantColumn { view: &'static str, path: String, tag: String },

    #[error("{view}: the row of the variant table `{path}` was not found")]
    MissingVariant { view: &'static str, path: String },

    #[error("{view}: the list `{path}` cannot be placed: {message}")]
    ListIndex { view: &'static str, path: String, message: String },

    #[error("{view}: the rows of the recursive collection `{path}` form a cycle, which a tree cannot hold")]
    Cycle { view: &'static str, path: String },

    #[error("a reference into the graph points to a {view} that was not loaded")]
    UnloadedReference { view: &'static str },

    #[error("{view} has references into a graph (`Ref<T>`); load it with `.graph(..)`")]
    GraphRequired { view: &'static str },

    #[error("{view}: two elements of the map `{path}` have the same key")]
    DuplicateMapKey { view: &'static str, path: String },

    #[error("{view}: the keys to load have different types")]
    MixedKeys { view: &'static str },

    #[error("{view}: no row found")]
    NotFound { view: &'static str },

    #[error("{view}: expected one row, found {count}")]
    TooManyRows { view: &'static str, count: usize },

    /// The views or their overrides do not pass the checks.
    #[error("the views do not match the database\n{0}")]
    Invalid(crate::Report),

    /// The connection failed while checking the views.
    #[error("checking the views failed: {0}")]
    Check(#[source] sqlx::Error),

    #[error("{view} is not registered; register it with Refract::builder().register::<{view}>()")]
    NotRegistered { view: &'static str },

    #[error("{view}: the override of the root query ({origin}) takes the keys as $1, so load it by_key or by_keys")]
    KeysRequired { view: &'static str, origin: String },

    /// A column used by `order_by` or `filter` is not selected by the view, and the root
    /// query is overridden, so it can only refer to the columns the view selects.
    #[error(
        "{view}: cannot order or filter by `{column}`: the root query is overridden and the view does not select it"
    )]
    ColumnNotSelected { view: &'static str, column: String },
}
