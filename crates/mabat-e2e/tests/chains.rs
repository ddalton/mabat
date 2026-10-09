//! References back to their own view on every database: breadcrumbs up a tree of categories,
//! as a whole chain in one query or a few levels by level, owned (`Box`) or shared (`Arc`), as
//! JSON and through GraphQL; chains that loop; saving a reference; and the checks of the
//! queries.

use std::sync::Arc;

use mabat::{Conn, Error, Mabat, Selection, View, ViewDecoder};
use sqlx::{AssertSqlSafe, Connection, Executor};

/// Categories, and a ring of nodes that reference each other.
const SCHEMA: &str = r#"
CREATE TABLE category (
    id BIGINT PRIMARY KEY, name VARCHAR(100) NOT NULL, parent_id BIGINT,
    FOREIGN KEY (parent_id) REFERENCES category (id)
);
INSERT INTO category VALUES (1, 'Home', NULL);
INSERT INTO category VALUES (2, 'Electronics', 1);
INSERT INTO category VALUES (3, 'Phones', 2);
INSERT INTO category VALUES (4, 'Android', 3);
INSERT INTO category VALUES (5, 'Books', 1);
CREATE TABLE ring (id BIGINT PRIMARY KEY, next_id BIGINT);
INSERT INTO ring VALUES (1, 2);
INSERT INTO ring VALUES (2, 3);
INSERT INTO ring VALUES (3, 1);
"#;

/// A category and every category above it, in one query.
#[derive(View, Debug, PartialEq)]
#[view(table = "category")]
pub struct Crumb {
    pub id: i64,
    pub name: String,
    #[view(to_one(fk = "parent_id", recursive = "cte"))]
    pub parent: Option<Box<Crumb>>,
}

/// A category and at most two categories above it, a query per level.
#[derive(View, Debug, PartialEq)]
#[view(table = "category")]
pub struct Near {
    pub id: i64,
    pub name: String,
    #[view(to_one(fk = "parent_id", depth = 2))]
    pub parent: Option<Box<Near>>,
}

/// A category above several categories is decoded once and shared.
#[derive(View, Debug, PartialEq)]
#[view(table = "category")]
pub struct Shared {
    pub id: i64,
    pub name: String,
    #[view(to_one(fk = "parent_id", recursive = "cte"))]
    pub parent: Option<Arc<Shared>>,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "ring")]
pub struct Ring {
    pub id: i64,
    #[view(to_one(fk = "next_id", recursive = "cte"))]
    pub next: Option<Box<Ring>>,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "ring")]
pub struct RingLevels {
    pub id: i64,
    #[view(to_one(fk = "next_id", depth = 4))]
    pub next: Option<Box<RingLevels>>,
}

fn names(crumb: &Crumb) -> Vec<&str> {
    let mut out = vec![crumb.name.as_str()];
    out.extend(crumb.parent.as_deref().map(names).unwrap_or_default());
    out
}

fn near_names(near: &Near) -> Vec<&str> {
    let mut out = vec![near.name.as_str()];
    out.extend(near.parent.as_deref().map(near_names).unwrap_or_default());
    out
}

fn ring_ids(ring: &RingLevels) -> Vec<i64> {
    let mut ids = vec![ring.id];
    ids.extend(ring.next.as_deref().map(ring_ids).unwrap_or_default());
    ids
}

