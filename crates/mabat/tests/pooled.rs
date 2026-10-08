//! Loads on a pool: the child queries of a level run at the same time, on connections that
//! share a snapshot with `Pooled::snapshot`, or see what is committed with
//! `Pooled::read_committed`.

mod common;

use std::time::{Duration, Instant};

use common::TestDb;
use mabat::{Error, Mabat, Pooled, Ref, View};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{AssertSqlSafe, Executor};

const SCHEMA: &str = r#"
CREATE TABLE member (id BIGINT PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE board  (id BIGINT PRIMARY KEY, name TEXT NOT NULL, owner_id BIGINT NOT NULL REFERENCES member (id));
CREATE TABLE list   (id BIGINT PRIMARY KEY, board_id BIGINT NOT NULL REFERENCES board (id), name TEXT NOT NULL);
CREATE TABLE label  (id BIGINT PRIMARY KEY, board_id BIGINT NOT NULL REFERENCES board (id), name TEXT NOT NULL);
CREATE TABLE card   (id BIGINT PRIMARY KEY, list_id BIGINT NOT NULL REFERENCES list (id), title TEXT NOT NULL);

INSERT INTO member SELECT i, 'member ' || i FROM generate_series(1, 5) i;
INSERT INTO board SELECT i, 'board ' || i, 1 + i % 5 FROM generate_series(1, 20) i;
INSERT INTO list SELECT i, 1 + i % 20, 'list ' || i FROM generate_series(1, 60) i;
INSERT INTO label SELECT i, 1 + i % 20, 'label ' || i FROM generate_series(1, 40) i;
INSERT INTO card SELECT i, 1 + i % 60, 'card ' || i FROM generate_series(1, 300) i;
"#;

#[derive(View, Debug, PartialEq)]
#[view(table = "board", databases = "postgres")]
struct BoardView {
    id: i64,
    name: String,
    #[view(to_one(fk = "owner_id"))]
    owner: MemberView,
    #[view(child(fk = "board_id", order_by = "id"))]
    lists: Vec<ListView>,
    #[view(child(fk = "board_id", order_by = "id"))]
    labels: Vec<LabelView>,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "member", databases = "postgres")]
struct MemberView {
    name: String,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "list", databases = "postgres")]
struct ListView {
    id: i64,
    name: String,
    #[view(child(fk = "list_id", order_by = "id"))]
    cards: Vec<CardView>,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "label", databases = "postgres")]
struct LabelView {
    name: String,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "card", databases = "postgres")]
struct CardView {
    title: String,
}

