//! Ordered lists, maps, many-to-many collections and recursive views.

mod common;

use std::collections::{BTreeMap, HashMap};

use common::TestDb;
use mabat::{Error, Mabat, View};

const SCHEMA: &str = r#"
CREATE TABLE playlist (
    id   BIGINT PRIMARY KEY,
    name TEXT NOT NULL
);
CREATE TABLE song (
    id    BIGINT PRIMARY KEY,
    title TEXT NOT NULL
);
CREATE TABLE playlist_song (
    playlist_id BIGINT NOT NULL REFERENCES playlist (id),
    song_id     BIGINT NOT NULL REFERENCES song (id),
    seq         INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, seq)
);
CREATE TABLE setting (
    id          SERIAL PRIMARY KEY,
    playlist_id BIGINT NOT NULL REFERENCES playlist (id),
    name        TEXT NOT NULL,
    value       TEXT NOT NULL
);
CREATE TABLE category (
    id        BIGINT PRIMARY KEY,
    parent_id BIGINT REFERENCES category (id),
    name      TEXT NOT NULL,
    position  INTEGER NOT NULL
);

INSERT INTO playlist (id, name) VALUES (1, 'Road trip'), (2, 'Empty');
INSERT INTO song (id, title) VALUES (10, 'Alpha'), (11, 'Beta'), (12, 'Gamma');
-- inserted out of order; Beta is on the list twice
INSERT INTO playlist_song (playlist_id, song_id, seq) VALUES (1, 12, 7), (1, 10, 2), (1, 11, 5), (1, 11, 0);
INSERT INTO setting (playlist_id, name, value) VALUES (1, 'shuffle', 'off'), (1, 'volume', '8');

-- Catalog > (Books > (Fiction > (Fantasy > Epic), Poetry), Music); positions place the siblings
INSERT INTO category (id, parent_id, name, position) VALUES
    (1, NULL, 'Catalog', 0),
    (2, 1, 'Music', 1),
    (3, 1, 'Books', 0),
    (4, 3, 'Poetry', 1),
    (5, 3, 'Fiction', 0),
    (6, 5, 'Fantasy', 0),
    (7, 6, 'Epic', 0);
"#;

#[derive(View, Debug, PartialEq)]
#[view(table = "playlist")]
struct PlaylistView {
    name: String,
    #[view(child(through = "playlist_song", fk = "playlist_id", target = "song_id", index = "seq"))]
    songs: Vec<SongView>,
    #[view(child(fk = "playlist_id", key = "name"))]
    settings: BTreeMap<String, SettingView>,
    #[view(child(fk = "playlist_id", key = "name"))]
    settings_by_hash: HashMap<String, SettingView>,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "song")]
struct SongView {
    title: String,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "setting")]
struct SettingView {
    value: String,
}

/// Loaded level by level, at most 3 levels below the root, placed by position.
#[derive(View, Debug, PartialEq)]
#[view(table = "category")]
struct CategoryTree {
    name: String,
    #[view(child(fk = "parent_id", index = "position", depth = 3))]
    children: Vec<CategoryTree>,
}

/// Loaded with one query for all levels, ordered by name.
#[derive(View, Debug, PartialEq)]
#[view(table = "category")]
struct CategoryCte {
    name: String,
    #[view(child(fk = "parent_id", order_by = "name", recursive = "cte"))]
    children: Vec<CategoryCte>,
}

async fn setup(name: &str) -> Option<TestDb> {
    TestDb::new(name, SCHEMA).await
}

fn tree(name: &str, children: Vec<CategoryTree>) -> CategoryTree {
    CategoryTree { name: name.into(), children }
}

fn cte(name: &str, children: Vec<CategoryCte>) -> CategoryCte {
    CategoryCte { name: name.into(), children }
}

#[tokio::test]
async fn lists_are_placed_by_index_and_maps_are_keyed() {
    let Some(mut db) = setup("collections").await else { return };

    let playlists = mabat::load::<PlaylistView>().order_by("id").all(&mut db.conn).await.unwrap();
    let titles: Vec<&str> = playlists[0].songs.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, ["Beta", "Alpha", "Beta", "Gamma"]);
    let settings: Vec<(&str, &str)> =
        playlists[0].settings.iter().map(|(k, v)| (k.as_str(), v.value.as_str())).collect();
    assert_eq!(settings, [("shuffle", "off"), ("volume", "8")]);
    assert_eq!(playlists[0].settings_by_hash["volume"], SettingView { value: "8".into() });

    assert_eq!(
        playlists[1],
        PlaylistView {
            name: "Empty".into(),
            songs: vec![],
            settings: BTreeMap::new(),
            settings_by_hash: HashMap::new()
        }
    );

    db.drop().await;
}

