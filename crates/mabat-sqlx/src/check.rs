//! Checking the queries of views against the database.
//!
//! Every query of every view of a [`Manifest`] is prepared on the database, without
//! running it, and the columns and parameters of the prepared statement are compared with
//! the view: the generated query, which catches a schema that no longer matches the view,
//! and the override, which must select the same aliases with compatible types. The
//! application checks its views at startup this way, and `mabat check` checks a manifest
//! written by the application.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use mabat_core::{INDEX_ALIAS, KEY_ALIAS, MAP_KEY_ALIAS, PARENT_ALIAS, QueryPlan, ViewShape};
use sqlx::postgres::{PgStatement, PgTypeInfo};
use sqlx::{AssertSqlSafe, Column, Connection, Either, Executor, PgConnection, SqlSafeStr, Statement, TypeInfo};

use crate::describe::Description;
use crate::key::KeyClass;
use crate::manifest::{CheckedOverrides, LinkManifest, Manifest, QueryManifest, Role, TypeManifest, ViewManifest};
use crate::overrides::{OverrideFile, QueryOverride};
use crate::registry::{ActiveOverride, Overrides, ShadowStats};
use crate::report::{Diagnostic, Report, Severity, suggest};

/// A registered view.
#[derive(Clone, Copy)]
pub(crate) struct ViewEntry {
    pub(crate) shape: &'static ViewShape,
    pub(crate) describe: fn(&mut Description),
}

/// A checked view: its plan, and the overrides that passed the checks.
pub(crate) struct Checked {
    pub(crate) shape: &'static ViewShape,
    pub(crate) plan: Arc<QueryPlan>,
    pub(crate) overrides: Overrides,
}

/// Check the views of the manifest and their override files. Problems are added to the
/// report. Returns the overrides that passed the checks, by view.
///
/// Fails only if the connection fails.
pub(crate) async fn check(
    conn: &mut PgConnection,
    manifest: &Manifest,
    files: Vec<OverrideFile>,
    report: &mut Report,
) -> Result<CheckedOverrides, sqlx::Error> {
    let mut files: HashMap<String, OverrideFile> = files.into_iter().map(|f| (f.view.clone(), f)).collect();
    let mut checked = CheckedOverrides::new();

    for view in &manifest.views {
        let file = files.remove(&view.name);
        let mut by_query: HashMap<&str, &QueryOverride> = HashMap::new();
        if let Some(file) = &file {
            let names: Vec<&str> = view.queries.iter().map(|q| q.name.as_str()).collect();
            for query in &file.queries {
                if names.contains(&query.query.as_str()) {
                    by_query.insert(&query.query, query);
                } else {
                    let mut notes = vec![format!("the queries of {} are: {}", view.name, names.join(", "))];
                    if let Some(name) = suggest(&query.query, names.iter().copied()) {
                        notes.insert(0, format!("did you mean \"{name}\"?"));
                    }
                    report.push(Diagnostic {
                        severity: Severity::Error,
                        code: "M0101",
                        view: view.name.clone(),
                        query: query.query.clone(),
                        origin: Some(query.origin.to_string()),
                        summary: format!("\"{}\" is not a query of {}", query.query, view.name),
                        notes,
                    });
                }
            }
        }

        let mut overrides = Overrides::new();
        // The key classes of the columns of each checked query, to check the links of its children
        let mut classes: Vec<Option<KeyClasses>> = Vec::with_capacity(view.queries.len());
        for query in &view.queries {
            let link_class = query.parent.and_then(|p| {
                let parent = classes[p].as_ref()?;
                match &query.link {
                    LinkManifest::Child | LinkManifest::Variant { .. } => {
                        parent.get(&view.queries[p].key_alias).copied()
                    }
                    LinkManifest::ToOne { ref_alias } => parent.get(ref_alias).copied(),
                    LinkManifest::Root => None,
                }
            });
            let context = Query { view, query, link_class };
            classes.push(check_query(conn, &context, &by_query, &mut overrides, report).await?);
        }
        checked.insert(view.name.clone(), overrides);
    }

    for file in files.into_values() {
        let mut notes = Vec::new();
        if let Some(name) = suggest(&file.view, manifest.views.iter().map(|v| v.name.as_str())) {
            notes.push(format!("did you mean {name}?"));
        }
        notes.push("override files are named after a registered view, e.g. TaskView.toml".to_string());
        report.push(Diagnostic {
            severity: Severity::Error,
            code: "M0101",
            view: file.view.clone(),
            query: String::new(),
            origin: Some(file.file.clone()),
            summary: format!("{} is not a registered view", file.view),
            notes,
        });
    }

    Ok(checked)
}

