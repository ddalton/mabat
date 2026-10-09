//! The manifest of views: everything needed to check override SQL against the views, as
//! data, so that it can be checked without the application.
//!
//! The application writes the manifest of its views, typically from a test, and commits
//! it next to the override files:
//!
//! ```ignore
//! #[test]
//! fn views_manifest_is_up_to_date() {
//!     let manifest = Mabat::builder().register::<TaskView>().manifest().unwrap();
//!     assert!(!manifest.write("mabat/views.json").unwrap(), "mabat/views.json was out of date");
//! }
//! ```
//!
//! A DBA then checks overrides against a database with the `mabat` command line tool,
//! with no Rust toolchain:
//!
//! ```text
//! mabat check --manifest mabat/views.json --overrides mabat/overrides --database-url postgres://...
//! ```

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use mabat_core::sql::{self, Layout, Render, RootOptions};
use mabat_core::{INDEX_ALIAS, KEY_ALIAS, Link, MAP_KEY_ALIAS, PARENT_ALIAS, PlanError, QueryPlan, REF_ALIAS_PREFIX};
use serde::{Deserialize, Serialize};
use sqlx::Database;

use crate::Error;
use crate::backend::{Backend, Conn};
use crate::check::{self, ViewEntry};
use crate::describe::{ColumnType, DescribeFn, Description, short_type_name};
use crate::overrides::OverrideFile;
use crate::registry::{self, Overrides};
use crate::report::Report;

/// The version of the manifest format.
pub const FORMAT: u32 = 2;

/// The views of an application and the queries that fill them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// The database the views are described for: `PostgreSQL`, `MySQL` or `SQLite`, as
    /// `sqlx::Database::NAME`. Column types and the generated SQL depend on it.
    pub backend: String,
    pub views: Vec<ViewManifest>,
}

/// A registered view and its queries, parents before children.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewManifest {
    pub name: String,
    pub queries: Vec<QueryManifest>,
}

/// A query of a view: what an override of it needs to select.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryManifest {
    /// The name overrides address the query by: `$root` or the path of the field it fills.
    pub name: String,
    /// The view the rows are decoded as.
    pub view: String,
    /// The index of the parent query in [`ViewManifest::queries`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    pub link: LinkManifest,
    /// The table the rows come from, and its key column (format 2).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub table: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key_column: String,
    /// The database generates the key: `#[view(generated)]`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub generated: bool,
    /// The alias of the key column.
    pub key_alias: String,
    /// The generated SQL.
    pub sql: String,
    pub columns: Vec<ColumnManifest>,
}

/// How a query is linked to its parent query, see [`Link`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LinkManifest {
    Root,
    /// Rows whose `fk` column is one of the parent keys, or with `through`, rows linked to
    /// the parent rows by a link table whose `fk` column is one of them (format 2).
    Child {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        fk: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        through: Option<ThroughManifest>,
    },
    ToOne {
        ref_alias: String,
    },
    Variant {
        tag_alias: String,
        tag_value: String,
    },
}

/// The link table of a many-to-many collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThroughManifest {
    pub table: String,
    /// The column of the link table that references the element's key.
    pub target: String,
}

/// A column a query selects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnManifest {
    pub alias: String,
    pub role: Role,
    /// The column it is selected from: of the query's table, or of the link table with
    /// `link_table` (format 2).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub column: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub link_table: bool,
    /// `true` for an `Option` field, which an override may leave out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub optional: bool,
    /// The Rust type of a field or map key, and the PostgreSQL types it can be decoded from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<TypeManifest>,
}

/// What a column is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A field of the view; the key column too, when a field holds it.
    Field,
    /// `$key`: the key, when no field holds it.
    Key,
    /// `$parent`: the key of the parent row.
    Parent,
    /// `$ref.<field>`: the foreign key of a to-one field.
    Reference,
    /// `$index`: the position in a list.
    Index,
    /// `$map_key`: the key in a map.
    MapKey,
}

/// The Rust type of a column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeManifest {
    /// The Rust type, e.g. `Option<String>`.
    pub rust: String,
    /// The PostgreSQL type it is usually stored as, e.g. `TEXT`.
    pub sql: String,
    /// The names of the PostgreSQL types it can be decoded from, e.g. `TEXT`, `VARCHAR`.
    pub accepts: Vec<String>,
}

impl From<&ColumnType> for TypeManifest {
    fn from(column: &ColumnType) -> Self {
        TypeManifest {
            rust: short_type_name(column.rust_type),
            sql: column.sql_type.clone(),
            accepts: column.accepts.clone(),
        }
    }
}

