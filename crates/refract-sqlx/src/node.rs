//! Loaded query results and the helpers used by generated decoders.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use refract_core::sql::{self, RootOptions};
use refract_core::{KEY_ALIAS, Link, PARENT_ALIAS, QueryPlan, TAG_ALIAS};
use sqlx::postgres::PgRow;
use sqlx::{AssertSqlSafe, Column, Decode, PgConnection, Postgres, Row, Type, ValueRef};

use crate::key::{Key, KeyArray, KeyColumn};
use crate::registry::{ActiveOverride, Overrides};
use crate::{Error, View};

/// The rows of one query of a plan, with the rows of its child queries.
#[derive(Debug)]
pub struct Node {
    view: &'static str,
    path: String,
    rows: Vec<PgRow>,
    /// Alias of the key column, see [`QueryPlan::key_alias`].
    key_alias: String,
    /// The key columns of the rows by alias, resolved from the first row. The key column
    /// is stored as [`KEY_ALIAS`].
    key_columns: Vec<(String, KeyColumn)>,
    /// The child queries: child and to-one fields, and variant tables.
    children: Vec<ChildEntry>,
    /// The enums stored in the rows, for strict decoding.
    sums: Vec<SumColumns>,
}

#[derive(Debug)]
struct ChildEntry {
    field_index: usize,
    variant: Option<&'static str>,
    child: ChildNode,
}

/// The columns that must be NULL for each variant of an enum, see
/// [`refract_core::VariantPlan::exclusive`], resolved from the first row. Columns that a
/// query does not select are left out.
#[derive(Debug)]
struct SumColumns {
    alias_prefix: String,
    /// `(variant, [(ordinal, alias)])`
    variants: Vec<(&'static str, Vec<(usize, String)>)>,
}

#[derive(Debug)]
struct ChildNode {
    node: Node,
    /// Rows of the child query by the key they are attached with, in row order.
    by_key: HashMap<Key, Vec<usize>>,
}

impl Node {
    pub(crate) fn rows(&self) -> &[PgRow] {
        &self.rows
    }

    fn child(&self, field_index: usize) -> &ChildNode {
        self.children
            .iter()
            .find(|c| c.field_index == field_index && c.variant.is_none())
            .map(|c| &c.child)
            .expect("plan and decoder disagree on the fields of a view")
    }

    fn variant_child(&self, field_index: usize, variant: &str) -> &ChildNode {
        self.children
            .iter()
            .find(|c| c.field_index == field_index && c.variant == Some(variant))
            .map(|c| &c.child)
            .expect("plan and decoder disagree on the variants of an enum")
    }

    fn path_of(&self, alias: &str) -> String {
        join(&self.path, alias)
    }

    fn new(view: &'static str, path: String, rows: Vec<PgRow>, plan: &QueryPlan) -> Result<Node, Error> {
        let mut key_columns = Vec::new();
        let mut sums = Vec::new();
        if let Some(row) = rows.first() {
            for sum in &plan.sums {
                let variants = sum
                    .variants
                    .iter()
                    .map(|variant| {
                        let columns = variant
                            .exclusive
                            .iter()
                            .filter_map(|alias| {
                                row.try_column(alias.as_str()).ok().map(|c| (c.ordinal(), alias.clone()))
                            })
                            .collect();
                        (variant.name, columns)
                    })
                    .collect();
                sums.push(SumColumns { alias_prefix: sum.alias_prefix.clone(), variants });
            }
            // (name in key_columns, alias in the result)
            let mut aliases = vec![(KEY_ALIAS.to_string(), plan.key_alias.clone())];
            if let Link::Child { .. } = plan.link {
                aliases.push((PARENT_ALIAS.to_string(), PARENT_ALIAS.to_string()));
            }
            for child in &plan.children {
                if let Link::ToOne { ref_alias } = &child.plan.link {
                    aliases.push((ref_alias.clone(), ref_alias.clone()));
                }
            }
            for (name, alias) in aliases {
                let column = KeyColumn::resolve(row, &alias).map_err(|source| Error::Decode {
                    view,
                    path: join(&path, &alias),
                    source,
                })?;
                key_columns.push((name, column));
            }
        }
        Ok(Node { view, path, rows, key_alias: plan.key_alias.clone(), key_columns, children: Vec::new(), sums })
    }

