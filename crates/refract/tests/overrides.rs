//! Overrides: replacing queries of a view with SQL from configuration, checked against the
//! view and the database.

mod common;

use common::TestDb;
use common::fixture::*;
use refract::query::{Link, QueryPlan};
use refract::{Error, OnInvalid, Refract, Report, Severity};

/// A query of a plan as editable parts, rendered the way a DBA would write it.
#[derive(Clone)]
struct Parts {
    /// `(expression, alias)`
    columns: Vec<(String, String)>,
    from: String,
}

impl Parts {
    fn of(plan: &QueryPlan) -> Parts {
        let columns = plan.columns.iter().map(|c| (format!("t0.\"{}\"", c.column), c.alias.clone())).collect();
        let filter = match &plan.link {
            Link::Root => String::new(),
            Link::Child { fk } => format!(" WHERE t0.\"{fk}\" = ANY($1)"),
            Link::ToOne { .. } | Link::Variant { .. } => format!(" WHERE t0.\"{}\" = ANY($1)", plan.shape.key_column),
        };
        Parts { columns, from: format!("FROM \"{}\" AS t0{filter}", plan.shape.table) }
    }

    fn sql(&self) -> String {
        let columns: Vec<String> = self.columns.iter().map(|(expr, alias)| format!("{expr} AS \"{alias}\"")).collect();
        format!("SELECT {}\n{}", columns.join(",\n       "), self.from)
    }
}

fn toml_for(query: &str, sql: &str) -> String {
    format!("[query.\"{query}\"]\nsql = '''\n{sql}\n'''\n")
}

fn queries(plan: &QueryPlan) -> Vec<&QueryPlan> {
    let mut queries = Vec::new();
    plan.walk(&mut |p| queries.push(p));
    queries
}

async fn check(db: &mut TestDb, toml: &str) -> Report {
    Refract::builder().register::<TaskView>().overrides("TaskView", toml).check(&mut db.conn).await.unwrap()
}

/// Paths of `TaskView` and the views it contains whose fields are `Option`s.
const OPTIONAL: [&str; 4] = ["description", "address.geo.lat", "address.geo.lon", "email"];

/// Broken variants of a query, each labelled.
fn mutations(plan: &QueryPlan, parts: &Parts) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let mut add = |label: String, parts: Parts| result.push((label, parts.sql()));

    for (i, (expr, alias)) in parts.columns.iter().enumerate() {
        if !OPTIONAL.contains(&alias.as_str()) {
            let mut dropped = parts.clone();
            dropped.columns.remove(i);
            add(format!("drop {alias}"), dropped);
        }

        let mut typo = parts.clone();
        typo.columns[i].1 = format!("{alias}x");
        add(format!("misspell {alias}"), typo);

        // A ROW value matches no field type and cannot hold a key
        let mut retyped = parts.clone();
        retyped.columns[i].0 = format!("ROW({expr})");
        add(format!("retype {alias}"), retyped);
    }

    let mut duplicated = parts.clone();
    duplicated.columns.push(parts.columns[0].clone());
    add("duplicate the first column".into(), duplicated);

    let mut missing_table = parts.clone();
    missing_table.from = missing_table.from.replacen(&format!("\"{}\"", plan.shape.table), "\"missing_table\"", 1);
    add("select from a missing table".into(), missing_table);

    match &plan.link {
        Link::Root => {
            let mut two_params = parts.clone();
            two_params.from.push_str(" WHERE t0.\"id\" = ANY($1) AND t0.\"name\" = $2");
            add("take two parameters".into(), two_params);

            let mut scalar = parts.clone();
            scalar.from.push_str(" WHERE t0.\"id\" = $1");
            add("take a single key".into(), scalar);
        }
        Link::Child { .. } | Link::ToOne { .. } | Link::Variant { .. } => {
            let mut unfiltered = parts.clone();
            unfiltered.from = unfiltered.from.replace(" = ANY($1)", " IS NOT NULL");
            add("ignore the keys".into(), unfiltered);

            let mut scalar = parts.clone();
            scalar.from = scalar.from.replace(" = ANY($1)", " = $1");
            add("take a single key".into(), scalar);
        }
    }

    if let Some(i) = parts.columns.iter().position(|(_, alias)| alias == "$parent") {
        let mut text_parent = parts.clone();
        text_parent.columns[i].0 = format!("({})::text", parts.columns[i].0);
        add("select the parent key as text".into(), text_parent);
    }

    result
}

