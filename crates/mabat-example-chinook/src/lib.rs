//! A web service on Chinook, a digital music store, built with Mabat: the views of the store as
//! Rust types, served as REST and GraphQL, streamed as NDJSON, saved, and tuned by a DBA with an
//! override file, all checked against the schema when it is built.
//!
//! [`open`] creates the SQLite database from the Chinook script on first use, and builds the
//! registry of the views with the overrides of `mabat/overrides`, checked against the database.
//! [`router`] serves it; `src/main.rs` runs it on `127.0.0.1:3000`. See the README for a tour.

use std::path::Path;
use std::sync::Arc;

use async_graphql::dynamic;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use chrono::NaiveDateTime;
use mabat::filter::{Condition, col};
use mabat::{Mabat, Selection, View};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, Connection, Executor, Sqlite, SqlitePool};

// An artist's albums and their tracks, typed, and serialized with serde

/// An artist with their albums, each with its tracks. The albums query has an override, in
/// `mabat/overrides/Discography.sql`.
#[derive(View, Serialize, Debug, Clone)]
#[view(table = "artist", key = "artist_id")]
pub struct Discography {
    pub artist_id: i32,
    pub name: Option<String>,
    #[view(child(fk = "artist_id", order_by = "title"))]
    pub albums: Vec<Album>,
}

#[derive(View, Serialize, Debug, Clone)]
#[view(table = "album", key = "album_id")]
pub struct Album {
    pub album_id: i32,
    pub title: String,
    #[view(child(fk = "album_id", order_by = "track_id"))]
    pub tracks: Vec<Track>,
}

/// A track, with its genre shared by every track of the genre.
#[derive(View, Serialize, Debug, Clone)]
#[view(table = "track", key = "track_id")]
pub struct Track {
    pub track_id: i32,
    pub name: String,
    pub composer: Option<String>,
    pub milliseconds: i32,
    pub unit_price: f64,
    #[view(to_one(fk = "genre_id"))]
    pub genre: Option<Arc<Genre>>,
}

#[derive(View, Serialize, Debug, Clone, PartialEq)]
#[view(table = "genre", key = "genre_id")]
pub struct Genre {
    pub genre_id: i32,
    pub name: Option<String>,
}

// A customer's account: loaded as JSON, whole or as much of it as `?select=` asks

#[derive(View, Debug, Clone)]
#[view(table = "customer", key = "customer_id")]
pub struct CustomerAccount {
    pub customer_id: i32,
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub country: Option<String>,
    #[view(to_one(fk = "support_rep_id"))]
    pub support_rep: Option<Arc<Colleague>>,
    #[view(child(fk = "customer_id", order_by = "invoice_date desc"))]
    pub invoices: Vec<Invoice>,
}

#[derive(View, Debug, Clone)]
#[view(table = "invoice", key = "invoice_id")]
pub struct Invoice {
    pub invoice_id: i32,
    pub invoice_date: NaiveDateTime,
    pub total: f64,
    #[view(embed(prefix = "billing_"))]
    pub billing: Address,
    #[view(child(fk = "invoice_id", order_by = "invoice_line_id"))]
    pub lines: Vec<InvoiceLine>,
}

#[derive(View, Debug, Clone)]
#[view(embedded)]
pub struct Address {
    pub address: Option<String>,
    pub city: Option<String>,
    pub country: Option<String>,
}

#[derive(View, Debug, Clone)]
#[view(table = "invoice_line", key = "invoice_line_id")]
pub struct InvoiceLine {
    pub invoice_line_id: i32,
    pub unit_price: f64,
    pub quantity: i32,
    #[view(to_one(fk = "track_id"))]
    pub track: Arc<TrackName>,
}

#[derive(View, Serialize, Debug, Clone, PartialEq)]
#[view(table = "employee", key = "employee_id")]
pub struct Colleague {
    pub employee_id: i32,
    pub first_name: String,
    pub last_name: String,
    pub title: Option<String>,
}

// A report: rows that are not a table, from the override in `mabat/overrides/TopCustomer.sql`

