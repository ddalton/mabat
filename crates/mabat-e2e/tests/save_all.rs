//! Saving many aggregates at once with `save_all`, on every database: the boards of `save.rs`
//! in bulk, created and then changed in every part, documents with versions, a view of some
//! of a table's columns, and keys the database generates.

use mabat::{Conn, Error, View};
use sqlx::{AssertSqlSafe, Connection, Executor};

mod boards;

use boards::*;

/// Tables with generated keys, with `{key}` the declaration of the key.
const GENERATED: &str = r#"
CREATE TABLE item (id {key}, name VARCHAR(100) NOT NULL);
CREATE TABLE item_part (id {key}, item_id BIGINT NOT NULL REFERENCES item (id), name VARCHAR(100) NOT NULL);
"#;

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "item")]
struct Item {
    #[view(generated)]
    id: Option<i64>,
    name: String,
    #[view(child(fk = "item_id", order_by = "id"))]
    parts: Vec<Part>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "item_part")]
struct Part {
    #[view(generated)]
    id: Option<i64>,
    name: String,
}

const BOARDS: i64 = 40;

/// Board `n` as `version_1`, with keys of its own.
fn board(n: i64) -> Board {
    let mut board = version_1();
    board.id = n;
    board.name = format!("Board {n}");
    for list in &mut board.lists {
        list.id += n * 100;
        for card in &mut list.cards {
            card.id += n * 1000;
        }
    }
    for setting in board.settings.values_mut() {
        setting.id += n * 10;
    }
    board
}

/// Every part of a board changes, as `version_2` changes `version_1`.
fn change(board: &mut Board) {
    let n = board.id;
    let mut changed = version_2();
    changed.id = n;
    changed.name = format!("Board {n}, v2");
    for list in &mut changed.lists {
        list.id += n * 100;
        for card in &mut list.cards {
            card.id += n * 1000;
        }
    }
    for setting in changed.settings.values_mut() {
        setting.id += n * 10;
    }
    *board = changed;
}

async fn boards<C: Conn>(conn: &mut C) -> Vec<Board>
where
    Board: mabat::ViewDecoder<C::Backend>,
{
    mabat::load::<Board>().order_by("id").all(conn).await.unwrap()
}

async fn scenario<C: Conn>(conn: &mut C)
where
    C::Backend: Send,
    <C::Backend as sqlx::Database>::Connection: Send,
    Board: mabat::ViewEncoder<C::Backend>,
    Person: mabat::ViewEncoder<C::Backend>,
    Label: mabat::ViewEncoder<C::Backend>,
    List: mabat::ViewDecoder<C::Backend>,
    Card: mabat::ViewDecoder<C::Backend>,
    Doc: mabat::ViewEncoder<C::Backend>,
    BoardColor: mabat::ViewEncoder<C::Backend>,
    Item: mabat::ViewEncoder<C::Backend>,
{
    // What boards reference, which saving them does not write
    let mut people = vec![Person { id: 1, name: "Ada".into() }];
    mabat::save_all(&mut people, conn).await.unwrap();
    let mut labels: Vec<Label> =
        ["bug", "docs", "perf"].iter().zip(1..).map(|(name, id)| Label { id, name: (*name).into() }).collect();
    mabat::save_all(&mut labels, conn).await.unwrap();

    // New boards, with their lists, cards, links, settings and variant rows
    let mut all: Vec<Board> = (1..=BOARDS).map(board).collect();
    mabat::save_all(&mut all, conn).await.unwrap();
    assert_eq!(boards(conn).await, all);

    // Half of them change in every part, and new ones are added
    for board in all.iter_mut().filter(|b| b.id % 2 == 0) {
        change(board);
    }
    all.extend((BOARDS + 1..=BOARDS + 10).map(board));
    mabat::save_all(&mut all, conn).await.unwrap();
    assert_eq!(boards(conn).await, all);
    let lists: usize = all.iter().map(|b| b.lists.len()).sum();
    let cards: usize = all.iter().flat_map(|b| &b.lists).map(|l| l.cards.len()).sum();
    assert_eq!(mabat::load::<List>().count(conn).await.unwrap(), lists as i64, "lists that are gone are deleted");
    assert_eq!(mabat::load::<Card>().count(conn).await.unwrap(), cards as i64, "with their cards");

    // Saving them again changes nothing
    mabat::save_all(&mut all, conn).await.unwrap();
    assert_eq!(boards(conn).await, all);

    // A view of some columns saves them in rows that exist, and cannot insert new rows
    let mut colors: Vec<BoardColor> = all
        .iter()
        .map(|b| BoardColor { id: b.id, color: Color { red: Some(b.id as i32), green: None, blue: Some(1) } })
        .collect();
    mabat::save_all(&mut colors, conn).await.unwrap();
    let loaded = boards(conn).await;
    assert!(loaded.iter().zip(&colors).all(|(b, c)| b.color == c.color && b.name.starts_with("Board")));
    let mut new_color = vec![BoardColor { id: 999, color: Color { red: None, green: None, blue: None } }];
    let error = mabat::save_all(&mut new_color, conn).await.unwrap_err();
    assert!(matches!(error, Error::Query { .. }), "{error}");

    // Versions: written back, incremented, and checked
    let mut docs: Vec<Doc> = (1..=5)
        .map(|n| Doc {
            id: n,
            title: format!("Doc {n}"),
            body: None,
            version: 0,
            sections: (0..3).map(|s| Section { id: n * 10 + s, heading: format!("Section {s}"), version: 0 }).collect(),
        })
        .collect();
    mabat::save_all(&mut docs, conn).await.unwrap();
    for doc in &mut docs {
        doc.title.push_str(" (edited)");
        doc.sections.pop();
    }
    let stale = docs.clone();
    mabat::save_all(&mut docs, conn).await.unwrap();
    assert!(docs.iter().all(|d| d.version == 1 && d.sections.iter().all(|s| s.version == 1)), "{docs:?}");
    let loaded = mabat::load::<Doc>().order_by("id").all(conn).await.unwrap();
    assert_eq!(loaded, docs);
    let mut stale = stale;
    let error = mabat::save_all(&mut stale, conn).await.unwrap_err();
    assert!(matches!(error, Error::Conflict { .. }), "{error}");
    assert_eq!(mabat::load::<Doc>().order_by("id").all(conn).await.unwrap(), docs, "nothing was saved");

    // Keys the database generates: inserted one by one, then their parts together
    let mut items: Vec<Item> = (0..5)
        .map(|n| Item {
            id: None,
            name: format!("Item {n}"),
            parts: (0..3).map(|p| Part { id: None, name: format!("Part {p}") }).collect(),
        })
        .collect();
    mabat::save_all(&mut items, conn).await.unwrap();
    assert!(items.iter().all(|i| i.id.is_some() && i.parts.iter().all(|p| p.id.is_some())));
    items[0].parts.push(Part { id: None, name: "Part 3".into() });
    items[1].parts.clear();
    mabat::save_all(&mut items, conn).await.unwrap();
    let loaded = mabat::load::<Item>().order_by("id").all(conn).await.unwrap();
    assert_eq!(loaded, items);
}

