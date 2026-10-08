//! Arguments of nested collections: a filter, an order and a page for the elements of each
//! parent.

mod common;

use common::fixture::{TaskView, id, setup};
use mabat::filter::col;
use mabat::{Error, Mabat, Nested, Selection};

fn children(tasks: &[TaskView]) -> Vec<Vec<&str>> {
    tasks.iter().map(|t| t.children.iter().map(|c| c.name.as_str()).collect()).collect()
}

#[tokio::test]
async fn filter_order_and_page_the_elements_of_each_parent() {
    let Some(mut db) = setup("nested_args").await else { return };
    let roots = || mabat::load::<TaskView>().filter(col("parent_id").is_null()).order_by("name");

    // Hiring has Interview; Release has Fix bugs, Tag build and Write docs, by position
    let all = roots().all(&mut db.conn).await.unwrap();
    assert_eq!(children(&all), [vec!["Interview"], vec!["Fix bugs", "Tag build", "Write docs"]]);

    let filtered = roots().nested("children", Nested::new().filter(col("name").ilike("%t%"))).all(&mut db.conn);
    assert_eq!(children(&filtered.await.unwrap()), [vec!["Interview"], vec!["Tag build", "Write docs"]]);

    let ordered = roots().nested("children", Nested::new().order_by_desc("name")).all(&mut db.conn).await.unwrap();
    assert_eq!(children(&ordered), [vec!["Interview"], vec!["Write docs", "Tag build", "Fix bugs"]]);

    // A page per parent: the first of each, then the second and third
    let first = roots().nested("children", Nested::new().limit(1)).all(&mut db.conn).await.unwrap();
    assert_eq!(children(&first), [vec!["Interview"], vec!["Fix bugs"]]);
    let rest = roots().nested("children", Nested::new().offset(1).limit(2)).all(&mut db.conn).await.unwrap();
    assert_eq!(children(&rest), [vec![], vec!["Tag build", "Write docs"]]);

    // Two levels: the first note of each subtask, newest first
    let notes = roots()
        .nested("children", Nested::new().filter(col("name").ne("Tag build")))
        .nested("children.notes", Nested::new().order_by_desc("id").limit(1))
        .all(&mut db.conn)
        .await
        .unwrap();
    let release: Vec<(&str, Vec<&str>)> = notes[1]
        .children
        .iter()
        .map(|c| (c.name.as_str(), c.notes.iter().map(|n| n.body.as_str()).collect()))
        .collect();
    assert_eq!(release, [("Fix bugs", vec!["Typo in error"]), ("Write docs", vec!["Untagged"])]);

    // As JSON with a selection
    let json = mabat::load::<TaskView>()
        .by_key(id(1))
        .select(Selection::parse("children { name }").unwrap())
        .nested("children", Nested::new().order_by_desc("position").limit(2))
        .json(&mut db.conn)
        .await
        .unwrap();
    assert_eq!(json, [serde_json::json!({ "children": [{ "name": "Write docs" }, { "name": "Tag build" }] })]);

    db.drop().await;
}

#[tokio::test]
async fn overridden_collections_take_arguments() {
    let Some(mut db) = setup("nested_override").await else { return };
    let shouting = r#"
-- mabat: query children
SELECT c.id AS "$key", c.parent_id AS "$parent", upper(c.name) AS "name", c.position AS "position"
FROM task c
WHERE c.parent_id = ANY(:keys)
ORDER BY c.position
"#;
    let mabat = Mabat::builder().register::<TaskView>().overrides_sql("TaskView", shouting).build(&mut db.conn);
    let mabat = mabat.await.unwrap();

    // The override's columns, by the alias they are selected as
    let tasks = mabat
        .load::<TaskView>()
        .by_key(id(1))
        .nested("children", Nested::new().filter(col("position").ge(1_i32)).order_by_desc("position").limit(1))
        .all(&mut db.conn)
        .await
        .unwrap();
    assert_eq!(children(&tasks), [vec!["WRITE DOCS"]]);

    // A column the override does not select cannot order the collection
    let error = mabat
        .load::<TaskView>()
        .by_key(id(1))
        .nested("children", Nested::new().order_by("created_at"))
        .all(&mut db.conn)
        .await
        .unwrap_err();
    assert!(matches!(error, Error::ColumnNotSelected { ref column, .. } if column == "created_at"), "{error}");

    db.drop().await;
}

#[tokio::test]
async fn arguments_need_a_collection() {
    let Some(mut db) = setup("nested_errors").await else { return };
    for (path, reason) in
        [("nope", "no query of the view has this name"), ("assignee", "only to-many collections take arguments")]
    {
        let error = mabat::load::<TaskView>().nested(path, Nested::new().limit(1)).all(&mut db.conn).await.unwrap_err();
        assert_eq!(error.to_string(), format!("TaskView: the arguments of `{path}` cannot be applied: {reason}"));
    }
    db.drop().await;
}
