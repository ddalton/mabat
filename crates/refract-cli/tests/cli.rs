//! The `refract` command line tool, run as a process.

#[path = "../../refract/tests/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::fixture::{SCHEMA, TaskView};
use refract::Refract;
use sqlx::{Connection, PgConnection};

/// A temporary directory with the manifest of `TaskView`, the schema and an override
/// directory, removed when dropped.
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Workspace {
        let dir = std::env::temp_dir().join(format!("refract-cli-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("overrides")).unwrap();
        let manifest = Refract::builder().register::<TaskView>().manifest().unwrap();
        assert!(manifest.write(dir.join("views.json")).unwrap());
        std::fs::write(dir.join("schema.sql"), SCHEMA).unwrap();
        Workspace(dir)
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).display().to_string()
    }

    fn write_override(&self, name: &str, content: &str) {
        std::fs::write(self.0.join("overrides").join(name), content).unwrap();
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

fn refract(args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_refract")).args(args).env_remove("DATABASE_URL").output().unwrap();
    Output {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn database_url() -> Option<String> {
    let url = std::env::var("REFRACT_TEST_DATABASE_URL").ok();
    if url.is_none() {
        eprintln!("skipping: REFRACT_TEST_DATABASE_URL is not set (see scripts/with-postgres.sh)");
    }
    url
}

fn check(workspace: &Workspace, url: &str) -> Output {
    refract(&[
        "check",
        "--manifest",
        &workspace.path("views.json"),
        "--overrides",
        &workspace.path("overrides"),
        "--schema",
        &workspace.path("schema.sql"),
        "--database-url",
        url,
    ])
}

#[test]
fn usage_and_argument_errors() {
    let out = refract(&[]);
    assert_eq!(out.code, 2);
    assert!(out.stdout.contains("Usage:"), "{}", out.stdout);

    assert_eq!(refract(&["help"]).code, 0);

    let out = refract(&["check"]);
    assert_eq!((out.code, out.stderr.as_str()), (2, "error: --manifest is required\n"));

    let out = refract(&["check", "--manifest", "/nonexistent/views.json"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.starts_with("error: cannot read /nonexistent/views.json"), "{}", out.stderr);

    let workspace = Workspace::new();
    let out = refract(&["frobnicate", "--manifest", &workspace.path("views.json")]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("unknown command frobnicate"), "{}", out.stderr);

    let out = refract(&["check", "--manifest", &workspace.path("views.json")]);
    assert_eq!((out.code, out.stderr.as_str()), (2, "error: --database-url or $DATABASE_URL is required\n"));
}

#[test]
fn explain_needs_no_database() {
    let workspace = Workspace::new();
    workspace.write_override("TaskView.sql", "-- refract: query children\nSELECT 1\n");

    let out =
        refract(&["explain", "--manifest", &workspace.path("views.json"), "--overrides", &workspace.path("overrides")]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.starts_with("TaskView\n  $root: TaskView\n    generated:\n      SELECT t0.\"id\" AS \"id\","),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("    children: SubtaskView (to-many)\n      override ("), "{}", out.stdout);
    assert!(out.stdout.contains("TaskView.sql:1):\n        SELECT 1\n"), "{}", out.stdout);
    assert!(out.stdout.contains("      children.notes.tag: TagView (to-one by $ref.tag)\n"), "{}", out.stdout);

    let out = refract(&["explain", "--manifest", &workspace.path("views.json"), "--view", "Nope"]);
    assert_eq!((out.code, out.stderr.as_str()), (2, "error: the manifest has no view Nope\n"));
}

#[test]
fn scaffolds_in_both_formats() {
    let workspace = Workspace::new();
    let toml = refract(&["scaffold", "--manifest", &workspace.path("views.json"), "--view", "TaskView"]);
    assert_eq!(toml.code, 0);
    assert!(toml.stdout.starts_with("# Overrides for TaskView.\n"), "{}", toml.stdout);
    assert!(
        toml.stdout.contains("\n[query.\"children.notes\"]\nsql = '''\nSELECT t0.\"id\" AS \"$key\",\n"),
        "{}",
        toml.stdout
    );

    let sql =
        refract(&["scaffold", "--manifest", &workspace.path("views.json"), "--view", "TaskView", "--format", "sql"]);
    assert_eq!(sql.code, 0);
    assert!(
        sql.stdout.contains("\n-- refract: query children.notes\nSELECT t0.\"id\" AS \"$key\",\n"),
        "{}",
        sql.stdout
    );
    assert!(sql.stdout.contains("ORDER BY t0.\"id\";\n"), "{}", sql.stdout);
}

#[test]
fn checks_against_a_schema_file() {
    let Some(url) = database_url() else { return };
    let workspace = Workspace::new();

    // The generated queries alone
    let out = check(&workspace, &url);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert_eq!(out.stdout, "0 error(s), 0 warning(s)\n");

    // Scaffolded files pass, in both formats
    for (format, file) in [("toml", "TaskView.toml"), ("sql", "TaskView.sql")] {
        let scaffold = refract(&[
            "scaffold",
            "--manifest",
            &workspace.path("views.json"),
            "--view",
            "TaskView",
            "--format",
            format,
        ]);
        workspace.write_override(file, &scaffold.stdout);
        let out = check(&workspace, &url);
        assert_eq!(out.code, 0, "{format}: {}{}", out.stdout, out.stderr);
        std::fs::remove_file(Path::new(&workspace.path("overrides")).join(file)).unwrap();
    }

    // A broken override is reported like at startup
    workspace.write_override(
        "TaskView.sql",
        "-- refract: query children.notes\n\
         SELECT n.id AS \"$key\", n.task_id AS \"$parent\", n.body AS \"bdy\", n.tag_code AS \"$ref.tag\"\n\
         FROM task_note n WHERE n.task_id = ANY($1)\n",
    );
    let out = check(&workspace, &url);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stdout.starts_with("error[R0102]: override for TaskView.children.notes does not match the view\n"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("TaskView.sql:1\n"), "{}", out.stdout);
    assert!(
        out.stdout.contains("column 3 \"bdy\" is not a path of NoteView in this query (did you mean \"body\"?)"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.ends_with("1 error(s), 0 warning(s)\n"), "{}", out.stdout);
}

#[tokio::test]
async fn schema_checks_leave_nothing_behind() {
    let Some(url) = database_url() else { return };
    let workspace = Workspace::new();
    assert_eq!(check(&workspace, &url).code, 0);

    let mut conn = PgConnection::connect(&url).await.unwrap();
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_namespace WHERE nspname LIKE 'refract_check_%'")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(left, 0);

    // A schema file that does not run is a usage problem, not a check result
    std::fs::write(workspace.0.join("schema.sql"), "CREATE TABLE broken (").unwrap();
    let out = check(&workspace, &url);
    assert_eq!(out.code, 2);
    assert!(out.stderr.starts_with("error: the schema file failed:"), "{}", out.stderr);
}