/// A pool whose connections use the schema of the test.
async fn pool(db: &mut TestDb, connections: u32) -> PgPool {
    let schema: String = sqlx::query_scalar("SELECT current_schema()").fetch_one(&mut db.conn).await.unwrap();
    let url = std::env::var("MABAT_TEST_DATABASE_URL").unwrap();
    PgPoolOptions::new()
        .max_connections(connections)
        .after_connect(move |conn, _| {
            let schema = schema.clone();
            Box::pin(async move {
                conn.execute(AssertSqlSafe(format!("SET search_path TO \"{schema}\""))).await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap()
}

#[tokio::test]
async fn pooled_loads_match_one_connection() {
    let Some(mut db) = TestDb::new("pooled_match", SCHEMA).await else { return };
    let pool = pool(&mut db, 4).await;

    let expected = mabat::load::<BoardView>().order_by("id").all(&mut db.conn).await.unwrap();
    assert_eq!(expected.len(), 20);
    assert_eq!(expected.iter().map(|b| b.lists.iter().map(|l| l.cards.len()).sum::<usize>()).sum::<usize>(), 300);

    for mut pooled in [Pooled::snapshot(&pool, 4), Pooled::read_committed(&pool, 4)] {
        let boards = mabat::load::<BoardView>().order_by("id").all(&mut pooled).await.unwrap();
        assert_eq!(boards, expected);
        let one = mabat::load::<BoardView>().by_key(7_i64).one(&mut pooled).await.unwrap();
        assert_eq!(one, expected[6]);
        let n = mabat::load::<BoardView>().count(&mut pooled).await.unwrap();
        assert_eq!(n, 20);
    }
    // The owner, lists and labels of the boards were queried on connections of their own
    assert!(pool.size() > 1, "{} connections", pool.size());

    // A registry is built and used on a pool too
    let mut pooled = Pooled::snapshot(&pool, 4);
    let mabat = Mabat::builder().register::<BoardView>().build(&mut pooled).await.unwrap();
    assert_eq!(mabat.load::<BoardView>().order_by("id").all(&mut pooled).await.unwrap(), expected);

    pool.close().await;
    db.drop().await;
}

/// The lists and the labels each wait half a second.
const SLOW: &str = r#"
-- mabat: query lists
SELECT l.board_id AS "$parent", l.id AS "id", l.name AS "name"
FROM list l, pg_sleep(0.5)
WHERE l.board_id = ANY(:keys)
ORDER BY l.id

-- mabat: query labels
SELECT l.board_id AS "$parent", l.id AS "$key", l.name AS "name"
FROM label l, pg_sleep(0.5)
WHERE l.board_id = ANY(:keys)
ORDER BY l.id
"#;

#[tokio::test]
async fn sibling_queries_run_at_the_same_time() {
    let Some(mut db) = TestDb::new("pooled_time", SCHEMA).await else { return };
    let pool = pool(&mut db, 4).await;
    let mabat = Mabat::builder().register::<BoardView>().overrides_sql("BoardView", SLOW).build(&mut db.conn);
    let mabat = mabat.await.unwrap();

    let start = Instant::now();
    let one = mabat.load::<BoardView>().all(&mut db.conn).await.unwrap();
    let sequential = start.elapsed();
    let start = Instant::now();
    let pooled = mabat.load::<BoardView>().all(&mut Pooled::snapshot(&pool, 4)).await.unwrap();
    let concurrent = start.elapsed();

    assert_eq!(pooled, one);
    assert!(sequential >= Duration::from_millis(1000), "{sequential:?}");
    assert!(concurrent < Duration::from_millis(900), "{concurrent:?} on a pool, {sequential:?} on one connection");

    pool.close().await;
    db.drop().await;
}

#[tokio::test]
async fn a_snapshot_hides_what_commits_during_the_load() {
    let Some(mut db) = TestDb::new("pooled_snapshot", SCHEMA).await else { return };
    let pool = pool(&mut db, 4).await;
    let mabat = Mabat::builder().register::<BoardView>().overrides_sql("BoardView", SLOW).build(&mut db.conn);
    let mabat = mabat.await.unwrap();
    let cards = |boards: &[BoardView]| boards.iter().flat_map(|b| &b.lists).map(|l| l.cards.len()).sum::<usize>();

    // While the lists wait, a card is added; the cards of the lists are queried after that
    for (mut pooled, expected) in [(Pooled::snapshot(&pool, 4), 300), (Pooled::read_committed(&pool, 4), 301)] {
        sqlx::query("DELETE FROM card WHERE id >= 1000").execute(&pool).await.unwrap();
        let writer = {
            let pool = pool.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                sqlx::query("INSERT INTO card VALUES (1000, 1, 'late')").execute(&pool).await.unwrap();
            })
        };
        let boards = mabat.load::<BoardView>().all(&mut pooled).await.unwrap();
        writer.await.unwrap();
        assert_eq!(cards(&boards), expected, "{pooled:?}");
    }

    pool.close().await;
    db.drop().await;
}

/// The cards fail once the lists are loaded.
const FAILING: &str = r#"
-- mabat: query lists.cards
SELECT c.list_id AS "$parent", c.id AS "$key", c.title AS "title"
FROM card c
WHERE c.list_id = ANY(:keys) AND 1 / (c.id - c.id) = 1
"#;

#[tokio::test]
async fn a_failed_load_returns_its_connections() {
    let Some(mut db) = TestDb::new("pooled_failure", SCHEMA).await else { return };
    let pool = pool(&mut db, 2).await;
    let failing = Mabat::builder().register::<BoardView>().overrides_sql("BoardView", FAILING).build(&mut db.conn);
    let failing = failing.await.unwrap();

    for _ in 0..5 {
        let error = failing.load::<BoardView>().all(&mut Pooled::snapshot(&pool, 2)).await.unwrap_err();
        assert!(matches!(error, Error::Query { .. }), "{error}");
        assert!(error.to_string().contains("division by zero"), "{error}");
    }
    // The connections and their permits are back: the pool of two still serves loads
    let boards = mabat::load::<BoardView>().all(&mut Pooled::snapshot(&pool, 2)).await.unwrap();
    assert_eq!(boards.len(), 20);

    pool.close().await;
    db.drop().await;
}

#[derive(View, Debug)]
#[view(table = "member", databases = "postgres")]
struct Member {
    name: String,
    #[view(child(fk = "owner_id", order_by = "id"))]
    boards: Vec<Ref<Board>>,
}

#[derive(View, Debug)]
#[view(table = "board", databases = "postgres")]
struct Board {
    name: String,
    #[view(to_one(fk = "owner_id"))]
    owner: Ref<Member>,
}

#[tokio::test]
async fn graphs_load_on_a_pool() {
    let Some(mut db) = TestDb::new("pooled_graph", SCHEMA).await else { return };
    let pool = pool(&mut db, 4).await;

    let one = mabat::load::<Member>().order_by("id").graph(&mut db.conn).await.unwrap();
    let pooled = mabat::load::<Member>().order_by("id").graph(&mut Pooled::snapshot(&pool, 4)).await.unwrap();
    assert_eq!(pooled.count::<Member>(), one.count::<Member>());
    assert_eq!(pooled.count::<Board>(), 20);
    let names = |graph: &mabat::Graph<Member>| {
        graph
            .roots()
            .map(|m| (m.name.clone(), m.boards(graph).map(|b| b.name.clone()).collect::<Vec<_>>()))
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&pooled), names(&one));
    for member in pooled.roots() {
        assert!(member.boards(&pooled).all(|b| b.owner(&pooled).name == member.name));
    }

    pool.close().await;
    db.drop().await;
}
