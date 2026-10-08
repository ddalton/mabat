//! Features on MySQL that Chinook does not exercise: enums in columns, with a MySQL `ENUM`
//! tag, and in a table per variant, embedded structs, JSON, `BINARY(16)` UUID keys, unsigned
//! keys, keys computed by an override, and the checks. Each test runs in a new database on
//! the server of `MABAT_TEST_MYSQL_URL` (see `scripts/with-mysql.sh`), and is skipped
//! without one.
#![cfg(feature = "mysql")]

use chrono::{DateTime, NaiveDate, Utc};
use mabat::filter::col;
use mabat::{Mabat, Severity, View};
use sqlx::{AssertSqlSafe, Connection, Executor, MySqlConnection};
use uuid::Uuid;

const SCHEMA: &str = r#"
CREATE TABLE person (
    id   BINARY(16) PRIMARY KEY,
    name VARCHAR(100) NOT NULL
);
CREATE TABLE issue (
    id                BIGINT PRIMARY KEY,
    title             VARCHAR(100) NOT NULL,
    state             ENUM('open', 'assigned', 'blocked', 'closed') NOT NULL,
    assignee_id       BINARY(16) REFERENCES person (id),
    blocked_since     DATETIME,
    closed_resolution VARCHAR(100),
    metadata          JSON,
    payment_kind      VARCHAR(10) NOT NULL,
    done              BOOLEAN NOT NULL,
    due               DATE,
    geo_lat           DOUBLE,
    geo_lon           DOUBLE
);
CREATE TABLE card_payment (
    issue_id  BIGINT PRIMARY KEY REFERENCES issue (id),
    last4     VARCHAR(4) NOT NULL,
    holder_id BINARY(16) REFERENCES person (id)
);
CREATE TABLE comment (
    id        BIGINT UNSIGNED PRIMARY KEY,
    issue_id  BIGINT NOT NULL REFERENCES issue (id),
    author_id BINARY(16) NOT NULL REFERENCES person (id),
    body      TEXT NOT NULL
);
"#;

fn person(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// A connection to a new database, dropped by [`teardown`].
async fn setup() -> Option<MySqlConnection> {
    let Ok(url) = std::env::var("MABAT_TEST_MYSQL_URL") else {
        eprintln!("skipping: MABAT_TEST_MYSQL_URL is not set (see scripts/with-mysql.sh)");
        return None;
    };
    let mut conn = MySqlConnection::connect(&url).await.unwrap();
    let database = format!("mabat_test_{}", Uuid::new_v4().simple());
    conn.execute(AssertSqlSafe(format!("CREATE DATABASE `{database}`; USE `{database}`"))).await.unwrap();
    conn.execute(SCHEMA).await.unwrap();
    for (id, name) in [(1, "Ada"), (2, "Grace")] {
        sqlx::query("INSERT INTO person (id, name) VALUES (?, ?)")
            .bind(person(id))
            .bind(name)
            .execute(&mut conn)
            .await
            .unwrap();
    }
    let issues = r#"
        INSERT INTO issue VALUES
            (1, 'Crash', 'open',     NULL, NULL,                   NULL,    NULL,                   'none', 0, '2026-05-01', 52.5, 13.4),
            (2, 'Typo',  'assigned', ?,    NULL,                   NULL,    '{"labels": ["docs"]}', 'card', 0, NULL,         NULL, NULL),
            (3, 'Slow',  'blocked',  NULL, '2026-03-04 05:06:07',  NULL,    '{"labels": []}',       'none', 0, '2026-06-01', NULL, NULL),
            (4, 'Old',   'closed',   NULL, NULL,                   'fixed', NULL,                   'card', 1, NULL,         NULL, NULL)
    "#;
    sqlx::query(issues).bind(person(1)).execute(&mut conn).await.unwrap();
    conn.execute(
        "INSERT INTO card_payment VALUES (2, '4242', x'00000000000000000000000000000002'), (4, '1881', NULL);
         INSERT INTO comment VALUES (1, 1, x'00000000000000000000000000000002', 'seen'),
                                    (2, 1, x'00000000000000000000000000000001', 'me too'),
                                    (3, 4, x'00000000000000000000000000000002', 'done');",
    )
    .await
    .unwrap();
    Some(conn)
}

async fn teardown(mut conn: MySqlConnection) {
    let database: String = sqlx::query_scalar("SELECT DATABASE()").fetch_one(&mut conn).await.unwrap();
    conn.execute(AssertSqlSafe(format!("DROP DATABASE `{database}`"))).await.unwrap();
}

#[derive(View, Debug, PartialEq)]
#[view(table = "issue")]
struct IssueView {
    id: i64,
    title: String,
    #[view(embed)]
    state: State,
    #[view(json)]
    metadata: Option<Metadata>,
    #[view(embed(prefix = "payment_"))]
    payment: Payment,
    done: bool,
    due: Option<NaiveDate>,
    #[view(embed(prefix = "geo_"))]
    geo: Geo,
    #[view(child(fk = "issue_id", order_by = "id"))]
    comments: Vec<CommentView>,
}

/// Tagged by a MySQL `ENUM`.
#[derive(View, Debug, PartialEq)]
#[view(tag = "state")]
enum State {
    #[view(tag_value = "open")]
    Open,
    #[view(tag_value = "assigned")]
    Assigned { assignee_id: Uuid },
    #[view(tag_value = "blocked")]
    Blocked {
        #[view(column = "blocked_since")]
        since: DateTime<Utc>,
    },
    #[view(tag_value = "closed")]
    Closed(#[view(column = "closed_resolution")] String),
}

#[derive(serde::Deserialize, Debug, PartialEq)]
struct Metadata {
    labels: Vec<String>,
}

#[derive(View, Debug, PartialEq)]
#[view(tag = "kind", strategy = "table_per_variant")]
enum Payment {
    #[view(tag_value = "none")]
    Unpaid,
    #[view(tag_value = "card", table = "card_payment", key = "issue_id")]
    Card {
        last4: String,
        #[view(to_one(fk = "holder_id"))]
        holder: Option<PersonView>,
    },
}

#[derive(View, Debug, PartialEq)]
#[view(embedded)]
struct Geo {
    lat: Option<f64>,
    lon: Option<f64>,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "person")]
struct PersonView {
    id: Uuid,
    name: String,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "comment")]
