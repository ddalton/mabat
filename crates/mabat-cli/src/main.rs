//! `mabat`: check, explain and scaffold Mabat override SQL without the application.
//!
//! The application writes the manifest of its views (`Builder::manifest`); this tool reads
//! it, so a DBA can work on override files with no Rust toolchain.

use std::path::PathBuf;
use std::process::ExitCode;

use mabat_sqlx::manifest::{Manifest, ScaffoldFormat};
use sqlx::{AssertSqlSafe, Connection, Executor};

const USAGE: &str = "\
mabat: check, explain and scaffold Mabat override SQL

Usage:
  mabat check    --manifest <file> [--overrides <dir>]... [--database-url <url>] [--schema <file>]
  mabat explain  --manifest <file> [--overrides <dir>]... [--view <name>]
  mabat scaffold --manifest <file> --view <name> [--query <name>]... [--format toml|sql] [--out <dir>]

check     Prepare every query, generated and overridden, on the database without running it,
          and compare its columns and parameters with the views. The database URL defaults to
          $DATABASE_URL. With --schema, the schema file is created in a temporary schema inside
          a transaction that is rolled back, so any database with no access to the
          application's data will do, and nothing is left behind. On MySQL, which cannot roll
          back DDL, the schema is created in a temporary database that is dropped afterwards.
          On SQLite, --schema without a database URL checks against a new in-memory database.
explain   Show the queries of the views and the SQL that runs for each.
scaffold  Print an override file with the generated SQL of a view's queries, to edit and tune:
          the queries named with --query, or every query. Each query in the file replaces the
          generated one, so name only the queries being tuned; `mabat explain` lists them.
          With --out, write it to the view's file in that directory instead, or add the queries
          to the end of the file if it exists; a query the file already overrides is refused.

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
    let path = args.manifest.as_ref().ok_or("--manifest is required")?;
    let json = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let manifest = Manifest::from_json(&json).map_err(|e| format!("{} is not a manifest: {e}", path.display()))?;

    match args.command.as_str() {
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

const URL_REQUIRED: &str = "--database-url or $DATABASE_URL is required";

fn read_schema(schema: &std::path::Path) -> Result<String, String> {
    std::fs::read_to_string(schema).map_err(|e| format!("cannot read {}: {e}", schema.display()))
}

#[cfg(feature = "postgres")]
async fn check_postgres(args: &Args, manifest: &Manifest, url: &str) -> Result<mabat_sqlx::Report, String> {
    let mut conn = sqlx::PgConnection::connect(url).await.map_err(|e| format!("cannot connect: {e}"))?;
    let report = match &args.schema {
        None => manifest.check(&mut conn, &args.overrides).await.map_err(|e| e.to_string())?,
        Some(schema) => {
            let ddl = read_schema(schema)?;
            // Everything happens in a transaction that is rolled back
            let mut tx = conn.begin().await.map_err(|e| e.to_string())?;
            let name = format!("mabat_check_{}", std::process::id());
            let setup = format!("CREATE SCHEMA \"{name}\"; SET LOCAL search_path TO \"{name}\", public");
            tx.execute(AssertSqlSafe(setup)).await.map_err(|e| e.to_string())?;
            tx.execute(AssertSqlSafe(ddl)).await.map_err(|e| format!("the schema file failed: {e}"))?;
            let report = manifest.check(&mut tx, &args.overrides).await.map_err(|e| e.to_string());
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
        return manifest.check(&mut conn, &args.overrides).await.map_err(|e| e.to_string());
    };
    let ddl = read_schema(schema)?;
    // MySQL commits DDL, so the schema goes in a database of its own, dropped afterwards
    let name = format!("mabat_check_{}", std::process::id());
    let setup = format!("CREATE DATABASE `{name}`; USE `{name}`");
    conn.execute(AssertSqlSafe(setup)).await.map_err(|e| e.to_string())?;
    let report = match conn.execute(AssertSqlSafe(ddl)).await {
        Ok(_) => manifest.check(&mut conn, &args.overrides).await.map_err(|e| e.to_string()),
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
        None => manifest.check(&mut conn, &args.overrides).await.map_err(|e| e.to_string()),
        Some(schema) => {
            let ddl = read_schema(schema)?;
            // SQLite rolls back DDL too
            let mut tx = conn.begin().await.map_err(|e| e.to_string())?;
            tx.execute(AssertSqlSafe(ddl)).await.map_err(|e| format!("the schema file failed: {e}"))?;
            let report = manifest.check(&mut tx, &args.overrides).await.map_err(|e| e.to_string());
            tx.rollback().await.map_err(|e| e.to_string())?;
            report
        }
    }
}
