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

use mabat_core::sql::{self, KEYS_TOKEN};
use mabat_core::{INDEX_ALIAS, KEY_ALIAS, MAP_KEY_ALIAS, PARENT_ALIAS, QueryPlan, ViewShape};

use crate::backend::{Backend, Inspected, InspectedParams, KeyClass, KeyKind};
use crate::describe::DescribeFn;
use crate::manifest::{CheckedOverrides, LinkManifest, Manifest, QueryManifest, Role, TypeManifest, ViewManifest};
use crate::overrides::{OverrideFile, QueryOverride};
use crate::registry::{ActiveOverride, Overrides, ShadowStats};
use crate::report::{Diagnostic, Report, Severity, suggest};

/// A registered view.
pub(crate) struct ViewEntry<B: Backend> {
    pub(crate) shape: &'static ViewShape,
    pub(crate) describe: DescribeFn<B>,
}

impl<B: Backend> Clone for ViewEntry<B> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B: Backend> Copy for ViewEntry<B> {}

/// A checked view: its plan, and the overrides that passed the checks.
pub(crate) struct Checked {
    pub(crate) shape: &'static ViewShape,
    pub(crate) plan: Arc<QueryPlan>,
    pub(crate) overrides: Overrides,
    /// The database the overrides were checked against, by `sqlx::Database::NAME`.
    pub(crate) backend: &'static str,
}

