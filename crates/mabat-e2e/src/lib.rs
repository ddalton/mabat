//! End-to-end tests of Mabat against two well-known sample databases, both MIT licensed:
//!
//! - [`pagila`]: Pagila, a DVD rental store, written for PostgreSQL. It has a PostgreSQL
//!   enum, a domain, arrays, `numeric` money, partitioned tables, many-to-many link tables
//!   and a reference cycle between stores and their staff.
//! - [`chinook`]: Chinook, a digital music store. It has a reporting hierarchy, invoices
//!   with lines, and playlists of tracks.
//!
//! On PostgreSQL, each dataset is loaded once per database into a schema named after a hash
//! of its SQL, and the tests only read it. Chinook also runs on MySQL, loaded once into a
//! database named the same way, with the views of [`chinook`], and on SQLite, loaded into a
//! new in-memory database per test, with the views of [`chinook_sqlite`].

use std::hash::Hasher;

use sqlx::{AssertSqlSafe, Connection, Executor, MySqlConnection, PgConnection, SqliteConnection};

pub mod chinook;
pub mod chinook_sqlite;
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

    /// The SQL that creates and fills Chinook on SQLite, translated from the PostgreSQL
    /// script: without the foreign keys added by `ALTER TABLE`, which SQLite does not
    /// support, without the `N` of national string literals, with ISO dates, and with money
    /// as `REAL`, since SQLite has no decimal type.
    pub fn sqlite_sql(self) -> String {
        assert_eq!(self, Dataset::Chinook, "only Chinook runs on SQLite");
        let sql = Dataset::Chinook.sql();
        let mut statements = Vec::new();
        let mut skip = false;
        for line in sql.lines() {
            // An ALTER TABLE statement spans two lines, up to the semicolon
            if line.starts_with("ALTER TABLE") {
                skip = true;
            }
            if !skip {
                statements.push(line);
            }
            if skip && line.trim_end().ends_with(';') {
                skip = false;
            }
        }
        translate_literals(&statements.join("\n"), false).replace("NUMERIC(10,2)", "REAL")
    }

    /// The SQL that creates and fills Chinook on MySQL, translated from the PostgreSQL
    /// script: without the `N` of national string literals, with ISO dates and escaped
    /// backslashes, and with `DATETIME` for `TIMESTAMP`, whose range starts in 1970.
    pub fn mysql_sql(self) -> String {
        assert_eq!(self, Dataset::Chinook, "only Chinook runs on MySQL");
        translate_literals(&Dataset::Chinook.sql(), true).replace(" TIMESTAMP", " DATETIME")
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

impl Dataset {
    /// Connect to the MySQL server of `MABAT_TEST_MYSQL_URL` and use a database with the
    /// dataset loaded, named like its PostgreSQL schema. `None` if the variable is not set.
    pub async fn connect_mysql(self) -> Option<MySqlConnection> {
        let Ok(url) = std::env::var("MABAT_TEST_MYSQL_URL") else {
            eprintln!("skipping: MABAT_TEST_MYSQL_URL is not set (see scripts/with-mysql.sh)");
            return None;
        };
        let mut conn = MySqlConnection::connect(&url).await.expect("connect to the MySQL test server");
        let database = self.schema();
        // Loaded once, under a lock, and marked as loaded by a table created last
        sqlx::query("SELECT GET_LOCK(?, 600)").bind(&database).execute(&mut conn).await.unwrap();
        let loaded: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.tables WHERE table_schema = ? AND table_name = 'mabat_loaded'",
        )
        .bind(&database)
        .fetch_one(&mut conn)
        .await
        .unwrap();
        if loaded == 0 {
            let setup = format!("DROP DATABASE IF EXISTS `{database}`; CREATE DATABASE `{database}`; USE `{database}`");
            conn.execute(AssertSqlSafe(setup)).await.unwrap();
            conn.execute(AssertSqlSafe(self.mysql_sql())).await.unwrap_or_else(|e| panic!("load {}: {e}", self.name()));
            conn.execute("CREATE TABLE mabat_loaded (id INT)").await.unwrap();
        }
        sqlx::query("SELECT RELEASE_LOCK(?)").bind(&database).execute(&mut conn).await.unwrap();
        conn.execute(AssertSqlSafe(format!("USE `{database}`"))).await.unwrap();
        Some(conn)
    }

    /// A pool of up to `connections` connections to the database of
    /// `MABAT_TEST_DATABASE_URL`, with the dataset loaded and its schema first on the
    /// `search_path`. `None` if the variable is not set.
    pub async fn pg_pool(self, connections: u32) -> Option<sqlx::PgPool> {
        drop(self.connect().await?);
        let schema = self.schema();
        let url = std::env::var("MABAT_TEST_DATABASE_URL").ok()?;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(connections)
            .after_connect(move |conn, _| {
                let set = format!("SET search_path TO \"{schema}\"");
                Box::pin(async move { conn.execute(AssertSqlSafe(set)).await.map(drop) })
            })
            .connect(&url)
            .await
            .expect("connect to the test database");
        Some(pool)
    }

    /// A pool of up to `connections` connections to the MySQL database of the dataset, see
    /// [`Dataset::connect_mysql`]. `None` if `MABAT_TEST_MYSQL_URL` is not set.
    pub async fn mysql_pool(self, connections: u32) -> Option<sqlx::MySqlPool> {
        drop(self.connect_mysql().await?);
        let database = self.schema();
        let url = std::env::var("MABAT_TEST_MYSQL_URL").ok()?;
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(connections)
            .after_connect(move |conn, _| {
                let use_database = format!("USE `{database}`");
                Box::pin(async move { conn.execute(AssertSqlSafe(use_database)).await.map(drop) })
            })
            .connect(&url)
            .await
            .expect("connect to the MySQL test server");
        Some(pool)
    }

    /// A pool of up to `connections` connections to a new SQLite database file with the
    /// dataset loaded, which is removed when the returned guard is dropped. In-memory
    /// databases are one per connection, so a pool needs a file.
    pub async fn sqlite_pool(self, connections: u32) -> (sqlx::SqlitePool, TempFile) {
        let file = TempFile(std::env::temp_dir().join(format!(
            "mabat-e2e-{}-{}-{}.db",
            self.name(),
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        )));
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&file.0)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(connections)
            .connect_with(options)
            .await
            .expect("create an SQLite database file");
        let mut tx = pool.begin().await.unwrap();
        tx.execute(AssertSqlSafe(self.sqlite_sql())).await.unwrap_or_else(|e| panic!("load {}: {e}", self.name()));
        tx.commit().await.unwrap();
        (pool, file)
    }

    /// A new in-memory SQLite database with the dataset loaded.
    pub async fn connect_sqlite(self) -> SqliteConnection {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.expect("open an in-memory SQLite database");
        let mut tx = conn.begin().await.unwrap();
        tx.execute(AssertSqlSafe(self.sqlite_sql())).await.unwrap_or_else(|e| panic!("load {}: {e}", self.name()));
        tx.commit().await.unwrap();
        conn
    }
}

