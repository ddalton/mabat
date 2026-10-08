//! Checking the queries of views against the database.
//!
//! Every query of every registered view is prepared on the database, without running it,
//! and the columns and parameters of the prepared statement are compared with the view:
//! the generated query, which catches a schema that no longer matches the view, and the
//! override, which must select the same aliases with compatible types.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use refract_core::sql::{self, RootOptions};
use refract_core::{KEY_ALIAS, Link, PARENT_ALIAS, QueryPlan, ViewShape};
use sqlx::postgres::{PgStatement, PgTypeInfo};
use sqlx::{AssertSqlSafe, Column, Connection, Either, Executor, PgConnection, SqlSafeStr, Statement, TypeInfo};

use crate::describe::{ColumnType, Description, short_type_name};
use crate::key::KeyClass;
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

/// Check the registered views and their override files. Problems are added to the report.
///
/// Fails only if the connection fails.
pub(crate) async fn check(
    conn: &mut PgConnection,
    views: &[ViewEntry],
    files: Vec<OverrideFile>,
    report: &mut Report,
) -> Result<Vec<Checked>, sqlx::Error> {
    let mut files: HashMap<String, OverrideFile> = files.into_iter().map(|f| (f.view.clone(), f)).collect();
    let mut checked = Vec::new();

    for view in views {
        let file = files.remove(view.shape.name);
        let plan = match QueryPlan::build(view.shape) {
            Ok(plan) => plan,
            Err(e) => {
                report.push(Diagnostic {
                    severity: Severity::Error,
                    code: "R0301",
                    view: view.shape.name.to_string(),
                    query: String::new(),
                    origin: None,
                    summary: format!("{} cannot be planned", view.shape.name),
                    notes: vec![e.to_string()],
                });
                continue;
            }
        };

        let mut by_query: HashMap<&str, &QueryOverride> = HashMap::new();
        if let Some(file) = &file {
            let mut names = Vec::new();
            plan.walk(&mut |p| names.push(p.query_name().to_string()));
            for query in &file.queries {
                if names.contains(&query.query) {
                    by_query.insert(&query.query, query);
                } else {
                    let mut notes = vec![format!("the queries of {} are: {}", view.shape.name, names.join(", "))];
                    if let Some(name) = suggest(&query.query, names.iter().map(String::as_str)) {
                        notes.insert(0, format!("did you mean \"{name}\"?"));
                    }
                    report.push(Diagnostic {
                        severity: Severity::Error,
                        code: "R0101",
                        view: view.shape.name.to_string(),
                        query: query.query.clone(),
                        origin: Some(query.origin.to_string()),
                        summary: format!("\"{}\" is not a query of {}", query.query, view.shape.name),
                        notes,
                    });
                }
            }
        }

        let mut overrides = Overrides::new();
        let context = Context { view: view.shape.name, by_query: &by_query };
        check_query(conn, &context, &plan, view.describe, None, &mut overrides, report).await?;
        checked.push(Checked { shape: view.shape, plan: Arc::new(plan), overrides });
    }

    let registered: Vec<&str> = views.iter().map(|v| v.shape.name).collect();
    for file in files.into_values() {
        let mut notes = Vec::new();
        if let Some(name) = suggest(&file.view, registered.iter().copied()) {
            notes.push(format!("did you mean {name}?"));
        }
        notes.push("override files are named after a registered view, e.g. TaskView.toml".to_string());
        report.push(Diagnostic {
            severity: Severity::Error,
            code: "R0101",
            view: file.view.clone(),
            query: String::new(),
            origin: Some(file.file.clone()),
            summary: format!("{} is not a registered view", file.view),
            notes,
        });
    }

    Ok(checked)
}

struct Context<'a> {
    view: &'static str,
    by_query: &'a HashMap<&'a str, &'a QueryOverride>,
}

/// The key class the query is linked to its parent query with: the class of the parent's
/// key for a to-many query, the class of the parent's reference column for a to-one query.
/// `None` for the root query, or when the parent query could not be checked.
type LinkClass = Option<KeyClass>;

/// The key classes of the columns of a checked query, by alias.
type KeyClasses = HashMap<String, KeyClass>;