/// The key class the query is linked to its parent query with: the class of the parent's
/// key for a to-many query, the class of the parent's reference column for a to-one query.
/// `None` for the root query, or when the parent query could not be checked.
type LinkClass = Option<KeyClass>;

/// The key classes of the columns of a checked query, by alias.
type KeyClasses = HashMap<String, KeyClass>;

/// Check a query and its override. Returns the key classes of its columns, `None` if it
/// could not be checked.
async fn check_query(
    conn: &mut PgConnection,
    query: &Query<'_>,
    by_query: &HashMap<&str, &QueryOverride>,
    overrides: &mut Overrides,
    report: &mut Report,
) -> Result<Option<KeyClasses>, sqlx::Error> {
    let name = query.query.name.as_str();
    let override_ = by_query.get(name).copied();
    let mut classes = None;

    // The override, if any
    let mut active = None;
    if let Some(override_) = override_ {
        match inspect(conn, &override_.sql).await? {
            Err(message) => report.push(query.diagnostic(Some(override_), "M0103", "does not prepare", vec![message])),
            Ok(statement) => {
                if let Some((found, keys_param)) = query.compare(&statement, Some(override_), report) {
                    classes = Some(found);
                    active = Some((override_, keys_param));
                }
            }
        }
    }

    // The generated query is checked too: it runs when there is no valid override, and in
    // shadow mode. A problem with it is a warning when a valid override replaces it.
    let mut generated_report = Report::default();
    match inspect(conn, &query.query.sql).await? {
        Err(message) => generated_report.push(query.diagnostic(None, "M0103", "does not prepare", vec![message])),
        Ok(statement) => {
            if let Some((found, _)) = query.compare(&statement, None, &mut generated_report) {
                classes.get_or_insert(found);
            }
        }
    }
    let generated_ok = generated_report.is_ok();
    for mut diagnostic in generated_report.diagnostics {
        match active {
            Some((override_, _)) if override_.shadow => {
                diagnostic.notes.push("the override is shadowed, which runs the generated query too".to_string());
            }
            Some(_) => {
                diagnostic.severity = Severity::Warning;
                diagnostic.notes.push("the query is replaced by a valid override".to_string());
            }
            None => {}
        }
        report.push(diagnostic);
    }

    if let Some((override_, keys_param)) = active {
        overrides.insert(
            name.to_string(),
            ActiveOverride {
                sql: override_.sql.clone(),
                keys_param,
                shadow: override_.shadow && generated_ok,
                origin: override_.origin.clone(),
                stats: Arc::new(ShadowStats::default()),
            },
        );
    }
    Ok(classes)
}

/// Prepare the SQL in a transaction (a savepoint, if the connection is in a transaction)
/// that is rolled back, so that a failing statement does not abort the caller's transaction.
///
/// Returns the database's message if the statement does not prepare.
async fn inspect(conn: &mut PgConnection, sql: &str) -> Result<Result<PgStatement, String>, sqlx::Error> {
    let mut tx = conn.begin().await?;
    let result = (&mut *tx).prepare(AssertSqlSafe(sql.to_string()).into_sql_str()).await;
    tx.rollback().await?;
    match result {
        Ok(statement) => Ok(Ok(statement)),
        Err(sqlx::Error::Database(e)) => Ok(Err(e.message().to_string())),
        Err(e) => Err(e),
    }
}

struct Query<'a> {
    view: &'a ViewManifest,
    query: &'a QueryManifest,
    link_class: LinkClass,
}