#[tokio::test]
async fn broken_overrides_fail_the_checks() {
    let Some(mut db) = setup("mutations").await else { return };
    let plan = refract::plan::<TaskView>().unwrap();

    let mut checked = 0;
    for query in queries(&plan) {
        let name = query.query_name();
        let parts = Parts::of(query);

        let report = check(&mut db, &toml_for(name, &parts.sql())).await;
        assert!(report.diagnostics().is_empty(), "the unchanged {name} query has problems:\n{report}");

        for (label, sql) in mutations(query, &parts) {
            let report = check(&mut db, &toml_for(name, &sql)).await;
            assert!(!report.is_ok(), "{name}: \"{label}\" passed the checks:\n{sql}");
            assert!(
                report.errors().any(|d| d.origin.is_some() || d.query != name),
                "{name}: \"{label}\" is not reported for the override:\n{report}"
            );
            checked += 1;
        }
    }
    assert!(checked >= 60, "only {checked} mutations");

    db.drop().await;
}

#[tokio::test]
async fn optional_paths_can_be_left_out() {
    let Some(mut db) = setup("optional").await else { return };
    let root = &refract::plan::<TaskView>().unwrap();
    let mut parts = Parts::of(root);
    parts.columns.retain(|(_, alias)| alias != "description" && alias != "address.geo.lat");
    let toml = toml_for("$root", &parts.sql());

    let report = check(&mut db, &toml).await;
    assert!(report.is_ok(), "{report}");
    let warnings: Vec<_> = report.warnings().collect();
    assert_eq!(warnings.len(), 1, "{report}");
    assert_eq!(warnings[0].code, "R0105");
    assert_eq!(warnings[0].notes, ["path \"description\" is always None", "path \"address.geo.lat\" is always None"]);

    let refract =
        Refract::builder().register::<TaskView>().overrides("TaskView", toml).build(&mut db.conn).await.unwrap();
    let task = refract.load::<TaskView>().by_key(id(1)).one(&mut db.conn).await.unwrap();
    assert_eq!(task.description, None);
    assert_eq!(task.address.geo, Geo { lat: None, lon: Some(-2.5) });
    assert_eq!(task.name, "Release");

    db.drop().await;
}

/// Overrides written differently from the generated queries: other join orders, CTEs,
/// column order and table aliases.
const TUNED: &str = r#"
[query."$root"]
sql = '''
WITH roots AS (SELECT * FROM task WHERE parent_id IS NULL)
SELECT r.assignee_id  AS "$ref.assignee",
       r.name         AS "name",
       r.id           AS "id",
       r.description  AS "description",
       r.created_at   AS "created_at",
       r.addr_street  AS "address.street",
       r.addr_city    AS "address.city",
       r.addr_geo_lat AS "address.geo.lat",
       r.addr_geo_lon AS "address.geo.lon"
FROM roots r
'''

[query.children]
sql = '''
SELECT s.parent_id AS "$parent", s.id AS "$key", s.name AS "name", s.position AS "position"
FROM task p
JOIN task s ON s.parent_id = p.id
WHERE p.id = ANY($1)
ORDER BY s.position, s.name, s.id
'''

[query."children.notes"]
sql = '''
SELECT n.task_id AS "$parent", n.id AS "$key", n.body AS "body", t.code AS "$ref.tag"
FROM task_note n
LEFT JOIN tag t ON t.code = n.tag_code
WHERE n.task_id = ANY($1)
ORDER BY n.id
'''

[query."children.notes.tag"]
sql = "SELECT label AS \"label\", code AS \"code\" FROM tag WHERE code = ANY($1)"
"#;

