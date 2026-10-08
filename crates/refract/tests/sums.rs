//! Enums with data: stored in columns of the row, in a table per variant, or as JSON.

mod common;

use chrono::{DateTime, Utc};
use common::TestDb;
use refract::{Error, Refract, View};

const SCHEMA: &str = r#"
CREATE TYPE issue_state AS ENUM ('open', 'assigned', 'blocked', 'closed');
CREATE TABLE person (
    id   BIGINT PRIMARY KEY,
    name TEXT NOT NULL
);
CREATE TABLE issue (
    id                        BIGINT PRIMARY KEY,
    title                     TEXT NOT NULL,
    state                     issue_state NOT NULL,
    assignee                  TEXT,
    blocked_reason_kind       TEXT,
    blocked_reason_waiting_on TEXT,
    blocked_since             TIMESTAMPTZ,
    closed_resolution         TEXT,
    metadata                  JSONB,
    payment_kind              TEXT
);
CREATE TABLE card_payment (
    issue_id  BIGINT PRIMARY KEY REFERENCES issue (id),
    last4     TEXT NOT NULL,
    holder_id BIGINT REFERENCES person (id)
);
CREATE TABLE bank_payment (
    issue_id BIGINT PRIMARY KEY REFERENCES issue (id),
    iban     TEXT NOT NULL
);
CREATE TABLE payment_note (
    id              SERIAL PRIMARY KEY,
    bank_payment_id BIGINT NOT NULL REFERENCES bank_payment (issue_id),
    body            TEXT NOT NULL
);

INSERT INTO person (id, name) VALUES (7, 'Grace Hopper');
INSERT INTO issue (id, title, state, assignee, blocked_reason_kind, blocked_reason_waiting_on, blocked_since,
                   closed_resolution, metadata, payment_kind) VALUES
    (1, 'Crash',    'open',     NULL,   NULL,      NULL,     NULL,                   NULL,    NULL,                     'none'),
    (2, 'Typo',     'assigned', 'ada',  NULL,      NULL,     NULL,                   NULL,    '{"labels": ["docs"]}',   'card'),
    (3, 'Slow',     'blocked',  NULL,   'waiting', 'vendor', '2026-03-04T05:06:07Z', NULL,    NULL,                     'bank'),
    (4, 'Flaky',    'blocked',  NULL,   'other',   NULL,     NULL,                   NULL,    '{"labels": []}',         'card'),
    (5, 'Old',      'closed',   NULL,   NULL,      NULL,     NULL,                   'fixed', NULL,                     'none');
INSERT INTO card_payment (issue_id, last4, holder_id) VALUES (2, '4242', 7), (4, '1881', NULL);
INSERT INTO bank_payment (issue_id, iban) VALUES (3, 'DE89 3704');
INSERT INTO payment_note (bank_payment_id, body) VALUES (3, 'first'), (3, 'second');
"#;

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
}

/// Stored in columns of `issue`, with a PostgreSQL enum as the tag.
#[derive(View, Debug, PartialEq)]
#[view(tag = "state")]
enum State {
    #[view(tag_value = "open")]
    Open,
    #[view(tag_value = "assigned")]
    Assigned { assignee: String },
    #[view(tag_value = "blocked")]
    Blocked {
        #[view(embed(prefix = "blocked_reason_"))]
        reason: Reason,
        #[view(column = "blocked_since")]
        since: Option<DateTime<Utc>>,
    },
    #[view(tag_value = "closed")]
    Closed(#[view(column = "closed_resolution")] String),
}

/// Nested in a variant of `State`.
#[derive(View, Debug, PartialEq)]
#[view(tag = "kind")]
enum Reason {
    #[view(tag_value = "waiting")]
    Waiting {
        #[view(column = "waiting_on")]
        on: String,
    },
    #[view(tag_value = "other")]
    Other,
}

#[derive(serde::Deserialize, Debug, PartialEq)]
struct Metadata {
    labels: Vec<String>,
}

/// Stored in a table per variant, keyed by the issue.
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
    #[view(tag_value = "bank", table = "bank_payment", key = "issue_id")]
    Bank {
        iban: String,
        #[view(child(fk = "bank_payment_id", order_by = "id"))]
        notes: Vec<PaymentNote>,
    },
}

#[derive(View, Debug, PartialEq)]
#[view(table = "person")]
struct PersonView {
    name: String,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "payment_note")]
struct PaymentNote {
    body: String,
}

