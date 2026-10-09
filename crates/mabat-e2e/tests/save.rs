//! Saving and deleting aggregates on every database: each version of a board is saved,
//! loaded back and compared, and the rows it no longer owns are checked to be gone.

use std::collections::BTreeMap;

use mabat::{Conn, Error, ViewEncoder};
use sqlx::{AssertSqlSafe, Connection, Executor};

mod boards;

use boards::*;

async fn scenario<C: Conn>(conn: &mut C)
where
    C::Backend: Send,
    <C::Backend as sqlx::Database>::Connection: Send,
    Board: ViewEncoder<C::Backend>,
    Person: ViewEncoder<C::Backend>,
    Label: ViewEncoder<C::Backend>,
    List: ViewEncoder<C::Backend>,
    Card: ViewEncoder<C::Backend>,
    Setting: ViewEncoder<C::Backend>,
    PersonName: ViewEncoder<C::Backend>,
    BoardNode: ViewEncoder<C::Backend>,
{
    // The referenced people and the linked labels are aggregates of their own
    for person in [Person { id: 1, name: "Ada".into() }, Person { id: 2, name: "Grace".into() }] {
        mabat::save(&mut person.clone(), conn).await.unwrap();
    }
    for (id, name) in [(1, "bug"), (2, "docs"), (3, "perf")] {
        mabat::save(&mut Label { id, name: name.into() }, conn).await.unwrap();
    }

    mabat::save(&mut version_1(), conn).await.unwrap();
    assert_eq!(load_board(conn).await, version_1());
    assert_eq!(count::<Card, _>(conn).await, 3);

    mabat::save(&mut version_2(), conn).await.unwrap();
    assert_eq!(load_board(conn).await, version_2());
    // The list that is gone took its card along; the people and labels stay
    assert_eq!(count::<List, _>(conn).await, 2);
    assert_eq!(count::<Card, _>(conn).await, 3);
    assert_eq!(count::<Setting, _>(conn).await, 2);
    assert_eq!((count::<Person, _>(conn).await, count::<Label, _>(conn).await), (2, 3));

    // Saving again changes nothing
    mabat::save(&mut version_2(), conn).await.unwrap();
    assert_eq!(load_board(conn).await, version_2());

    // Back to private: the row of the public variant is deleted
    let mut version_3 = version_2();
    version_3.visibility = Visibility::Private;
    version_3.owner = Some(Person { id: 2, name: "Grace".into() });
    mabat::save(&mut version_3, conn).await.unwrap();
    assert_eq!(load_board(conn).await, version_3);

    // A second board, then the first one deleted with all it owns
    let mut other = version_1();
    other.id = 2;
    other.lists = vec![List { id: 20, name: "Only".into(), cards: vec![Card { id: 200, title: "Card".into() }] }];
    other.settings = BTreeMap::from([("lang".to_string(), Setting { id: 20, value: "c".into() })]);
    mabat::save(&mut other, conn).await.unwrap();
    assert!(mabat::delete::<Board, _>(1_i64, conn).await.unwrap());
    assert!(!mabat::delete::<Board, _>(1_i64, conn).await.unwrap());
    assert_eq!(mabat::load::<Board>().all(conn).await.unwrap(), [other]);
    assert_eq!(
        (count::<List, _>(conn).await, count::<Card, _>(conn).await, count::<Setting, _>(conn).await),
        (1, 1, 1)
    );
    assert_eq!((count::<Person, _>(conn).await, count::<Label, _>(conn).await), (2, 3));

    // What cannot be saved
    let error = mabat::save(&mut PersonName { name: "Nobody".into() }, conn).await.unwrap_err();
    assert!(matches!(error, Error::Write { view: "PersonName", .. }), "{error}");
    let mut node = BoardNode { id: 3, lists: Vec::new() };
    let error = mabat::save(&mut node, conn).await.unwrap_err();
    assert!(error.to_string().contains("references into a graph"), "{error}");
}

