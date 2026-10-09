//! `mabat`: check, explain and scaffold Mabat override SQL without the application.
//!
//! The application writes the manifest of its views (`Builder::manifest`); this tool reads
//! it, so a DBA can work on override files with no Rust toolchain.

use std::path::PathBuf;
use std::process::ExitCode;

use mabat_sqlx::manifest::{Manifest, ScaffoldFormat};
use mabat_sqlx::schema::Snapshot;
use sqlx::{AssertSqlSafe, Connection, Executor};

const USAGE: &str = "\
mabat: check, explain and scaffold Mabat override SQL

Usage:
  mabat check    --manifest <file> [--overrides <dir>]... [--database-url <url>] [--schema <file>]
  mabat check    --manifest <file> [--overrides <dir>]... --snapshot <file>
  mabat explain  --manifest <file> [--overrides <dir>]... [--view <name>]
  mabat scaffold --manifest <file> --view <name> [--query <name>]... [--format toml|sql] [--out <dir>]
  mabat schema   [--database-url <url>] [--out <file>]
  mabat schema   --check <file> [--database-url <url>]

check     Prepare every query, generated and overridden, on the database without running it,
          and compare its columns and parameters with the views. The database URL defaults to
          $DATABASE_URL. With --schema, the schema file is created in a temporary schema inside
          a transaction that is rolled back, so any database with no access to the
          application's data will do, and nothing is left behind. On MySQL, which cannot roll
          back DDL, the schema is created in a temporary database that is dropped afterwards.
          On SQLite, --schema without a database URL checks against a new in-memory database.
          With --snapshot, check the views against a snapshot written by `mabat schema` instead,
          with no database: the tables and columns they read, the types and nullability of the
          columns, the keys and the columns that link queries. The override files are checked
          for their names only, as their SQL needs a database to prepare it.
explain   Show the queries of the views and the SQL that runs for each.
scaffold  Print an override file with the generated SQL of a view's queries, to edit and tune:
          the queries named with --query, or every query. Each query in the file replaces the
          generated one, so name only the queries being tuned; `mabat explain` lists them.
          With --out, write it to the view's file in that directory instead, or add the queries
          to the end of the file if it exists; a query the file already overrides is refused.
schema    Write a snapshot of the database's schema as JSON, to commit as mabat/schema.json and check
          views against without a database: its tables and views, their columns with their types,
          nullability and generated values, and their primary and foreign keys. With --check,
          compare the database with a snapshot instead, and list how they differ.

Exit status: 0 if there are no errors, 1 if the checks found errors, 2 for any other problem.
";