async fn setup(name: &str) -> Option<TestDb> {
    TestDb::new(name, SCHEMA).await
}

fn expected() -> Vec<IssueView> {
    vec![
        IssueView { id: 1, title: "Crash".into(), state: State::Open, metadata: None, payment: Payment::Unpaid },
        IssueView {
            id: 2,
            title: "Typo".into(),
            state: State::Assigned { assignee: "ada".into() },
            metadata: Some(Metadata { labels: vec!["docs".into()] }),
            payment: Payment::Card { last4: "4242".into(), holder: Some(PersonView { name: "Grace Hopper".into() }) },
        },
        IssueView {
            id: 3,
            title: "Slow".into(),
            state: State::Blocked {
                reason: Reason::Waiting { on: "vendor".into() },
                since: Some("2026-03-04T05:06:07Z".parse().unwrap()),
            },
            metadata: None,
            payment: Payment::Bank {
                iban: "DE89 3704".into(),
                notes: vec![PaymentNote { body: "first".into() }, PaymentNote { body: "second".into() }],
            },
        },
        IssueView {
            id: 4,
            title: "Flaky".into(),
            state: State::Blocked { reason: Reason::Other, since: None },
            metadata: Some(Metadata { labels: vec![] }),
            payment: Payment::Card { last4: "1881".into(), holder: None },
        },
        IssueView {
            id: 5,
            title: "Old".into(),
            state: State::Closed("fixed".into()),
            metadata: None,
            payment: Payment::Unpaid,
        },
    ]
}

#[tokio::test]
async fn loads_every_variant() {
    let Some(mut db) = setup("sums").await else { return };

    let issues = refract::load::<IssueView>().order_by("id").all(&mut db.conn).await.unwrap();
    assert_eq!(issues, expected());

    let one = refract::load::<IssueView>().by_key(3_i64).one(&mut db.conn).await.unwrap();
    assert_eq!(one, expected().remove(2));

    db.drop().await;
}

#[test]
fn plan_has_a_query_per_variant_table() {
    let plan = refract::plan::<IssueView>().unwrap();
    let mut names = Vec::new();
    plan.walk(&mut |p| names.push(p.query_name().to_string()));
    assert_eq!(names, ["$root", "payment.Card", "payment.Card.holder", "payment.Bank", "payment.Bank.notes"]);

    let aliases: Vec<&str> = plan.columns.iter().map(|c| c.alias.as_str()).collect();
    assert_eq!(
        aliases,
        [
            "id",
            "title",
            "state.$tag",
            "state.Assigned.assignee",
            "state.Blocked.reason.$tag",
            "state.Blocked.reason.Waiting.on",
            "state.Blocked.since",
            "state.Closed.0",
            "metadata",
            "payment.$tag",
        ]
    );
}

#[tokio::test]
async fn generated_queries_pass_the_checks() {
    let Some(mut db) = setup("sums_check").await else { return };
    let report = Refract::builder().register::<IssueView>().check(&mut db.conn).await.unwrap();
    assert!(report.diagnostics().is_empty(), "{report}");
    db.drop().await;
}

#[tokio::test]
async fn columns_of_other_variants_must_be_null() {
    let Some(mut db) = setup("sums_strict").await else { return };
    db.execute("UPDATE issue SET assignee = 'bob' WHERE id = 1").await;

    let err = refract::load::<IssueView>().by_key(1_i64).one(&mut db.conn).await.unwrap_err();
    match &err {
        Error::OtherVariantColumn { view, path, tag } => {
            assert_eq!((*view, path.as_str(), tag.as_str()), ("IssueView", "state.Assigned.assignee", "open"));
        }
        other => panic!("unexpected error: {other}"),
    }

    // A lenient enum ignores them
    #[derive(View, Debug, PartialEq)]
    #[view(table = "issue")]
    struct LenientIssue {
        #[view(embed)]
        state: LenientState,
    }

    #[derive(View, Debug, PartialEq)]
    #[view(tag = "state", lenient)]
    enum LenientState {
        #[view(tag_value = "open")]
        Open,
        #[view(tag_value = "assigned")]
        Assigned { assignee: String },
        #[view(tag_value = "blocked")]
        Blocked,
        #[view(tag_value = "closed")]
        Closed,
    }

    let issue = refract::load::<LenientIssue>().by_key(1_i64).one(&mut db.conn).await.unwrap();
    assert_eq!(issue.state, LenientState::Open);

    db.drop().await;
}

