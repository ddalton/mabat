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

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;

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
pub(crate) struct QueryOverride {
    pub(crate) query: String,
    pub(crate) sql: Arc<str>,
    pub(crate) shadow: bool,
    pub(crate) origin: Origin,
}

/// The overrides of one view, parsed from a file.
#[derive(Debug, Clone)]
pub(crate) struct OverrideFile {
    pub(crate) view: String,
    pub(crate) file: String,
    pub(crate) queries: Vec<QueryOverride>,
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
pub(crate) fn parse(view: &str, file: &str, content: &str) -> Result<OverrideFile, (Origin, String)> {
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
    fn reports_unknown_keys() {
        let content = "[query.\"$root\"]\nsql = \"SELECT 1\"\nshadwo = true\n";
        let (origin, message) = parse("TaskView", "TaskView.toml", content).unwrap_err();
        assert_eq!(origin.line, 3);
        assert!(message.contains("shadwo"), "{message}");
    }
}
