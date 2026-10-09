//! The boards, documents and views that the save tests share: `save.rs` saves them one by one,
//! `save_all.rs` many at once.
#![allow(dead_code)]

use std::collections::BTreeMap;

use mabat::{Conn, Ref, View};

pub const SCHEMA: &str = r#"
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
pub struct Board {
    pub id: i64,
    pub name: String,
    #[view(to_one(fk = "owner_id"))]
    pub owner: Option<Person>,
    #[view(embed)]
    pub state: State,
    #[view(embed(prefix = "color_"))]
    pub color: Color,
    #[view(embed)]
    pub visibility: Visibility,
    /// Placed by `position`, which saving writes
    #[view(child(fk = "board_id", index = "position"))]
    pub lists: Vec<List>,
    /// Many-to-many: saving writes the links only
    #[view(child(through = "board_label", fk = "board_id", target = "label_id", order_by = "id"))]
    pub labels: Vec<Label>,
    #[view(child(fk = "board_id", key = "name"))]
    pub settings: BTreeMap<String, Setting>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "person")]
pub struct Person {
    pub id: i64,
    pub name: String,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "label")]
pub struct Label {
    pub id: i64,
    pub name: String,
}

/// In columns of the board.
#[derive(View, Debug, Clone, PartialEq)]
#[view(tag = "state")]
pub enum State {
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
pub struct Color {
    pub red: Option<i32>,
    pub green: Option<i32>,
    pub blue: Option<i32>,
}

/// In a table per variant.
#[derive(View, Debug, Clone, PartialEq)]
#[view(tag = "visibility", strategy = "table_per_variant")]
pub enum Visibility {
    #[view(tag_value = "private")]
    Private,
    #[view(tag_value = "public", table = "public_board", key = "board_id")]
    Public { url: String },
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "list")]
pub struct List {
    pub id: i64,
    pub name: String,
    #[view(child(fk = "list_id", order_by = "id"))]
    pub cards: Vec<Card>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "card")]
pub struct Card {
    pub id: i64,
    pub title: String,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "setting")]
pub struct Setting {
    pub id: i64,
    pub value: String,
}

/// Versioned, with versioned sections.
#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "doc")]
pub struct Doc {
    pub id: i64,
    pub title: String,
    pub body: Option<String>,
    #[view(version)]
    pub version: i32,
    #[view(child(fk = "doc_id", index = "position"))]
    pub sections: Vec<Section>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "section")]
pub struct Section {
    pub id: i64,
    pub heading: String,
    #[view(version)]
    pub version: i32,
}

/// Another view of the board's table, to change columns behind another view's back.
#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "board")]
pub struct BoardColor {
    pub id: i64,
    #[view(embed(prefix = "color_"))]
    pub color: Color,
}

/// A view without a key field cannot be saved.
#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "person")]
pub struct PersonName {
    pub name: String,
}

/// A graph view cannot be saved yet. Only saved, never read.
#[allow(dead_code)]
#[derive(View, Debug)]
#[view(table = "board")]
pub struct BoardNode {
    pub id: i64,
    #[view(child(fk = "board_id", order_by = "id"))]
    pub lists: Vec<Ref<ListNode>>,
}

#[derive(View, Debug)]
#[view(table = "list")]
pub struct ListNode {
    pub id: i64,
}

pub fn version_1() -> Board {
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
pub fn version_2() -> Board {
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
pub async fn load_board<C: Conn>(conn: &mut C) -> Board
where
    Board: mabat::ViewDecoder<C::Backend>,
{
    mabat::load::<Board>().by_key(1_i64).one(conn).await.unwrap()
}

/// The rows of a view's table.
pub async fn count<T: View + mabat::ViewDecoder<C::Backend>, C: Conn>(conn: &mut C) -> i64 {
    mabat::load::<T>().count(conn).await.unwrap()
}
