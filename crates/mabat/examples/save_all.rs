//! Compare saving many aggregates with `save`, one after the other, to `save_all`.
//!
//! ```sh
//! MABAT_TEST_DATABASE_URL=postgres://... cargo run --release -p mabat --example save_all
//! ```
//!
//! Creates a temporary schema, saves 1,000 tasks of 10 subtasks each both ways, as new rows and
//! again as rows that exist, and prints the times.

use std::time::{Duration, Instant};

use mabat::View;
use mabat::filter::col;
use sqlx::{Connection, Executor, PgConnection};

const ROOTS: i64 = 1_000;
const CHILDREN: i64 = 10;

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "task")]
struct Task {
    id: i64,
    name: String,
    description: Option<String>,
    #[view(child(fk = "parent_id", order_by = "position"))]
    children: Vec<Subtask>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "task")]
struct Subtask {
    id: i64,
    name: String,
    position: i32,
}

fn tasks() -> Vec<Task> {
    (0..ROOTS)
        .map(|i| {
            let id = i * 100;
            Task {
                id,
                name: format!("task {id}"),
                description: Some("a task".into()),
                children: (1..=CHILDREN)
                    .map(|c| Subtask { id: id + c, name: format!("subtask {c}"), position: c as i32 })
                    .collect(),
            }
        })
        .collect()
}

async fn one_by_one(conn: &mut PgConnection, values: &mut [Task]) -> Duration {
    let start = Instant::now();
    for value in values {
        mabat::save(value, &mut *conn).await.unwrap();
    }
    start.elapsed()
}

async fn all_at_once(conn: &mut PgConnection, values: &mut [Task]) -> Duration {
    let start = Instant::now();
    mabat::save_all(values, conn).await.unwrap();
    start.elapsed()
}

#[tokio::main]
async fn main() {
    let url = std::env::var("MABAT_TEST_DATABASE_URL").expect("MABAT_TEST_DATABASE_URL is the database to use");
    let mut conn = PgConnection::connect(&url).await.unwrap();
    let schema = format!("mabat_save_all_{}", std::process::id());
    let setup = format!(
        "CREATE SCHEMA \"{schema}\"; SET search_path TO \"{schema}\";
         CREATE TABLE task (id BIGINT PRIMARY KEY, name TEXT NOT NULL, description TEXT,
                            parent_id BIGINT REFERENCES task (id), position INT);"
    );
    conn.execute(sqlx::AssertSqlSafe(setup)).await.unwrap();
    let rows = ROOTS * (CHILDREN + 1);

    let mut values = tasks();
    let new = one_by_one(&mut conn, &mut values).await;
    let existing = one_by_one(&mut conn, &mut values).await;
    println!("save, one by one:   {new:>10.1?} new, {existing:>10.1?} existing ({rows} rows)");

    conn.execute("TRUNCATE task").await.unwrap();
    let new = all_at_once(&mut conn, &mut values).await;
    let existing = all_at_once(&mut conn, &mut values).await;
    println!("save_all:           {new:>10.1?} new, {existing:>10.1?} existing ({rows} rows)");

    let roots = mabat::load::<Task>().filter(col("parent_id").is_null()).order_by("id").all(&mut conn).await.unwrap();
    assert_eq!(roots, values);
    conn.execute(sqlx::AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE"))).await.unwrap();
}
