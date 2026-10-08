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
}