/// A customer with what they spent in a period, computed by the report's SQL, and their invoices
/// of the period, loaded as any collection.
#[derive(View, Serialize, Debug, Clone)]
#[view(table = "customer", key = "customer_id")]
pub struct TopCustomer {
    pub customer_id: i32,
    pub first_name: String,
    pub last_name: String,
    pub country: Option<String>,
    #[view(computed)]
    pub invoices: i64,
    #[view(computed)]
    pub spent: f64,
    #[view(child(fk = "customer_id", order_by = "invoice_date"))]
    pub period: Vec<InvoiceTotal>,
}

#[derive(View, Serialize, Debug, Clone)]
#[view(table = "invoice", key = "invoice_id")]
pub struct InvoiceTotal {
    pub invoice_id: i32,
    pub invoice_date: NaiveDateTime,
    pub total: f64,
}

// The staff: up the chain of managers, and down the organization chart

/// An employee and every manager above, in one `WITH RECURSIVE` query.
#[derive(View, Serialize, Debug, Clone)]
#[view(table = "employee", key = "employee_id")]
pub struct ManagerChain {
    pub employee_id: i32,
    pub first_name: String,
    pub last_name: String,
    pub title: Option<String>,
    #[view(to_one(fk = "reports_to", recursive = "cte"))]
    pub manager: Option<Box<ManagerChain>>,
}

/// An employee and everyone who reports to them, at any depth, in one query.
#[derive(View, Serialize, Debug, Clone)]
#[view(table = "employee", key = "employee_id")]
pub struct OrgChart {
    pub employee_id: i32,
    pub first_name: String,
    pub last_name: String,
    pub title: Option<String>,
    #[view(child(fk = "reports_to", order_by = "employee_id", recursive = "cte"))]
    pub reports: Vec<OrgChart>,
}

// Playlists: written as well as read

/// A playlist; its key is generated by the database, and its tracks are links in
/// `playlist_track`.
#[derive(View, Serialize, Debug, Clone, PartialEq)]
#[view(table = "playlist", key = "playlist_id")]
pub struct Playlist {
    #[view(generated)]
    pub playlist_id: Option<i32>,
    pub name: Option<String>,
    #[view(child(through = "playlist_track", fk = "playlist_id", target = "track_id", order_by = "track_id"))]
    pub tracks: Vec<TrackName>,
}

#[derive(View, Serialize, Debug, Clone, PartialEq)]
#[view(table = "track", key = "track_id")]
pub struct TrackName {
    pub track_id: i32,
    pub name: String,
    pub milliseconds: i32,
}

/// The views of the service, with the overrides of `mabat/overrides`. `tests/files.rs` writes
/// their manifest to `mabat/views.json`, which `build.rs` checks against the schema.
pub fn views() -> mabat::Builder<Sqlite> {
    Mabat::builder()
        .register::<Discography>()
        .register::<Track>()
        .register::<CustomerAccount>()
        .register::<ManagerChain>()
        .register::<OrgChart>()
        .register::<Playlist>()
        .register::<TrackName>()
        .register::<TopCustomer>()
        .overrides_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/mabat/overrides"))
}

/// The SQL that creates and fills the store: the Chinook script for SQLite, with playlist keys
/// generated by the database (`INTEGER PRIMARY KEY`), so that new playlists get one.
pub fn schema_sql() -> String {
    let sql = mabat_e2e::Dataset::Chinook.sqlite_sql();
    let playlist = "CREATE TABLE playlist\n(\n    playlist_id INT NOT NULL,\n    name VARCHAR(120),\n    \
                    CONSTRAINT playlist_pkey PRIMARY KEY  (playlist_id)\n);";
    assert!(sql.contains(playlist), "the Chinook script declares the playlist table as expected");
    sql.replace(playlist, "CREATE TABLE playlist\n(\n    playlist_id INTEGER PRIMARY KEY,\n    name VARCHAR(120)\n);")
}

/// The service: a pool of connections to the store, the registry of the views, and the GraphQL
/// schema generated from them.
pub struct App {
    pub pool: SqlitePool,
    pub mabat: Arc<Mabat<Sqlite>>,
    pub graphql: dynamic::Schema,
}