impl Manifest {
    /// The manifest as pretty JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a manifest serializes") + "\n"
    }

    /// Read a manifest from JSON.
    pub fn from_json(json: &str) -> Result<Manifest, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Write the manifest to a file, unless the file already holds it. Returns `true` if
    /// the file was written, so a test can fail when the committed manifest is out of date.
    pub fn write(&self, path: impl AsRef<Path>) -> std::io::Result<bool> {
        let path = path.as_ref();
        let json = self.to_json();
        if std::fs::read_to_string(path).is_ok_and(|current| current == json) {
            return Ok(false);
        }
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, json)?;
        Ok(true)
    }

    /// The view with the given name.
    pub fn view(&self, name: &str) -> Option<&ViewManifest> {
        self.views.iter().find(|v| v.name == name)
    }

    /// Check the override files of the directories against the views and the database, as
    /// the application does at startup.
    pub async fn check<C: Conn>(&self, conn: &mut C, overrides: &[PathBuf]) -> Result<Report, Error> {
        if self.backend != C::Backend::NAME {
            return Err(Error::ManifestBackend { manifest: self.backend.clone(), connection: C::Backend::NAME });
        }
        let mut report = Report::default();
        let files = registry::read_override_files(overrides, &[], &mut report);
        let mut conn = conn.source().single().await?;
        check::check::<C::Backend>(&mut conn, self, files, &mut report).await.map_err(Error::Check)?;
        Ok(report)
    }

    /// The queries of the views, with the SQL that runs for each: the override in the
    /// directories, or the generated SQL. Override files that cannot be read are reported
    /// in the report, without checking anything against a database.
    pub fn explain(&self, view: Option<&str>, overrides: &[PathBuf]) -> (String, Report) {
        let mut report = Report::default();
        let files = registry::read_override_files(overrides, &[], &mut report);
        let mut out = String::new();
        for manifest in self.views.iter().filter(|v| view.is_none_or(|name| v.name == name)) {
            let file = files.iter().find(|f| f.view == manifest.name);
            explain_view(&mut out, manifest, file);
        }
        (out, report)
    }

    /// An override file for a view, with the generated SQL of every query, as a starting
    /// point for tuning.
    pub fn scaffold(&self, view: &str, format: ScaffoldFormat) -> Option<String> {
        self.scaffold_queries(view, format, &[]).ok()
    }

    /// An override file for a view with the generated SQL of the named queries only, or of
    /// every query if `queries` is empty: a file overrides each query it holds, so a DBA
    /// tuning one query scaffolds that one.
    pub fn scaffold_queries(&self, view: &str, format: ScaffoldFormat, queries: &[&str]) -> Result<String, String> {
        let manifest = self.view(view).ok_or_else(|| format!("the manifest has no view {view}"))?;
        let chosen = chosen_queries(manifest, queries)?;
        let mut out = intro(manifest, format, self.backend == "PostgreSQL");
        out.push_str(&sections(&chosen, format));
        Ok(out)
    }

    /// Write the generated SQL of the named queries of a view (every query if `queries` is
    /// empty) into its override file in `dir`: a new file, as [`Manifest::scaffold_queries`]
    /// writes it, or the queries added to the end of the view's file. A query the file
    /// already overrides is never replaced. Returns the path of the file.
    pub fn scaffold_into(
        &self,
        dir: &Path,
        view: &str,
        format: ScaffoldFormat,
        queries: &[&str],
    ) -> Result<PathBuf, String> {
        let manifest = self.view(view).ok_or_else(|| format!("the manifest has no view {view}"))?;
        let chosen = chosen_queries(manifest, queries)?;
        let (extension, other) = match format {
            ScaffoldFormat::Toml => ("toml", "sql"),
            ScaffoldFormat::Sql => ("sql", "toml"),
        };
        let path = dir.join(format!("{view}.{extension}"));
        let other = dir.join(format!("{view}.{other}"));
        if other.exists() {
            return Err(format!(
                "{} holds the overrides of {view}, and a view has one override file: use --format {}",
                other.display(),
                if extension == "toml" { "sql" } else { "toml" }
            ));
        }
        let shown = path.display().to_string();
        let content = match std::fs::read_to_string(&path) {
            Ok(existing) => {
                let file = match format {
                    ScaffoldFormat::Toml => crate::overrides::parse(view, &shown, &existing),
                    ScaffoldFormat::Sql => crate::overrides::parse_sql(view, &shown, &existing),
                }
                .map_err(|(origin, message)| format!("{origin}: {message}"))?;
                let overridden: Vec<&str> = chosen
                    .iter()
                    .map(|q| q.name.as_str())
                    .filter(|name| file.queries.iter().any(|o| o.query == *name))
                    .collect();
                if !overridden.is_empty() {
                    return Err(format!("{shown} already overrides {}: edit it there", overridden.join(", ")));
                }
                let mut content = existing;
                if !content.is_empty() && !content.ends_with('\n') {
                    content.push('\n');
                }
                content.push_str(&sections(&chosen, format));
                content
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut content = intro(manifest, format, self.backend == "PostgreSQL");
                content.push_str(&sections(&chosen, format));
                content
            }
            Err(e) => return Err(format!("cannot read {shown}: {e}")),
        };
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        std::fs::write(&path, content).map_err(|e| format!("cannot write {shown}: {e}"))?;
        Ok(path)
    }
}

