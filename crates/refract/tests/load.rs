//! Loading views from PostgreSQL.

mod common;

use common::fixture::*;
use uuid::Uuid;

#[tokio::test]
async fn loads_a_whole_aggregate() {
    let Some(mut db) = setup("aggregate").await else { return };

    let task = refract::load::<TaskView>().by_key(id(1)).one(&mut db.conn).await.unwrap();

    let expected = TaskView {
        id: id(1),
        name: "Release".into(),
        description: Some("Ship 1.0".into()),
        created_at: "2026-01-02T03:04:05Z".parse().unwrap(),
        address: Address {
            street: "1 Main St".into(),
            city: "Springfield".into(),
            geo: Geo { lat: Some(1.5), lon: Some(-2.5) },
        },
        assignee: Some(PersonView { id: 1, name: "Ada Lovelace".into(), email: Some("ada@example.com".into()) }),
        children: vec![
            SubtaskView {
                name: "Fix bugs".into(),
                position: 0,
                notes: vec![
                    NoteView {
                        body: "Crash on start".into(),
                        tag: Some(TagView { code: "bug".into(), label: "Bug".into() }),
                    },
                    NoteView {
                        body: "Typo in error".into(),
                        tag: Some(TagView { code: "bug".into(), label: "Bug".into() }),
                    },
                ],
            },
            SubtaskView { name: "Tag build".into(), position: 1, notes: vec![] },
            SubtaskView {
                name: "Write docs".into(),
                position: 2,
                notes: vec![
                    NoteView {
                        body: "Explain views".into(),
                        tag: Some(TagView { code: "doc".into(), label: "Documentation".into() }),
                    },
                    NoteView { body: "Untagged".into(), tag: None },
                ],
            },
        ],
    };
    assert_eq!(task, expected);

    db.drop().await;
}

#[tokio::test]
async fn children_are_attached_to_their_own_parent() {
    let Some(mut db) = setup("parents").await else { return };

    let tasks = refract::load::<TaskView>().by_keys([id(1), id(2)]).order_by("name").all(&mut db.conn).await.unwrap();

    let summary: Vec<(String, Vec<String>)> =
        tasks.iter().map(|t| (t.name.clone(), t.children.iter().map(|c| c.name.clone()).collect())).collect();
    assert_eq!(
        summary,
        [
            ("Hiring".to_string(), vec!["Interview".to_string()]),
            ("Release".to_string(), vec!["Fix bugs".to_string(), "Tag build".to_string(), "Write docs".to_string()]),
        ]
    );
    assert_eq!(tasks[0].assignee.as_ref().map(|p| p.name.as_str()), Some("Alan Turing"));
    assert_eq!(tasks[0].address.geo, Geo { lat: None, lon: None });

    db.drop().await;
}

#[tokio::test]
async fn root_paging() {
    let Some(mut db) = setup("paging").await else { return };

    let names: Vec<String> = refract::load::<SubtaskView>()
        .order_by_desc("name")
        .limit(3)
        .offset(1)
        .all(&mut db.conn)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    // All tasks by name descending: Write docs, Tag build, Release, Interview, Hiring, Fix bugs
    assert_eq!(names, ["Tag build", "Release", "Interview"]);

    db.drop().await;
}

#[tokio::test]
async fn one_and_optional() {
    let Some(mut db) = setup("one").await else { return };

    let missing = refract::load::<TaskView>().by_key(id(99)).optional(&mut db.conn).await.unwrap();
    assert!(missing.is_none());

    let err = refract::load::<TaskView>().by_key(id(99)).one(&mut db.conn).await.unwrap_err();
    assert!(matches!(err, refract::Error::NotFound { view: "TaskView" }), "{err}");

    let err = refract::load::<TaskView>().by_keys([id(1), id(2)]).one(&mut db.conn).await.unwrap_err();
    assert!(matches!(err, refract::Error::TooManyRows { view: "TaskView", count: 2 }), "{err}");

    let none = refract::load::<TaskView>().by_keys(Vec::<Uuid>::new()).all(&mut db.conn).await.unwrap();
    assert!(none.is_empty());

    db.drop().await;
}

#[tokio::test]
async fn decode_errors_name_the_path() {
    let Some(mut db) = setup("decode").await else { return };

    // The child task has no description
    let err = refract::load::<StrictTaskView>().by_key(id(0x11)).one(&mut db.conn).await.unwrap_err();
    match &err {
        refract::Error::Decode { view, path, .. } => {
            assert_eq!(*view, "StrictTaskView");
            assert_eq!(path, "description");
        }
        other => panic!("unexpected error: {other}"),
    }

    // The child task has no assignee, but the view requires one
    let err = refract::load::<AssignedTaskView>().by_key(id(0x11)).one(&mut db.conn).await.unwrap_err();
    match &err {
        refract::Error::MissingReference { view, path } => {
            assert_eq!(*view, "AssignedTaskView");
            assert_eq!(path, "assignee");
        }
        other => panic!("unexpected error: {other}"),
    }

    db.drop().await;
}

#[tokio::test]
async fn query_errors_include_the_sql() {
    let Some(mut db) = setup("query_error").await else { return };
    db.execute("ALTER TABLE task_note RENAME COLUMN body TO text").await;

    let err = refract::load::<TaskView>().by_key(id(1)).one(&mut db.conn).await.unwrap_err();
    match &err {
        refract::Error::Query { view, path, sql, .. } => {
            assert_eq!(*view, "NoteView");
            assert_eq!(path, "children.notes");
            assert!(sql.contains("FROM \"task_note\""), "{sql}");
        }
        other => panic!("unexpected error: {other}"),
    }

    db.drop().await;
}

#[tokio::test]
async fn sees_uncommitted_changes_of_the_transaction() {
    use sqlx::Connection;

    let Some(mut db) = setup("transaction").await else { return };

    let mut tx = db.conn.begin().await.unwrap();
    sqlx::query("INSERT INTO task (id, name, parent_id, position) VALUES ($1, 'Late addition', $2, 9)")
        .bind(id(0x14))
        .bind(id(1))
        .execute(&mut *tx)
        .await
        .unwrap();
    let task = refract::load::<TaskView>().by_key(id(1)).one(&mut tx).await.unwrap();
    assert_eq!(task.children.last().map(|c| c.name.as_str()), Some("Late addition"));
    tx.rollback().await.unwrap();

    let task = refract::load::<TaskView>().by_key(id(1)).one(&mut db.conn).await.unwrap();
    assert_eq!(task.children.len(), 3);

    db.drop().await;
}

#[test]
fn plan_has_one_query_per_relationship() {
    let plan = refract::plan::<TaskView>().unwrap();
    // task, assignee, children, children.notes, children.notes.tag
    assert_eq!(plan.query_count(), 5);

    let explain = plan.explain();
    assert!(explain.contains("children.notes.tag: TagView (to-one by $ref.tag)"), "{explain}");

    let root = refract::query::select(&plan, &refract::query::RootOptions { by_keys: true, ..Default::default() });
    assert_eq!(
        root,
        "SELECT t0.\"id\" AS \"id\", t0.\"name\" AS \"name\", \
         t0.\"description\" AS \"description\", t0.\"created_at\" AS \"created_at\", \
         t0.\"addr_street\" AS \"address.street\", t0.\"addr_city\" AS \"address.city\", \
         t0.\"addr_geo_lat\" AS \"address.geo.lat\", t0.\"addr_geo_lon\" AS \"address.geo.lon\", \
         t0.\"assignee_id\" AS \"$ref.assignee\" FROM \"task\" AS t0 WHERE t0.\"id\" = ANY($1)"
    );
}
