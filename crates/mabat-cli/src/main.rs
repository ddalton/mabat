//! `mabat`: check, explain and scaffold Mabat override SQL without the application.
//!
//! The application writes the manifest of its views (`Builder::manifest`); this tool reads
//! it, so a DBA can work on override files with no Rust toolchain.

use std::path::PathBuf;
use std::process::ExitCode;

use mabat_sqlx::manifest::{Manifest, ScaffoldFormat};
use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection};

const USAGE: &str = "\
mabat: check, explain and scaffold Mabat override SQL

Usage:
  mabat check    --manifest <file> [--overrides <dir>]... [--database-url <url>] [--schema <file>]
  mabat explain  --manifest <file> [--overrides <dir>]... [--view <name>]
  mabat scaffold --manifest <file> --view <name> [--format toml|sql]

check     Prepare every query, generated and overridden, on the database without running it,
          and compare its columns and parameters with the views. The database URL defaults to
          $DATABASE_URL. With --schema, the schema file is created in a temporary schema inside
          a transaction that is rolled back, so any database with no access to the
          application's data will do, and nothing is left behind.
explain   Show the queries of the views and the SQL that runs for each.
scaffold  Write an override file with the generated SQL of every query of a view.

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
    format: Option<String>,
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
            "--format" => parsed.format = Some(value()?),
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
            let file = manifest.scaffold(view, format).ok_or_else(|| format!("the manifest has no view {view}"))?;
            print!("{file}");
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command {other}\n\n{USAGE}")),
    }
}

async fn check(args: &Args, manifest: &Manifest) -> Result<ExitCode, String> {
    let url = match &args.database_url {
        Some(url) => url.clone(),
        None => std::env::var("DATABASE_URL").map_err(|_| "--database-url or $DATABASE_URL is required")?,
    };
    let mut conn = PgConnection::connect(&url).await.map_err(|e| format!("cannot connect: {e}"))?;

    let report = match &args.schema {
        None => manifest.check(&mut conn, &args.overrides).await.map_err(|e| e.to_string())?,
        Some(schema) => {
            let ddl = std::fs::read_to_string(schema).map_err(|e| format!("cannot read {}: {e}", schema.display()))?;
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
    println!("{report}");
    Ok(if report.is_ok() { ExitCode::SUCCESS } else { ExitCode::from(1) })
}