    fn key(&self, row: &PgRow, alias: &str) -> Result<Option<Key>, Error> {
        let column = match self.key_columns.iter().find(|(a, _)| a == alias) {
            Some((_, column)) => *column,
            None => KeyColumn::resolve(row, if alias == KEY_ALIAS { &self.key_alias } else { alias })
                .map_err(|source| Error::Decode { view: self.view, path: self.path_of(alias), source })?,
        };
        column.read(row).map_err(|source| Error::Decode { view: self.view, path: self.path_of(alias), source })
    }
}

fn join(path: &str, alias: &str) -> String {
    if path.is_empty() { alias.to_string() } else { format!("{path}.{alias}") }
}

/// Decode the column with the given alias.
#[doc(hidden)]
pub fn column<T>(row: &PgRow, node: &Node, alias: &str) -> Result<T, Error>
where
    T: for<'r> Decode<'r, Postgres> + Type<Postgres>,
{
    row.try_get::<T, _>(alias).map_err(|source| Error::Decode { view: node.view, path: node.path_of(alias), source })
}

/// Decode the elements of a to-many collection of the row, in the order of the child query.
#[doc(hidden)]
pub fn children<C: View>(row: &PgRow, node: &Node, field_index: usize) -> Result<Vec<C>, Error> {
    let child = node.child(field_index);
    let Some(key) = node.key(row, KEY_ALIAS)? else {
        return Ok(Vec::new());
    };
    match child.by_key.get(&key) {
        Some(indices) => indices.iter().map(|&i| C::decode(&child.node.rows[i], &child.node)).collect(),
        None => Ok(Vec::new()),
    }
}

/// Decode the column of an `Option` field with the given alias. An override may leave the
/// column out, and then the value is `None`.
#[doc(hidden)]
pub fn optional_column<T>(row: &PgRow, node: &Node, alias: &str) -> Result<Option<T>, Error>
where
    T: for<'r> Decode<'r, Postgres> + Type<Postgres>,
{
    match row.try_get::<Option<T>, _>(alias) {
        Ok(value) => Ok(value),
        Err(sqlx::Error::ColumnNotFound(_)) => Ok(None),
        Err(source) => Err(Error::Decode { view: node.view, path: node.path_of(alias), source }),
    }
}

/// Read the tag of the enum whose aliases start with `prefix`.
#[doc(hidden)]
pub fn tag(row: &PgRow, node: &Node, prefix: &str) -> Result<String, Error> {
    let alias = format!("{prefix}{TAG_ALIAS}");
    match row.try_get::<Option<String>, _>(alias.as_str()) {
        Ok(Some(tag)) => Ok(tag),
        Ok(None) => Err(Error::NullTag { view: node.view, path: node.path_of(&alias) }),
        Err(source) => Err(Error::Decode { view: node.view, path: node.path_of(&alias), source }),
    }
}

/// Check that the columns of the other variants than `variant` are NULL, for the enum
/// whose aliases start with `prefix`.
#[doc(hidden)]
pub fn strict(row: &PgRow, node: &Node, prefix: &str, variant: &str, tag: &str) -> Result<(), Error> {
    let Some(sum) = node.sums.iter().find(|s| s.alias_prefix == prefix) else { return Ok(()) };
    let Some((_, columns)) = sum.variants.iter().find(|(name, _)| *name == variant) else { return Ok(()) };
    for (ordinal, alias) in columns {
        let raw = row.try_get_raw(*ordinal).map_err(|source| Error::Decode {
            view: node.view,
            path: node.path_of(alias),
            source,
        })?;
        if !raw.is_null() {
            return Err(Error::OtherVariantColumn { view: node.view, path: node.path_of(alias), tag: tag.to_string() });
        }
    }
    Ok(())
}

/// The error for a tag that names no variant.
#[doc(hidden)]
pub fn unknown_tag(node: &Node, prefix: &str, tag: &str, expected: &[&'static str]) -> Error {
    Error::UnknownTag {
        view: node.view,
        path: node.path_of(&format!("{prefix}{TAG_ALIAS}")),
        tag: tag.to_string(),
        expected: expected.to_vec(),
    }
}

/// The row of the variant table of `variant` for the row, and its node, for an enum stored
/// in a table per variant in the field `field_index` of the view.
#[doc(hidden)]
pub fn variant<'a>(
    row: &PgRow,
    node: &'a Node,
    field_index: usize,
    variant: &str,
) -> Result<(&'a PgRow, &'a Node), Error> {
    let child = node.variant_child(field_index, variant);
    let missing = || Error::MissingVariant { view: node.view, path: child.node.path.clone() };
    let key = node.key(row, KEY_ALIAS)?.ok_or_else(missing)?;
    match child.by_key.get(&key).and_then(|indices| indices.first()) {
        Some(&i) => Ok((&child.node.rows[i], &child.node)),
        None => Err(missing()),
    }
}

/// Decode an optional to-one reference of the row.
#[doc(hidden)]
pub fn to_one<C: View>(row: &PgRow, node: &Node, field_index: usize, ref_alias: &str) -> Result<Option<C>, Error> {
    let child = node.child(field_index);
    let Some(key) = node.key(row, ref_alias)? else {
        return Ok(None);
    };
    match child.by_key.get(&key).and_then(|indices| indices.first()) {
        Some(&i) => C::decode(&child.node.rows[i], &child.node).map(Some),
        None => Err(Error::MissingReference { view: node.view, path: child.node.path.clone() }),
    }
}

/// Decode a required to-one reference of the row.
#[doc(hidden)]
pub fn to_one_required<C: View>(row: &PgRow, node: &Node, field_index: usize, ref_alias: &str) -> Result<C, Error> {
    to_one(row, node, field_index, ref_alias)?
        .ok_or_else(|| Error::MissingReference { view: node.view, path: node.child(field_index).node.path.clone() })
}

type NodeFuture<'a> = Pin<Box<dyn Future<Output = Result<Node, Error>> + Send + 'a>>;

/// Run the query of the plan, then its child queries, on the connection. Queries with an
/// override in `overrides` run the override instead of the generated SQL.
pub(crate) fn load<'a>(
    conn: &'a mut PgConnection,
    plan: &'a QueryPlan,
    options: &'a RootOptions,
    keys: Option<KeyArray>,
    overrides: Option<&'a Overrides>,
) -> NodeFuture<'a> {
    Box::pin(async move {
        let view = plan.shape.name;
        let active = overrides.and_then(|o| o.get(plan.query_name()));
        let has_keys = keys.is_some();
        let sql = statement(plan, options, has_keys, active)?;

        let rows = match active {
            Some(active) if active.shadow => {
                let generated: Arc<str> =
                    sql::select(plan, &RootOptions { by_keys: has_keys, ..options.clone() }).into();
                let start = Instant::now();
                let rows = fetch(conn, plan, &sql, keys.clone()).await?;
                let override_time = start.elapsed();
                let start = Instant::now();
                let expected = fetch(conn, plan, &generated, keys).await?;
                let generated_time = start.elapsed();
                active.stats.record(override_time, generated_time);
                if let Err(difference) = compare(plan, &rows, &expected) {
                    active.stats.mismatch();
                    tracing::warn!(
                        view,
                        query = plan.query_name(),
                        origin = %active.origin,
                        "shadowed override returned different rows than the generated query: {difference}"
                    );
                } else {
                    tracing::debug!(
                        view,
                        query = plan.query_name(),
                        ?override_time,
                        ?generated_time,
                        "shadowed override returned the same rows as the generated query"
                    );
                }
                rows
            }
            _ => fetch(conn, plan, &sql, keys).await?,
        };

        let mut node = Node::new(view, plan.path.clone(), rows, plan)?;

        for child in &plan.children {
            // The keys the child rows are selected by
            let (parent_alias, tag) = match &child.plan.link {
                Link::Child { .. } => (KEY_ALIAS, None),
                Link::ToOne { ref_alias } => (ref_alias.as_str(), None),
                Link::Variant { tag_alias, tag_value } => (KEY_ALIAS, Some((tag_alias.as_str(), *tag_value))),
                Link::Root => unreachable!("a child query is never a root query"),
            };
            let mut seen = HashSet::new();
            let mut keys = Vec::new();
            for row in &node.rows {
                // A variant table is only queried for the rows of the variant
                if let Some((tag_alias, tag_value)) = tag {
                    let tag = row.try_get::<Option<&str>, _>(tag_alias).map_err(|source| Error::Decode {
                        view,
                        path: node.path_of(tag_alias),
                        source,
                    })?;
                    if tag != Some(tag_value) {
                        continue;
                    }
                }
                if let Some(key) = node.key(row, parent_alias)?
                    && seen.insert(key.clone())
                {
                    keys.push(key);
                }
            }

            let child_node = if keys.is_empty() {
                Node::new(child.plan.shape.name, child.plan.path.clone(), Vec::new(), &child.plan)?
            } else {
                let keys = KeyArray::new(keys).map_err(|_| Error::MixedKeys { view: child.plan.shape.name })?;
                load(&mut *conn, &child.plan, options, Some(keys), overrides).await?
            };

            let by_key = child_node.group(attach_alias(&child.plan.link))?;
            node.children.push(ChildEntry {
                field_index: child.field_index,
                variant: child.variant,
                child: ChildNode { node: child_node, by_key },
            });
        }

        Ok(node)
    })
}