struct CommentView {
    body: String,
    #[view(to_one(fk = "author_id"))]
    author: PersonView,
}

fn ada() -> PersonView {
    PersonView { id: person(1), name: "Ada".into() }
}

fn grace() -> PersonView {
    PersonView { id: person(2), name: "Grace".into() }
}

#[tokio::test]
async fn loads_enums_embedded_json_and_uuid_keys() {
    let Some(mut conn) = setup().await else { return };
    let issues = mabat::load::<IssueView>().order_by("id").all(&mut conn).await.unwrap();
    let none = || Geo { lat: None, lon: None };
    assert_eq!(
        issues,
        [
            IssueView {
                id: 1,
                title: "Crash".into(),
                state: State::Open,
                metadata: None,
                payment: Payment::Unpaid,
                done: false,
                due: Some(NaiveDate::from_ymd_opt(2026, 5, 1).unwrap()),
                geo: Geo { lat: Some(52.5), lon: Some(13.4) },
                comments: vec![
                    CommentView { body: "seen".into(), author: grace() },
                    CommentView { body: "me too".into(), author: ada() },
                ],
            },
            IssueView {
                id: 2,
                title: "Typo".into(),
                state: State::Assigned { assignee_id: person(1) },
                metadata: Some(Metadata { labels: vec!["docs".into()] }),
                payment: Payment::Card { last4: "4242".into(), holder: Some(grace()) },
                done: false,
                due: None,
                geo: none(),
                comments: vec![],
            },
            IssueView {
                id: 3,
                title: "Slow".into(),
                state: State::Blocked { since: "2026-03-04T05:06:07Z".parse().unwrap() },
                metadata: Some(Metadata { labels: vec![] }),
                payment: Payment::Unpaid,
                done: false,
                due: Some(NaiveDate::from_ymd_opt(2026, 6, 1).unwrap()),
                geo: none(),
                comments: vec![],
            },
            IssueView {
                id: 4,
                title: "Old".into(),
                state: State::Closed("fixed".into()),
                metadata: None,
                payment: Payment::Card { last4: "1881".into(), holder: None },
                done: true,
                due: None,
                geo: none(),
                comments: vec![CommentView { body: "done".into(), author: grace() }],
            },
        ]
    );

    // By UUID key
    let people = mabat::load::<PersonView>().by_keys([person(2), person(1)]).all(&mut conn).await.unwrap();
    let mut names: Vec<String> = people.into_iter().map(|p| p.name).collect();
    names.sort();
    assert_eq!(names, ["Ada", "Grace"]);
    teardown(conn).await;
}

