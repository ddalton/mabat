//! Streaming loads on every database: the same values as `all`, a batch at a time, with
//! filters, order and pages, overrides, JSON and selections; roots deleted between batches,
//! one snapshot in a repeatable-read transaction, and errors and early drops.

use futures_util::{StreamExt, TryStreamExt};
use mabat::filter::col;
use mabat::{Conn, Error, Mabat, Ref, Selection, View, ViewDecoder};
use sqlx::{AssertSqlSafe, Connection, Executor};

const TASKS: i64 = 2500;

const SCHEMA: &str = r#"
CREATE TABLE person (id BIGINT PRIMARY KEY, name VARCHAR(100) NOT NULL);
CREATE TABLE task (
    id BIGINT PRIMARY KEY, name VARCHAR(100) NOT NULL, priority INT NOT NULL,
    owner_id BIGINT REFERENCES person (id)
);
CREATE TABLE note (id BIGINT PRIMARY KEY, task_id BIGINT NOT NULL REFERENCES task (id), body VARCHAR(100) NOT NULL);
"#;

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "task")]
pub struct TaskView {
    pub id: i64,
    pub name: String,
    pub priority: i32,
    #[view(to_one(fk = "owner_id"))]
    pub owner: Option<Person>,
    #[view(child(fk = "task_id", order_by = "id"))]
    pub notes: Vec<Note>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "person")]
pub struct Person {
    pub id: i64,
    pub name: String,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "note")]
pub struct Note {
    pub id: i64,
    pub body: String,
}

/// A view into a graph, which cannot be streamed.
#[derive(View, Debug)]
#[view(table = "task")]
pub struct TaskNode {
    pub id: i64,
    #[view(to_one(fk = "owner_id"))]
    pub owner: Option<Ref<PersonNode>>,
}

#[derive(View, Debug)]
#[view(table = "person")]
pub struct PersonNode {
    pub id: i64,
    pub name: String,
}

/// The rows: 10 people, 2,500 tasks owned by them (every seventh by nobody), 2 notes each.
fn data() -> Vec<String> {
    let people: Vec<String> = (1..=10).map(|i| format!("({i}, 'Person {i}')")).collect();
    let tasks: Vec<String> = (1..=TASKS)
        .map(|i| {
            let owner = if i % 7 == 0 { "NULL".to_string() } else { (i % 10 + 1).to_string() };
            format!("({i}, 'Task {i}', {}, {owner})", i % 5)
        })
        .collect();
    let notes: Vec<String> =
        (1..=TASKS).flat_map(|i| [2 * i - 1, 2 * i].map(|n| format!("({n}, {i}, 'Note {n}')"))).collect();
    vec![
        format!("INSERT INTO person (id, name) VALUES {}", people.join(", ")),
        format!("INSERT INTO task (id, name, priority, owner_id) VALUES {}", tasks.join(", ")),
        format!("INSERT INTO note (id, task_id, body) VALUES {}", notes.join(", ")),
    ]
}

async fn setup<C: Connection>(conn: &mut C)
where
    for<'e> &'e mut C: Executor<'e, Database = C::Database>,
{
    conn.execute(SCHEMA).await.unwrap();
    for insert in data() {
        conn.execute(AssertSqlSafe(insert)).await.unwrap();
    }
}

