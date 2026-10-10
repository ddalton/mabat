//! The manifest and the snapshot that `build.rs` checks, kept up to date, and the views checked
//! against them.

use mabat::schema;
use mabat_check::Outcome;
use mabat_example_chinook::{schema_sql, views};
use sqlx::{AssertSqlSafe, Connection, Executor};

#[test]
fn views_manifest_is_up_to_date() {
    let manifest = views().manifest().unwrap();
    assert!(!manifest.write("mabat/views.json").unwrap(), "mabat/views.json was out of date; it is now written");
}

/// In an application, `mabat schema --out mabat/schema.json` against the database writes the
/// snapshot, and `mabat schema --check` in CI keeps it up to date.
#[tokio::test]
async fn schema_snapshot_is_up_to_date() {
    let mut conn = sqlx::SqliteConnection::connect("sqlite::memory:").await.unwrap();
    conn.execute(AssertSqlSafe(schema_sql())).await.unwrap();
    let json = schema::snapshot(&mut conn).await.unwrap().to_json();
    let current = std::fs::read_to_string("mabat/schema.json").unwrap_or_default();
    if current != json {
        std::fs::write("mabat/schema.json", json).unwrap();
        panic!("mabat/schema.json was out of date; it is now written");
    }
}

#[test]
fn the_views_match_the_schema() {
    let outcome = mabat_check::build("mabat/views.json", "mabat/schema.json").overrides("mabat/overrides").check();
    let Ok(Outcome::Checked(report)) = outcome else { panic!("{outcome:?}") };
    assert!(report.is_ok(), "{report}");
}

/// The README's example: a column the views read is gone, and the build fails with its line.
#[test]
fn a_schema_without_a_column_fails_the_build() {
    let json = std::fs::read_to_string("mabat/schema.json").unwrap();
    let mut snapshot = mabat_check::Snapshot::from_json(&json).unwrap();
    let album = snapshot.tables.iter_mut().find(|t| t.name == "album").unwrap();
    album.columns.retain(|c| c.name != "title");
    let path = std::env::temp_dir().join(format!("mabat-chinook-schema-{}.json", std::process::id()));
    std::fs::write(&path, snapshot.to_json()).unwrap();
    let outcome = mabat_check::build(std::path::Path::new("mabat/views.json"), &path).check().unwrap();
    let lines = mabat_check::directives(&outcome);
    std::fs::remove_file(&path).unwrap();
    assert!(
        lines.contains(&format!(
            "cargo::error=mabat error[M0202]: Discography.albums: column `album.title` is not in the schema ({})",
            path.display()
        )),
        "{lines:#?}"
    );
}
