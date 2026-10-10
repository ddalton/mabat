//! Run the Chinook service: `cargo run -p mabat-example-chinook`, then open
//! <http://127.0.0.1:3000/graphql> or try the routes in the README.
//!
//! `CHINOOK_DB` is the database file, `chinook.db` by default, created on first run, and
//! `ADDR` the address to listen on, `127.0.0.1:3000` by default.

use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(std::env::var("CHINOOK_DB").unwrap_or_else(|_| "chinook.db".into()));
    let app = mabat_example_chinook::open(&path).await?;
    let addr = std::env::var("ADDR").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("Chinook on http://{addr} (database {})", path.display());
    println!("  GET  /artists/22                 an artist's albums and tracks");
    println!("  GET  /tracks?genre_id=1&limit=5  a filtered page of tracks");
    println!("  GET  /tracks/export              every track as NDJSON, streamed");
    println!("  GET  /customers/5?select=...     a customer's account, or part of it");
    println!("  GET  /employees/8/managers       the chain of managers above an employee");
    println!("  GET  /employees/chart            the organization chart");
    println!("  POST /playlists                  a new playlist; GET, PUT, DELETE /playlists/{{id}}");
    println!("  GET  /reports/top-customers?from=2022-01-01&to=2023-01-01  a report");
    println!("  GET  /explain/Discography        the SQL of a view, overrides included");
    println!("  GET  /graphql                    GraphiQL");
    axum::serve(listener, mabat_example_chinook::router(Arc::new(app))).await?;
    Ok(())
}
