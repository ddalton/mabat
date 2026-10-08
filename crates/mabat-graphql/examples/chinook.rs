//! A GraphQL server for the Chinook music store, on a new SQLite database: GraphiQL at
//! <http://127.0.0.1:8000>, and GraphQL at `POST /`.
//!
//! ```sh
//! cargo run -p mabat-graphql --example chinook
//! ```
//!
//! Try:
//!
//! ```graphql
//! {
//!   invoices(where: { total: { gt: 15 } }, orderBy: [{ total: DESC }], limit: 3) {
//!     invoice_id total
//!     customer { first_name last_name }
//!     lines { quantity track { name album { title artist { name } } } }
//!   }
//!   employee(key: 1) { title reports { title reports { first_name: title } } }
//! }
//! ```
//!
//! Each root field is one Mabat load of the selected fields: only their columns are selected,
//! and only the queries of the selected collections and references run.

use async_graphql::http::GraphiQLSource;
use async_graphql_axum::GraphQL;
use axum::Router;
use axum::response::Html;
use axum::routing::get;
use mabat_e2e::Dataset;
use mabat_e2e::chinook_sqlite::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (pool, _file) = Dataset::Chinook.sqlite_pool(4).await;
    let schema = mabat_graphql::schema(&pool)
        .list::<InvoiceView>("invoices")
        .by_key::<InvoiceView>("invoice")
        .list::<ArtistAlbums>("artists")
        .list::<PlaylistView>("playlists")
        .by_key::<EmployeeTree>("employee")
        .connections(4)
        .finish()?;

    let graphiql = Html(GraphiQLSource::build().endpoint("/").finish());
    let app = Router::new().route("/", get(move || async move { graphiql }).post_service(GraphQL::new(schema)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8000").await?;
    println!("GraphiQL: http://127.0.0.1:8000");
    axum::serve(listener, app).await?;
    Ok(())
}
