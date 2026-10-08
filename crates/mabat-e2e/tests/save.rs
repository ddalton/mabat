//! Saving and deleting aggregates on every database: each version of a board is saved,
//! loaded back and compared, and the rows it no longer owns are checked to be gone.

use std::collections::BTreeMap;

use mabat::{Conn, Error, Ref, View, ViewEncoder};
use sqlx::{AssertSqlSafe, Connection, Executor};

const SCHEMA: &str = r#"
CREATE TABLE person (id BIGINT PRIMARY KEY, name VARCHAR(100) NOT NULL);
CREATE TABLE label (id BIGINT PRIMARY KEY, name VARCHAR(100) NOT NULL);
CREATE TABLE board (
    id BIGINT PRIMARY KEY, name VARCHAR(100) NOT NULL, owner_id BIGINT REFERENCES person (id),
    state VARCHAR(20) NOT NULL, archived_reason VARCHAR(100),
    color_red INT, color_green INT, color_blue INT,
    visibility VARCHAR(20) NOT NULL
);
CREATE TABLE public_board (board_id BIGINT PRIMARY KEY REFERENCES board (id), url VARCHAR(100) NOT NULL);
CREATE TABLE list (
    id BIGINT PRIMARY KEY, board_id BIGINT NOT NULL REFERENCES board (id), name VARCHAR(100) NOT NULL,
    position INT NOT NULL
);
CREATE TABLE card (id BIGINT PRIMARY KEY, list_id BIGINT NOT NULL REFERENCES list (id), title VARCHAR(100) NOT NULL);
CREATE TABLE board_label (
    board_id BIGINT NOT NULL REFERENCES board (id), label_id BIGINT NOT NULL REFERENCES label (id),
    PRIMARY KEY (board_id, label_id)
);
CREATE TABLE doc (id BIGINT PRIMARY KEY, title VARCHAR(100) NOT NULL, body VARCHAR(500), version INT NOT NULL);
CREATE TABLE section (
    id BIGINT PRIMARY KEY, doc_id BIGINT NOT NULL REFERENCES doc (id), heading VARCHAR(100) NOT NULL,
    position INT NOT NULL, version INT NOT NULL
);
CREATE TABLE setting (
    id BIGINT PRIMARY KEY, board_id BIGINT NOT NULL REFERENCES board (id), name VARCHAR(50) NOT NULL,
    value VARCHAR(100) NOT NULL
);
"#;

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "board")]
struct Board {
    id: i64,
    name: String,
    #[view(to_one(fk = "owner_id"))]
    owner: Option<Person>,
    #[view(embed)]
    state: State,
    #[view(embed(prefix = "color_"))]
    color: Color,
    #[view(embed)]
    visibility: Visibility,
    /// Placed by `position`, which saving writes
    #[view(child(fk = "board_id", index = "position"))]
    lists: Vec<List>,
    /// Many-to-many: saving writes the links only
    #[view(child(through = "board_label", fk = "board_id", target = "label_id", order_by = "id"))]
    labels: Vec<Label>,
    #[view(child(fk = "board_id", key = "name"))]
    settings: BTreeMap<String, Setting>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "person")]
struct Person {
    id: i64,
    name: String,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "label")]
struct Label {
    id: i64,
    name: String,
}

/// In columns of the board.
#[derive(View, Debug, Clone, PartialEq)]
#[view(tag = "state")]
enum State {
    #[view(tag_value = "active")]
    Active,
    #[view(tag_value = "archived")]
    Archived {
        #[view(column = "archived_reason")]
        reason: String,
    },
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(embedded)]
struct Color {
    red: Option<i32>,
    green: Option<i32>,
    blue: Option<i32>,
}

/// In a table per variant.
#[derive(View, Debug, Clone, PartialEq)]
#[view(tag = "visibility", strategy = "table_per_variant")]
enum Visibility {
    #[view(tag_value = "private")]
    Private,
    #[view(tag_value = "public", table = "public_board", key = "board_id")]
    Public { url: String },
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "list")]
struct List {
    id: i64,
    name: String,
    #[view(child(fk = "list_id", order_by = "id"))]
    cards: Vec<Card>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "card")]
