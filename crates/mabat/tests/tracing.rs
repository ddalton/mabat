//! The spans of a load and a save, as a `tracing` subscriber sees them, on SQLite: the
//! operation, each query with its view, name, override and rows, and each statement with its SQL.
#![cfg(feature = "sqlite")]

use std::io::Write;
use std::sync::{Arc, Mutex};

use mabat::{Mabat, View};
use sqlx::{Connection, Executor, SqliteConnection};
use tracing_subscriber::fmt::format::FmtSpan;

#[derive(View, Debug)]
#[view(table = "project")]
pub struct Project {
    pub id: i64,
    pub name: String,
    #[view(child(fk = "project_id", order_by = "id"))]
    pub tasks: Vec<Task>,
}

#[derive(View, Debug)]
#[view(table = "task")]
pub struct Task {
    pub id: i64,
    pub title: String,
}

/// What the subscriber writes.
#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn loads_and_saves_are_traced() {
    let output = Output::default();
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(move || writer.clone())
        .finish();
    let _default = tracing::subscriber::set_default(subscriber);

    let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    conn.execute(
        "CREATE TABLE project (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
         CREATE TABLE task (id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL, title TEXT NOT NULL);
         INSERT INTO project VALUES (1, 'Mabat');
         INSERT INTO task VALUES (1, 1, 'Trace'), (2, 1, 'Test');",
    )
    .await
    .unwrap();
    let overrides = "-- mabat: query tasks\nSELECT t.id AS \"id\", t.project_id AS \"$parent\", t.title AS \"title\" \
                     FROM task t WHERE t.project_id IN (:keys) ORDER BY t.id";
    let mabat = Mabat::builder().register::<Project>().overrides_sql("Project", overrides).build(&mut conn).await;
    let mabat = mabat.unwrap();
    let project = mabat.load::<Project>().by_key(1_i64).one(&mut conn).await.unwrap();
    assert_eq!(project.tasks.len(), 2);
    let mut renamed = Project { name: "Mabat 0.1".into(), ..project };
    mabat::save(&mut renamed, &mut conn).await.unwrap();

    let text = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let has = |parts: &[&str]| lines.iter().any(|line| parts.iter().all(|part| line.contains(part)));
    // The load, its root query and its overridden collection, with their rows
    assert!(has(&["mabat.load", "view=\"Project\"", "close"]), "{text}");
    assert!(has(&["mabat.query", "query=\"$root\"", "overridden=false", "rows=1"]), "{text}");
    assert!(has(&["mabat.query", "query=\"tasks\"", "overridden=true", "keys=1", "rows=2"]), "{text}");
    // Each statement, with its SQL, inside its query
    assert!(
        has(&["mabat.statement", "db.system.name=\"SQLite\"", "FROM task t WHERE", "statement ran", "rows=2"]),
        "{text}"
    );
    // The save and its statements
    assert!(has(&["mabat.save", "view=\"Project\"", "mabat.statement", "UPDATE"]), "{text}");
}