/// The format of an override file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldFormat {
    Toml,
    Sql,
}

/// Build the manifest of a view and its plan.
pub(crate) fn build<B: Backend>(view: &ViewEntry<B>) -> Result<(ViewManifest, QueryPlan), PlanError> {
    let plan = QueryPlan::build(view.shape)?;
    let mut queries = Vec::new();
    add_query::<B>(&mut queries, &plan, view.describe, None, None);
    Ok((ViewManifest { name: view.shape.name.to_string(), queries }, plan))
}

fn add_query<B: Backend>(
    queries: &mut Vec<QueryManifest>,
    plan: &QueryPlan,
    describe: DescribeFn<B>,
    map_key: Option<&ColumnType>,
    parent: Option<usize>,
) {
    let mut description = Description::<B>::default();
    describe(&mut description);

    let columns = plan
        .columns
        .iter()
        .map(|c| {
            let alias = c.alias.clone();
            let (role, column) = match alias.as_str() {
                KEY_ALIAS => (Role::Key, None),
                PARENT_ALIAS => (Role::Parent, None),
                INDEX_ALIAS => (Role::Index, None),
                MAP_KEY_ALIAS => (Role::MapKey, map_key),
                a if a.starts_with(REF_ALIAS_PREFIX) => (Role::Reference, None),
                a => (Role::Field, description.column_type(a)),
            };
            ColumnManifest {
                alias,
                role,
                column: c.column.clone(),
                link_table: c.from_link,
                optional: column.is_some_and(|c| c.optional),
                r#type: column.map(TypeManifest::from),
            }
        })
        .collect();
    let link = match &plan.link {
        Link::Root => LinkManifest::Root,
        Link::Child { fk, through } => LinkManifest::Child {
            fk: fk.to_string(),
            through: through.map(|t| ThroughManifest { table: t.table.to_string(), target: t.target.to_string() }),
        },
        Link::ToOne { ref_alias } => LinkManifest::ToOne { ref_alias: ref_alias.clone() },
        Link::Variant { tag_alias, tag_value } => {
            LinkManifest::Variant { tag_alias: tag_alias.clone(), tag_value: tag_value.to_string() }
        }
    };
    queries.push(QueryManifest {
        name: plan.query_name().to_string(),
        view: plan.shape.name.to_string(),
        parent,
        link,
        table: plan.shape.table.to_string(),
        key_column: plan.shape.key_column.to_string(),
        generated: description.generated_key,
        key_alias: plan.key_alias.clone(),
        sql: sql::render(
            plan,
            &RootOptions::default(),
            &Render::new(B::DIALECT).with_layout(Layout::Multiline).with_keys_token(),
        ),
        columns,
    });
    let index = queries.len() - 1;

    // Queries that are repeated for the levels of a recursive collection appear once
    for child in &plan.children {
        let Some(child_plan) = child.plan() else { continue };
        let (describe, map_key) = match child.variant {
            None => description.child(child.field_index),
            Some(variant) => description.variant(child.field_index, variant).map(|describe| (describe, None)),
        }
        .expect("the description and the shape of a view have the same fields");
        add_query::<B>(queries, child_plan, describe, map_key, Some(index));
    }
}

impl LinkManifest {
    /// How the query is linked, for `explain`.
    fn describe(&self) -> String {
        match self {
            LinkManifest::Root => String::new(),
            LinkManifest::Child { .. } => " (to-many)".to_string(),
            LinkManifest::ToOne { ref_alias } => format!(" (to-one by {ref_alias})"),
            LinkManifest::Variant { tag_alias, tag_value } => format!(" (variant where {tag_alias} = '{tag_value}')"),
        }
    }
}

