//! Loaded query results and the helpers used by generated decoders.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use refract_core::sql::{self, RootOptions};
use refract_core::{KEY_ALIAS, Link, PARENT_ALIAS, QueryPlan};
use sqlx::postgres::PgRow;
use sqlx::{AssertSqlSafe, Decode, PgConnection, Postgres, Row, Type};

use crate::key::{Key, KeyArray, KeyColumn};
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
    /// Indexed by field index of the view; `Some` for child and to-one fields.
    children: Vec<Option<ChildNode>>,
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
        self.children[field_index].as_ref().expect("plan and decoder disagree on the fields of a view")
    }

    fn path_of(&self, alias: &str) -> String {
        join(&self.path, alias)
    }

    fn new(view: &'static str, path: String, rows: Vec<PgRow>, plan: &QueryPlan) -> Result<Node, Error> {
        let mut key_columns = Vec::new();
        if let Some(row) = rows.first() {
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
        Ok(Node {
            view,
            path,
            rows,
            key_alias: plan.key_alias.clone(),
            key_columns,
            children: (0..plan.shape.fields.len()).map(|_| None).collect(),
        })
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

/// Run the query of the plan, then its child queries, on the connection.
pub(crate) fn load<'a>(
    conn: &'a mut PgConnection,
    plan: &'a QueryPlan,
    options: &'a RootOptions,
    keys: Option<KeyArray>,
) -> NodeFuture<'a> {
    Box::pin(async move {
        let view = plan.shape.name;
        let sql: Arc<str> = sql::select(plan, options).into();
        // The SQL only contains quoted identifiers from the static shape of the view and
        // integer limits; all values, including the keys, are bound parameters.
        let mut query = sqlx::query(AssertSqlSafe(sql.clone()));
        if let Some(keys) = keys {
            query = keys.bind(query);
        }
        let rows = query.fetch_all(&mut *conn).await.map_err(|source| Error::Query {
            view,
            path: plan.path.clone(),
            sql: sql.to_string(),
            source,
        })?;

        let mut node = Node::new(view, plan.path.clone(), rows, plan)?;

        for child in &plan.children {
            // The keys the child rows are selected by
            let parent_alias = match &child.plan.link {
                Link::Child { .. } => KEY_ALIAS,
                Link::ToOne { ref_alias } => ref_alias.as_str(),
                Link::Root => unreachable!("a child query is never a root query"),
            };
            let mut seen = HashSet::new();
            let mut keys = Vec::new();
            for row in &node.rows {
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
                load(&mut *conn, &child.plan, options, Some(keys)).await?
            };

            // Child rows are attached by their parent key, referenced rows by their own key
            let attach_alias = match &child.plan.link {
                Link::Child { .. } => PARENT_ALIAS,
                _ => KEY_ALIAS,
            };
            let mut by_key: HashMap<Key, Vec<usize>> = HashMap::new();
            for (i, row) in child_node.rows.iter().enumerate() {
                if let Some(key) = child_node.key(row, attach_alias)? {
                    by_key.entry(key).or_default().push(i);
                }
            }

            node.children[child.field_index] = Some(ChildNode { node: child_node, by_key });
        }

        Ok(node)
    })
}