/// Open the store at `path`, creating it from the Chinook script if it does not exist, and
/// build the registry. Fails if an override no longer matches its view.
pub async fn open(path: &Path) -> std::result::Result<App, Box<dyn std::error::Error>> {
    let created = !path.exists();
    let options =
        SqliteConnectOptions::new().filename(path).create_if_missing(true).journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new().max_connections(4).connect_with(options).await?;
    let mut conn = pool.acquire().await?;
    if created {
        let mut tx = conn.begin().await?;
        tx.execute(AssertSqlSafe(schema_sql())).await?;
        tx.commit().await?;
    }
    // Every override is checked against the database here, at startup
    let mabat = Arc::new(views().build(&mut conn).await?);
    drop(conn);
    let graphql = mabat_graphql::schema(&pool)
        .registry(mabat.clone())
        .by_key::<Discography>("artist")
        .list::<Track>("tracks")
        .by_key::<CustomerAccount>("customer")
        .by_key::<ManagerChain>("employee")
        .list::<Playlist>("playlists")
        .finish()?;
    Ok(App { pool, mabat, graphql })
}

/// The routes of the service.
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/artists/{id}", get(artist))
        .route("/tracks", get(tracks))
        .route("/tracks/export", get(export))
        .route("/customers/{id}", get(customer))
        .route("/employees/{id}/managers", get(managers))
        .route("/employees/chart", get(chart))
        .route("/playlists", axum::routing::post(create_playlist))
        .route("/playlists/{id}", get(playlist).put(update_playlist).delete(delete_playlist))
        .route("/reports/top-customers", get(top_customers))
        .route("/explain/{view}", get(explain))
        .route("/graphql", get(graphiql).post(graphql))
        .with_state(app)
}

/// An error as an HTTP response: what was not found is a 404, what cannot be written a 422,
/// anything else a 500, each with a JSON body naming the error.
#[derive(Debug)]
pub struct ApiError(StatusCode, String);

impl From<mabat::Error> for ApiError {
    fn from(error: mabat::Error) -> Self {
        let status = match error {
            mabat::Error::NotFound { .. } => StatusCode::NOT_FOUND,
            mabat::Error::Write { .. } | mabat::Error::Conflict { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError(status, error.to_string())
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, axum::Json(json!({ "error": self.1 }))).into_response()
    }
}

type Result<T> = std::result::Result<T, ApiError>;
type Json = axum::Json<Value>;

fn to_json(value: impl Serialize) -> Result<Json> {
    serde_json::to_value(value).map(axum::Json).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// `GET /artists/{id}`: typed values, serialized with serde.
async fn artist(State(app): State<Arc<App>>, UrlPath(id): UrlPath<i32>) -> Result<Json> {
    let mut conn = app.pool.acquire().await?;
    let artist = app.mabat.load::<Discography>().by_key(id).one(&mut conn).await?;
    to_json(artist)
}

#[derive(Deserialize)]
struct TrackQuery {
    genre_id: Option<i32>,
    composer: Option<String>,
    #[serde(default = "page_size")]
    limit: u64,
    #[serde(default)]
    offset: u64,
}

fn page_size() -> u64 {
    20
}

/// `GET /tracks?genre_id=1&composer=page&limit=20&offset=0`: a filtered page, as JSON straight
/// from the rows.
async fn tracks(State(app): State<Arc<App>>, Query(query): Query<TrackQuery>) -> Result<Json> {
    let mut conditions: Vec<Condition> = Vec::new();
    if let Some(genre) = query.genre_id {
        conditions.push(col("genre_id").eq(genre));
    }
    if let Some(composer) = query.composer {
        conditions.push(col("composer").ilike(format!("%{composer}%")));
    }
    let mut load = app.mabat.load::<Track>().order_by("track_id").limit(query.limit.min(500)).offset(query.offset);
    if let Some(filter) = conditions.into_iter().reduce(|a, b| a & b) {
        load = load.filter(filter);
    }
    let mut conn = app.pool.acquire().await?;
    Ok(axum::Json(Value::Array(load.json(&mut conn).await?)))
}

/// `GET /tracks/export`: every track as NDJSON, a batch of 500 at a time, without holding them
/// all in memory. The stream runs on a task of its own, which owns its connection.
async fn export(State(app): State<Arc<App>>) -> Result<Response> {
    let mut conn = app.pool.acquire().await?;
    let (lines, receiver) = tokio::sync::mpsc::channel::<std::result::Result<String, std::io::Error>>(16);
    tokio::spawn(async move {
        use futures_util::StreamExt;
        let mut tracks =
            Box::pin(app.mabat.load::<Track>().order_by("track_id").batch_size(500).json_stream(&mut conn));
        while let Some(track) = tracks.next().await {
            let line = match track {
                Ok(track) => Ok(format!("{track}\n")),
                Err(error) => Err(std::io::Error::other(error.to_string())),
            };
            // The client went away
            if lines.send(line).await.is_err() {
                break;
            }
        }
    });
    let body = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|line| (line, receiver))
    });
    Ok(([(header::CONTENT_TYPE, "application/x-ndjson")], Body::from_stream(body)).into_response())
}

