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
        let manifest = Mabat::<sqlx::Postgres>::builder().register::<TaskView>().manifest().unwrap();
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
fn scaffolds_the_queries_being_tuned() {
    let workspace = Workspace::new();
    let manifest = workspace.path("views.json");
    let scaffold = |extra: &[&str]| {
        let mut args = vec!["scaffold", "--manifest", &manifest, "--view", "TaskView", "--format", "sql"];
        args.extend_from_slice(extra);
        mabat(&args)
    };

    // Only the queries named
    let out = scaffold(&["--query", "children.notes", "--query", "assignee"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let queries: Vec<&str> = out.stdout.lines().filter(|l| l.starts_with("-- mabat: query")).collect();
    assert_eq!(queries, ["-- mabat: query assignee", "-- mabat: query children.notes"]);

    // A query the view does not have
    let out = scaffold(&["--query", "children.note"]);
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.starts_with("error: TaskView has no query children.note; its queries are $root, assignee,"),
        "{}",
        out.stderr
    );

    // Into the overrides directory: a new file, then a query added to it
    let dir = Path::new(&workspace.path("overrides")).join("tuned");
    let dir_arg = dir.to_str().unwrap();
    let out = scaffold(&["--query", "children.notes", "--out", dir_arg]);
    assert_eq!((out.code, out.stdout.as_str()), (0, ""), "{}", out.stderr);
    let file = dir.join("TaskView.sql");
    assert_eq!(out.stderr, format!("wrote {}\n", file.display()));
    let out = scaffold(&["--query", "assignee", "--out", dir_arg]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(content.starts_with("-- Overrides for TaskView.\n"), "{content}");
    assert_eq!(content.matches("-- Overrides for TaskView.").count(), 1, "{content}");
    let queries: Vec<&str> = content.lines().filter(|l| l.starts_with("-- mabat: query")).collect();
    assert_eq!(queries, ["-- mabat: query children.notes", "-- mabat: query assignee"]);

    // An edited query is never replaced, and a view has one file
    let edited =
        content.replace("ORDER BY t0.\"id\";\n\n-- mabat: query assignee", "ORDER BY 1;\n\n-- mabat: query assignee");
    std::fs::write(&file, &edited).unwrap();
    let out = scaffold(&["--query", "assignee", "--query", "$root", "--out", dir_arg]);
    assert_eq!(out.code, 2);
    assert_eq!(out.stderr, format!("error: {} already overrides assignee: edit it there\n", file.display()));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), edited);
    let out = mabat(&["scaffold", "--manifest", &manifest, "--view", "TaskView", "--out", dir_arg]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("holds the overrides of TaskView, and a view has one override file"), "{}", out.stderr);

    // The file is an override file: explain shows what it overrides
    let out = mabat(&["explain", "--manifest", &manifest, "--overrides", dir_arg, "--view", "TaskView"]);
    assert_eq!(out.code, 0, "{}{}", out.stdout, out.stderr);
    assert!(out.stdout.contains("ORDER BY 1"), "{}", out.stdout);
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

    let manifest = Mabat::<sqlx::Postgres>::builder()
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

#[test]
fn checks_a_sqlite_manifest_in_memory() {
    use mabat_e2e::{Dataset, chinook_sqlite};

    // No database is needed: the schema is created in a new in-memory database
    let workspace = Workspace::new();
    let manifest = Mabat::<sqlx::Sqlite>::builder()
        .register::<chinook_sqlite::InvoiceView>()
        .register::<chinook_sqlite::EmployeeTree>()
        .manifest()
        .unwrap();
    assert!(manifest.write(workspace.0.join("chinook.json")).unwrap());
    std::fs::write(workspace.0.join("chinook.sql"), Dataset::Chinook.sqlite_sql()).unwrap();
    let check = |workspace: &Workspace| {
        mabat(&[
            "check",
            "--manifest",
            &workspace.path("chinook.json"),
            "--overrides",
            &workspace.path("overrides"),
            "--schema",
            &workspace.path("chinook.sql"),
        ])
    };

    let output = check(&workspace);
    assert_eq!(output.code, 0, "{}{}", output.stdout, output.stderr);

    workspace.write_override("InvoiceView.sql", "-- mabat: query lines\nSELECT 1 AS \"$parent\" FROM invoice_line\n");
    let output = check(&workspace);
    assert_eq!(output.code, 1, "{}{}", output.stdout, output.stderr);
    assert!(output.stdout.contains("InvoiceView.lines"), "{}", output.stdout);

    // A URL is needed without a schema
    let output = mabat(&["check", "--manifest", &workspace.path("chinook.json")]);
    assert_eq!(output.code, 2, "{}", output.stderr);
    assert!(output.stderr.contains("--schema"), "{}", output.stderr);
}

#[test]
fn checks_a_mysql_manifest_against_a_schema_file() {
    use mabat_e2e::{Dataset, chinook};

    let Ok(url) = std::env::var("MABAT_TEST_MYSQL_URL") else {
        eprintln!("skipping: MABAT_TEST_MYSQL_URL is not set (see scripts/with-mysql.sh)");
        return;
    };
    let workspace = Workspace::new();
    let manifest = Mabat::<sqlx::MySql>::builder()
        .register::<chinook::InvoiceView>()
        .register::<chinook::EmployeeTree>()
        .manifest()
        .unwrap();
    assert!(manifest.write(workspace.0.join("chinook.json")).unwrap());
    std::fs::write(workspace.0.join("chinook.sql"), Dataset::Chinook.mysql_sql()).unwrap();
    let check = |workspace: &Workspace| {
        mabat(&[
            "check",
            "--manifest",
            &workspace.path("chinook.json"),
            "--overrides",
            &workspace.path("overrides"),
            "--schema",
            &workspace.path("chinook.sql"),
            "--database-url",
            &url,
        ])
    };

    let output = check(&workspace);
    assert_eq!(output.code, 0, "{}{}", output.stdout, output.stderr);

    workspace.write_override("InvoiceView.sql", "-- mabat: query lines\nSELECT 1 AS \"$parent\" FROM invoice_line\n");
    let output = check(&workspace);
    assert_eq!(output.code, 1, "{}{}", output.stdout, output.stderr);
    assert!(output.stdout.contains("InvoiceView.lines"), "{}", output.stdout);
}

#[tokio::test]
async fn writes_and_checks_a_schema_snapshot() {
    let workspace = Workspace::new();
    let url = format!("sqlite://{}?mode=rwc", workspace.path("app.db"));
    let mut conn = sqlx::SqliteConnection::connect(&url).await.unwrap();
    sqlx::raw_sql("CREATE TABLE team (id INTEGER PRIMARY KEY, name VARCHAR(100) NOT NULL)")
        .execute(&mut conn)
        .await
        .unwrap();
    let snapshot = workspace.path("mabat/schema.json");

    // Written to a file, which an unchanged database matches
    let out = mabat(&["schema", "--database-url", &url, "--out", &snapshot]);
    assert_eq!((out.code, out.stderr.as_str()), (0, format!("wrote {snapshot}\n").as_str()));
    let json = std::fs::read_to_string(&snapshot).unwrap();
    assert!(json.contains("\"backend\": \"SQLite\"") && json.contains("\"declared\": \"VARCHAR(100)\""), "{json}");
    let printed = mabat(&["schema", "--database-url", &url]);
    assert_eq!(printed.stdout, json, "without --out, the snapshot is printed");
    let out = mabat(&["schema", "--check", &snapshot, "--database-url", &url]);
    assert_eq!((out.code, out.stdout), (0, format!("the database matches {snapshot}\n")));

    // A change of schema is a difference, and a failure
    sqlx::raw_sql("ALTER TABLE team ADD COLUMN motto TEXT").execute(&mut conn).await.unwrap();
    let out = mabat(&["schema", "--check", &snapshot, "--database-url", &url]);
    assert_eq!(out.code, 1);
    assert_eq!(
        out.stdout,
        format!(
            "the database differs from {snapshot}:\n  column `team.motto` is in the database but not in the \
             snapshot\nwrite a new snapshot with `mabat schema --out {snapshot}`\n"
        )
    );

    // A URL is required, and must name a database this build supports
    let out = mabat(&["schema"]);
    assert_eq!((out.code, out.stderr.as_str()), (2, "error: --database-url or $DATABASE_URL is required\n"));
    let out = mabat(&["schema", "--database-url", "oracle://db"]);
    assert_eq!(out.code, 2);
    assert!(out.stderr.contains("unknown database `oracle:`"), "{}", out.stderr);
}