fn explain_view(out: &mut String, view: &ViewManifest, file: Option<&OverrideFile>) {
    let _ = writeln!(out, "{}", view.name);
    for query in &view.queries {
        let mut depth = 1;
        let mut parent = query.parent;
        while let Some(p) = parent {
            depth += 1;
            parent = view.queries[p].parent;
        }
        let indent = "  ".repeat(depth);
        let _ = writeln!(out, "{indent}{}: {}{}", query.name, query.view, query.link.describe());
        let (label, sql) = match file.and_then(|f| f.queries.iter().find(|q| q.query == query.name)) {
            Some(override_) => {
                let shadow = if override_.shadow { ", shadowed" } else { "" };
                (format!("override ({}{shadow})", override_.origin), override_.sql.to_string())
            }
            None => ("generated".to_string(), query.sql.clone()),
        };
        let _ = writeln!(out, "{indent}  {label}:");
        for line in sql.trim().lines() {
            let _ = writeln!(out, "{indent}    {}", line.trim_end());
        }
    }
}

/// The queries of a view with the given names, in the order of the plan; all of them if
/// `names` is empty.
fn chosen_queries<'m>(view: &'m ViewManifest, names: &[&str]) -> Result<Vec<&'m QueryManifest>, String> {
    let unknown: Vec<&str> = names.iter().copied().filter(|n| !view.queries.iter().any(|q| q.name == *n)).collect();
    if !unknown.is_empty() {
        let known: Vec<&str> = view.queries.iter().map(|q| q.name.as_str()).collect();
        return Err(format!("{} has no query {}; its queries are {}", view.name, unknown.join(", "), known.join(", ")));
    }
    Ok(view.queries.iter().filter(|q| names.is_empty() || names.contains(&q.name.as_str())).collect())
}

/// The comment that starts a new override file.
fn intro(view: &ViewManifest, format: ScaffoldFormat, arrays: bool) -> String {
    let name = &view.name;
    let keys = if arrays { "the array of root keys as $1" } else { "the root keys as IN (:keys)" };
    let child_keys = if arrays {
        "the array of the keys they are\nselected by as $1"
    } else {
        "the keys they are selected by as\nIN (:keys)"
    };
    let intro = [
        format!("Overrides for {name}."),
        String::new(),
        "Each query is addressed by its name and replaced by its SQL. The rows are decoded".to_string(),
        "by column alias, so keep the aliases; joins, ordering, hints and the tables themselves".to_string(),
        format!("can change. Every query is checked against {name} at startup and by `mabat check`."),
        String::new(),
        format!("$root takes no keys, and is then filtered, ordered and paged as a subquery, or {keys}."),
        format!(
            "The other queries take {child_keys}. Mark a query shadowed to also run the generated query and compare."
        ),
    ];
    let intro: Vec<String> = intro
        .iter()
        .flat_map(|line| {
            line.lines().map(str::to_string).collect::<Vec<_>>().into_iter().chain(line.is_empty().then(String::new))
        })
        .collect();
    let comment = match format {
        ScaffoldFormat::Toml => "#",
        ScaffoldFormat::Sql => "--",
    };
    let mut out = String::new();
    for line in &intro {
        let _ = writeln!(out, "{comment}{}{line}", if line.is_empty() { "" } else { " " });
    }
    out
}

/// The generated SQL of the queries, as sections of an override file.
fn sections(queries: &[&QueryManifest], format: ScaffoldFormat) -> String {
    let mut out = String::new();
    for query in queries {
        match format {
            ScaffoldFormat::Toml => {
                let _ = write!(out, "\n[query.{}]\nsql = {}\n", toml_key(&query.name), toml_multiline(&query.sql));
            }
            ScaffoldFormat::Sql => {
                let _ = write!(out, "\n-- mabat: query {}\n{};\n", query.name, query.sql);
            }
        }
    }
    out
}

fn toml_key(key: &str) -> String {
    if key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        key.to_string()
    } else {
        toml_string(key)
    }
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

fn toml_multiline(sql: &str) -> String {
    if sql.contains("'''") { toml_string(sql) } else { format!("'''\n{sql}\n'''") }
}

/// The overrides of a view that passed the checks, by view name.
pub(crate) type CheckedOverrides = std::collections::HashMap<String, Overrides>;
