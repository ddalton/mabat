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

use mabat_core::sql::{self, Layout, RootOptions};
use mabat_core::{INDEX_ALIAS, KEY_ALIAS, Link, MAP_KEY_ALIAS, PARENT_ALIAS, PlanError, QueryPlan, REF_ALIAS_PREFIX};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;

use crate::Error;
use crate::check::{self, ViewEntry};
use crate::describe::{ColumnType, DescribeFn, Description, short_type_name};
use crate::overrides::OverrideFile;
use crate::registry::{self, Overrides};
use crate::report::Report;

/// The version of the manifest format.
pub const FORMAT: u32 = 1;

/// The views of an application and the queries that fill them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
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
    Child,
    ToOne { ref_alias: String },
    Variant { tag_alias: String, tag_value: String },
}

/// A column a query selects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnManifest {
    pub alias: String,
    pub role: Role,
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

impl TypeManifest {
    /// Whether a column of the type can be decoded. A domain is decoded as its base type.
    pub(crate) fn accepts(&self, ty: &sqlx::postgres::PgTypeInfo) -> bool {
        crate::describe::accepts(&self.accepts, ty)
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
    pub async fn check(&self, conn: &mut PgConnection, overrides: &[PathBuf]) -> Result<Report, Error> {
        let mut report = Report::default();
        let files = registry::read_override_files(overrides, &[], &mut report);
        check::check(conn, self, files, &mut report).await.map_err(Error::Check)?;
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
        self.view(view).map(|view| scaffold(view, format))
    }
}

/// The format of an override file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldFormat {
    Toml,
    Sql,
}

/// Build the manifest of a view and its plan.
pub(crate) fn build(view: &ViewEntry) -> Result<(ViewManifest, QueryPlan), PlanError> {
    let plan = QueryPlan::build(view.shape)?;
    let mut queries = Vec::new();
    add_query(&mut queries, &plan, view.describe, None, None);
    Ok((ViewManifest { name: view.shape.name.to_string(), queries }, plan))
}

fn add_query(
    queries: &mut Vec<QueryManifest>,
    plan: &QueryPlan,
    describe: DescribeFn,
    map_key: Option<&ColumnType>,
    parent: Option<usize>,
) {
    let mut description = Description::default();
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
                optional: column.is_some_and(|c| c.optional),
                r#type: column.map(TypeManifest::from),
            }
        })
        .collect();
    let link = match &plan.link {
        Link::Root => LinkManifest::Root,
        Link::Child { .. } => LinkManifest::Child,
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
        key_alias: plan.key_alias.clone(),
        sql: sql::select_with(plan, &RootOptions::default(), Layout::Multiline),
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
        add_query(queries, child_plan, describe, map_key, Some(index));
    }
}

impl LinkManifest {
    /// How the query is linked, for `explain`.
    fn describe(&self) -> String {
        match self {
            LinkManifest::Root => String::new(),
            LinkManifest::Child => " (to-many)".to_string(),
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

fn scaffold(view: &ViewManifest, format: ScaffoldFormat) -> String {
    let name = &view.name;
    let intro = [
        format!("Overrides for {name}."),
        String::new(),
        "Each query is addressed by its name and replaced by its SQL. The rows are decoded".to_string(),
        "by column alias, so keep the aliases; joins, ordering, hints and the tables themselves".to_string(),
        format!("can change. Every query is checked against {name} at startup and by `mabat check`."),
        String::new(),
        "$root takes no parameter, and is then filtered, ordered and paged as a subquery, or".to_string(),
        "the array of root keys as $1. The other queries take the array of the keys they are".to_string(),
        "selected by as $1. Mark a query shadowed to also run the generated query and compare.".to_string(),
    ];
    let mut out = String::new();
    match format {
        ScaffoldFormat::Toml => {
            for line in &intro {
                let _ = writeln!(out, "#{}{line}", if line.is_empty() { "" } else { " " });
            }
            for query in &view.queries {
                let _ = write!(out, "\n[query.{}]\nsql = {}\n", toml_key(&query.name), toml_multiline(&query.sql));
            }
        }
        ScaffoldFormat::Sql => {
            for line in &intro {
                let _ = writeln!(out, "--{}{line}", if line.is_empty() { "" } else { " " });
            }
            for query in &view.queries {
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