#[tokio::test]
async fn tuned_overrides_load_the_same_aggregates() {
    let Some(mut db) = setup("tuned").await else { return };
    let refract =
        Refract::builder().register::<TaskView>().overrides("TaskView", TUNED).build(&mut db.conn).await.unwrap();
    assert!(refract.report().diagnostics().is_empty(), "{}", refract.report());

    let explain = refract.explain::<TaskView>().unwrap();
    assert!(
        explain.contains("children: SubtaskView (to-many by parent_id)\n    override (TaskView.toml (inline):"),
        "{explain}"
    );
    assert!(explain.contains("WITH roots AS"), "{explain}");
    assert!(explain.contains("assignee: PersonView (to-one by $ref.assignee)\n    SELECT t0."), "{explain}");

    // By key, by keys with ordering, and all roots (the override only selects roots)
    let generated = refract::load::<TaskView>().by_key(id(1)).one(&mut db.conn).await.unwrap();
    let tuned = refract.load::<TaskView>().by_key(id(1)).one(&mut db.conn).await.unwrap();
    assert_eq!(tuned, generated);

    let generated =
        refract::load::<TaskView>().by_keys([id(1), id(2)]).order_by("name").all(&mut db.conn).await.unwrap();
    let tuned = refract.load::<TaskView>().by_keys([id(1), id(2)]).order_by("name").all(&mut db.conn).await.unwrap();
    assert_eq!(tuned, generated);

    let tuned = refract.load::<TaskView>().order_by_desc("name").limit(1).all(&mut db.conn).await.unwrap();
    assert_eq!(tuned.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Release"]);

    let err = refract.load::<TaskView>().order_by("parent_id").all(&mut db.conn).await.unwrap_err();
    assert!(matches!(&err, Error::ColumnNotSelected { column, .. } if column == "parent_id"), "{err}");

    db.drop().await;
}

#[tokio::test]
async fn root_override_can_take_the_keys() {
    let Some(mut db) = setup("root_keys").await else { return };
    let toml = r#"
        [query."$root"]
        sql = '''
        SELECT t0.id AS "name_is_not_the_key_alias"
        FROM task t0 WHERE t0.id = ANY($1)
        '''
    "#;
    let report = check(&mut db, toml).await;
    let notes: Vec<&str> = report.errors().flat_map(|d| d.notes.iter().map(String::as_str)).collect();
    assert!(
        notes.contains(&"column 1 \"name_is_not_the_key_alias\" is not a path of TaskView in this query"),
        "{report}"
    );
    assert!(notes.contains(&"path \"id\" is not selected"), "{report}");

    let root = &refract::plan::<TaskView>().unwrap();
    let mut parts = Parts::of(root);
    parts.from.push_str(" WHERE t0.\"id\" = ANY($1)");
    let refract = Refract::builder()
        .register::<TaskView>()
        .overrides("TaskView", toml_for("$root", &parts.sql()))
        .build(&mut db.conn)
        .await
        .unwrap();

    let tasks = refract.load::<TaskView>().by_keys([id(2), id(1)]).order_by("name").all(&mut db.conn).await.unwrap();
    assert_eq!(tasks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Hiring", "Release"]);

    let err = refract.load::<TaskView>().all(&mut db.conn).await.unwrap_err();
    assert!(matches!(err, Error::KeysRequired { view: "TaskView", .. }), "{err}");

    db.drop().await;
}

#[tokio::test]
async fn reports_read_like_compiler_errors() {
    let Some(mut db) = setup("report").await else { return };
    let toml = r#"# TaskView overrides

[query."$root"]
sql = '''
SELECT t.id AS "id", t.nmae AS "name" FROM task t
'''

[query."children.notes"]
sql = '''
SELECT n.id AS "$key", n.task_id AS "$parent", length(n.body) AS "body", n.tag_code AS "$ref.tga"
FROM task_note n WHERE n.task_id = ANY($1)
'''

[query.childern]
sql = "SELECT 1"
"#;
    let err = Refract::builder().register::<TaskView>().overrides("TaskView", toml).build(&mut db.conn).await;
    let Err(Error::Invalid(report)) = err else { panic!("expected the build to fail") };
    let text = report.to_string();

    assert!(text.contains(
        "error[R0101]: \"childern\" is not a query of TaskView\n  --> TaskView.toml (inline):14\n   | did you mean \"children\"?\n"
    ), "{text}");
    assert!(text.contains("error[R0103]: override for TaskView.$root does not prepare\n  --> TaskView.toml (inline):3\n   | column t.nmae does not exist\n"), "{text}");
    assert!(text.contains(
        "error[R0102]: override for TaskView.children.notes does not match the view\n  --> TaskView.toml (inline):8\n"
    ), "{text}");
    assert!(text.contains("   | column 3 \"body\" has type INT4, expected TEXT for String\n"), "{text}");
    assert!(
        text.contains(
            "   | column 4 \"$ref.tga\" is not a path of NoteView in this query (did you mean \"$ref.tag\"?)\n"
        ),
        "{text}"
    );
    assert!(text.contains("   | column \"$ref.tag\" is not selected; it holds a key\n"), "{text}");
    assert!(text.ends_with("3 error(s), 0 warning(s)"), "{text}");

    db.drop().await;
}

#[tokio::test]
async fn invalid_overrides_can_fall_back_to_the_generated_queries() {
    let Some(mut db) = setup("fallback").await else { return };
    let toml = toml_for("children", "SELECT 1 AS \"name\"");

    let refract = Refract::builder()
        .register::<TaskView>()
        .overrides("TaskView", toml)
        .on_invalid(OnInvalid::UseGenerated)
        .build(&mut db.conn)
        .await
        .unwrap();
    assert!(!refract.report().is_ok());
    assert_eq!(refract.report().errors().next().map(|d| d.query.as_str()), Some("children"));
    assert!(!refract.explain::<TaskView>().unwrap().contains("override"));

    let generated = refract::load::<TaskView>().by_key(id(1)).one(&mut db.conn).await.unwrap();
    let loaded = refract.load::<TaskView>().by_key(id(1)).one(&mut db.conn).await.unwrap();
    assert_eq!(loaded, generated);

    db.drop().await;
}

#[tokio::test]
async fn schema_drift_is_found_in_generated_queries() {
    let Some(mut db) = setup("drift").await else { return };
    db.execute("ALTER TABLE task_note RENAME COLUMN body TO text").await;

    let report = Refract::builder().register::<TaskView>().check(&mut db.conn).await.unwrap();
    let errors: Vec<_> = report.errors().collect();
    assert_eq!(errors.len(), 1, "{report}");
    assert_eq!(errors[0].code, "R0103");
    assert_eq!(errors[0].summary, "generated query for TaskView.children.notes does not prepare");
    assert_eq!(errors[0].origin, None);

    // Falling back does not help when the generated query is broken
    let err = Refract::builder().register::<TaskView>().on_invalid(OnInvalid::UseGenerated).build(&mut db.conn).await;
    assert!(matches!(err, Err(Error::Invalid(_))));

    // An override that follows the schema replaces it, and the problem becomes a warning
    let notes = r#"SELECT id AS "$key", task_id AS "$parent", text AS "body", tag_code AS "$ref.tag"
                   FROM task_note WHERE task_id = ANY($1) ORDER BY id"#;
    let refract = Refract::builder()
        .register::<TaskView>()
        .overrides("TaskView", toml_for("children.notes", notes))
        .build(&mut db.conn)
        .await
        .unwrap();
    assert!(refract.report().is_ok());
    assert_eq!(refract.report().warnings().count(), 1);
    let task = refract.load::<TaskView>().by_key(id(1)).one(&mut db.conn).await.unwrap();
    assert_eq!(task.children[0].notes[0].body, "Crash on start");

    db.drop().await;
}

#[tokio::test]
async fn shadow_mode_compares_with_the_generated_query() {
    let Some(mut db) = setup("shadow").await else { return };
    let children = Parts::of(&refract::plan::<TaskView>().unwrap().children[1].plan);
    let same = format!("{}\nORDER BY t0.\"position\", t0.\"name\", t0.\"id\"", children.sql());
    let reversed = format!("{}\nORDER BY t0.\"position\" DESC", children.sql());

    for (sql, mismatches) in [(same, 0), (reversed, 1)] {
        let toml = format!("{}shadow = true\n", toml_for("children", &sql));
        let refract =
            Refract::builder().register::<TaskView>().overrides("TaskView", toml).build(&mut db.conn).await.unwrap();
        assert!(refract.explain::<TaskView>().unwrap().contains(", shadowed):"));

        let task = refract.load::<TaskView>().by_key(id(1)).one(&mut db.conn).await.unwrap();
        let stats = refract.shadow_stats();
        assert_eq!(stats.len(), 1);
        assert_eq!((stats[0].view, stats[0].query.as_str()), ("TaskView", "children"));
        assert_eq!((stats[0].runs, stats[0].mismatches), (1, mismatches), "{sql}");
        assert!(stats[0].override_time > std::time::Duration::ZERO);

        // The override's rows are used
        let first = if mismatches == 0 { "Fix bugs" } else { "Write docs" };
        assert_eq!(task.children[0].name, first);
    }

    db.drop().await;
}

#[tokio::test]
async fn override_files_are_read_from_a_directory() {
    let Some(mut db) = setup("files").await else { return };
    let dir = std::env::temp_dir().join(format!("refract-overrides-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("TaskView.toml"), refract::scaffold::<TaskView>().unwrap()).unwrap();
    std::fs::write(dir.join("TaskVeiw.toml"), "").unwrap();
    std::fs::write(dir.join("PersonView.toml"), "[query.\"$root\"]\nsql = 42\n").unwrap();
    std::fs::write(dir.join("README.md"), "ignored").unwrap();

    let report = Refract::builder()
        .register::<TaskView>()
        .register::<PersonView>()
        .overrides_dir(&dir)
        .check(&mut db.conn)
        .await
        .unwrap();
    let errors: Vec<(&str, &str)> = report.errors().map(|d| (d.code, d.summary.as_str())).collect();
    assert_eq!(
        errors,
        [("R0100", "override file cannot be used"), ("R0101", "TaskVeiw is not a registered view")],
        "{report}"
    );
    assert!(report.errors().next().unwrap().origin.as_ref().unwrap().ends_with("PersonView.toml:2"), "{report}");

    std::fs::remove_dir_all(&dir).unwrap();
    db.drop().await;
}

#[tokio::test]
async fn scaffold_is_a_valid_override_file() {
    let Some(mut db) = setup("scaffold").await else { return };
    let scaffold = refract::scaffold::<TaskView>().unwrap();
    assert!(
        scaffold.contains("[query.\"children.notes.tag\"]\nsql = '''\nSELECT t0.\"code\" AS \"code\",\n"),
        "{scaffold}"
    );

    let refract =
        Refract::builder().register::<TaskView>().overrides("TaskView", scaffold).build(&mut db.conn).await.unwrap();
    assert!(refract.report().diagnostics().is_empty(), "{}", refract.report());
    let generated =
        refract::load::<TaskView>().by_keys([id(1), id(2)]).order_by("name").all(&mut db.conn).await.unwrap();
    let loaded = refract.load::<TaskView>().by_keys([id(1), id(2)]).order_by("name").all(&mut db.conn).await.unwrap();
    assert_eq!(loaded, generated);

    db.drop().await;
}

#[tokio::test]
async fn checking_in_a_transaction_does_not_abort_it() {
    use sqlx::Connection;

    let Some(mut db) = setup("check_tx").await else { return };
    let mut tx = db.conn.begin().await.unwrap();
    let report = Refract::builder()
        .register::<TaskView>()
        .overrides("TaskView", toml_for("$root", "SELECT nope FROM nowhere"))
        .check(&mut tx)
        .await
        .unwrap();
    assert_eq!(report.errors().next().map(|d| d.code), Some("R0103"));
    // The transaction is still usable
    let task = refract::load::<TaskView>().by_key(id(1)).one(&mut tx).await.unwrap();
    assert_eq!(task.name, "Release");
    tx.rollback().await.unwrap();

    db.drop().await;
}

#[tokio::test]
async fn unregistered_views_cannot_be_loaded() {
    let Some(mut db) = setup("unregistered").await else { return };
    let refract = Refract::builder().register::<TaskView>().build(&mut db.conn).await.unwrap();
    let err = refract.load::<PersonView>().all(&mut db.conn).await.unwrap_err();
    assert!(matches!(err, Error::NotRegistered { view: "PersonView" }), "{err}");
    assert_eq!(refract.report().diagnostics().iter().filter(|d| d.severity == Severity::Error).count(), 0);

    db.drop().await;
}

/// A temporary directory for override files, removed when dropped.
struct OverridesDir(std::path::PathBuf);

impl OverridesDir {
    fn new() -> OverridesDir {
        let dir = std::env::temp_dir().join(format!("refract-overrides-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir(&dir).unwrap();
        OverridesDir(dir)
    }

    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.0.join(name), content).unwrap();
    }

    fn remove(&self, name: &str) {
        std::fs::remove_file(self.0.join(name)).unwrap();
    }
}

impl Drop for OverridesDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The children query of `TaskView`, ordered by the given SQL.
fn children_ordered_by(order: &str) -> String {
    let children = Parts::of(&refract::plan::<TaskView>().unwrap().children[1].plan);
    format!("{}\nORDER BY {order}", children.sql())
}

async fn child_names(refract: &Refract, db: &mut TestDb) -> Vec<String> {
    let task = refract.load::<TaskView>().by_key(id(1)).one(&mut db.conn).await.unwrap();
    task.children.into_iter().map(|c| c.name).collect()
}

#[tokio::test]
async fn reload_puts_valid_changes_in_use() {
    use refract::Reloaded;

    let Some(mut db) = setup("reload").await else { return };
    let dir = OverridesDir::new();
    dir.write("TaskView.toml", &toml_for("children", &children_ordered_by("t0.\"name\"")));
    let refract = Refract::builder().register::<TaskView>().overrides_dir(&dir.0).build(&mut db.conn).await.unwrap();
    assert_eq!(child_names(&refract, &mut db).await, ["Fix bugs", "Tag build", "Write docs"]);

    // Unchanged files are not checked again
    assert_eq!(refract.reload(&mut db.conn).await.unwrap(), Reloaded::Unchanged);

    // A valid change is put in use
    dir.write("TaskView.toml", &toml_for("children", &children_ordered_by("t0.\"name\" DESC")));
    assert!(matches!(refract.reload(&mut db.conn).await.unwrap(), Reloaded::Updated(report) if report.is_ok()));
    assert_eq!(child_names(&refract, &mut db).await, ["Write docs", "Tag build", "Fix bugs"]);

    // An invalid change is rejected, and the overrides in use stay
    dir.write("TaskView.toml", &toml_for("children", "SELECT t0.\"name\" AS \"name\" FROM task t0"));
    let err = refract.reload(&mut db.conn).await.unwrap_err();
    assert!(matches!(&err, Error::Invalid(report) if report.errors().any(|d| d.query == "children")), "{err}");
    assert_eq!(child_names(&refract, &mut db).await, ["Write docs", "Tag build", "Fix bugs"]);
    // and is not checked again until it changes
    assert_eq!(refract.reload(&mut db.conn).await.unwrap(), Reloaded::Unchanged);

    // Removing the file goes back to the generated queries, ordered by position
    dir.remove("TaskView.toml");
    assert!(matches!(refract.reload(&mut db.conn).await.unwrap(), Reloaded::Updated(_)));
    assert_eq!(child_names(&refract, &mut db).await, ["Fix bugs", "Tag build", "Write docs"]);
    assert!(!refract.explain::<TaskView>().unwrap().contains("override"));

    db.drop().await;
}

#[tokio::test]
async fn reload_keeps_the_statistics_of_unchanged_shadowed_overrides() {
    let Some(mut db) = setup("reload_stats").await else { return };
    let dir = OverridesDir::new();
    let shadowed =
        format!("{}shadow = true\n", toml_for("children", &children_ordered_by("t0.\"position\", t0.\"id\"")));
    dir.write("TaskView.toml", &shadowed);
    let refract = Refract::builder().register::<TaskView>().overrides_dir(&dir.0).build(&mut db.conn).await.unwrap();
    child_names(&refract, &mut db).await;
    assert_eq!(refract.shadow_stats()[0].runs, 1);

    // Another query changes: the shadowed override keeps its statistics
    let tag = "\n[query.\"children.notes.tag\"]\nsql = \"SELECT code AS \\\"code\\\", label AS \\\"label\\\" FROM tag WHERE code = ANY($1)\"\n";
    dir.write("TaskView.toml", &format!("{shadowed}{tag}"));
    refract.reload(&mut db.conn).await.unwrap();
    assert_eq!(refract.shadow_stats()[0].runs, 1);
    child_names(&refract, &mut db).await;
    assert_eq!(refract.shadow_stats()[0].runs, 2);

    // Its SQL changes: the statistics start again
    let changed = format!("{}shadow = true\n", toml_for("children", &children_ordered_by("t0.\"position\"")));
    dir.write("TaskView.toml", &changed);
    refract.reload(&mut db.conn).await.unwrap();
    assert_eq!(refract.shadow_stats()[0].runs, 0);

    db.drop().await;
}

const TUNED_SQL: &str = r#"
-- Overrides for TaskView, tuned by the DBA team

-- refract: query children
SELECT s.parent_id AS "$parent", s.id AS "$key", s.name AS "name", s.position AS "position"
FROM task s
WHERE s.parent_id = ANY($1)
ORDER BY s.position, s.id;

-- refract: query children.notes, shadow
SELECT n.task_id AS "$parent", n.id AS "$key", n.body AS "body", n.tag_code AS "$ref.tag"
FROM task_note n
WHERE n.task_id = ANY($1)
ORDER BY n.id;
"#;

#[tokio::test]
async fn sql_override_files() {
    let Some(mut db) = setup("sql_files").await else { return };
    let dir = OverridesDir::new();
    dir.write("TaskView.sql", TUNED_SQL);

    let refract = Refract::builder().register::<TaskView>().overrides_dir(&dir.0).build(&mut db.conn).await.unwrap();
    assert!(refract.report().diagnostics().is_empty(), "{}", refract.report());
    let explain = refract.explain::<TaskView>().unwrap();
    assert!(explain.contains("override (") && explain.contains("TaskView.sql:4):"), "{explain}");
    assert!(explain.contains("TaskView.sql:10, shadowed):"), "{explain}");

    let generated = refract::load::<TaskView>().by_key(id(1)).one(&mut db.conn).await.unwrap();
    let loaded = refract.load::<TaskView>().by_key(id(1)).one(&mut db.conn).await.unwrap();
    assert_eq!(loaded, generated);
    assert_eq!(refract.shadow_stats()[0].mismatches, 0);

    // Errors point to the marker line of the query
    let broken = TUNED_SQL.replace("s.name AS \"name\"", "s.name AS \"nmae\"");
    let report =
        Refract::builder().register::<TaskView>().overrides_sql("TaskView", broken).check(&mut db.conn).await.unwrap();
    let error = report.errors().next().unwrap();
    assert_eq!(error.origin.as_deref(), Some("TaskView.sql (inline):4"), "{report}");

    // One file per view
    dir.write("TaskView.toml", "");
    let report = Refract::builder().register::<TaskView>().overrides_dir(&dir.0).check(&mut db.conn).await.unwrap();
    let error = report.errors().next().unwrap();
    assert_eq!(error.code, "R0100", "{report}");
    assert!(error.notes[0].starts_with("TaskView already has an override file"), "{report}");

    db.drop().await;
}
