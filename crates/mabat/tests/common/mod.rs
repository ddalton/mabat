//! Test database helper.
//!
//! Tests run against the database in `MABAT_TEST_DATABASE_URL`, each in its own schema.
//! Use `scripts/with-postgres.sh` to run them against a throwaway PostgreSQL cluster.
//! Without the variable, database tests are skipped.

#![allow(dead_code)]

pub mod fixture;

use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection};

pub struct TestDb {
    pub conn: PgConnection,
    schema: String,
}

impl TestDb {
    /// Connect and create a schema for the test, or `None` if no test database is configured.
    pub async fn new(name: &str, ddl: &str) -> Option<TestDb> {
        let Ok(url) = std::env::var("MABAT_TEST_DATABASE_URL") else {
            eprintln!("skipping {name}: MABAT_TEST_DATABASE_URL is not set (see scripts/with-postgres.sh)");
            return None;
        };
        let mut conn = PgConnection::connect(&url).await.expect("connect to the test database");
        let schema = format!("mabat_{name}_{}", uuid::Uuid::new_v4().simple());
        conn.execute(AssertSqlSafe(format!("CREATE SCHEMA \"{schema}\"; SET search_path TO \"{schema}\"")))
            .await
            .expect("create the test schema");
        conn.execute(AssertSqlSafe(ddl.to_string())).await.expect("create the test tables");
        Some(TestDb { conn, schema })
    }

    pub async fn execute(&mut self, sql: &str) {
        self.conn.execute(AssertSqlSafe(sql.to_string())).await.unwrap_or_else(|e| panic!("{e}\n  sql: {sql}"));
    }

    pub async fn drop(mut self) {
        self.conn
            .execute(AssertSqlSafe(format!("DROP SCHEMA \"{}\" CASCADE", self.schema)))
            .await
            .expect("drop the test schema");
    }
}