/// Every task, streamed, is every task loaded at once, with any batch size.
async fn same_as_all<C: Conn>(conn: &mut C)
where
    TaskView: ViewDecoder<C::Backend>,
    TaskNode: ViewDecoder<C::Backend>,
{
    let all = mabat::load::<TaskView>().order_by("id").all(conn).await.unwrap();
    assert_eq!(all.len(), TASKS as usize);
    assert_eq!(all[1].notes.len(), 2);
    for batch_size in [1000, 7, TASKS as usize * 2] {
        let streamed: Vec<TaskView> =
            mabat::load::<TaskView>().order_by("id").batch_size(batch_size).stream(conn).try_collect().await.unwrap();
        assert!(streamed == all, "batch size {batch_size}");
    }

    // Filtered, ordered and paged, as `all` does
    let query = || {
        mabat::load::<TaskView>()
            .filter(col("priority").ge(2))
            .order_by_desc("priority")
            .order_by("id")
            .limit(1200)
            .offset(30)
    };
    let all = query().all(conn).await.unwrap();
    assert_eq!(all.len(), 1200);
    let streamed: Vec<TaskView> = query().batch_size(500).stream(conn).try_collect().await.unwrap();
    assert!(streamed == all);

    // By keys, in the order of the root query
    let keys = [2400, 3, 77, 9999];
    let streamed: Vec<TaskView> =
        mabat::load::<TaskView>().by_keys(keys).order_by("id").batch_size(2).stream(conn).try_collect().await.unwrap();
    assert_eq!(streamed.iter().map(|t| t.id).collect::<Vec<_>>(), [3, 77, 2400]);
    let none: Vec<TaskView> =
        mabat::load::<TaskView>().by_keys(Vec::<i64>::new()).stream(conn).try_collect().await.unwrap();
    assert!(none.is_empty());

    // JSON, of a selection, as `json` writes it
    let selection = || Selection::parse("name owner { name } notes { body }").unwrap();
    let json = mabat::load::<TaskView>().select(selection()).order_by("id").limit(50).json(conn).await.unwrap();
    let streamed: Vec<serde_json::Value> = mabat::load::<TaskView>()
        .select(selection())
        .order_by("id")
        .limit(50)
        .batch_size(16)
        .json_stream(conn)
        .try_collect()
        .await
        .unwrap();
    assert_eq!(streamed, json);
    // A graph view needs a selection to stream as JSON
    let nodes: Vec<serde_json::Value> = mabat::load::<TaskNode>()
        .select(Selection::parse("id owner { name }").unwrap())
        .order_by("id")
        .limit(3)
        .json_stream(conn)
        .try_collect()
        .await
        .unwrap();
    assert_eq!(nodes[0]["owner"]["name"], "Person 2");

    // What cannot be streamed is an error, yielded once
    let mut graph = Box::pin(mabat::load::<TaskNode>().stream(conn));
    assert!(matches!(graph.next().await, Some(Err(Error::GraphRequired { view: "TaskNode" }))));
    assert!(graph.next().await.is_none());
    drop(graph);
    let mut selected = Box::pin(mabat::load::<TaskView>().select(selection()).stream(conn));
    assert!(matches!(selected.next().await, Some(Err(Error::SelectionWithoutJson { .. }))));
    assert!(selected.next().await.is_none());
    drop(selected);
    let mut unknown = Box::pin(mabat::load::<TaskView>().filter(col("nope").eq(1)).stream(conn));
    assert!(matches!(unknown.next().await, Some(Err(Error::Query { .. }))), "an unknown column fails the keys query");
    assert!(unknown.next().await.is_none());
    drop(unknown);

    // A stream dropped early gives the connection back
    let mut first = Box::pin(mabat::load::<TaskView>().order_by("id").batch_size(100).stream(conn));
    assert_eq!(first.next().await.unwrap().unwrap().id, 1);
    drop(first);
    assert_eq!(mabat::load::<TaskView>().count(conn).await.unwrap(), TASKS);
}

/// Overrides of the root and child queries apply to every batch.
async fn overrides<C: Conn>(conn: &mut C, quote: char, keys: &str)
where
    TaskView: ViewDecoder<C::Backend>,
{
    let q = |alias: &str| format!("{quote}{alias}{quote}");
    let toml = format!(
        "[query.\"$root\"]\nsql = '''SELECT t.id AS id, t.name AS name, t.priority AS priority, t.owner_id AS {} FROM \
         task t WHERE t.priority < 4'''\n\n[query.notes]\nsql = '''SELECT n.id AS id, n.body AS body, n.task_id AS {} \
         FROM note n WHERE n.task_id {keys} AND n.id % 2 = 0'''\n",
        q("$ref.owner"),
        q("$parent"),
    );
    let mabat = Mabat::builder().register::<TaskView>().overrides("TaskView", toml).build(conn).await.unwrap();
    assert!(mabat.report().is_ok(), "{}", mabat.report());
    let all = mabat.load::<TaskView>().order_by("id").all(conn).await.unwrap();
    assert_eq!(all.len(), 2000);
    assert!(all.iter().all(|t| t.notes.len() == 1 && t.notes[0].id % 2 == 0));
    let streamed: Vec<TaskView> =
        mabat.load::<TaskView>().order_by("id").batch_size(300).stream(conn).try_collect().await.unwrap();
    assert!(streamed == all);
}

/// A root deleted after the keys were read is skipped by a later batch. `present` tasks exist.
async fn deleted_between_batches<C: Conn, O: Connection>(conn: &mut C, other: &mut O, present: usize)
where
    TaskView: ViewDecoder<C::Backend>,
    for<'e> &'e mut O: Executor<'e, Database = O::Database>,
{
    let mut stream = Box::pin(mabat::load::<TaskView>().order_by("id").batch_size(1000).stream(conn));
    assert_eq!(stream.next().await.unwrap().unwrap().id, 1);
    other.execute("DELETE FROM note WHERE task_id = 2400").await.unwrap();
    other.execute("DELETE FROM task WHERE id = 2400").await.unwrap();
    let rest: Vec<TaskView> = stream.try_collect().await.unwrap();
    assert_eq!(rest.len(), present - 2);
    assert!(!rest.iter().any(|t| t.id == 2400));
}

