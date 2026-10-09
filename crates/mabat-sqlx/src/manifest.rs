//! The manifest of views: see [`mabat_check::manifest`]. The application writes it with
//! [`Builder::manifest`](crate::Builder::manifest), and [`check`] checks it against a database.

use std::path::PathBuf;

pub use mabat_check::manifest::*;
use mabat_core::sql::{self, Layout, Render, RootOptions};
use mabat_core::{INDEX_ALIAS, KEY_ALIAS, Link, MAP_KEY_ALIAS, PARENT_ALIAS, PlanError, QueryPlan, REF_ALIAS_PREFIX};
use sqlx::Database;

use crate::Error;
use crate::backend::{Backend, Conn};
use crate::check::{self, ViewEntry};
use crate::describe::{ColumnType, DescribeFn, Description, short_type_name};
use crate::registry::Overrides;
use crate::report::Report;

/// Check the override files of the directories against the views and the database, as
/// the application does at startup.
pub async fn check<C: Conn>(manifest: &Manifest, conn: &mut C, overrides: &[PathBuf]) -> Result<Report, Error> {
    if manifest.backend != C::Backend::NAME {
        return Err(Error::ManifestBackend { manifest: manifest.backend.clone(), connection: C::Backend::NAME });
    }
    let mut report = Report::default();
    let files = crate::overrides::read_override_files(overrides, &[], &mut report);
    let mut conn = conn.source().single().await?;
    check::check::<C::Backend>(&mut conn, manifest, files, &mut report).await.map_err(Error::Check)?;
    Ok(report)
}

/// The type of a column, as the manifest records it.
fn type_manifest(column: &ColumnType) -> TypeManifest {
    TypeManifest {
        rust: short_type_name(column.rust_type),
        sql: column.sql_type.clone(),
        accepts: column.accepts.clone(),
    }
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
                r#type: column.map(type_manifest),
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

/// The overrides of a view that passed the checks, by view name.
pub(crate) type CheckedOverrides = std::collections::HashMap<String, Overrides>;