#[tokio::test]
async fn sqlite() {
    let mut conn = sqlx::SqliteConnection::connect("sqlite::memory:").await.unwrap();
    conn.execute(SCHEMA).await.unwrap();
    conn.execute(AssertSqlSafe(GENERATED.replace("{key}", "INTEGER PRIMARY KEY"))).await.unwrap();
    scenario(&mut conn).await;
}

#[tokio::test]
async fn postgres() {
    let Ok(url) = std::env::var("MABAT_TEST_DATABASE_URL") else {
        eprintln!("skipping: MABAT_TEST_DATABASE_URL is not set (see scripts/with-postgres.sh)");
        return;
    };
    let mut conn = sqlx::PgConnection::connect(&url).await.unwrap();
    let schema = format!("mabat_save_all_{}", std::process::id());
    let setup = format!(
        "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE; CREATE SCHEMA \"{schema}\"; SET search_path TO \"{schema}\""
    );
    conn.execute(AssertSqlSafe(setup)).await.unwrap();
    conn.execute(SCHEMA).await.unwrap();
    let generated = GENERATED.replace("{key}", "BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY");
    conn.execute(AssertSqlSafe(generated)).await.unwrap();
    scenario(&mut conn).await;
    conn.execute(AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE"))).await.unwrap();
}

#[tokio::test]
async fn mysql() {
    let Ok(url) = std::env::var("MABAT_TEST_MYSQL_URL") else {
        eprintln!("skipping: MABAT_TEST_MYSQL_URL is not set (see scripts/with-mysql.sh)");
        return;
    };
    let mut conn = sqlx::MySqlConnection::connect(&url).await.unwrap();
    let database = format!("mabat_save_all_{}", std::process::id());
    let setup = format!("DROP DATABASE IF EXISTS `{database}`; CREATE DATABASE `{database}`; USE `{database}`");
    conn.execute(AssertSqlSafe(setup)).await.unwrap();
    conn.execute(SCHEMA).await.unwrap();
    conn.execute(AssertSqlSafe(GENERATED.replace("{key}", "BIGINT AUTO_INCREMENT PRIMARY KEY"))).await.unwrap();
    scenario(&mut conn).await;
    conn.execute(AssertSqlSafe(format!("DROP DATABASE `{database}`"))).await.unwrap();
}
