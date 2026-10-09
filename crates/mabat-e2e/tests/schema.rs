//! Snapshots of a database's schema on every database: tables and views, column types and
//! nullability, generated keys, primary and foreign keys, and the differences after a change;
//! and views checked against them without a database.

mod boards;

use mabat::manifest::Manifest;
use mabat::{Conn, Mabat, View, schema};
use sqlx::{AssertSqlSafe, Connection, Executor};

/// The schema, with `{key}` the declaration of a generated 64 bit key.
const SCHEMA: &str = r#"
CREATE TABLE team (id {key}, name VARCHAR(100) NOT NULL, motto TEXT);
CREATE TABLE member (
    id BIGINT NOT NULL, team_id BIGINT NOT NULL, name VARCHAR(50),
    PRIMARY KEY (id, team_id), FOREIGN KEY (team_id) REFERENCES team (id)
);
CREATE VIEW team_name AS SELECT id, name FROM team;
"#;

#[derive(View, Debug)]
#[view(table = "team")]
pub struct TeamView {
    #[view(generated)]
    pub id: Option<i64>,
    pub name: String,
    pub motto: Option<String>,
    #[view(child(fk = "team_id", order_by = "id"))]
    pub members: Vec<MemberView>,
}

/// Keyed by `id`, which is only part of the primary key of `member`.
#[derive(View, Debug)]
#[view(table = "member")]
pub struct MemberView {
    pub id: i64,
    pub name: Option<String>,
}

/// The manifest of the views, for a database.
macro_rules! manifest {
    ($db:ty) => {
        Mabat::<$db>::builder()
            .register::<TeamView>()
            .register::<boards::Board>()
            .register::<boards::Doc>()
            .manifest()
            .unwrap()
    };
}

/// The codes and summaries of the diagnostics of checking the views against the snapshot.
fn check_views(manifest: &Manifest, snapshot: &schema::Snapshot) -> Vec<(&'static str, String)> {
    let report = manifest.check_snapshot(snapshot, "schema.json", &[]).unwrap();
    report.diagnostics().iter().map(|d| (d.code, d.summary.clone())).collect()
}

