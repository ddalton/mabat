//! The `mabat` command line tool, run as a process.

#[path = "../../mabat/tests/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::fixture::{SCHEMA, TaskView};
use mabat::Mabat;
use sqlx::{Connection, PgConnection};

/// A temporary directory with the manifest of `TaskView`, the schema and an override
/// directory, removed when dropped.
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Workspace {
        let dir = std::env::temp_dir().join(format!("mabat-cli-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("overrides")).unwrap();
        let manifest = Mabat::builder().register::<TaskView>().manifest().unwrap();
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

fn mabat(args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_mabat")).args(args).env_remove("DATABASE_URL").output().unwrap();
    Output {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn database_url() -> Option<String> {
    let url = std::env::var("MABAT_TEST_DATABASE_URL").ok();
    if url.is_none() {
        eprintln!("skipping: MABAT_TEST_DATABASE_URL is not set (see scripts/with-postgres.sh)");
    }
    url
}

fn check(workspace: &Workspace, url: &str) -> Output {
    mabat(&[
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
    let out = mabat(&[]);
    assert_eq!(out.code, 2);
    assert!(out.stdout.contains("Usage:"), "{}", out.stdout);

    assert_eq!(mabat(&["help"]).code, 0);

    let out = mabat(&["check"]);
    assert_eq!((out.code, out.stderr.as_str()), (2, "error: --manifest is required\n"));

    let out = mabat(&["check", "--manifest", "/nonexistent/views.json"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.starts_with("error: cannot read /nonexistent/views.json"), "{}", out.stderr);

    let workspace = Workspace::new();
    let out = mabat(&["frobnicate", "--manifest", &workspace.path("views.json")]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("unknown command frobnicate"), "{}", out.stderr);

    let out = mabat(&["check", "--manifest", &workspace.path("views.json")]);
    assert_eq!((out.code, out.stderr.as_str()), (2, "error: --database-url or $DATABASE_URL is required\n"));
}

#[test]
fn explain_needs_no_database() {
    let workspace = Workspace::new();
    workspace.write_override("TaskView.sql", "-- mabat: query children\nSELECT 1\n");

    let out =
        mabat(&["explain", "--manifest", &workspace.path("views.json"), "--overrides", &workspace.path("overrides")]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stdout.starts_with("TaskView\n  $root: TaskView\n    generated:\n      SELECT t0.\"id\" AS \"id\","),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("    children: SubtaskView (to-many)\n      override ("), "{}", out.stdout);
    assert!(out.stdout.contains("TaskView.sql:1):\n        SELECT 1\n"), "{}", out.stdout);
    assert!(out.stdout.contains("      children.notes.tag: TagView (to-one by $ref.tag)\n"), "{}", out.stdout);

    let out = mabat(&["explain", "--manifest", &workspace.path("views.json"), "--view", "Nope"]);
    assert_eq!((out.code, out.stderr.as_str()), (2, "error: the manifest has no view Nope\n"));
}

#[test]
fn scaffolds_in_both_formats() {
    let workspace = Workspace::new();
    let toml = mabat(&["scaffold", "--manifest", &workspace.path("views.json"), "--view", "TaskView"]);
    assert_eq!(toml.code, 0);
    assert!(toml.stdout.starts_with("# Overrides for TaskView.\n"), "{}", toml.stdout);
    assert!(
        toml.stdout.contains("\n[query.\"children.notes\"]\nsql = '''\nSELECT t0.\"id\" AS \"$key\",\n"),
        "{}",
        toml.stdout
    );

    let sql =
        mabat(&["scaffold", "--manifest", &workspace.path("views.json"), "--view", "TaskView", "--format", "sql"]);
    assert_eq!(sql.code, 0);
    assert!(sql.stdout.contains("\n-- mabat: query children.notes\nSELECT t0.\"id\" AS \"$key\",\n"), "{}", sql.stdout);
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
        let scaffold =
            mabat(&["scaffold", "--manifest", &workspace.path("views.json"), "--view", "TaskView", "--format", format]);
        workspace.write_override(file, &scaffold.stdout);
        let out = check(&workspace, &url);
        assert_eq!(out.code, 0, "{format}: {}{}", out.stdout, out.stderr);
        std::fs::remove_file(Path::new(&workspace.path("overrides")).join(file)).unwrap();
    }

    // A broken override is reported like at startup
    workspace.write_override(
        "TaskView.sql",
        "-- mabat: query children.notes\n\
         SELECT n.id AS \"$key\", n.task_id AS \"$parent\", n.body AS \"bdy\", n.tag_code AS \"$ref.tag\"\n\
         FROM task_note n WHERE n.task_id = ANY($1)\n",
    );
    let out = check(&workspace, &url);
    assert_eq!(out.code, 1, "{}{}", out.stdout, out.stderr);
    assert!(
        out.stdout.starts_with("error[M0102]: override for TaskView.children.notes does not match the view\n"),
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
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_namespace WHERE nspname LIKE 'mabat_check_%'")
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

/// `mabat check --schema` with the Pagila and Chinook sample databases: the schema files a
/// DBA would use, with overrides that derive enums from legacy columns and call a stored
/// function.
#[test]
fn checks_sample_databases_from_schema_files() {
    use mabat_e2e::{Dataset, chinook, pagila};

    let Some(url) = database_url() else { return };
    let dir = std::env::temp_dir().join(format!("mabat-cli-samples-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(dir.join("overrides")).unwrap();
    let path = |name: &str| dir.join(name).display().to_string();

    let manifest = Mabat::builder()
        .register::<pagila::FilmView>()
        .register::<pagila::CustomerView>()
        .register::<pagila::RentalStatusView>()
        .register::<pagila::FilmStock>()
        .register::<pagila::Store>()
        .manifest()
        .unwrap();
    manifest.write(dir.join("pagila.json")).unwrap();
    std::fs::write(dir.join("pagila.sql"), Dataset::Pagila.sql()).unwrap();
    std::fs::write(dir.join("overrides/RentalStatusView.sql"), pagila::RENTAL_STATUS_OVERRIDE).unwrap();
    std::fs::write(dir.join("overrides/FilmStock.sql"), pagila::FILM_STOCK_OVERRIDE).unwrap();

    let out = mabat(&[
        "check",
        "--manifest",
        &path("pagila.json"),
        "--overrides",
        &path("overrides"),
        "--schema",
        &path("pagila.sql"),
        "--database-url",
        &url,
    ]);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    // The generated queries of the two views without columns of their own are replaced
    assert!(out.stdout.ends_with("0 error(s), 2 warning(s)\n"), "{}", out.stdout);

    // Without the overrides, those views cannot run
    let out =
        mabat(&["check", "--manifest", &path("pagila.json"), "--schema", &path("pagila.sql"), "--database-url", &url]);
    assert_eq!(out.code, 1, "{}", out.stdout);
    assert!(
        out.stdout.contains("error[M0103]: generated query for RentalStatusView.$root does not prepare"),
        "{}",
        out.stdout
    );

    let manifest = Mabat::builder()
        .register::<chinook::InvoiceView>()
        .register::<chinook::EmployeeTree>()
        .register::<chinook::Employee>()
        .manifest()
        .unwrap();
    manifest.write(dir.join("chinook.json")).unwrap();
    std::fs::write(dir.join("chinook.sql"), Dataset::Chinook.sql()).unwrap();
    let out = mabat(&[
        "check",
        "--manifest",
        &path("chinook.json"),
        "--schema",
        &path("chinook.sql"),
        "--database-url",
        &url,
    ]);
    assert_eq!((out.code, out.stdout.as_str()), (0, "0 error(s), 0 warning(s)\n"), "{}", out.stderr);

    std::fs::remove_dir_all(&dir).unwrap();
}