fn check_query<'a>(
    conn: &'a mut PgConnection,
    context: &'a Context<'a>,
    plan: &'a QueryPlan,
    describe: fn(&mut Description),
    link_class: LinkClass,
    overrides: &'a mut Overrides,
    report: &'a mut Report,
) -> std::pin::Pin<Box<dyn Future<Output = Result<(), sqlx::Error>> + Send + 'a>> {
    Box::pin(async move {
        let mut description = Description::default();
        describe(&mut description);
        let query = Query { context, plan, description: &description, link_class };

        let override_ = context.by_query.get(plan.query_name()).copied();
        let mut classes = None;

        // The override, if any
        let mut active = None;
        if let Some(override_) = override_ {
            match inspect(conn, &override_.sql).await? {
                Err(message) => {
                    report.push(query.diagnostic(Some(override_), "R0103", "does not prepare", vec![message]))
                }
                Ok(statement) => {
                    let result = query.compare(&statement, Some(override_), report);
                    if let Some((found, keys_param)) = result {
                        classes = Some(found);
                        active = Some((override_, keys_param));
                    }
                }
            }
        }

        // The generated query is checked too: it runs when there is no valid override, and
        // in shadow mode. A problem with it is a warning when a valid override replaces it.
        let generated = sql::select(plan, &RootOptions::default());
        let mut generated_report = Report::default();
        match inspect(conn, &generated).await? {
            Err(message) => generated_report.push(query.diagnostic(None, "R0103", "does not prepare", vec![message])),
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
                plan.query_name().to_string(),
                ActiveOverride {
                    sql: override_.sql.clone(),
                    keys_param,
                    shadow: override_.shadow && generated_ok,
                    origin: override_.origin.clone(),
                    stats: Arc::new(ShadowStats::default()),
                },
            );
        }

        for child in &plan.children {
            let child_class = classes.as_ref().and_then(|classes| match &child.plan.link {
                Link::Child { .. } => classes.get(&plan.key_alias).copied(),
                Link::ToOne { ref_alias } => classes.get(ref_alias).copied(),
                Link::Root => None,
            });
            let describe = description
                .child(child.field_index)
                .expect("the description and the shape of a view have the same fields");
            check_query(conn, context, &child.plan, describe, child_class, overrides, report).await?;
        }
        Ok(())
    })
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
    context: &'a Context<'a>,
    plan: &'a QueryPlan,
    description: &'a Description,
    link_class: LinkClass,
}

/// A column a query needs to select.
struct Expected<'a> {
    alias: &'a str,
    /// The Rust type of a field column, `None` for a system column.
    column: Option<&'a ColumnType>,
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
        let view = self.context.view;
        let name = self.plan.query_name();
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
        self.plan
            .columns
            .iter()
            .map(|c| {
                let alias = c.alias.as_str();
                if alias.starts_with('$') {
                    Expected { alias, column: None, key: true }
                } else {
                    Expected { alias, column: self.description.column_type(alias), key: alias == self.plan.key_alias }
                }
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
                let mut note = format!("column {n} \"{name}\" is not a path of {} in this query", self.plan.shape.name);
                if name == KEY_ALIAS {
                    note.push_str(&format!(" (the key is selected as \"{}\")", self.plan.key_alias));
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
                    column.sql_type,
                    short_type_name(column.rust_type)
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
            let (alias, what) = match &self.plan.link {
                Link::Child { .. } => (PARENT_ALIAS, "the key of the parent query".to_string()),
                Link::ToOne { ref_alias } => (self.plan.key_alias.as_str(), format!("the parent's \"{ref_alias}\"")),
                Link::Root => unreachable!("the root query has no parent"),
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
                Some(column) if column.optional => missing_optional.push(expected.alias),
                _ if expected.alias == PARENT_ALIAS => errors
                    .push(format!("column \"{PARENT_ALIAS}\" is not selected; it attaches the rows to their parent")),
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
            report.push(self.diagnostic(override_, "R0102", problem, errors));
        }

        // Only overrides choose their parameters
        let mut keys_param = !matches!(self.plan.link, Link::Root);
        if override_.is_some() {
            match self.parameters(statement, &classes) {
                Ok(takes_keys) => keys_param = takes_keys,
                Err(notes) => {
                    report.push(self.diagnostic(override_, "R0104", "has the wrong parameters", notes));
                    return None;
                }
            }
            if !missing_optional.is_empty() && !has_errors {
                let mut diagnostic = self.diagnostic(
                    override_,
                    "R0105",
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
        let root = matches!(self.plan.link, Link::Root);
        let (keys, of) = match &self.plan.link {
            Link::Root => (classes.get(&self.plan.key_alias).copied(), "root keys"),
            Link::Child { .. } => (self.link_class, "parent keys"),
            Link::ToOne { .. } => (self.link_class, "referenced keys"),
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