/// The doc with key 1.
async fn load_doc<C: Conn>(conn: &mut C) -> Doc
where
    Doc: mabat::ViewDecoder<C::Backend>,
{
    mabat::load::<Doc>().by_key(1_i64).one(conn).await.unwrap()
}

async fn versions_and_changes<C: Conn>(conn: &mut C)
where
    C::Backend: Send,
    <C::Backend as sqlx::Database>::Connection: Send,
    Board: ViewEncoder<C::Backend>,
    BoardColor: ViewEncoder<C::Backend>,
    Person: ViewEncoder<C::Backend>,
    Label: ViewEncoder<C::Backend>,
    Doc: ViewEncoder<C::Backend>,
    Section: ViewEncoder<C::Backend>,
{
    let section = |id, heading: &str| Section { id, heading: heading.into(), version: 0 };
    let mut doc = Doc {
        id: 1,
        title: "Guide".into(),
        body: None,
        version: 0,
        sections: vec![section(1, "Intro"), section(2, "Usage"), section(3, "FAQ")],
    };

    // Inserted with its versions, then updated: the versions are written back
    mabat::save(&mut doc, conn).await.unwrap();
    assert_eq!(doc.version, 0);
    let stale = doc.clone();
    mabat::save(&mut doc, conn).await.unwrap();
    assert_eq!((doc.version, doc.sections[2].version), (1, 1));
    assert_eq!(load_doc(conn).await, doc);

    // A copy with an old version conflicts, and changes nothing
    let mut stale_edit = stale.clone();
    stale_edit.title = "Lost update".into();
    let error = mabat::save(&mut stale_edit, conn).await.unwrap_err();
    assert!(matches!(error, Error::Conflict { view: "Doc", .. }), "{error}");
    assert_eq!(load_doc(conn).await.title, "Guide");

    // Only what changed: the title, one heading, a section gone, one new, two moved
    let before = load_doc(conn).await;
    let mut after = before.clone();
    after.title = "User guide".into();
    after.sections.remove(1);
    after.sections.swap(0, 1);
    after.sections[0].heading = "Questions".into();
    after.sections.push(section(4, "Changes"));
    mabat::save_changes(&before, &mut after, conn).await.unwrap();
    let loaded = load_doc(conn).await;
    assert_eq!(loaded, after);
    let versions: Vec<(i64, i32)> = loaded.sections.iter().map(|s| (s.id, s.version)).collect();
    // FAQ changed its heading; Intro only moved, which updates its position but not its data
    assert_eq!(versions, [(3, 2), (1, 2), (4, 0)]);
    assert_eq!(loaded.version, 2);

    // No change: no statement, and no new version
    let unchanged = loaded.clone();
    let mut same = loaded.clone();
    mabat::save_changes(&unchanged, &mut same, conn).await.unwrap();
    assert_eq!(load_doc(conn).await, loaded);

    // A stale `before` conflicts
    let mut edit = before.clone();
    edit.title = "Again".into();
    let error = mabat::save_changes(&before, &mut edit, conn).await.unwrap_err();
    assert!(matches!(error, Error::Conflict { .. }), "{error}");

    // Two different values cannot be compared
    let mut other = loaded.clone();
    other.id = 2;
    let error = mabat::save_changes(&loaded, &mut other, conn).await.unwrap_err();
    assert!(error.to_string().contains("different keys"), "{error}");

    // Columns that did not change are not written: a change made through another view stays
    let mut board = version_1();
    board.id = 5;
    board.lists.clear();
    board.settings.clear();
    mabat::save(&mut board, conn).await.unwrap();
    let before = mabat::load::<Board>().by_key(5_i64).one(conn).await.unwrap();
    // A view of some columns saves them, whole or changed, in a row that exists; it cannot insert
    // a new row, whose other columns are NOT NULL
    let color_before = mabat::load::<BoardColor>().by_key(5_i64).one(conn).await.unwrap();
    let mut color = color_before.clone();
    color.color = Color { red: Some(9), green: None, blue: None };
    mabat::save(&mut color, conn).await.unwrap();
    assert_eq!(mabat::load::<Board>().by_key(5_i64).one(conn).await.unwrap().color, color.color);
    let mut new_color = BoardColor { id: 77, color: color.color.clone() };
    let error = mabat::save(&mut new_color, conn).await.unwrap_err();
    assert!(matches!(error, Error::Query { .. }), "{error}");
    let color_before = color.clone();
    color.color = Color { red: Some(1), green: Some(2), blue: Some(3) };
    mabat::save_changes(&color_before, &mut color, conn).await.unwrap();
    let mut after = before.clone();
    after.name = "Renamed".into();
    mabat::save_changes(&before, &mut after, conn).await.unwrap();
    let loaded = mabat::load::<Board>().by_key(5_i64).one(conn).await.unwrap();
    assert_eq!((loaded.name.as_str(), &loaded.color), ("Renamed", &color.color));
    // A whole save writes every column: the color goes back
    mabat::save(&mut after, conn).await.unwrap();
    assert_eq!(mabat::load::<Board>().by_key(5_i64).one(conn).await.unwrap().color, before.color);
    // Saving it again changes nothing, which is still a row found: MySQL reports matched rows
    mabat::save(&mut after, conn).await.unwrap();
    assert_eq!(mabat::load::<Board>().by_key(5_i64).one(conn).await.unwrap(), after);
}

