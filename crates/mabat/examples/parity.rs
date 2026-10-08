//! Compare loading with Mabat to hand-written SQLx code running the same queries.
//!
//! ```sh
//! MABAT_TEST_DATABASE_URL=postgres://... cargo run --release -p mabat --example parity
//! ```
//!
//! Creates a temporary schema with 1,000 tasks of 10 subtasks each, loads them all
//! repeatedly both ways, and prints the median times.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use mabat::View;
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection, Row};
use uuid::Uuid;

const ROOTS: usize = 1_000;
const CHILDREN: usize = 10;
const RUNS: usize = 20;

#[derive(View, Debug, PartialEq)]
#[view(table = "task")]
struct Task {
    id: Uuid,
    name: String,
    description: Option<String>,
    #[view(child(fk = "parent_id", order_by = "position"))]
    children: Vec<Subtask>,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "task")]
struct Subtask {
    id: Uuid,
    name: String,
    position: i32,
}

/// The same load, written by hand with SQLx.
async fn load_by_hand(conn: &mut PgConnection, keys: &[Uuid]) -> Result<Vec<Task>, sqlx::Error> {
    let rows = sqlx::query("SELECT id, name, description FROM task WHERE id = ANY($1)")
        .bind(keys)
        .fetch_all(&mut *conn)
        .await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.get("id")).collect();

    let child_rows =
        sqlx::query("SELECT id, parent_id, name, position FROM task WHERE parent_id = ANY($1) ORDER BY position, id")
            .bind(&ids)
            .fetch_all(&mut *conn)
            .await?;
    let mut children: HashMap<Uuid, Vec<Subtask>> = HashMap::new();
    for row in &child_rows {
        children.entry(row.try_get("parent_id")?).or_default().push(Subtask {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            position: row.try_get("position")?,
        });
    }

    rows.iter()
        .map(|row| {
            let id: Uuid = row.try_get("id")?;
            Ok(Task {
                id,
                name: row.try_get("name")?,
                description: row.try_get("description")?,
                children: children.remove(&id).unwrap_or_default(),
            })
        })
        .collect()
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("MABAT_TEST_DATABASE_URL").map_err(|_| "set MABAT_TEST_DATABASE_URL")?;
    let mut conn = PgConnection::connect(&url).await?;
    let schema = format!("mabat_parity_{}", Uuid::new_v4().simple());
    conn.execute(AssertSqlSafe(format!(
        "CREATE SCHEMA \"{schema}\"; SET search_path TO \"{schema}\";
         CREATE TABLE task (id UUID PRIMARY KEY, name TEXT NOT NULL, description TEXT,
                            parent_id UUID REFERENCES task (id), position INTEGER NOT NULL DEFAULT 0);
         CREATE INDEX ON task (parent_id);
         INSERT INTO task (id, name, description)
             SELECT md5('root' || r)::uuid, 'Task ' || r, 'Description ' || r FROM generate_series(1, {ROOTS}) r;
         INSERT INTO task (id, name, parent_id, position)
             SELECT md5('child' || r || '-' || c)::uuid, 'Subtask ' || c, md5('root' || r)::uuid, {CHILDREN} - c
             FROM generate_series(1, {ROOTS}) r, generate_series(1, {CHILDREN}) c;
         ANALYZE task;"
    )))
    .await?;

    let roots: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM task WHERE parent_id IS NULL").fetch_all(&mut conn).await?;

    let by_hand = load_by_hand(&mut conn, &roots).await?;
    let mut with_mabat = mabat::load::<Task>().by_keys(roots.clone()).all(&mut conn).await?;
    with_mabat.sort_by_key(|t| by_hand.iter().position(|h| h.id == t.id));
    assert_eq!(with_mabat, by_hand, "both loads return the same values");

    let mut mabat_times = Vec::new();
    let mut hand_times = Vec::new();
    for _ in 0..RUNS {
        let start = Instant::now();
        let tasks = mabat::load::<Task>().by_keys(roots.clone()).all(&mut conn).await?;
        mabat_times.push(start.elapsed());
        assert_eq!(tasks.len(), ROOTS);

        let start = Instant::now();
        let tasks = load_by_hand(&mut conn, &roots).await?;
        hand_times.push(start.elapsed());
        assert_eq!(tasks.len(), ROOTS);
    }

    let mabat_time = median(mabat_times);
    let hand_time = median(hand_times);
    println!("{ROOTS} tasks x {CHILDREN} subtasks, median of {RUNS} runs");
    println!("  mabat:      {mabat_time:?}");
    println!("  hand-written: {hand_time:?}");
    println!("  overhead:     {:+.1}%", (mabat_time.as_secs_f64() / hand_time.as_secs_f64() - 1.0) * 100.0);

    conn.execute(AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE"))).await?;
    Ok(())
}