async fn chains<C: Conn>(conn: &mut C)
where
    <C::Backend as sqlx::Database>::Connection: Send,
    Crumb: ViewDecoder<C::Backend> + mabat::ViewEncoder<C::Backend>,
    Near: ViewDecoder<C::Backend>,
    Shared: ViewDecoder<C::Backend>,
    Ring: ViewDecoder<C::Backend>,
    RingLevels: ViewDecoder<C::Backend>,
{
    // Every category above, whatever the depth, in one query for all the chains
    let crumbs = mabat::load::<Crumb>().order_by("id").all(&mut *conn).await.unwrap();
    let all: Vec<Vec<&str>> = crumbs.iter().map(names).collect();
    assert_eq!(
        all,
        [
            vec!["Home"],
            vec!["Electronics", "Home"],
            vec!["Phones", "Electronics", "Home"],
            vec!["Android", "Phones", "Electronics", "Home"],
            vec!["Books", "Home"],
        ]
    );
    assert_eq!(mabat::plan::<Crumb>().unwrap().query_count(), 2);

    // Two levels up at most: the chain ends there
    let android = mabat::load::<Near>().by_key(4_i64).one(&mut *conn).await.unwrap();
    assert_eq!(near_names(&android), ["Android", "Phones", "Electronics"]);
    let phones = mabat::load::<Near>().by_key(3_i64).one(&mut *conn).await.unwrap();
    assert_eq!(near_names(&phones), ["Phones", "Electronics", "Home"]);

    // Shared: Home is decoded once, for both chains
    let shared = mabat::load::<Shared>().by_keys([2_i64, 5]).order_by("id").all(&mut *conn).await.unwrap();
    let home = |s: &Shared| s.parent.clone().unwrap();
    assert!(Arc::ptr_eq(&home(&shared[0]), &home(&shared[1])));

    // JSON, whole or as deep as a selection asks
    let json = mabat::load::<Crumb>().by_key(3_i64).json(&mut *conn).await.unwrap();
    assert_eq!(json[0]["parent"]["parent"]["name"], "Home");
    assert!(json[0]["parent"]["parent"]["parent"].is_null());
    let selection = Selection::parse("name parent { name }").unwrap();
    let json = mabat::load::<Crumb>().by_key(4_i64).select(selection).json(&mut *conn).await.unwrap();
    assert_eq!(json, [serde_json::json!({ "name": "Android", "parent": { "name": "Phones" } })]);

    // A loop in the data cannot be held by a chain of values; levels end it
    let looped = mabat::load::<Ring>().by_key(1_i64).one(&mut *conn).await.unwrap_err();
    assert!(matches!(looped, Error::Cycle { view: "Ring", .. }), "{looped}");
    let ring = mabat::load::<RingLevels>().by_key(1_i64).one(&mut *conn).await.unwrap();
    assert_eq!(ring_ids(&ring), [1, 2, 3, 1, 2]);

    // Saving writes the reference's key
    let electronics = mabat::load::<Crumb>().by_key(2_i64).one(&mut *conn).await.unwrap();
    let mut tablets = Crumb { id: 6, name: "Tablets".into(), parent: Some(Box::new(electronics)) };
    mabat::save(&mut tablets, &mut *conn).await.unwrap();
    let tablets = mabat::load::<Crumb>().by_key(6_i64).one(&mut *conn).await.unwrap();
    assert_eq!(names(&tablets), ["Tablets", "Electronics", "Home"]);

    // The generated queries prepare against the database
    let report = Mabat::<C::Backend>::builder()
        .register::<Crumb>()
        .register::<Near>()
        .register::<Shared>()
        .register::<Ring>()
        .check(&mut *conn)
        .await
        .unwrap();
    assert!(report.is_ok(), "{report}");
}

#[tokio::test]
async fn sqlite() {
    let dir = std::env::temp_dir().join(format!("mabat-chains-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.join("chains.db").display());
    let mut conn = sqlx::SqliteConnection::connect(&url).await.unwrap();
    conn.execute(SCHEMA).await.unwrap();
    chains(&mut conn).await;

    // Through GraphQL, as deep as the query asks
    let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
    let schema = mabat_graphql::schema(&pool).by_key::<Crumb>("category").finish().unwrap();
    let response = schema.execute("{ category(key: 4) { name parent { name parent { name } } } }").await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    let expected = serde_json::json!({
        "category": { "name": "Android", "parent": { "name": "Phones", "parent": { "name": "Electronics" } } }
    });
    assert_eq!(response.data.into_json().unwrap(), expected);
    pool.close().await;
    drop(conn);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn postgres() {
    let Ok(url) = std::env::var("MABAT_TEST_DATABASE_URL") else {
        eprintln!("skipping: MABAT_TEST_DATABASE_URL is not set (see scripts/with-postgres.sh)");
        return;
    };
    let mut conn = sqlx::PgConnection::connect(&url).await.unwrap();
    let schema = format!("mabat_chains_{}", std::process::id());
    let setup = format!(
        "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE; CREATE SCHEMA \"{schema}\"; SET search_path TO \"{schema}\""
    );
    conn.execute(AssertSqlSafe(setup)).await.unwrap();
    conn.execute(SCHEMA).await.unwrap();
    chains(&mut conn).await;
    conn.execute(AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE"))).await.unwrap();
}

#[tokio::test]
async fn mysql() {
    let Ok(url) = std::env::var("MABAT_TEST_MYSQL_URL") else {
        eprintln!("skipping: MABAT_TEST_MYSQL_URL is not set (see scripts/with-mysql.sh)");
        return;
    };
    let mut conn = sqlx::MySqlConnection::connect(&url).await.unwrap();
    let database = format!("mabat_chains_{}", std::process::id());
    let setup = format!("DROP DATABASE IF EXISTS `{database}`; CREATE DATABASE `{database}`; USE `{database}`");
    conn.execute(AssertSqlSafe(setup)).await.unwrap();
    conn.execute(SCHEMA).await.unwrap();
    chains(&mut conn).await;
    conn.execute(AssertSqlSafe(format!("DROP DATABASE `{database}`"))).await.unwrap();
}