#[tokio::test]
async fn sqlite() {
    let mut conn = sqlx::SqliteConnection::connect("sqlite::memory:").await.unwrap();
    conn.execute(SCHEMA).await.unwrap();
    scenario(&mut conn).await;
    versions_and_changes(&mut conn).await;

    // In the caller's transaction: rolled back with it
    let mut tx = conn.begin().await.unwrap();
    let mut board = version_1();
    board.id = 9;
    board.lists.clear();
    board.settings.clear();
    mabat::save(&mut board, &mut tx).await.unwrap();
    assert!(mabat::load::<Board>().by_key(9_i64).optional(&mut tx).await.unwrap().is_some());
    tx.rollback().await.unwrap();
    assert!(mabat::load::<Board>().by_key(9_i64).optional(&mut conn).await.unwrap().is_none());
}

#[tokio::test]
async fn postgres() {
    let Ok(url) = std::env::var("MABAT_TEST_DATABASE_URL") else {
        eprintln!("skipping: MABAT_TEST_DATABASE_URL is not set (see scripts/with-postgres.sh)");
        return;
    };
    let mut conn = sqlx::PgConnection::connect(&url).await.unwrap();
    let schema = format!("mabat_save_{}", std::process::id());
    let setup = format!(
        "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE; CREATE SCHEMA \"{schema}\"; SET search_path TO \"{schema}\""
    );
    conn.execute(AssertSqlSafe(setup)).await.unwrap();
    conn.execute(SCHEMA).await.unwrap();
    scenario(&mut conn).await;
    versions_and_changes(&mut conn).await;
    conn.execute(AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE"))).await.unwrap();
}

#[tokio::test]
async fn mysql() {
    let Ok(url) = std::env::var("MABAT_TEST_MYSQL_URL") else {
        eprintln!("skipping: MABAT_TEST_MYSQL_URL is not set (see scripts/with-mysql.sh)");
        return;
    };
    let mut conn = sqlx::MySqlConnection::connect(&url).await.unwrap();
    let database = format!("mabat_save_{}", std::process::id());
    let setup = format!("DROP DATABASE IF EXISTS `{database}`; CREATE DATABASE `{database}`; USE `{database}`");
    conn.execute(AssertSqlSafe(setup)).await.unwrap();
    conn.execute(SCHEMA).await.unwrap();
    scenario(&mut conn).await;
    versions_and_changes(&mut conn).await;
    conn.execute(AssertSqlSafe(format!("DROP DATABASE `{database}`"))).await.unwrap();
}