/// In a repeatable-read transaction, every batch sees the snapshot of the first.
async fn one_snapshot<C: Conn, O: Connection>(conn: &mut C, other: &mut O)
where
    TaskView: ViewDecoder<C::Backend>,
    for<'e> &'e mut O: Executor<'e, Database = O::Database>,
{
    let mut stream = Box::pin(mabat::load::<TaskView>().order_by("id").batch_size(1000).stream(conn));
    assert_eq!(stream.next().await.unwrap().unwrap().id, 1);
    other.execute("DELETE FROM note WHERE task_id = 2401").await.unwrap();
    other.execute("DELETE FROM task WHERE id = 2401").await.unwrap();
    let rest: Vec<TaskView> = stream.try_collect().await.unwrap();
    assert_eq!(rest.len(), TASKS as usize - 1);
    assert!(rest.iter().any(|t| t.id == 2401 && t.notes.len() == 2));
}

#[tokio::test]
async fn sqlite() {
    let dir = std::env::temp_dir().join(format!("mabat-stream-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.join("stream.db").display());
    let mut conn = sqlx::SqliteConnection::connect(&url).await.unwrap();
    setup(&mut conn).await;
    same_as_all(&mut conn).await;
    overrides(&mut conn, '"', "IN (:keys)").await;
    let mut other = sqlx::SqliteConnection::connect(&url).await.unwrap();
    deleted_between_batches(&mut conn, &mut other, TASKS as usize).await;
    drop((conn, other));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn postgres() {
    let Ok(url) = std::env::var("MABAT_TEST_DATABASE_URL") else {
        eprintln!("skipping: MABAT_TEST_DATABASE_URL is not set (see scripts/with-postgres.sh)");
        return;
    };
    let schema = format!("mabat_stream_{}", std::process::id());
    let connect = || async {
        let mut conn = sqlx::PgConnection::connect(&url).await.unwrap();
        conn.execute(AssertSqlSafe(format!("SET search_path TO \"{schema}\""))).await.unwrap();
        conn
    };
    let mut conn = sqlx::PgConnection::connect(&url).await.unwrap();
    let setup_sql = format!(
        "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE; CREATE SCHEMA \"{schema}\"; SET search_path TO \"{schema}\""
    );
    conn.execute(AssertSqlSafe(setup_sql)).await.unwrap();
    setup(&mut conn).await;
    same_as_all(&mut conn).await;
    overrides(&mut conn, '"', "= ANY(:keys)").await;

    // Concurrently on pooled connections, in one snapshot, and on a task of its own
    let options: sqlx::postgres::PgConnectOptions = url.parse().unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options.options([("search_path", schema.as_str())]))
        .await
        .unwrap();
    let all = mabat::load::<TaskView>().order_by("id").all(&mut conn).await.unwrap();
    let mut pooled = mabat::Pooled::snapshot(&pool, 3);
    let streamed: Vec<TaskView> =
        mabat::load::<TaskView>().order_by("id").batch_size(400).stream(&mut pooled).try_collect().await.unwrap();
    assert!(streamed == all);
    let spawned = tokio::spawn({
        let pool = pool.clone();
        async move {
            let mut conn = pool.acquire().await.unwrap();
            mabat::load::<TaskView>().stream(&mut conn).try_fold(0, |n, _| async move { Ok(n + 1) }).await.unwrap()
        }
    });
    assert_eq!(spawned.await.unwrap(), TASKS);
    pool.close().await;

    let mut other = connect().await;
    conn.execute("BEGIN ISOLATION LEVEL REPEATABLE READ").await.unwrap();
    one_snapshot(&mut conn, &mut other).await;
    conn.execute("ROLLBACK").await.unwrap();
    // The snapshot's transaction is over: task 2401 is gone
    deleted_between_batches(&mut conn, &mut other, TASKS as usize - 1).await;
    drop(other);
    conn.execute(AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE"))).await.unwrap();
}

#[tokio::test]
async fn mysql() {
    let Ok(url) = std::env::var("MABAT_TEST_MYSQL_URL") else {
        eprintln!("skipping: MABAT_TEST_MYSQL_URL is not set (see scripts/with-mysql.sh)");
        return;
    };
    let database = format!("mabat_stream_{}", std::process::id());
    let mut conn = sqlx::MySqlConnection::connect(&url).await.unwrap();
    let setup_sql = format!("DROP DATABASE IF EXISTS `{database}`; CREATE DATABASE `{database}`; USE `{database}`");
    conn.execute(AssertSqlSafe(setup_sql)).await.unwrap();
    setup(&mut conn).await;
    same_as_all(&mut conn).await;
    overrides(&mut conn, '`', "IN (:keys)").await;

    let mut other = sqlx::MySqlConnection::connect(&url).await.unwrap();
    other.execute(AssertSqlSafe(format!("USE `{database}`"))).await.unwrap();
    conn.execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ").await.unwrap();
    conn.execute("START TRANSACTION WITH CONSISTENT SNAPSHOT").await.unwrap();
    one_snapshot(&mut conn, &mut other).await;
    conn.execute("ROLLBACK").await.unwrap();
    // The snapshot's transaction is over: task 2401 is gone
    deleted_between_batches(&mut conn, &mut other, TASKS as usize - 1).await;
    drop(other);
    conn.execute(AssertSqlSafe(format!("DROP DATABASE `{database}`"))).await.unwrap();
}
