//! Override files: replacement SQL for queries of a view, supplied as configuration.
//!
//! Each view has at most one file, named after the view, e.g. `TaskView.toml`. Each query
//! of the view's plan is addressed by its name, `$root` or the path of the field it fills:
//!
//! ```toml
//! [query."$root"]
//! sql = """
//! SELECT t.id AS "id", t.name AS "name" FROM task t
//! """
//!
//! [query."children.notes"]
//! sql = """
//! SELECT n.id AS "$key", n.task_id AS "$parent", n.body AS "body"
//! FROM task_note n WHERE n.task_id = ANY($1) ORDER BY n.id
//! """
//! ```
//!
//! or plain SQL, e.g. `TaskView.sql`, with a marker comment before each query, so that the
//! file can be edited with SQL tooling and each query pasted into `psql`:
//!
//! ```sql
//! -- mabat: query $root
//! SELECT t.id AS "id", t.name AS "name" FROM task t;
//!
//! -- mabat: query children.notes, shadow
//! SELECT n.id AS "$key", n.task_id AS "$parent", n.body AS "body"
//! FROM task_note n WHERE n.task_id = ANY($1) ORDER BY n.id;
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;

use crate::report::{Diagnostic, Report, Severity};

/// Where an override comes from, for messages: `file:line`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub file: String,
    pub line: usize,
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.file, self.line)
    }
}

/// The override of one query, as written in a file.
#[derive(Debug, Clone)]
pub struct QueryOverride {
    pub query: String,
    pub sql: Arc<str>,
    pub shadow: bool,
    pub origin: Origin,
}

/// The overrides of one view, parsed from a file.
#[derive(Debug, Clone)]
pub struct OverrideFile {
    pub view: String,
    pub file: String,
    pub queries: Vec<QueryOverride>,
}

/// The content of an override file, before it is parsed.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Source {
    view: String,
    file: String,
    format: Format,
    content: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Format {
    Toml,
    Sql,
}

impl Source {
    /// Overrides of a view in TOML, given in code; `file` names them in messages.
    pub fn toml(view: impl Into<String>, file: impl Into<String>, content: impl Into<String>) -> Source {
        Source { view: view.into(), file: file.into(), format: Format::Toml, content: content.into() }
    }

    /// Overrides of a view in SQL, given in code; `file` names them in messages.
    pub fn sql(view: impl Into<String>, file: impl Into<String>, content: impl Into<String>) -> Source {
        Source { view: view.into(), file: file.into(), format: Format::Sql, content: content.into() }
    }
}

/// Read and parse the override files of the directories. Problems are added to the report.
pub fn read_override_files(dirs: &[PathBuf], inline: &[Source], report: &mut Report) -> Vec<OverrideFile> {
    let sources = read_sources(dirs, inline, report);
    parse_sources(&sources, report)
}

/// Read the override files of the directories, and the inline overrides.
pub fn read_sources(dirs: &[PathBuf], inline: &[Source], report: &mut Report) -> Vec<Source> {
    {
        let mut sources = Vec::new();
        for dir in dirs {
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(e) => {
                    report.push(file_error(&dir.display().to_string(), 1, format!("cannot read the directory: {e}")));
                    continue;
                }
            };
            let mut paths: Vec<(PathBuf, Format)> = entries
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter_map(|path| {
                    let format = match path.extension()?.to_str()? {
                        "toml" => Format::Toml,
                        "sql" => Format::Sql,
                        _ => return None,
                    };
                    Some((path, format))
                })
                .collect();
            paths.sort_by(|a, b| a.0.cmp(&b.0));
            for (path, format) in paths {
                let file = path.display().to_string();
                let view = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                match std::fs::read_to_string(&path) {
                    Ok(content) => sources.push(Source { view, file, format, content }),
                    Err(e) => report.push(file_error(&file, 1, format!("cannot read the file: {e}"))),
                }
            }
        }
        sources.extend(inline.iter().cloned());
        sources
    }
}

pub fn parse_sources(sources: &[Source], report: &mut Report) -> Vec<OverrideFile> {
    let mut files: Vec<OverrideFile> = Vec::new();
    for source in sources {
        if let Some(other) = files.iter().find(|f| f.view == source.view) {
            report.push(file_error(
                &source.file,
                1,
                format!("{} already has an override file, {}", source.view, other.file),
            ));
            continue;
        }
        let parsed = match source.format {
            Format::Toml => parse(&source.view, &source.file, &source.content),
            Format::Sql => parse_sql(&source.view, &source.file, &source.content),
        };
        match parsed {
            Ok(parsed) => files.push(parsed),
            Err((origin, message)) => report.push(file_error(&origin.file, origin.line, message)),
        }
    }
    files
}