/// The index places the elements whatever the order of the rows, so an override can use
/// any join order and leave out `ORDER BY`.
#[tokio::test]
async fn overrides_return_rows_in_any_order() {
    let Some(mut db) = setup("collections_order").await else { return };
    let overrides = r#"
        [query.songs]
        sql = '''
        SELECT s.id AS "$key", ps.playlist_id AS "$parent", ps.seq AS "$index", s.title AS "title"
        FROM playlist_song ps JOIN song s ON s.id = ps.song_id
        WHERE ps.playlist_id = ANY($1)
        ORDER BY random()
        '''
    "#;
    let mabat = Mabat::builder()
        .register::<PlaylistView>()
        .overrides("PlaylistView", overrides)
        .build(&mut db.conn)
        .await
        .unwrap();
    assert!(mabat.report().diagnostics().is_empty(), "{}", mabat.report());
    for _ in 0..10 {
        let playlist = mabat.load::<PlaylistView>().by_key(1_i64).one(&mut db.conn).await.unwrap();
        let titles: Vec<&str> = playlist.songs.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Beta", "Alpha", "Beta", "Gamma"]);
    }

    // The index and the map key are checked like other columns
    let broken = overrides.replace("ps.seq AS \"$index\"", "s.title AS \"$index\"");
    let report =
        Mabat::builder().register::<PlaylistView>().overrides("PlaylistView", broken).check(&mut db.conn).await;
    let report = report.unwrap();
    assert!(
        report.errors().flat_map(|d| &d.notes).any(|n| n
            == "column 3 \"$index\" has type TEXT; it places the elements of the list, so it needs an integer type"),
        "{report}"
    );
    let report = Mabat::builder().register::<PlaylistView>().check(&mut db.conn).await.unwrap();
    assert!(report.diagnostics().is_empty(), "{report}");

    db.drop().await;
}

#[tokio::test]
async fn duplicate_indices_and_map_keys_are_errors() {
    let Some(mut db) = setup("collections_duplicates").await else { return };
    db.execute("INSERT INTO setting (playlist_id, name, value) VALUES (1, 'volume', '11')").await;
    let err = mabat::load::<PlaylistView>().by_key(1_i64).one(&mut db.conn).await.unwrap_err();
    assert!(matches!(&err, Error::DuplicateMapKey { path, .. } if path == "settings"), "{err}");

    db.execute("UPDATE category SET position = 0 WHERE id = 4").await;
    let err = mabat::load::<CategoryTree>().by_key(1_i64).one(&mut db.conn).await.unwrap_err();
    assert!(matches!(&err, Error::ListIndex { path, .. } if path == "children.children"), "{err}");
    assert!(err.to_string().contains("two elements of a list have the index 0"), "{err}");

    db.drop().await;
}

#[tokio::test]
async fn recursive_views_level_by_level() {
    let Some(mut db) = setup("collections_depth").await else { return };

    // At most 3 levels below the root: Epic, 4 levels down, is not loaded
    let catalog = mabat::load::<CategoryTree>().by_key(1_i64).one(&mut db.conn).await.unwrap();
    assert_eq!(
        catalog,
        tree(
            "Catalog",
            vec![
                tree("Books", vec![tree("Fiction", vec![tree("Fantasy", vec![])]), tree("Poetry", vec![])]),
                tree("Music", vec![]),
            ]
        )
    );

    // Any node can be the root
    let fiction = mabat::load::<CategoryTree>().by_key(5_i64).one(&mut db.conn).await.unwrap();
    assert_eq!(fiction, tree("Fiction", vec![tree("Fantasy", vec![tree("Epic", vec![])])]));

    let plan = mabat::plan::<CategoryTree>().unwrap();
    assert_eq!(plan.query_count(), 2);

    db.drop().await;
}

#[tokio::test]
async fn recursive_views_in_one_query() {
    let Some(mut db) = setup("collections_cte").await else { return };

    let catalog = mabat::load::<CategoryCte>().by_key(1_i64).one(&mut db.conn).await.unwrap();
    assert_eq!(
        catalog,
        cte(
            "Catalog",
            vec![
                cte(
                    "Books",
                    vec![cte("Fiction", vec![cte("Fantasy", vec![cte("Epic", vec![])])]), cte("Poetry", vec![])]
                ),
                cte("Music", vec![]),
            ]
        )
    );

    // Roots that are below other roots: each row is loaded once and placed under its parent
    let both = mabat::load::<CategoryCte>().by_keys([3_i64, 5]).order_by("id").all(&mut db.conn).await.unwrap();
    assert_eq!(both[0].children[0], both[1]);
    assert_eq!(both[1].children.len(), 1);

    let report = Mabat::builder().register::<CategoryCte>().check(&mut db.conn).await.unwrap();
    assert!(report.diagnostics().is_empty(), "{report}");

    // A cycle in the data cannot be a tree
    db.execute("UPDATE category SET parent_id = 7 WHERE id = 5").await;
    let err = mabat::load::<CategoryCte>().by_key(6_i64).one(&mut db.conn).await.unwrap_err();
    assert!(matches!(&err, Error::Cycle { path, .. } if path == "children"), "{err}");

    db.drop().await;
}

#[tokio::test]
async fn recursive_overrides_apply_to_every_level() {
    let Some(mut db) = setup("collections_recursive_override").await else { return };
    // One override of the children query, used for all levels
    let overrides = r#"
        [query.children]
        sql = '''
        SELECT c.id AS "$key", c.parent_id AS "$parent", c.position AS "$index", c.name AS "name"
        FROM category c WHERE c.parent_id = ANY($1)
        '''
    "#;
    let mabat = Mabat::builder()
        .register::<CategoryTree>()
        .overrides("CategoryTree", overrides)
        .build(&mut db.conn)
        .await
        .unwrap();
    let explain = mabat.explain::<CategoryTree>().unwrap();
    assert!(explain.contains("children: repeats the query 0 level(s) up, at most 3 levels"), "{explain}");

    let generated = mabat::load::<CategoryTree>().by_key(1_i64).one(&mut db.conn).await.unwrap();
    let loaded = mabat.load::<CategoryTree>().by_key(1_i64).one(&mut db.conn).await.unwrap();
    assert_eq!(loaded, generated);

    db.drop().await;
}
