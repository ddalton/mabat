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

    #[error("{view}: cannot order by `{column}`, the root query is overridden and the view does not select the column")]
    UnknownOrderBy { view: &'static str, column: String },
}