/// A column a query needs to select.
struct Expected<'a> {
    alias: &'a str,
    /// The Rust type of a field or map key column, `None` for a system column.
    column: Option<&'a TypeManifest>,
    optional: bool,
    /// The column holds a key: the key of the view, the parent key or a reference.
    key: bool,
}

impl Query<'_> {
    fn diagnostic(
        &self,
        override_: Option<&QueryOverride>,
        code: &'static str,
        problem: &str,
        notes: Vec<String>,
    ) -> Diagnostic {
        let view = &self.view.name;
        let name = &self.query.name;
        let summary = match override_ {
            Some(_) => format!("override for {view}.{name} {problem}"),
            None => format!("generated query for {view}.{name} {problem}"),
        };
        Diagnostic {
            severity: Severity::Error,
            code,
            view: view.to_string(),
            query: name.to_string(),
            origin: override_.map(|o| o.origin.to_string()),
            summary,
            notes,
        }
    }

    fn expected(&self) -> Vec<Expected<'_>> {
        self.query
            .columns
            .iter()
            .map(|c| {
                let key = match c.role {
                    Role::Key | Role::Parent | Role::Reference => true,
                    Role::Field => c.alias == self.query.key_alias,
                    Role::Index | Role::MapKey => false,
                };
                Expected { alias: &c.alias, column: c.r#type.as_ref(), optional: c.optional, key }
            })
            .collect()
    }

    /// Compare a prepared statement with the view. Adds the problems to the report and
    /// returns the key classes of the columns and whether the statement takes the keys as
    /// a parameter, or `None` if there are errors.
    fn compare(
        &self,
        statement: &PgStatement,
        override_: Option<&QueryOverride>,
        report: &mut Report,
    ) -> Option<(KeyClasses, bool)> {
        let expected = self.expected();
        let mut errors = Vec::new();
        let mut classes = KeyClasses::new();
        let mut seen = HashSet::new();

        for (i, column) in statement.columns().iter().enumerate() {
            let name = column.name();
            let ty = column.type_info();
            let n = i + 1;
            if !seen.insert(name) {
                errors.push(format!("column {n} \"{name}\" is selected more than once"));
                continue;
            }
            let Some(expected) = expected.iter().find(|e| e.alias == name) else {
                let mut note = format!("column {n} \"{name}\" is not a path of {} in this query", self.query.view);
                if name == KEY_ALIAS {
                    note.push_str(&format!(" (the key is selected as \"{}\")", self.query.key_alias));
                } else if let Some(alias) = suggest(name, expected.iter().map(|e| e.alias)) {
                    note.push_str(&format!(" (did you mean \"{alias}\"?)"));
                }
                errors.push(note);
                continue;
            };
            if let Some(column) = expected.column
                && !column.accepts(ty)
            {
                errors.push(format!(
                    "column {n} \"{name}\" has type {}, expected {} for {}",
                    ty.name(),
                    column.sql,
                    column.rust
                ));
                continue;
            }
            if name == INDEX_ALIAS && KeyClass::of(ty) != Some(KeyClass::Int) {
                errors.push(format!(
                    "column {n} \"{name}\" has type {}; it places the elements of the list, so it needs an integer type",
                    ty.name()
                ));
                continue;
            }
            if expected.key {
                match KeyClass::of(ty) {
                    None => errors.push(format!(
                        "column {n} \"{name}\" has type {}, which cannot hold a key; expected an integer, text or uuid type",
                        ty.name()
                    )),
                    Some(class) => {
                        classes.insert(name.to_string(), class);
                    }
                }
            }
        }

        // The link to the parent query
        if let Some(parent) = self.link_class {
            let key_alias = self.query.key_alias.as_str();
            let (alias, what) = match &self.query.link {
                LinkManifest::Child => (PARENT_ALIAS, "the key of the parent query".to_string()),
                LinkManifest::ToOne { ref_alias } => (key_alias, format!("the parent's \"{ref_alias}\"")),
                LinkManifest::Variant { .. } => (key_alias, "the key of the parent query".to_string()),
                LinkManifest::Root => unreachable!("the root query has no parent"),
            };
            if let Some(class) = classes.get(alias)
                && *class != parent
            {
                errors.push(format!("column \"{alias}\" holds {class} key, but {what} holds {parent} key"));
            }
        }

        let mut missing_optional = Vec::new();
        for expected in &expected {
            if seen.contains(expected.alias) {
                continue;
            }
            match expected.column {
                Some(_) if expected.optional => missing_optional.push(expected.alias),
                _ if expected.alias == PARENT_ALIAS => errors
                    .push(format!("column \"{PARENT_ALIAS}\" is not selected; it attaches the rows to their parent")),
                _ if expected.alias == INDEX_ALIAS => {
                    errors.push(format!("column \"{INDEX_ALIAS}\" is not selected; it places the elements of the list"))
                }
                _ if expected.alias == MAP_KEY_ALIAS => {
                    errors.push(format!("column \"{MAP_KEY_ALIAS}\" is not selected; it is the key of each element"))
                }
                _ if expected.alias.starts_with('$') => {
                    errors.push(format!("column \"{}\" is not selected; it holds a key", expected.alias))
                }
                _ => errors.push(format!("path \"{}\" is not selected", expected.alias)),
            }
        }

        let has_errors = !errors.is_empty();
        if has_errors {
            let problem = match override_ {
                Some(_) => "does not match the view",
                None => "does not match the database",
            };
            report.push(self.diagnostic(override_, "M0102", problem, errors));
        }

        // Only overrides choose their parameters
        let mut keys_param = !matches!(self.query.link, LinkManifest::Root);
        if override_.is_some() {
            match self.parameters(statement, &classes) {
                Ok(takes_keys) => keys_param = takes_keys,
                Err(notes) => {
                    report.push(self.diagnostic(override_, "M0104", "has the wrong parameters", notes));
                    return None;
                }
            }
            if !missing_optional.is_empty() && !has_errors {
                let mut diagnostic = self.diagnostic(
                    override_,
                    "M0105",
                    "does not select every optional path",
                    missing_optional.iter().map(|alias| format!("path \"{alias}\" is always None")).collect(),
                );
                diagnostic.severity = Severity::Warning;
                report.push(diagnostic);
            }
        }

        (!has_errors).then_some((classes, keys_param))
    }

    /// Check the parameters of an override: the root query takes either no parameter or
    /// the array of root keys as `$1`, other queries take the array of keys they are
    /// selected by as `$1`. Returns whether the query takes the keys.
    fn parameters(&self, statement: &PgStatement, classes: &KeyClasses) -> Result<bool, Vec<String>> {
        let types: Vec<Option<&PgTypeInfo>> = match statement.parameters() {
            Some(Either::Left(types)) => types.iter().map(Some).collect(),
            Some(Either::Right(count)) => vec![None; count],
            None => Vec::new(),
        };
        let root = matches!(self.query.link, LinkManifest::Root);
        let (keys, of) = match &self.query.link {
            LinkManifest::Root => (classes.get(&self.query.key_alias).copied(), "root keys"),
            LinkManifest::Child | LinkManifest::Variant { .. } => (self.link_class, "parent keys"),
            LinkManifest::ToOne { .. } => (self.link_class, "referenced keys"),
        };
        match types.as_slice() {
            [] if root => Ok(false),
            [] => Err(vec![format!("the query needs to take the array of {of} as $1, e.g. `WHERE fk = ANY($1)`")]),
            [ty] => match ty.map(|ty| (ty, KeyClass::of_array(ty))) {
                Some((ty, None)) => {
                    Err(vec![format!("$1 has type {}; it is the array of {of}, use it as `= ANY($1)`", ty.name())])
                }
                Some((_, Some(class))) if keys.is_some_and(|keys| keys != class) => Err(vec![format!(
                    "$1 is an array of {class} keys, but the {of} are {} keys",
                    keys.expect("checked above")
                )]),
                _ => Ok(true),
            },
            more => {
                let expected = if root { "no parameter or the array of root keys" } else { "only the array of keys" };
                Err(vec![format!("the query takes {} parameters, expected {expected} as $1", more.len())])
            }
        }
    }
}
