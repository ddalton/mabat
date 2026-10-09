//! The manifest and the snapshot that `build.rs` checks, kept up to date, and what the check
//! finds when the schema no longer matches the views.

use std::path::Path;

use mabat::schema;
use mabat_check::{Outcome, Severity, directives};
use mabat_example_build::views;
use sqlx::{AssertSqlSafe, Connection, Executor};

/// Write `content` to the file unless it holds it already; `true` if it was written.
fn update(path: &str, content: &str) -> bool {
    if std::fs::read_to_string(path).is_ok_and(|current| current == content) {
        return false;
    }
    std::fs::write(path, content).unwrap();
    true
}

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
    let ddl = std::fs::read_to_string("mabat/schema.sql").unwrap();
    conn.execute(AssertSqlSafe(ddl)).await.unwrap();
    let snapshot = schema::snapshot(&mut conn).await.unwrap();
    assert!(!update("mabat/schema.json", &snapshot.to_json()), "mabat/schema.json was out of date; it is now written");
}

#[test]
fn the_views_match_the_schema() {
    let outcome = mabat_check::build("mabat/views.json", "mabat/schema.json").overrides("mabat/overrides").check();
    let Ok(Outcome::Checked(report)) = outcome else { panic!("{outcome:?}") };
    assert!(report.diagnostics().is_empty(), "{report}");
    assert!(directives(&Outcome::Checked(report)).is_empty());
}

#[test]
fn a_schema_that_no_longer_matches_fails_the_build() {
    let dir = std::env::temp_dir().join(format!("mabat-example-build-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let json = std::fs::read_to_string("mabat/schema.json").unwrap();
    // The `done` column is dropped, and `description` made NOT NULL
    let mut snapshot = mabat_check::Snapshot::from_json(&json).unwrap();
    let task = snapshot.tables.iter_mut().find(|t| t.name == "task").unwrap();
    task.columns.retain(|c| c.name != "done");
    let snapshot_path = dir.join("schema.json");
    std::fs::write(&snapshot_path, snapshot.to_json()).unwrap();

    let outcome = mabat_check::build(Path::new("mabat/views.json"), &snapshot_path).check().unwrap();
    let lines = directives(&outcome);
    let Outcome::Checked(report) = outcome else { panic!("not checked") };
    assert_eq!(report.errors().count(), 1, "{report}");
    assert_eq!(report.diagnostics()[0].severity, Severity::Error);
    assert_eq!(
        lines,
        [format!(
            "cargo::error=mabat error[M0202]: ProjectView.tasks: column `task.done` is not in the schema ({})",
            snapshot_path.display()
        )]
    );

    // Before the manifest test first writes the manifest, nothing is checked
    let missing = mabat_check::build(dir.join("views.json"), &snapshot_path).check().unwrap();
    assert!(matches!(missing, Outcome::Missing(_)));
    assert_eq!(directives(&missing).len(), 1);
    assert!(directives(&missing)[0].starts_with("cargo::warning=mabat: the views were not checked"));
    std::fs::remove_dir_all(dir).unwrap();
}