/// Child rows are attached by their parent key, referenced rows by their own key.
fn attach_alias(link: &Link) -> &'static str {
    match link {
        Link::Child { .. } => PARENT_ALIAS,
        _ => KEY_ALIAS,
    }
}

impl Node {
    /// The indices of the rows by the key in the column with the given alias, in row order.
    fn group(&self, alias: &str) -> Result<HashMap<Key, Vec<usize>>, Error> {
        let mut by_key: HashMap<Key, Vec<usize>> = HashMap::new();
        for (i, row) in self.rows.iter().enumerate() {
            if let Some(key) = self.key(row, alias)? {
                by_key.entry(key).or_default().push(i);
            }
        }
        Ok(by_key)
    }
}

/// The SQL of a query: its override, or the generated SQL.
fn statement(
    plan: &QueryPlan,
    options: &RootOptions,
    has_keys: bool,
    active: Option<&ActiveOverride>,
) -> Result<Arc<str>, Error> {
    let view = plan.shape.name;
    match (active, &plan.link) {
        (None, _) => Ok(sql::select(plan, &RootOptions { by_keys: has_keys, ..options.clone() }).into()),
        (Some(active), Link::Root) => {
            if active.keys_param && !has_keys {
                return Err(Error::KeysRequired { view, origin: active.origin.to_string() });
            }
            let sql = sql::wrap_root(&active.sql, plan, options, has_keys && !active.keys_param)
                .map_err(|column| Error::UnknownOrderBy { view, column: column.to_string() })?;
            Ok(sql.into())
        }
        (Some(active), _) => Ok(active.sql.clone()),
    }
}