#[derive(Deserialize)]
struct Select {
    select: Option<String>,
}

/// `GET /customers/{id}?select=first_name invoices { total lines { quantity } }`: JSON, whole or
/// only the selected fields, whose queries are the only ones that run.
async fn customer(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<i32>,
    Query(select): Query<Select>,
) -> Result<Json> {
    let mut load = app.mabat.load::<CustomerAccount>().by_key(id);
    if let Some(text) = select.select {
        let selection = Selection::parse(&text).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
        load = load.select(selection);
    }
    let mut conn = app.pool.acquire().await?;
    let mut rows = load.json(&mut conn).await?;
    match rows.pop() {
        Some(customer) => Ok(axum::Json(customer)),
        None => Err(ApiError(StatusCode::NOT_FOUND, format!("no customer {id}"))),
    }
}

/// `GET /employees/{id}/managers`: an employee and the chain of managers above them.
async fn managers(State(app): State<Arc<App>>, UrlPath(id): UrlPath<i32>) -> Result<Json> {
    let mut conn = app.pool.acquire().await?;
    to_json(app.mabat.load::<ManagerChain>().by_key(id).one(&mut conn).await?)
}

/// `GET /employees/chart`: the organization chart from the top.
async fn chart(State(app): State<Arc<App>>) -> Result<Json> {
    let mut conn = app.pool.acquire().await?;
    to_json(app.mabat.load::<OrgChart>().filter(col("reports_to").is_null()).one(&mut conn).await?)
}

async fn playlist(State(app): State<Arc<App>>, UrlPath(id): UrlPath<i32>) -> Result<Json> {
    let mut conn = app.pool.acquire().await?;
    to_json(app.mabat.load::<Playlist>().by_key(id).one(&mut conn).await?)
}

#[derive(Deserialize)]
struct PlaylistBody {
    name: Option<String>,
    track_ids: Option<Vec<i32>>,
}

/// The tracks with the keys, in the order of the keys; a 422 naming the keys of no track.
async fn tracks_by_keys(app: &App, ids: &[i32]) -> Result<Vec<TrackName>> {
    let mut conn = app.pool.acquire().await?;
    let found = app.mabat.load::<TrackName>().by_keys(ids.iter().copied()).all(&mut conn).await?;
    let missing: Vec<i32> = ids.iter().copied().filter(|id| !found.iter().any(|t| t.track_id == *id)).collect();
    if !missing.is_empty() {
        return Err(ApiError(StatusCode::UNPROCESSABLE_ENTITY, format!("no tracks {missing:?}")));
    }
    Ok(ids.iter().filter_map(|id| found.iter().find(|t| t.track_id == *id).cloned()).collect())
}