#[tokio::test]
async fn unknown_and_null_tags_are_errors() {
    #[derive(View, Debug)]
    #[view(table = "issue")]
    struct NarrowIssue {
        #[view(embed)]
        #[allow(dead_code)]
        state: NarrowState,
    }

    #[derive(View, Debug)]
    #[view(tag = "state")]
    enum NarrowState {
        #[view(tag_value = "open")]
        Open,
        #[view(tag_value = "closed")]
        Closed,
    }

    let Some(mut db) = setup("sums_tags").await else { return };
    let err = refract::load::<NarrowIssue>().by_key(2_i64).one(&mut db.conn).await.unwrap_err();
    match &err {
        Error::UnknownTag { path, tag, expected, .. } => {
            assert_eq!((path.as_str(), tag.as_str()), ("state.$tag", "assigned"));
            assert_eq!(expected, &["open", "closed"]);
        }
        other => panic!("unexpected error: {other}"),
    }

    db.execute("UPDATE issue SET payment_kind = NULL WHERE id = 1").await;
    let err = refract::load::<IssueView>().by_key(1_i64).one(&mut db.conn).await.unwrap_err();
    assert!(matches!(&err, Error::NullTag { path, .. } if path == "payment.$tag"), "{err}");

    db.drop().await;
}

#[tokio::test]
async fn missing_variant_rows_are_errors() {
    let Some(mut db) = setup("sums_missing").await else { return };
    db.execute("UPDATE issue SET payment_kind = 'bank' WHERE id = 5").await;

    let err = refract::load::<IssueView>().by_key(5_i64).one(&mut db.conn).await.unwrap_err();
    assert!(matches!(&err, Error::MissingVariant { view: "IssueView", path } if path == "payment.Bank"), "{err}");

    db.drop().await;
}

/// The tag computed from other columns, and a variant table read through a join.
const OVERRIDES: &str = r#"
[query."$root"]
sql = '''
SELECT i.id AS "id",
       i.title AS "title",
       CASE WHEN i.closed_resolution IS NOT NULL THEN 'closed' ELSE i.state::text END AS "state.$tag",
       i.assignee AS "state.Assigned.assignee",
       i.blocked_reason_kind AS "state.Blocked.reason.$tag",
       i.blocked_reason_waiting_on AS "state.Blocked.reason.Waiting.on",
       i.blocked_since AS "state.Blocked.since",
       i.closed_resolution AS "state.Closed.0",
       i.metadata AS "metadata",
       i.payment_kind AS "payment.$tag"
FROM issue i
'''

[query."payment.Card"]
sql = '''
SELECT c.issue_id AS "$key", c.last4 AS "last4", c.holder_id AS "$ref.holder"
FROM card_payment c JOIN issue i ON i.id = c.issue_id
WHERE c.issue_id = ANY($1) AND i.payment_kind = 'card'
'''
"#;

#[tokio::test]
async fn overrides_of_enums() {
    let Some(mut db) = setup("sums_overrides").await else { return };
    let refract =
        Refract::builder().register::<IssueView>().overrides("IssueView", OVERRIDES).build(&mut db.conn).await.unwrap();
    assert!(refract.report().diagnostics().is_empty(), "{}", refract.report());
    let explain = refract.explain::<IssueView>().unwrap();
    assert!(
        explain.contains("payment.Card: Payment::Card (variant where payment.$tag = 'card')\n    override"),
        "{explain}"
    );

    let issues = refract.load::<IssueView>().order_by("id").all(&mut db.conn).await.unwrap();
    assert_eq!(issues, expected());

    // Variant paths are checked like any other path
    let broken = OVERRIDES
        .replace("AS \"state.Assigned.assignee\"", "AS \"state.Asigned.assignee\"")
        .replace("ELSE i.state::text END", "ELSE NULL END::int");
    let report =
        Refract::builder().register::<IssueView>().overrides("IssueView", broken).check(&mut db.conn).await.unwrap();
    let notes: Vec<&str> = report.errors().flat_map(|d| d.notes.iter().map(String::as_str)).collect();
    assert!(
        notes.contains(&"column 4 \"state.Asigned.assignee\" is not a path of IssueView in this query (did you mean \"state.Assigned.assignee\"?)"),
        "{report}"
    );
    assert!(notes.contains(&"path \"state.Assigned.assignee\" is not selected"), "{report}");
    assert!(
        notes.iter().any(|n| n.starts_with("column 3 \"state.$tag\" has type INT4, expected TEXT for String")),
        "{report}"
    );

    db.drop().await;
}