async fn fetch(
    conn: &mut PgConnection,
    plan: &QueryPlan,
    sql: &Arc<str>,
    keys: Option<KeyArray>,
) -> Result<Vec<PgRow>, Error> {
    // Generated SQL only contains quoted identifiers from the static shape of the view and
    // integer limits. Override SQL comes from configuration and was checked when the
    // registry was built. All values, including the keys, are bound parameters.
    let mut query = sqlx::query(AssertSqlSafe(sql.clone()));
    if let Some(keys) = keys {
        query = keys.bind(query);
    }
    query.fetch_all(&mut *conn).await.map_err(|source| Error::Query {
        view: plan.shape.name,
        path: plan.path.clone(),
        sql: sql.to_string(),
        source,
    })
}

/// The encoded values of a row, by alias.
type RowValues = Vec<Option<Vec<u8>>>;

/// Compare the rows of an override with the rows of the generated query, as they are
/// used: rows of a to-many query in order per parent, other rows by key.
fn compare(plan: &QueryPlan, actual: &[PgRow], expected: &[PgRow]) -> Result<(), String> {
    // Columns an override leaves out are optional and not compared
    let aliases: Vec<&str> = plan
        .columns
        .iter()
        .map(|c| c.alias.as_str())
        .filter(|alias| actual.first().is_none_or(|row| row.try_column(*alias).is_ok()))
        .collect();
    let group_alias = match plan.link {
        Link::Child { .. } => PARENT_ALIAS,
        _ => plan.key_alias.as_str(),
    };
    let actual = grouped(actual, &aliases, group_alias)?;
    let expected = grouped(expected, &aliases, group_alias)?;
    if actual.len() != expected.len() {
        return Err(format!("{} groups of rows, expected {}", actual.len(), expected.len()));
    }
    for (key, rows) in &expected {
        match actual.get(key) {
            None => return Err(format!("no rows for {group_alias} {}", display(key))),
            Some(found) if found.len() != rows.len() => {
                return Err(format!(
                    "{} rows for {group_alias} {}, expected {}",
                    found.len(),
                    display(key),
                    rows.len()
                ));
            }
            Some(found) => {
                for (i, (found, row)) in found.iter().zip(rows).enumerate() {
                    if let Some(column) = (0..aliases.len()).find(|&c| found[c] != row[c]) {
                        return Err(format!(
                            "row {} for {group_alias} {}: column \"{}\" differs",
                            i + 1,
                            display(key),
                            aliases[column]
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn grouped(rows: &[PgRow], aliases: &[&str], group_alias: &str) -> Result<HashMap<Vec<u8>, Vec<RowValues>>, String> {
    let mut groups: HashMap<Vec<u8>, Vec<RowValues>> = HashMap::new();
    for row in rows {
        let value = |alias: &str| -> Result<Option<Vec<u8>>, String> {
            let raw = row.try_get_raw(alias).map_err(|e| e.to_string())?;
            if raw.is_null() {
                return Ok(None);
            }
            Ok(Some(raw.as_bytes().map_err(|e| e.to_string())?.to_vec()))
        };
        let key = value(group_alias)?.unwrap_or_default();
        let values = aliases.iter().map(|alias| value(alias)).collect::<Result<_, _>>()?;
        groups.entry(key).or_default().push(values);
    }
    Ok(groups)
}

fn display(key: &[u8]) -> String {
    match std::str::from_utf8(key) {
        Ok(text) if text.chars().all(|c| !c.is_control()) => format!("\"{text}\""),
        _ => key.iter().map(|b| format!("{b:02x}")).collect(),
    }
}