fn file_error(file: &str, line: usize, message: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        code: "M0100",
        view: String::new(),
        query: String::new(),
        origin: Some(format!("{file}:{line}")),
        summary: "override file cannot be used".to_string(),
        notes: vec![message],
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDoc {
    #[serde(default)]
    query: BTreeMap<String, toml::Spanned<QueryDoc>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryDoc {
    sql: String,
    #[serde(default)]
    shadow: bool,
}

/// Parse the overrides of `view` from the TOML `content` of `file`.
///
/// Returns the parse error as a message with its line on failure.
pub fn parse(view: &str, file: &str, content: &str) -> Result<OverrideFile, (Origin, String)> {
    let doc: FileDoc = toml::from_str(content).map_err(|e| {
        let line = e.span().map_or(1, |span| line_of(content, span.start));
        (Origin { file: file.to_string(), line }, e.message().to_string())
    })?;
    let queries = doc
        .query
        .into_iter()
        .map(|(query, spanned)| {
            let line = line_of(content, spanned.span().start);
            let doc = spanned.into_inner();
            QueryOverride {
                query,
                sql: doc.sql.into(),
                shadow: doc.shadow,
                origin: Origin { file: file.to_string(), line },
            }
        })
        .collect();
    Ok(OverrideFile { view: view.to_string(), file: file.to_string(), queries })
}

/// The marker that starts a query in a SQL override file.
const SQL_MARKER: &str = "mabat:";

/// Parse the overrides of `view` from the SQL `content` of `file`.
pub fn parse_sql(view: &str, file: &str, content: &str) -> Result<OverrideFile, (Origin, String)> {
    let error = |line: usize, message: String| (Origin { file: file.to_string(), line }, message);
    let mut queries: Vec<QueryOverride> = Vec::new();
    let mut body = String::new();

    let finish = |queries: &mut Vec<QueryOverride>, body: &mut String| -> Result<(), (Origin, String)> {
        if let Some(query) = queries.last_mut() {
            let sql = body.trim().trim_end_matches(';').trim_end();
            if sql.is_empty() {
                return Err(error(query.origin.line, format!("query \"{}\" has no SQL", query.query)));
            }
            query.sql = sql.into();
        }
        body.clear();
        Ok(())
    };

    for (i, line) in content.lines().enumerate() {
        let number = i + 1;
        let marker = line.trim().strip_prefix("--").map(str::trim).and_then(|c| c.strip_prefix(SQL_MARKER));
        let Some(marker) = marker else {
            if queries.is_empty() && !line.trim().is_empty() && !line.trim().starts_with("--") {
                return Err(error(number, "SQL before the first `-- mabat: query <name>` line".to_string()));
            }
            body.push_str(line);
            body.push('\n');
            continue;
        };

        finish(&mut queries, &mut body)?;
        let mut parts = marker.split(',').map(str::trim);
        let query = parts
            .next()
            .and_then(|p| p.strip_prefix("query"))
            .map(str::trim)
            .filter(|name| !name.is_empty() && !name.contains(char::is_whitespace))
            .ok_or_else(|| {
                error(number, "expected `-- mabat: query <name>`, optionally followed by `, shadow`".into())
            })?;
        let mut shadow = false;
        for option in parts {
            match option {
                "shadow" => shadow = true,
                other => return Err(error(number, format!("unknown option `{other}`, expected `shadow`"))),
            }
        }
        if queries.iter().any(|q| q.query == query) {
            return Err(error(number, format!("query \"{query}\" appears more than once")));
        }
        queries.push(QueryOverride {
            query: query.to_string(),
            sql: "".into(),
            shadow,
            origin: Origin { file: file.to_string(), line: number },
        });
    }
    finish(&mut queries, &mut body)?;
    Ok(OverrideFile { view: view.to_string(), file: file.to_string(), queries })
}

/// The 1-based line of a byte offset.
fn line_of(content: &str, offset: usize) -> usize {
    content[..offset.min(content.len())].bytes().filter(|&b| b == b'\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_queries_with_lines() {
        let content = "# TaskView\n\n[query.\"$root\"]\nsql = \"SELECT 1\"\n\n[query.children]\nsql = \"SELECT 2\"\nshadow = true\n";
        let file = parse("TaskView", "TaskView.toml", content).unwrap();
        assert_eq!(file.queries.len(), 2);
        let root = &file.queries[0];
        assert_eq!(root.query, "$root");
        assert_eq!(&*root.sql, "SELECT 1");
        assert!(!root.shadow);
        assert_eq!(root.origin.line, 3);
        let children = &file.queries[1];
        assert_eq!(children.query, "children");
        assert!(children.shadow);
        assert_eq!(children.origin.line, 6);
    }

    #[test]
    fn parses_sql_files() {
        let content = "-- Tuned by the DBA team\n\n-- mabat: query $root\nSELECT 1\nFROM task;\n\n\
                       --mabat:query children.notes , shadow\n-- a comment inside\nSELECT 2\n";
        let file = parse_sql("TaskView", "TaskView.sql", content).unwrap();
        assert_eq!(file.queries.len(), 2);
        assert_eq!(file.queries[0].query, "$root");
        assert_eq!(&*file.queries[0].sql, "SELECT 1\nFROM task");
        assert_eq!(file.queries[0].origin.line, 3);
        assert!(!file.queries[0].shadow);
        assert_eq!(file.queries[1].query, "children.notes");
        assert_eq!(&*file.queries[1].sql, "-- a comment inside\nSELECT 2");
        assert_eq!(file.queries[1].origin.line, 7);
        assert!(file.queries[1].shadow);
    }

    #[test]
    fn reports_sql_file_errors() {
        let cases = [
            ("SELECT 1\n", 1, "SQL before the first"),
            ("-- mabat: query $root\n\n", 1, "has no SQL"),
            ("-- mabat: query\nSELECT 1\n", 1, "expected `-- mabat: query <name>`"),
            ("-- mabat: query $root, shadwo\nSELECT 1\n", 1, "unknown option `shadwo`"),
            ("-- mabat: query a\nSELECT 1\n-- mabat: query a\nSELECT 2\n", 3, "more than once"),
        ];
        for (content, line, message) in cases {
            let (origin, error) = parse_sql("TaskView", "TaskView.sql", content).unwrap_err();
            assert_eq!(origin.line, line, "{content}");
            assert!(error.contains(message), "{content}: {error}");
        }
    }

    #[test]
    fn reports_unknown_keys() {
        let content = "[query.\"$root\"]\nsql = \"SELECT 1\"\nshadwo = true\n";
        let (origin, message) = parse("TaskView", "TaskView.toml", content).unwrap_err();
        assert_eq!(origin.line, 3);
        assert!(message.contains("shadwo"), "{message}");
    }
}