#[tokio::test]
async fn filters_on_booleans_dates_and_text() {
    let Some(mut conn) = setup().await else { return };
    let ids = |issues: Vec<IssueView>| issues.into_iter().map(|i| i.id).collect::<Vec<_>>();

    let done = mabat::load::<IssueView>().filter(col("done").eq(true)).all(&mut conn).await.unwrap();
    assert_eq!(ids(done), [4]);
    let due = mabat::load::<IssueView>()
        .filter(col("due").lt(NaiveDate::from_ymd_opt(2026, 5, 15).unwrap()))
        .all(&mut conn)
        .await
        .unwrap();
    assert_eq!(ids(due), [1]);
    let titles = mabat::load::<IssueView>()
        .filter(col("title").ilike("%O%") & col("state").is_in(["closed", "blocked"]))
        .order_by("id")
        .all(&mut conn)
        .await
        .unwrap();
    assert_eq!(ids(titles), [3, 4]);
    let n = mabat::load::<IssueView>().filter(col("metadata").is_null()).count(&mut conn).await.unwrap();
    assert_eq!(n, 2);
    teardown(conn).await;
}

#[derive(View, Debug, PartialEq)]
#[view(table = "issue")]
struct IssueSummary {
    id: i64,
    title: String,
    #[view(child(fk = "issue_id", order_by = "id"))]
    comments: Vec<CommentView>,
}

/// Keys computed by expressions, and the unsigned keys of comments.
const COMPUTED_KEYS: &str = r#"
-- mabat: query $root
SELECT i.id + 0 AS "id", upper(i.title) AS "title" FROM issue i

-- mabat: query comments
SELECT c.issue_id + 0 AS "$parent", c.id AS "$key", c.body AS "body", c.author_id AS "$ref.author"
FROM comment c
WHERE c.issue_id IN (:keys)
ORDER BY c.id
"#;

#[tokio::test]
async fn overrides_with_computed_keys() {
    let Some(mut conn) = setup().await else { return };
    let mabat = Mabat::builder().register::<IssueSummary>().overrides_sql("IssueSummary", COMPUTED_KEYS);
    let mabat = mabat.build(&mut conn).await.unwrap();
    assert!(mabat.report().diagnostics().is_empty(), "{}", mabat.report());

    let issues = mabat.load::<IssueSummary>().order_by("id").all(&mut conn).await.unwrap();
    assert_eq!(issues.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["CRASH", "TYPO", "SLOW", "OLD"]);
    assert_eq!(issues[0].comments.iter().map(|c| &c.author).collect::<Vec<_>>(), [&grace(), &ada()]);
    assert_eq!(issues[3].comments.len(), 1);

    let one = mabat.load::<IssueSummary>().by_key(4_i64).one(&mut conn).await.unwrap();
    assert_eq!(one.title, "OLD");
    teardown(conn).await;
}

#[tokio::test]
async fn checks_find_errors_in_overrides() {
    let Some(mut conn) = setup().await else { return };

    let report = Mabat::<sqlx::MySql>::builder().register::<IssueView>().check(&mut conn).await.unwrap();
    assert!(report.diagnostics().is_empty(), "{report}");

    let missing_column = r#"
-- mabat: query $root
SELECT i.id AS "id", i.headline AS "title" FROM issue i
"#;
    let report = Mabat::builder()
        .register::<IssueSummary>()
        .overrides_sql("IssueSummary", missing_column)
        .check(&mut conn)
        .await
        .unwrap();
    let errors: Vec<String> =
        report.diagnostics().iter().filter(|d| d.severity == Severity::Error).map(|d| d.to_string()).collect();
    assert!(errors.iter().any(|e| e.contains("headline")), "{report}");

    // Text where the view has an integer
    let wrong_type = r#"
-- mabat: query $root
SELECT i.title AS "id", i.title AS "title" FROM issue i
"#;
    let report = Mabat::builder()
        .register::<IssueSummary>()
        .overrides_sql("IssueSummary", wrong_type)
        .check(&mut conn)
        .await
        .unwrap();
    assert!(!report.is_ok(), "{report}");
    assert!(report.to_string().contains("VARCHAR"), "{report}");

    // The checks leave nothing behind
    let tables: i64 = sqlx::query_scalar(AssertSqlSafe(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = DATABASE()",
    ))
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(tables, 4);
    teardown(conn).await;
}