/// The command line arguments.
#[derive(Default)]
struct Args {
    command: String,
    manifest: Option<PathBuf>,
    overrides: Vec<PathBuf>,
    database_url: Option<String>,
    schema: Option<PathBuf>,
    view: Option<String>,
    queries: Vec<String>,
    format: Option<String>,
    out: Option<PathBuf>,
    snapshot: Option<PathBuf>,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut args = args.into_iter();
    let mut parsed = Args { command: args.next().ok_or("missing command")?, ..Args::default() };
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--manifest" => parsed.manifest = Some(value()?.into()),
            "--overrides" => parsed.overrides.push(value()?.into()),
            "--database-url" => parsed.database_url = Some(value()?),
            "--schema" => parsed.schema = Some(value()?.into()),
            "--view" => parsed.view = Some(value()?),
            "--query" => parsed.queries.push(value()?),
            "--format" => parsed.format = Some(value()?),
            "--out" => parsed.out = Some(value()?.into()),
            "--check" | "--snapshot" => parsed.snapshot = Some(value()?.into()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(parsed)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "help" | "--help" | "-h") {
        print!("{USAGE}");
        return if args.is_empty() { ExitCode::from(2) } else { ExitCode::SUCCESS };
    }
    match run(args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<ExitCode, String> {
    let args = parse(args)?;
    if args.command == "schema" {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
        return runtime.block_on(schema(&args));
    }
    let path = args.manifest.as_ref().ok_or("--manifest is required")?;
    let json = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let manifest = Manifest::from_json(&json).map_err(|e| format!("{} is not a manifest: {e}", path.display()))?;

    match args.command.as_str() {
        "check" if args.snapshot.is_some() => check_snapshot(&args, &manifest),
        "check" => {
            let runtime =
                tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
            runtime.block_on(check(&args, &manifest))
        }
        "explain" => {
            let (explain, report) = manifest.explain(args.view.as_deref(), &args.overrides);
            if args.view.as_ref().is_some_and(|view| manifest.view(view).is_none()) {
                return Err(format!("the manifest has no view {}", args.view.unwrap_or_default()));
            }
            print!("{explain}");
            if !report.diagnostics().is_empty() {
                eprintln!("{report}");
            }
            Ok(if report.is_ok() { ExitCode::SUCCESS } else { ExitCode::from(1) })
        }
        "scaffold" => {
            let view = args.view.as_deref().ok_or("--view is required")?;
            let format = match args.format.as_deref() {
                None | Some("toml") => ScaffoldFormat::Toml,
                Some("sql") => ScaffoldFormat::Sql,
                Some(other) => return Err(format!("unknown format {other}, expected toml or sql")),
            };
            let queries: Vec<&str> = args.queries.iter().map(String::as_str).collect();
            match &args.out {
                Some(dir) => {
                    let path = manifest.scaffold_into(dir, view, format, &queries)?;
                    eprintln!("wrote {}", path.display());
                }
                None => print!("{}", manifest.scaffold_queries(view, format, &queries)?),
            }
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command {other}\n\n{USAGE}")),
    }
}

async fn check(args: &Args, manifest: &Manifest) -> Result<ExitCode, String> {
    let url = match &args.database_url {
        Some(url) => Some(url.clone()),
        None => std::env::var("DATABASE_URL").ok(),
    };
    let report = match manifest.backend.as_str() {
        #[cfg(feature = "postgres")]
        "PostgreSQL" => check_postgres(args, manifest, &url.ok_or(URL_REQUIRED)?).await?,
        #[cfg(feature = "mysql")]
        "MySQL" => check_mysql(args, manifest, &url.ok_or(URL_REQUIRED)?).await?,
        #[cfg(feature = "sqlite")]
        "SQLite" => check_sqlite(args, manifest, url).await?,
        other => return Err(format!("the manifest is for {other}, which this build of mabat does not support")),
    };
    println!("{report}");
    Ok(if report.is_ok() { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

/// Check the views against a snapshot of the schema, without a database.
fn check_snapshot(args: &Args, manifest: &Manifest) -> Result<ExitCode, String> {
    if args.database_url.is_some() || args.schema.is_some() {
        return Err("--snapshot checks without a database: leave out --database-url and --schema".to_string());
    }
    let path = args.snapshot.as_ref().expect("checked by the caller");
    let json = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let snapshot = Snapshot::from_json(&json).map_err(|e| format!("{} is not a snapshot: {e}", path.display()))?;
    let report = manifest.check_snapshot(&snapshot, &path.display().to_string(), &args.overrides)?;
    println!("{report}");
    if !args.overrides.is_empty() {
        eprintln!("note: the SQL of the overrides was not checked; `mabat check --database-url` prepares it");
    }
    Ok(if report.is_ok() { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

const URL_REQUIRED: &str = "--database-url or $DATABASE_URL is required";

/// Write a snapshot of the database's schema, or compare the database with one.
async fn schema(args: &Args) -> Result<ExitCode, String> {
    let url = match &args.database_url {
        Some(url) => url.clone(),
        None => std::env::var("DATABASE_URL").map_err(|_| URL_REQUIRED.to_string())?,
    };
    let actual = snapshot(&url).await?;
    match &args.snapshot {
        None => {
            let json = actual.to_json();
            match &args.out {
                None => print!("{json}"),
                Some(path) => {
                    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
                        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
                    }
                    std::fs::write(path, json).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                    eprintln!("wrote {}", path.display());
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(path) => {
            let json = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let expected =
                Snapshot::from_json(&json).map_err(|e| format!("{} is not a snapshot: {e}", path.display()))?;
            let differences = expected.differences(&actual);
            if differences.is_empty() {
                println!("the database matches {}", path.display());
                return Ok(ExitCode::SUCCESS);
            }
            println!("the database differs from {}:", path.display());
            for difference in &differences {
                println!("  {difference}");
            }
            println!("write a new snapshot with `mabat schema --out {}`", path.display());
            Ok(ExitCode::from(1))
        }
    }
}

/// A snapshot of the schema of the database at `url`, by its scheme.
async fn snapshot(url: &str) -> Result<Snapshot, String> {
    let connect = |e: sqlx::Error| format!("cannot connect: {e}");
    let read = |e: mabat_sqlx::Error| format!("cannot read the schema: {e}");
    let scheme = url.split(':').next().unwrap_or_default();
    match scheme {
        #[cfg(feature = "postgres")]
        "postgres" | "postgresql" => {
            let mut conn = sqlx::PgConnection::connect(url).await.map_err(connect)?;
            mabat_sqlx::schema::snapshot(&mut conn).await.map_err(read)
        }
        #[cfg(feature = "mysql")]
        "mysql" | "mariadb" => {
            let mut conn = sqlx::MySqlConnection::connect(url).await.map_err(connect)?;
            mabat_sqlx::schema::snapshot(&mut conn).await.map_err(read)
        }
        #[cfg(feature = "sqlite")]
        "sqlite" => {
            let mut conn = sqlx::SqliteConnection::connect(url).await.map_err(connect)?;
            mabat_sqlx::schema::snapshot(&mut conn).await.map_err(read)
        }
        other => Err(format!("unknown database `{other}:`, or not supported by this build of mabat")),
    }
}

fn read_schema(schema: &std::path::Path) -> Result<String, String> {
    std::fs::read_to_string(schema).map_err(|e| format!("cannot read {}: {e}", schema.display()))
}

#[cfg(feature = "postgres")]
async fn check_postgres(args: &Args, manifest: &Manifest, url: &str) -> Result<mabat_sqlx::Report, String> {
    let mut conn = sqlx::PgConnection::connect(url).await.map_err(|e| format!("cannot connect: {e}"))?;
    let report = match &args.schema {
        None => mabat_sqlx::manifest::check(manifest, &mut conn, &args.overrides).await.map_err(|e| e.to_string())?,
        Some(schema) => {
            let ddl = read_schema(schema)?;
            // Everything happens in a transaction that is rolled back
            let mut tx = conn.begin().await.map_err(|e| e.to_string())?;
            let name = format!("mabat_check_{}", std::process::id());
            let setup = format!("CREATE SCHEMA \"{name}\"; SET LOCAL search_path TO \"{name}\", public");
            tx.execute(AssertSqlSafe(setup)).await.map_err(|e| e.to_string())?;
            tx.execute(AssertSqlSafe(ddl)).await.map_err(|e| format!("the schema file failed: {e}"))?;
            let report =
                mabat_sqlx::manifest::check(manifest, &mut tx, &args.overrides).await.map_err(|e| e.to_string());
            tx.rollback().await.map_err(|e| e.to_string())?;
            report?
        }
    };
    Ok(report)
}

#[cfg(feature = "mysql")]
async fn check_mysql(args: &Args, manifest: &Manifest, url: &str) -> Result<mabat_sqlx::Report, String> {
    let mut conn = sqlx::MySqlConnection::connect(url).await.map_err(|e| format!("cannot connect: {e}"))?;
    let Some(schema) = &args.schema else {
        return mabat_sqlx::manifest::check(manifest, &mut conn, &args.overrides).await.map_err(|e| e.to_string());
    };
    let ddl = read_schema(schema)?;
    // MySQL commits DDL, so the schema goes in a database of its own, dropped afterwards
    let name = format!("mabat_check_{}", std::process::id());
    let setup = format!("CREATE DATABASE `{name}`; USE `{name}`");
    conn.execute(AssertSqlSafe(setup)).await.map_err(|e| e.to_string())?;
    let report = match conn.execute(AssertSqlSafe(ddl)).await {
        Ok(_) => mabat_sqlx::manifest::check(manifest, &mut conn, &args.overrides).await.map_err(|e| e.to_string()),
        Err(e) => Err(format!("the schema file failed: {e}")),
    };
    conn.execute(AssertSqlSafe(format!("DROP DATABASE `{name}`"))).await.map_err(|e| e.to_string())?;
    report
}

#[cfg(feature = "sqlite")]
async fn check_sqlite(args: &Args, manifest: &Manifest, url: Option<String>) -> Result<mabat_sqlx::Report, String> {
    let url = match (url, &args.schema) {
        (Some(url), _) => url,
        (None, Some(_)) => "sqlite::memory:".to_string(),
        (None, None) => return Err(format!("{URL_REQUIRED}, or --schema")),
    };
    let mut conn = sqlx::SqliteConnection::connect(&url).await.map_err(|e| format!("cannot connect: {e}"))?;
    match &args.schema {
        None => mabat_sqlx::manifest::check(manifest, &mut conn, &args.overrides).await.map_err(|e| e.to_string()),
        Some(schema) => {
            let ddl = read_schema(schema)?;
            // SQLite rolls back DDL too
            let mut tx = conn.begin().await.map_err(|e| e.to_string())?;
            tx.execute(AssertSqlSafe(ddl)).await.map_err(|e| format!("the schema file failed: {e}"))?;
            let report =
                mabat_sqlx::manifest::check(manifest, &mut tx, &args.overrides).await.map_err(|e| e.to_string());
            tx.rollback().await.map_err(|e| e.to_string())?;
            report
        }
    }
}
