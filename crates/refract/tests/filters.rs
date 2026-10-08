//! Filters on the root query, and counting.

mod common;

use common::TestDb;
use common::fixture::*;
use refract::filter::{Condition, col};
use refract::{Error, Refract};

async fn names_of_tasks(db: &mut TestDb, condition: Condition) -> Vec<String> {
    refract::load::<SubtaskView>()
        .filter(condition)
        .order_by("name")
        .all(&mut db.conn)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect()
}

#[tokio::test]
async fn comparisons_and_groups() {
    let Some(mut db) = setup("filter_ops").await else { return };

    // All tasks: Release, Hiring (roots); Write docs (2), Fix bugs (0), Tag build (1), Interview (0)
    assert_eq!(names_of_tasks(&mut db, col("parent_id").is_null()).await, ["Hiring", "Release"]);
    assert_eq!(names_of_tasks(&mut db, col("position").ge(1_i32)).await, ["Tag build", "Write docs"]);
    assert_eq!(names_of_tasks(&mut db, col("position").gt(0_i64).and(col("position").lt(2_i16))).await, ["Tag build"]);
    assert_eq!(
        names_of_tasks(&mut db, col("name").eq("Hiring").or(col("assignee_id").eq(1_i64))).await,
        ["Hiring", "Release"]
    );
    assert_eq!(
        names_of_tasks(&mut db, !col("parent_id").is_null() & col("name").ne("Interview")).await,
        ["Fix bugs", "Tag build", "Write docs"]
    );
    assert_eq!(names_of_tasks(&mut db, col("name").ilike("%BUG%")).await, ["Fix bugs"]);
    assert_eq!(names_of_tasks(&mut db, col("name").like("%bug%")).await, ["Fix bugs"]);
    assert_eq!(names_of_tasks(&mut db, col("name").like("%BUG%")).await, Vec::<String>::new());

    let since: chrono::DateTime<chrono::Utc> = "2026-01-02T03:04:05Z".parse().unwrap();
    assert_eq!(names_of_tasks(&mut db, col("created_at").le(since)).await, ["Release"]);

    db.drop().await;
}

#[tokio::test]
async fn lists_of_values() {
    let Some(mut db) = setup("filter_lists").await else { return };

    assert_eq!(names_of_tasks(&mut db, col("id").is_in([id(0x11), id(0x12)])).await, ["Fix bugs", "Write docs"]);
    assert_eq!(
        names_of_tasks(&mut db, col("parent_id").is_not_null().and(col("id").not_in([id(0x11), id(0x12)]))).await,
        ["Interview", "Tag build"]
    );
    assert_eq!(names_of_tasks(&mut db, col("name").is_in(["Hiring", "Nope"])).await, ["Hiring"]);

    // Empty lists match nothing, or everything for not_in
    assert!(names_of_tasks(&mut db, col("id").is_in(Vec::<uuid::Uuid>::new())).await.is_empty());
    assert_eq!(names_of_tasks(&mut db, col("id").not_in(Vec::<uuid::Uuid>::new())).await.len(), 6);

    db.drop().await;
}

#[tokio::test]
async fn filters_combine_with_keys_paging_and_count() {
    let Some(mut db) = setup("filter_paging").await else { return };

    let load = || {
        refract::load::<SubtaskView>()
            .by_keys([id(0x11), id(0x12), id(0x13), id(2)])
            .filter(col("parent_id").is_not_null())
            .filter(col("position").le(2_i32))
    };
    let page: Vec<String> =
        load().order_by("position").limit(2).all(&mut db.conn).await.unwrap().into_iter().map(|t| t.name).collect();
    assert_eq!(page, ["Fix bugs", "Tag build"]);

    // Count ignores ordering and paging
    assert_eq!(load().order_by("position").limit(2).count(&mut db.conn).await.unwrap(), 3);
    assert_eq!(refract::load::<TaskView>().count(&mut db.conn).await.unwrap(), 6);
    assert_eq!(refract::load::<TaskView>().filter(col("parent_id").is_null()).count(&mut db.conn).await.unwrap(), 2);
    assert_eq!(refract::load::<TaskView>().by_keys(Vec::<uuid::Uuid>::new()).count(&mut db.conn).await.unwrap(), 0);

    // Children of filtered roots are loaded as usual
    let roots = refract::load::<TaskView>().filter(col("name").eq("Release")).all(&mut db.conn).await.unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].children.len(), 3);

    db.drop().await;
}

#[tokio::test]
async fn filters_on_overridden_root_queries() {
    let Some(mut db) = setup("filter_overrides").await else { return };
    let without_keys = r#"
        [query."$root"]
        sql = '''
        SELECT t.id AS "$key", t.name AS "name", t.position AS "position" FROM task t WHERE t.parent_id IS NOT NULL
        '''
    "#;
    let with_keys = r#"
        [query."$root"]
        sql = '''
        SELECT t.id AS "$key", t.name AS "name", t.position AS "position" FROM task t WHERE t.id = ANY($1)
        '''
        shadow = true
    "#;

    for (toml, keys) in [(without_keys, false), (with_keys, true)] {
        let refract = Refract::builder()
            .register::<SubtaskView>()
            .overrides("SubtaskView", toml)
            .build(&mut db.conn)
            .await
            .unwrap();
        let load = || {
            let load = refract.load::<SubtaskView>().filter(col("position").ge(1_i32)).filter(col("name").ilike("%T%"));
            if keys { load.by_keys([id(0x11), id(0x13), id(1)]) } else { load }
        };
        let names: Vec<String> =
            load().order_by("name").all(&mut db.conn).await.unwrap().into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["Tag build", "Write docs"], "keys: {keys}");
        assert_eq!(load().count(&mut db.conn).await.unwrap(), 2, "keys: {keys}");

        // Overrides only expose the columns the view selects
        let err = refract
            .load::<SubtaskView>()
            .by_keys([id(0x11)])
            .filter(col("parent_id").is_null())
            .count(&mut db.conn)
            .await
            .unwrap_err();
        assert!(matches!(&err, Error::ColumnNotSelected { column, .. } if column == "parent_id"), "{err}");
    }

    // The shadowed query compares with the generated query and the same filter values
    let refract = Refract::builder()
        .register::<SubtaskView>()
        .overrides("SubtaskView", with_keys)
        .build(&mut db.conn)
        .await
        .unwrap();
    refract
        .load::<SubtaskView>()
        .by_keys([id(0x11), id(0x12)])
        .filter(col("name").ne("Nope"))
        .all(&mut db.conn)
        .await
        .unwrap();
    let stats = refract.shadow_stats();
    assert_eq!((stats[0].runs, stats[0].mismatches), (1, 0));

    db.drop().await;
}