/// Check the views of the manifest and their override files. Problems are added to the
/// report. Returns the overrides that passed the checks, by view.
///
/// Fails only if the connection fails.
pub(crate) async fn check<B: Backend>(
    conn: &mut B::Connection,
    manifest: &Manifest,
    files: Vec<OverrideFile>,
    report: &mut Report,
) -> Result<CheckedOverrides, sqlx::Error> {
    let mut files: HashMap<String, OverrideFile> = files.into_iter().map(|f| (f.view.clone(), f)).collect();
    let mut checked = CheckedOverrides::new();

    for view in &manifest.views {
        let file = files.remove(&view.name);
        let by_query = mabat_check::check::queries_of(view, file.as_ref(), report);

        let mut overrides = Overrides::new();
        // The key classes of the columns of each checked query, to check the links of its children
        let mut classes: Vec<Option<KeyClasses>> = Vec::with_capacity(view.queries.len());
        for query in &view.queries {
            let link_class = query.parent.and_then(|p| {
                let parent = classes[p].as_ref()?;
                match &query.link {
                    LinkManifest::Child { .. } | LinkManifest::Variant { .. } => {
                        parent.get(&view.queries[p].key_alias).copied()
                    }
                    LinkManifest::ToOne { ref_alias } => parent.get(ref_alias).copied(),
                    LinkManifest::Root => None,
                }
            });
            let context = Query { view, query, link_class };
            classes.push(check_query::<B>(conn, &context, &by_query, &mut overrides, report).await?);
        }
        checked.insert(view.name.clone(), overrides);
    }

    mabat_check::check::unknown_views(manifest, files.into_values(), report);

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
async fn check_query<B: Backend>(
    conn: &mut B::Connection,
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
        match inspect::<B>(conn, &override_.sql).await? {
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
    match inspect::<B>(conn, &query.query.sql).await? {
        Err(message) => generated_report.push(query.diagnostic(None, "M0103", "does not prepare", vec![message])),
        Ok(statement) => {
            if let Some((found, _)) = query.compare(&statement, None, &mut generated_report) {
                classes.get_or_insert(found);
            }
        }
    }
    let generated_ok = generated_report.is_ok();
    for mut diagnostic in generated_report.into_diagnostics() {
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
/// On MySQL and SQLite, the keys placeholder `:keys` stands for one key.
///
/// Returns the database's message if the statement does not prepare.
async fn inspect<B: Backend>(conn: &mut B::Connection, sql: &str) -> Result<Result<Inspected, String>, sqlx::Error> {
    // SQLx caches prepared statements by their SQL. Postgres infers the parameter types of a
    // statement prepared without arguments, e.g. `smallint[]` for `smallint_column = ANY($1)`,
    // while a load binds integer keys as `bigint[]`: the comment keeps the statements of the
    // checks apart from the statements that loads run on the same connection.
    let sql = format!("/* mabat check */ {}", sql::expand_keys(sql, B::DIALECT, 1));
    B::inspect(conn, sql).await
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
        statement: &Inspected,
        override_: Option<&QueryOverride>,
        report: &mut Report,
    ) -> Option<(KeyClasses, bool)> {
        let expected = self.expected();
        let mut errors = Vec::new();
        let mut classes = KeyClasses::new();
        let mut seen = HashSet::new();

        for (i, column) in statement.columns.iter().enumerate() {
            let name = column.name.as_str();
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
            // The type of a column the driver cannot type, such as an SQLite expression, is
            // not checked
            let type_name = column.type_name.as_deref();
            if let (Some(expected_type), Some(type_name)) = (expected.column, type_name)
                && !expected_type.accepts.iter().any(|accepted| accepted.eq_ignore_ascii_case(type_name))
            {
                errors.push(format!(
                    "column {n} \"{name}\" has type {type_name}, expected {} for {}",
                    expected_type.sql, expected_type.rust
                ));
                continue;
            }
            let class = column.key_kind.and_then(KeyKind::class);
            if name == INDEX_ALIAS && column.key_kind != Some(KeyKind::Dynamic) && class != Some(KeyClass::Int) {
                errors.push(format!(
                    "column {n} \"{name}\" has type {}; it places the elements of the list, so it needs an integer type",
                    type_name.unwrap_or("?")
                ));
                continue;
            }
            if expected.key {
                match (column.key_kind, class) {
                    (None, _) => errors.push(format!(
                        "column {n} \"{name}\" has type {}, which cannot hold a key; expected an integer, text or uuid type",
                        type_name.unwrap_or("?")
                    )),
                    (Some(_), Some(class)) => {
                        classes.insert(name.to_string(), class);
                    }
                    (Some(_), None) => {}
                }
            }
        }

        // The link to the parent query
        if let Some(parent) = self.link_class {
            let key_alias = self.query.key_alias.as_str();
            let (alias, what) = match &self.query.link {
                LinkManifest::Child { .. } => (PARENT_ALIAS, "the key of the parent query".to_string()),
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
            match self.parameters(statement, override_.map(|o| &*o.sql), &classes) {
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
    /// selected by as `$1`. On MySQL and SQLite, the keys are written `IN (:keys)` instead.
    /// Returns whether the query takes the keys.
    fn parameters(&self, statement: &Inspected, sql: Option<&str>, classes: &KeyClasses) -> Result<bool, Vec<String>> {
        let root = matches!(self.query.link, LinkManifest::Root);
        let (keys, of) = match &self.query.link {
            LinkManifest::Root => (classes.get(&self.query.key_alias).copied(), "root keys"),
            LinkManifest::Child { .. } | LinkManifest::Variant { .. } => (self.link_class, "parent keys"),
            LinkManifest::ToOne { .. } => (self.link_class, "referenced keys"),
        };
        match &statement.params {
            InspectedParams::Types(types) => match types.as_slice() {
                [] if root => Ok(false),
                [] => Err(vec![format!("the query needs to take the array of {of} as $1, e.g. `WHERE fk = ANY($1)`")]),
                [(None, name)] => {
                    Err(vec![format!("$1 has type {name}; it is the array of {of}, use it as `= ANY($1)`")])
                }
                [(Some(class), _)] if keys.is_some_and(|keys| keys != *class) => Err(vec![format!(
                    "$1 is an array of {class} keys, but the {of} are {} keys",
                    keys.expect("checked above")
                )]),
                [_] => Ok(true),
                more => {
                    let expected =
                        if root { "no parameter or the array of root keys" } else { "only the array of keys" };
                    Err(vec![format!("the query takes {} parameters, expected {expected} as $1", more.len())])
                }
            },
            InspectedParams::Count(count) => {
                let takes_keys = sql.is_some_and(|sql| sql.contains(KEYS_TOKEN));
                let expected = usize::from(takes_keys);
                if !takes_keys && !root {
                    return Err(vec![format!("the query needs to take the {of} as `IN ({KEYS_TOKEN})`")]);
                }
                if *count != expected {
                    let other = count - expected.min(*count);
                    return Err(vec![format!(
                        "the query takes {other} other parameter(s); only the keys can be bound, as `IN ({KEYS_TOKEN})`"
                    )]);
                }
                Ok(takes_keys)
            }
        }
    }
}
