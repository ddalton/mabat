//! Loading views as JSON, whole or a selection of their fields.

mod common;

use common::fixture::{TaskView, setup};
use mabat::filter::col;
use mabat::{Error, Mabat, Selection, View};
use serde_json::json;

#[tokio::test]
async fn whole_views_as_json() {
    let Some(mut db) = setup("json_whole").await else { return };

    let tasks = mabat::load::<TaskView>().by_key(common::fixture::id(1)).json(&mut db.conn).await.unwrap();
    assert_eq!(
        tasks,
        [json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "name": "Release",
            "description": "Ship 1.0",
            "created_at": "2026-01-02T03:04:05Z",
            "address": { "street": "1 Main St", "city": "Springfield", "geo": { "lat": 1.5, "lon": -2.5 } },
            "assignee": { "id": 1, "name": "Ada Lovelace", "email": "ada@example.com" },
            "children": [
                {
                    "name": "Fix bugs",
                    "position": 0,
                    "notes": [
                        { "body": "Crash on start", "tag": { "code": "bug", "label": "Bug" } },
                        { "body": "Typo in error", "tag": { "code": "bug", "label": "Bug" } },
                    ],
                },
                { "name": "Tag build", "position": 1, "notes": [] },
                {
                    "name": "Write docs",
                    "position": 2,
                    "notes": [
                        { "body": "Explain views", "tag": { "code": "doc", "label": "Documentation" } },
                        { "body": "Untagged", "tag": null },
                    ],
                },
            ],
        })]
    );

    // The same values as a typed load
    let typed = mabat::load::<TaskView>().by_key(common::fixture::id(1)).one(&mut db.conn).await.unwrap();
    assert_eq!(tasks[0]["children"][2]["notes"][1]["body"], typed.children[2].notes[1].body);

    db.drop().await;
}

#[tokio::test]
async fn selections_load_only_their_fields() {
    let Some(mut db) = setup("json_selection").await else { return };

    let selection = Selection::parse("name assignee { name } children { name notes { body } }").unwrap();
    let tasks = mabat::load::<TaskView>()
        .filter(col("parent_id").is_null())
        .order_by("name")
        .select(selection)
        .json(&mut db.conn)
        .await
        .unwrap();
    assert_eq!(
        tasks,
        [
            json!({
                "name": "Hiring",
                "assignee": { "name": "Alan Turing" },
                "children": [{ "name": "Interview", "notes": [] }],
            }),
            json!({
                "name": "Release",
                "assignee": { "name": "Ada Lovelace" },
                "children": [
                    { "name": "Fix bugs", "notes": [{ "body": "Crash on start" }, { "body": "Typo in error" }] },
                    { "name": "Tag build", "notes": [] },
                    { "name": "Write docs", "notes": [{ "body": "Explain views" }, { "body": "Untagged" }] },
                ],
            }),
        ]
    );

    // A view selected without fields: its columns and embedded values
    let tasks = mabat::load::<TaskView>()
        .by_key(common::fixture::id(2))
        .select(Selection::parse("assignee children").unwrap())
        .json(&mut db.conn)
        .await
        .unwrap();
    assert_eq!(
        tasks,
        [json!({
            "assignee": { "id": 2, "name": "Alan Turing", "email": null },
            "children": [{ "name": "Interview", "position": 0 }],
        })]
    );

    // Overrides apply to a selection: the subtasks come from an override that upper-cases
    // their names
    let shouting = r#"
-- mabat: query children
SELECT c.id AS "$key", c.parent_id AS "$parent", upper(c.name) AS "name", c.position AS "position"
FROM task c
WHERE c.parent_id = ANY(:keys)
ORDER BY c.position
"#;
    let mabat = Mabat::builder().register::<TaskView>().overrides_sql("TaskView", shouting).build(&mut db.conn);
    let mabat = mabat.await.unwrap();
    let tasks = mabat
        .load::<TaskView>()
        .by_key(common::fixture::id(1))
        .select(Selection::parse("name children { name }").unwrap())
        .json(&mut db.conn)
        .await
        .unwrap();
    assert_eq!(
        tasks,
        [json!({
            "name": "Release",
            "children": [{ "name": "FIX BUGS" }, { "name": "TAG BUILD" }, { "name": "WRITE DOCS" }],
        })]
    );

    db.drop().await;
}

/// A column type without `serde::Serialize`.
#[derive(Debug, PartialEq)]
struct Code(String);

impl sqlx::Type<sqlx::Postgres> for Code {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for Code {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        Ok(Code(<String as sqlx::Decode<sqlx::Postgres>>::decode(value)?))
    }
}

#[derive(View, Debug)]
#[view(table = "tag", key = "code", databases = "postgres")]
struct TagCode {
    code: Code,
    label: String,
}

#[tokio::test]
async fn columns_without_serialize_fail_only_when_loaded() {
    let Some(mut db) = setup("json_serialize").await else { return };

    // Typed loads are not affected
    let tags = mabat::load::<TagCode>().order_by("code").all(&mut db.conn).await.unwrap();
    assert_eq!((&tags[0].code, tags[0].label.as_str()), (&Code("bug".into()), "Bug"));

    let error = mabat::load::<TagCode>().json(&mut db.conn).await.unwrap_err();
    assert!(matches!(error, Error::Json { .. }), "{error}");
    assert!(error.to_string().contains("json::Code does not implement serde::Serialize"), "{error}");
    assert!(error.to_string().contains("TagCode: `code`"), "{error}");

    // Without the column, the rest loads
    let labels = mabat::load::<TagCode>()
        .order_by("code")
        .select(Selection::parse("label").unwrap())
        .json(&mut db.conn)
        .await
        .unwrap();
    assert_eq!(labels, [json!({ "label": "Bug" }), json!({ "label": "Documentation" })]);

    db.drop().await;
}

#[tokio::test]
async fn selections_need_json_and_fields_of_the_view() {
    let Some(mut db) = setup("json_errors").await else { return };

    let error = mabat::load::<TaskView>().select(Selection::parse("name").unwrap()).all(&mut db.conn).await;
    assert!(matches!(error, Err(Error::SelectionWithoutJson { view: "TaskView" })), "{error:?}");

    let error = mabat::load::<TaskView>().select(Selection::parse("children { title }").unwrap()).json(&mut db.conn);
    let error = error.await.unwrap_err().to_string();
    assert_eq!(error, "SubtaskView at `children`: cannot select `title`: no such field");

    db.drop().await;
}