/// A file removed when dropped, with the files SQLite keeps next to it.
pub struct TempFile(pub std::path::PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut path = self.0.clone().into_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Drop the `N` prefix of string literals, write `2021/1/2` dates as `2021-01-02 00:00:00`,
/// and escape backslashes, which are escapes in MySQL literals.
fn translate_literals(sql: &str, escape_backslashes: bool) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if c == 'N' && chars.peek() == Some(&'\'') && !out.ends_with(|p: char| p.is_alphanumeric() || p == '_') {
            continue;
        }
        if c != '\'' {
            out.push(c);
            continue;
        }
        // A literal, where '' is a quote
        let mut literal = String::new();
        while let Some(c) = chars.next() {
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    literal.push_str("''");
                    continue;
                }
                break;
            }
            if c == '\\' && escape_backslashes {
                literal.push('\\');
            }
            literal.push(c);
        }
        out.push('\'');
        out.push_str(&iso_date(&literal).unwrap_or(literal));
        out.push('\'');
    }
    out
}

fn iso_date(literal: &str) -> Option<String> {
    let parts: Vec<&str> = literal.split('/').collect();
    let [year, month, day] = parts[..] else { return None };
    let number = |part: &str| part.parse::<u32>().ok().filter(|_| part.bytes().all(|b| b.is_ascii_digit()));
    let (year, month, day) = (number(year)?, number(month)?, number(day)?);
    (year >= 1000).then(|| format!("{year:04}-{month:02}-{day:02} 00:00:00"))
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
