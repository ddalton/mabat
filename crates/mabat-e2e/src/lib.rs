//! End-to-end tests of Mabat against two well-known sample databases, both MIT licensed:
//!
//! - [`pagila`]: Pagila, a DVD rental store, written for PostgreSQL. It has a PostgreSQL
//!   enum, a domain, arrays, `numeric` money, partitioned tables, many-to-many link tables
//!   and a reference cycle between stores and their staff.
//! - [`chinook`]: Chinook, a digital music store. It has a reporting hierarchy, invoices
//!   with lines, and playlists of tracks.
//!
//! Each dataset is loaded once per database into a schema named after a hash of its SQL,
//! and the tests only read it.

use std::hash::Hasher;

use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection};

pub mod chinook;
pub mod pagila;

/// A sample database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dataset {
    Pagila,
    Chinook,
}

const PAGILA_SCHEMA: &str = include_str!("../data/pagila/pagila-schema.sql");
const PAGILA_DATA: &str = include_str!("../data/pagila/pagila-insert-data.sql");
const CHINOOK: &str = include_str!("../data/chinook/Chinook_PostgreSql.sql");

impl Dataset {
    pub fn name(self) -> &'static str {
        match self {
            Dataset::Pagila => "pagila",
            Dataset::Chinook => "chinook",
        }
    }

    /// The SQL that creates and fills the dataset in the current schema (the first schema
    /// of the `search_path`).
    pub fn sql(self) -> String {
        match self {
            // A pg_dump of the public schema: drop what refers to the public schema or to
            // roles, so that it loads into any schema
            Dataset::Pagila => {
                let keep = |line: &&str| {
                    !line.contains("pg_catalog.set_config('search_path'")
                        && !line.contains(" OWNER TO ")
                        && !line.contains("ON SCHEMA public")
                };
                let schema: Vec<&str> = PAGILA_SCHEMA.lines().filter(keep).collect();
                let data: Vec<&str> = PAGILA_DATA.lines().filter(keep).collect();
                format!("{}\n{}", schema.join("\n"), data.join("\n")).replace("public.", "")
            }
            // Skip the CREATE DATABASE and \c lines before the first table
            Dataset::Chinook => CHINOOK[CHINOOK.find("CREATE TABLE").expect("Chinook creates tables")..].to_string(),
        }
    }

    /// The schema the dataset is loaded into: its name and a hash of its SQL, so a changed
    /// dataset is loaded again.
    pub fn schema(self) -> String {
        let mut hash = Fnv1a::default();
        hash.write(self.sql().as_bytes());
        format!("mabat_e2e_{}_{:016x}", self.name(), hash.finish())
    }

    /// Connect to the database of `MABAT_TEST_DATABASE_URL` with the dataset loaded and its
    /// schema first on the `search_path`. `None` if the variable is not set.
    pub async fn connect(self) -> Option<PgConnection> {
        let Ok(url) = std::env::var("MABAT_TEST_DATABASE_URL") else {
            eprintln!("skipping: MABAT_TEST_DATABASE_URL is not set (see scripts/with-postgres.sh)");
            return None;
        };
        let mut conn = PgConnection::connect(&url).await.expect("connect to the test database");
        let schema = self.schema();
        self.ensure_loaded(&mut conn, &schema).await;
        conn.execute(AssertSqlSafe(format!("SET search_path TO \"{schema}\""))).await.expect("set the search path");
        Some(conn)
    }

    /// Load the dataset unless it is loaded, under an advisory lock so that concurrent tests
    /// and processes load it once.
    async fn ensure_loaded(self, conn: &mut PgConnection, schema: &str) {
        sqlx::query("SELECT pg_advisory_lock(hashtext($1))").bind(schema).execute(&mut *conn).await.unwrap();
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)")
            .bind(schema)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        if !exists {
            let mut tx = conn.begin().await.unwrap();
            tx.execute(AssertSqlSafe(format!("CREATE SCHEMA \"{schema}\"; SET LOCAL search_path TO \"{schema}\"")))
                .await
                .unwrap();
            tx.execute(AssertSqlSafe(self.sql())).await.unwrap_or_else(|e| panic!("load {}: {e}", self.name()));
            tx.commit().await.unwrap();
        }
        sqlx::query("SELECT pg_advisory_unlock(hashtext($1))").bind(schema).execute(&mut *conn).await.unwrap();
    }
}

/// FNV-1a, a hash that is the same on every platform and Rust version.
struct Fnv1a(u64);

impl Default for Fnv1a {
    fn default() -> Self {
        Fnv1a(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for Fnv1a {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}
