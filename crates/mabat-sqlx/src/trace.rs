//! Tracing, with the `tracing` crate, at the `debug` level:
//!
//! - `mabat.load`, `mabat.count`, `mabat.save`, … for each operation, with the view;
//! - `mabat.query` for each query of a load: the view, the query's name (`$root`, `children`),
//!   its path, whether an override ran, the number of keys it was given and of rows it returned;
//! - `mabat.statement` for each statement sent to the database: the database, the SQL, the rows
//!   it returned or changed and how long it took, with an event when it ends.
//!
//! With `tracing-subscriber`, `RUST_LOG=mabat=debug` shows them; with `tracing-opentelemetry`
//! they are spans of a trace, whose `db.*` fields follow OpenTelemetry's names.

use std::future::Future;
use std::time::Instant;

use tracing::{Instrument, field};

/// Run a statement in a `mabat.statement` span, and record what it did: `rows`, the rows it
/// returned or changed, and `elapsed_ms`.
pub(crate) async fn statement<T>(
    system: &'static str,
    sql: &str,
    rows: impl FnOnce(&T) -> u64,
    run: impl Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, sqlx::Error> {
    let span = tracing::debug_span!(
        "mabat.statement",
        db.system.name = system,
        db.query.text = sql,
        rows = field::Empty,
        elapsed_ms = field::Empty,
    );
    let start = Instant::now();
    let result = run.instrument(span.clone()).await;
    // In milliseconds, to the microsecond
    let elapsed_ms = start.elapsed().as_micros() as f64 / 1000.0;
    span.record("elapsed_ms", elapsed_ms);
    match &result {
        Ok(value) => {
            let rows = rows(value);
            span.record("rows", rows);
            tracing::debug!(parent: &span, rows, elapsed_ms, "statement ran");
        }
        Err(error) => tracing::debug!(parent: &span, %error, elapsed_ms, "statement failed"),
    }
    result
}