/// `POST /playlists {"name": "…", "track_ids": [1, 2]}`: a new playlist, whose key the database
/// generates and `save` writes back.
async fn create_playlist(
    State(app): State<Arc<App>>,
    axum::Json(body): axum::Json<PlaylistBody>,
) -> Result<(StatusCode, Json)> {
    let tracks = tracks_by_keys(&app, &body.track_ids.unwrap_or_default()).await?;
    let mut playlist = Playlist { playlist_id: None, name: body.name, tracks };
    let mut conn = app.pool.acquire().await?;
    mabat::save(&mut playlist, &mut conn).await?;
    Ok((StatusCode::CREATED, to_json(playlist)?))
}

/// `PUT /playlists/{id} {"name": "…"}`: only what changed is written, with `save_changes`.
async fn update_playlist(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<i32>,
    axum::Json(body): axum::Json<PlaylistBody>,
) -> Result<Json> {
    let before = {
        let mut conn = app.pool.acquire().await?;
        app.mabat.load::<Playlist>().by_key(id).one(&mut conn).await?
    };
    let mut after = before.clone();
    if let Some(name) = body.name {
        after.name = Some(name);
    }
    if let Some(ids) = body.track_ids {
        after.tracks = tracks_by_keys(&app, &ids).await?;
    }
    let mut conn = app.pool.acquire().await?;
    mabat::save_changes(&before, &mut after, &mut conn).await?;
    to_json(after)
}

/// `DELETE /playlists/{id}`: the playlist and its links.
async fn delete_playlist(State(app): State<Arc<App>>, UrlPath(id): UrlPath<i32>) -> Result<StatusCode> {
    let mut conn = app.pool.acquire().await?;
    let deleted = mabat::delete::<Playlist, _>(id, &mut conn).await?;
    Ok(if deleted { StatusCode::NO_CONTENT } else { StatusCode::NOT_FOUND })
}

#[derive(Deserialize)]
struct Period {
    from: chrono::NaiveDate,
    to: chrono::NaiveDate,
    #[serde(default = "top")]
    limit: u64,
}

fn top() -> u64 {
    5
}

/// `GET /reports/top-customers?from=2022-01-01&to=2023-01-01&limit=5`: the report of the DBA's
/// override, with its parameters bound, ordered by a computed field, and each customer's
/// invoices of the period.
async fn top_customers(State(app): State<Arc<App>>, Query(period): Query<Period>) -> Result<Json> {
    let (from, to) = (period.from.and_time(chrono::NaiveTime::MIN), period.to.and_time(chrono::NaiveTime::MIN));
    let in_period = col("invoice_date").ge(from) & col("invoice_date").lt(to);
    let mut conn = app.pool.acquire().await?;
    let report = app
        .mabat
        .load::<TopCustomer>()
        .bind("from", from)
        .bind("to", to)
        .nested("period", mabat::Nested::new().filter(in_period))
        .order_by_desc("spent")
        .order_by("customer_id")
        .limit(period.limit.min(100))
        .all(&mut conn)
        .await?;
    to_json(report)
}

/// `GET /explain/{view}`: the queries of a view with their SQL, generated or overridden, for a
/// DBA.
async fn explain(State(app): State<Arc<App>>, UrlPath(view): UrlPath<String>) -> Result<String> {
    let mabat = &app.mabat;
    let explained = match view.as_str() {
        "Discography" => mabat.explain::<Discography>(),
        "Track" => mabat.explain::<Track>(),
        "CustomerAccount" => mabat.explain::<CustomerAccount>(),
        "ManagerChain" => mabat.explain::<ManagerChain>(),
        "OrgChart" => mabat.explain::<OrgChart>(),
        "Playlist" => mabat.explain::<Playlist>(),
        "TrackName" => mabat.explain::<TrackName>(),
        "TopCustomer" => mabat.explain::<TopCustomer>(),
        _ => None,
    };
    explained.ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no view {view}")))
}

/// `POST /graphql`: a GraphQL request against the schema generated from the views.
async fn graphql(State(app): State<Arc<App>>, axum::Json(request): axum::Json<async_graphql::Request>) -> Response {
    axum::Json(app.graphql.execute(request).await).into_response()
}

/// `GET /graphql`: GraphiQL, to explore the schema in a browser.
async fn graphiql() -> Html<String> {
    Html(async_graphql::http::GraphiQLSource::build().endpoint("/graphql").finish())
}