struct Card {
    id: i64,
    title: String,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "setting")]
struct Setting {
    id: i64,
    value: String,
}

/// Versioned, with versioned sections.
#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "doc")]
struct Doc {
    id: i64,
    title: String,
    body: Option<String>,
    #[view(version)]
    version: i32,
    #[view(child(fk = "doc_id", index = "position"))]
    sections: Vec<Section>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "section")]
struct Section {
    id: i64,
    heading: String,
    #[view(version)]
    version: i32,
}

/// Another view of the board's table, to change columns behind another view's back.
#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "board")]
struct BoardColor {
    id: i64,
    #[view(embed(prefix = "color_"))]
    color: Color,
}

/// A view without a key field cannot be saved.
#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "person")]
struct PersonName {
    name: String,
}

/// A graph view cannot be saved yet. Only saved, never read.
#[allow(dead_code)]
#[derive(View, Debug)]
#[view(table = "board")]
struct BoardNode {
    id: i64,
    #[view(child(fk = "board_id", order_by = "id"))]
    lists: Vec<Ref<ListNode>>,
}

#[derive(View, Debug)]
#[view(table = "list")]
struct ListNode {
    id: i64,
}

fn version_1() -> Board {
    Board {
        id: 1,
        name: "Roadmap".into(),
        owner: Some(Person { id: 1, name: "Ada".into() }),
        state: State::Active,
        color: Color { red: Some(255), green: None, blue: Some(0) },
        visibility: Visibility::Private,
        lists: vec![
            List {
                id: 10,
                name: "Now".into(),
                cards: vec![Card { id: 100, title: "Parser".into() }, Card { id: 101, title: "Lexer".into() }],
            },
            List { id: 11, name: "Next".into(), cards: vec![Card { id: 110, title: "Linker".into() }] },
            List { id: 12, name: "Later".into(), cards: vec![] },
        ],
        labels: vec![Label { id: 1, name: "bug".into() }, Label { id: 2, name: "docs".into() }],
        settings: BTreeMap::from([
            ("lang".to_string(), Setting { id: 1, value: "rust".into() }),
            ("theme".to_string(), Setting { id: 2, value: "dark".into() }),
        ]),
    }
}

/// Every part of the board changes.
fn version_2() -> Board {
    let mut board = version_1();
    board.name = "Roadmap 2".into();
    board.owner = None;
    board.state = State::Archived { reason: "done".into() };
    board.color = Color { red: None, green: Some(128), blue: None };
    board.visibility = Visibility::Public { url: "https://example.com/b/1".into() };
    // "Next" and its card are gone, "Later" moves first, "Now" gains a card and renames one
    let mut now = board.lists.remove(0);
    board.lists.remove(0);
    now.cards[0].title = "Parser v2".into();
    now.cards.push(Card { id: 102, title: "Optimizer".into() });
    board.lists.push(now);
    board.labels = vec![Label { id: 2, name: "docs".into() }, Label { id: 3, name: "perf".into() }];
    board.settings.remove("theme");
    board.settings.insert("width".into(), Setting { id: 3, value: "wide".into() });
    board.settings.get_mut("lang").unwrap().value = "rust 2024".into();
    board
}

/// The board with key 1.
async fn load_board<C: Conn>(conn: &mut C) -> Board
where
    Board: mabat::ViewDecoder<C::Backend>,
{
    mabat::load::<Board>().by_key(1_i64).one(conn).await.unwrap()
}

/// The rows of a view's table.
async fn count<T: View + mabat::ViewDecoder<C::Backend>, C: Conn>(conn: &mut C) -> i64 {
    mabat::load::<T>().count(conn).await.unwrap()
}

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
    // A view of some columns updates them with save_changes; a whole save would also need to
    // be able to insert the row, with the columns the view does not have
    let color_before = mabat::load::<BoardColor>().by_key(5_i64).one(conn).await.unwrap();
    let mut color = color_before.clone();
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