/// The views match the schema, and each change of the snapshot is found.
fn views_against(manifest: &Manifest, snapshot: &schema::Snapshot) {
    let member_key = (
        "M0206",
        "TeamView.members: the key of MemberView, `member.id`, is not the primary key of `member`".to_string(),
    );
    assert_eq!(check_views(manifest, snapshot), std::slice::from_ref(&member_key));
    let report = manifest.check_snapshot(snapshot, "schema.json", &[]).unwrap();
    assert!(report.is_ok(), "{report}");

    let changed = |change: &dyn Fn(&mut schema::Snapshot)| {
        let mut snapshot = snapshot.clone();
        change(&mut snapshot);
        let mut found = check_views(manifest, &snapshot);
        found.retain(|d| *d != member_key);
        found
    };
    let table = |snapshot: &mut schema::Snapshot, name: &str| -> usize {
        snapshot.tables.iter().position(|t| t.name == name).unwrap()
    };
    let column = |snapshot: &mut schema::Snapshot, t: &str, c: &str| -> (usize, usize) {
        let t = table(snapshot, t);
        (t, snapshot.tables[t].columns.iter().position(|col| col.name == c).unwrap())
    };

    // M0201: a table the views read is missing
    let found = changed(&|s| {
        let t = table(s, "team");
        s.tables[t].name = "teams".into();
    });
    assert_eq!(found, [("M0201", "TeamView.$root: table `team` is not in the schema".to_string())]);

    // M0202: a column is missing
    let found = changed(&|s| {
        let (t, c) = column(s, "team", "motto");
        s.tables[t].columns.remove(c);
    });
    assert_eq!(found, [("M0202", "TeamView.$root: column `team.motto` is not in the schema".to_string())]);

    // M0203: a column has a type the field cannot be decoded from
    let found = changed(&|s| {
        let (t, id) = column(s, "team", "id");
        let (_, name) = column(s, "team", "name");
        let id = s.tables[t].columns[id].clone();
        let name = &mut s.tables[t].columns[name];
        (name.r#type, name.declared) = (id.r#type, id.declared);
    });
    assert_eq!(
        found,
        [(
            "M0203",
            "TeamView.$root: path \"name\" is String, which cannot be decoded from column `team.name`".to_string()
        )]
    );

    // M0204: a nullable column under a field that is not an Option, a warning
    let found = changed(&|s| {
        let (t, c) = column(s, "team", "name");
        s.tables[t].columns[c].nullable = true;
    });
    assert_eq!(
        found,
        [("M0204", "TeamView.$root: path \"name\" is String, but column `team.name` is nullable".to_string())]
    );

    // M0205: the columns linking a query to its parent hold different kinds of key
    let found = changed(&|s| {
        let (t, name) = column(s, "team", "name");
        let text = s.tables[t].columns[name].clone();
        let (m, fk) = column(s, "member", "team_id");
        let fk = &mut s.tables[m].columns[fk];
        (fk.r#type, fk.declared) = (text.r#type, text.declared);
    });
    assert_eq!(
        found,
        [(
            "M0205",
            "TeamView.members: TeamView.members links `member.team_id` to `team.id`, which hold different kinds of key"
                .to_string()
        )]
    );

    // M0207: the view's key is generated, but the column is not
    let found = changed(&|s| {
        let (t, c) = column(s, "team", "id");
        s.tables[t].columns[c].generated = false;
    });
    assert_eq!(
        found,
        [(
            "M0207",
            "TeamView.$root: the key of TeamView is generated, but the database does not generate `team.id`"
                .to_string()
        )]
    );
}

/// Check the snapshot of the schema, and return it.
async fn check<C: Conn>(conn: &mut C) -> schema::Snapshot {
    let snapshot = schema::snapshot(conn).await.unwrap();
    let names: Vec<&str> = snapshot.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["member", "team", "team_name"]);

    let team = snapshot.table("team").unwrap();
    let columns: Vec<(&str, bool, bool)> =
        team.columns.iter().map(|c| (c.name.as_str(), c.nullable, c.generated)).collect();
    assert_eq!(columns, [("id", false, true), ("name", false, false), ("motto", true, false)]);
    assert!(team.columns.iter().all(|c| !c.r#type.is_empty() && !c.declared.is_empty()), "{team:?}");
    assert_eq!(team.primary_key, ["id"]);
    assert!(!team.view);

    let member = snapshot.table("member").unwrap();
    assert_eq!(member.primary_key, ["id", "team_id"]);
    assert_eq!(member.foreign_keys.len(), 1, "{member:?}");
    let fk = &member.foreign_keys[0];
    assert_eq!(
        (fk.columns.as_slice(), fk.table.as_str(), fk.references.as_slice()),
        (&["team_id".to_string()][..], "team", &["id".to_string()][..])
    );
    assert!(snapshot.table("team_name").unwrap().view);

    // The same schema, the same file; a change, a difference
    let again = schema::snapshot(conn).await.unwrap();
    assert_eq!(again.to_json(), snapshot.to_json());
    assert!(snapshot.differences(&again).is_empty());
    snapshot
}

/// The schema has a new column since the snapshot.
async fn drift<C: Conn>(conn: &mut C, snapshot: &schema::Snapshot) {
    let changed = schema::snapshot(conn).await.unwrap();
    assert_eq!(snapshot.differences(&changed), ["column `team.founded` is in the database but not in the snapshot"]);
}

const ADD_COLUMN: &str = "ALTER TABLE team ADD COLUMN founded DATE";

#[tokio::test]
async fn sqlite() {
    let mut conn = sqlx::SqliteConnection::connect("sqlite::memory:").await.unwrap();
    conn.execute(AssertSqlSafe(SCHEMA.replace("{key}", "INTEGER PRIMARY KEY"))).await.unwrap();
    let snapshot = check(&mut conn).await;
    conn.execute(ADD_COLUMN).await.unwrap();
    drift(&mut conn, &snapshot).await;
    conn.execute(boards::SCHEMA).await.unwrap();
    views_against(&manifest!(sqlx::Sqlite), &schema::snapshot(&mut conn).await.unwrap());
}

#[tokio::test]
async fn postgres() {
    let Ok(url) = std::env::var("MABAT_TEST_DATABASE_URL") else {
        eprintln!("skipping: MABAT_TEST_DATABASE_URL is not set (see scripts/with-postgres.sh)");
        return;
    };
    let mut conn = sqlx::PgConnection::connect(&url).await.unwrap();
    let schema = format!("mabat_schema_{}", std::process::id());
    let setup = format!(
        "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE; CREATE SCHEMA \"{schema}\"; SET search_path TO \"{schema}\""
    );
    conn.execute(AssertSqlSafe(setup)).await.unwrap();
    let ddl = SCHEMA.replace("{key}", "BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY");
    conn.execute(AssertSqlSafe(ddl)).await.unwrap();
    let snapshot = check(&mut conn).await;
    conn.execute(ADD_COLUMN).await.unwrap();
    drift(&mut conn, &snapshot).await;
    conn.execute(boards::SCHEMA).await.unwrap();
    views_against(&manifest!(sqlx::Postgres), &schema::snapshot(&mut conn).await.unwrap());
    conn.execute(AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE"))).await.unwrap();
}

#[tokio::test]
async fn mysql() {
    let Ok(url) = std::env::var("MABAT_TEST_MYSQL_URL") else {
        eprintln!("skipping: MABAT_TEST_MYSQL_URL is not set (see scripts/with-mysql.sh)");
        return;
    };
    let mut conn = sqlx::MySqlConnection::connect(&url).await.unwrap();
    let database = format!("mabat_schema_{}", std::process::id());
    let setup = format!("DROP DATABASE IF EXISTS `{database}`; CREATE DATABASE `{database}`; USE `{database}`");
    conn.execute(AssertSqlSafe(setup)).await.unwrap();
    conn.execute(AssertSqlSafe(SCHEMA.replace("{key}", "BIGINT AUTO_INCREMENT PRIMARY KEY"))).await.unwrap();
    let snapshot = check(&mut conn).await;
    conn.execute(ADD_COLUMN).await.unwrap();
    drift(&mut conn, &snapshot).await;
    conn.execute(boards::SCHEMA).await.unwrap();
    views_against(&manifest!(sqlx::MySql), &schema::snapshot(&mut conn).await.unwrap());
    conn.execute(AssertSqlSafe(format!("DROP DATABASE `{database}`"))).await.unwrap();
}
