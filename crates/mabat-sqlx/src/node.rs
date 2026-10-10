//! Loaded query results and the helpers used by generated decoders.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use mabat_core::sql::{self, Render, RootOptions};
use mabat_core::{
    ChildPlan, ChildQuery, FieldKind, INDEX_ALIAS, KEY_ALIAS, Link, MAP_KEY_ALIAS, PARENT_ALIAS, QueryPlan,
    REF_ALIAS_PREFIX, TAG_ALIAS, ViewShape,
};
use sqlx::{Decode, Type};

use crate::backend::Backend;
use crate::filter::Bound;
use crate::graph::{GraphBuilder, Identity, Ref};
use crate::key::{Key, KeyColumn, KeyList};
use crate::pooled::Runner;
use crate::registry::{ActiveOverride, Overrides};
use crate::{Error, View, ViewDecoder};

/// The rows of one query of a plan, with the rows of its child queries.
pub struct Node<B: Backend> {
    view: &'static str,
    shape: &'static ViewShape,
    path: String,
    rows: Vec<B::Row>,
    /// The names of the columns of the rows, in order, from the first row, so that columns are
    /// decoded by position: see [`Node::ordinal`].
    columns: Vec<Box<str>>,
    /// The position after the column decoded last, where the next one usually is.
    next: AtomicUsize,
    /// Alias of the key column, see [`QueryPlan::key_alias`].
    key_alias: String,
    /// The key columns of the rows by alias, resolved from the first row. The key column
    /// is stored as [`KEY_ALIAS`].
    key_columns: Vec<(String, KeyColumn)>,
    /// The child queries: child and to-one fields, and variant tables.
    children: Vec<ChildEntry<B>>,
    /// The enums stored in the rows, for strict decoding.
    sums: Vec<SumColumns>,
    /// The entities seen by the load, for graphs and shared values.
    identity: Arc<Identity>,
    /// For the entities of a graph: whether each row is the first one of its entity. Only
    /// the first row of an entity is decoded and has its fields loaded. `None` if all rows
    /// are.
    fresh: Option<Vec<bool>>,
    /// Whether each field of the view is loaded, see [`QueryPlan::selected`].
    selected: Vec<bool>,
}

struct ChildEntry<B: Backend> {
    field_index: usize,
    variant: Option<&'static str>,
    /// The rows of the child query, or `None` when they are rows of this node: the next
    /// level of a recursive collection loaded with one query.
    node: Option<Node<B>>,
    /// Rows of the child query by the key they are attached with, in list order.
    by_key: Groups,
}

/// The indices of rows grouped by a key, kept in one list rather than a list per key.
struct Groups {
    /// The group of each key.
    groups: HashMap<Key, usize, foldhash::fast::RandomState>,
    /// Where each group starts in `rows`, and where the last one ends.
    starts: Vec<usize>,
    /// The indices of the rows, group after group.
    rows: Vec<usize>,
}

impl Groups {
    /// The rows with the key, `None` if there are none.
    fn get(&self, key: &Key) -> Option<&[usize]> {
        self.groups.get(key).map(|&group| &self.rows[self.starts[group]..self.starts[group + 1]])
    }

    fn iter(&self) -> impl Iterator<Item = (&Key, &[usize])> {
        self.groups.iter().map(|(key, &group)| (key, &self.rows[self.starts[group]..self.starts[group + 1]]))
    }
}

/// The columns that must be NULL for each variant of an enum, see
/// [`mabat_core::VariantPlan::exclusive`], resolved from the first row. Columns that a
/// query does not select are left out.
#[derive(Debug)]
struct SumColumns {
    alias_prefix: String,
    /// `(variant, [(ordinal, alias)])`
    variants: Vec<(&'static str, Vec<(usize, String)>)>,
}

impl<B: Backend> std::fmt::Debug for Node<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("view", &self.view)
            .field("path", &self.path)
            .field("rows", &self.rows.len())
            .finish()
    }
}

/// A row of a child query with its node.
pub(crate) type ChildRow<'a, B> = (&'a <B as sqlx::Database>::Row, &'a Node<B>);

/// The rows of a child field and the indexes of those rows by the key of their parent.
type Children<'a, B> = (&'a Node<B>, &'a Groups);

impl<B: Backend> Node<B> {
    pub(crate) fn rows(&self) -> &[B::Row] {
        &self.rows
    }

    /// The rows of a child field and their grouping, `None` when they were not loaded: the
    /// level below the depth limit of a recursive collection.
    fn child(&self, field_index: usize) -> Option<Children<'_, B>> {
        self.entry(field_index, None)
    }

    fn entry(&self, field_index: usize, variant: Option<&str>) -> Option<Children<'_, B>> {
        self.children
            .iter()
            .find(|c| c.field_index == field_index && c.variant == variant)
            .map(|c| (c.node.as_ref().unwrap_or(self), &c.by_key))
    }

    fn variant_child(&self, field_index: usize, variant: &str) -> Children<'_, B> {
        self.entry(field_index, Some(variant)).expect("plan and decoder disagree on the variants of an enum")
    }

    pub(crate) fn view(&self) -> &'static str {
        self.view
    }

    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    /// Whether the field `field_index` of the view is loaded: all fields are, unless the
    /// load is of a selection.
    #[doc(hidden)]
    pub fn selected(&self, field_index: usize) -> bool {
        self.selected.get(field_index).copied().unwrap_or(false)
    }

    /// The rows of the child field `field_index` attached to the row by its `alias` column,
    /// with their node, in order. None for a field that was not loaded.
    pub(crate) fn child_rows<'a>(
        &'a self,
        row: &B::Row,
        field_index: usize,
        alias: &str,
    ) -> Result<Vec<ChildRow<'a, B>>, Error> {
        let Some((child, by_key)) = self.child(field_index) else { return Ok(Vec::new()) };
        let Some(key) = self.key(row, alias)? else { return Ok(Vec::new()) };
        Ok(by_key
            .get(&key)
            .map(|indices| indices.iter().map(|&i| (&child.rows[i], child)).collect())
            .unwrap_or_default())
    }

    /// The row a to-one field of the row references, with its node: `None` if the foreign key
    /// is NULL, or if the row is past the depth limit of a recursive reference.
    pub(crate) fn referenced(
        &self,
        row: &B::Row,
        field_index: usize,
        ref_alias: &str,
    ) -> Result<Option<(Key, ChildRow<'_, B>)>, Error> {
        let Some(key) = self.key(row, ref_alias)? else { return Ok(None) };
        // The level below the last one of a recursive reference is not loaded
        let Some(entry) = self.children.iter().find(|c| c.field_index == field_index && c.variant.is_none()) else {
            return Ok(None);
        };
        let child = entry.node.as_ref().unwrap_or(self);
        match entry.by_key.get(&key).and_then(|indices| indices.first()) {
            Some(&i) => Ok(Some((key, (&child.rows[i], child)))),
            None => Err(Error::MissingReference { view: self.view, path: child.path.clone() }),
        }
    }

    /// The error for a value that cannot be written as JSON.
    pub(crate) fn json_error(&self, alias: &str, message: String) -> Error {
        Error::Json { view: self.view, path: self.path_of(alias), message }
    }

    /// The error for a to-one reference whose row was not found.
    pub(crate) fn missing_reference(&self, field_index: usize, ref_alias: &str) -> Error {
        let path = match self.child(field_index) {
            Some((child, _)) => child.path.clone(),
            None => self.path_of(ref_alias),
        };
        Error::MissingReference { view: self.view, path }
    }

    fn path_of(&self, alias: &str) -> String {
        join(&self.path, alias)
    }

    fn new(path: String, rows: Vec<B::Row>, plan: &QueryPlan, identity: &Arc<Identity>) -> Result<Node<B>, Error> {
        let view = plan.shape.name;
        let mut key_columns = Vec::new();
        let mut sums = Vec::new();
        let mut columns = rows.first().map(B::column_names).unwrap_or_default();
        // Of columns with the same name, SQLx reads the last one: hide the others
        for i in 0..columns.len() {
            if columns[i + 1..].contains(&columns[i]) {
                columns[i] = "".into();
            }
        }
        if let Some(row) = rows.first() {
            for sum in &plan.sums {
                let variants = sum
                    .variants
                    .iter()
                    .map(|variant| {
                        let columns = variant
                            .exclusive
                            .iter()
                            .filter_map(|alias| B::find_column(row, alias).map(|ordinal| (ordinal, alias.clone())))
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
            // The foreign keys of the selected to-one fields
            for (field, _) in plan.shape.fields.iter().zip(&plan.selected).filter(|(_, selected)| **selected) {
                if let FieldKind::ToOne { .. } = field.kind {
                    let ref_alias = format!("{REF_ALIAS_PREFIX}{}", field.name);
                    aliases.push((ref_alias.clone(), ref_alias));
                }
            }
            if plan.columns.iter().any(|c| c.alias == INDEX_ALIAS) {
                aliases.push((INDEX_ALIAS.to_string(), INDEX_ALIAS.to_string()));
            }
            for (name, alias) in aliases {
                let column = KeyColumn::resolve::<B>(row, &alias).map_err(|source| Error::Decode {
                    view,
                    path: join(&path, &alias),
                    source,
                })?;
                key_columns.push((name, column));
            }
        }
        Ok(Node {
            view,
            shape: plan.shape,
            path,
            rows,
            columns,
            next: AtomicUsize::new(0),
            key_alias: plan.key_alias.clone(),
            key_columns,
            children: Vec::new(),
            sums,
            identity: identity.clone(),
            fresh: None,
            selected: plan.selected.clone(),
        })
    }

    /// The position of the column with the given alias, `None` if the rows have none.
    ///
    /// Decoders read the columns of a row in about the order the query selects them, so the
    /// column after the one read last is tried first, and the names are only searched when
    /// it is not the one: a comparison per column instead of hashing the name.
    pub(crate) fn ordinal(&self, alias: &str) -> Option<usize> {
        let next = self.next.load(Ordering::Relaxed);
        let found = match self.columns.get(next) {
            Some(name) if **name == *alias => next,
            _ => self.columns.iter().position(|name| **name == *alias)?,
        };
        self.next.store(found + 1, Ordering::Relaxed);
        Some(found)
    }

    /// Decode the column with the given alias.
    fn get<T>(&self, row: &B::Row, alias: &str) -> Result<T, sqlx::Error>
    where
        T: for<'r> Decode<'r, B> + Type<B>,
    {
        match self.ordinal(alias) {
            Some(ordinal) => B::get_at::<T>(row, ordinal),
            None => Err(sqlx::Error::ColumnNotFound(alias.to_string())),
        }
    }

    pub(crate) fn key(&self, row: &B::Row, alias: &str) -> Result<Option<Key>, Error> {
        let column = match self.key_columns.iter().find(|(a, _)| a == alias) {
            Some((_, column)) => *column,
            None => KeyColumn::resolve::<B>(row, if alias == KEY_ALIAS { &self.key_alias } else { alias })
                .map_err(|source| Error::Decode { view: self.view, path: self.path_of(alias), source })?,
        };
        column.read::<B>(row).map_err(|source| Error::Decode { view: self.view, path: self.path_of(alias), source })
    }
}

fn join(path: &str, alias: &str) -> String {
    if path.is_empty() { alias.to_string() } else { format!("{path}.{alias}") }
}

/// Decode the column with the given alias.
#[doc(hidden)]
pub fn column<T, B: Backend>(row: &B::Row, node: &Node<B>, alias: &str) -> Result<T, Error>
where
    T: for<'r> Decode<'r, B> + Type<B>,
{
    node.get::<T>(row, alias).map_err(|source| Error::Decode { view: node.view, path: node.path_of(alias), source })
}

/// Decode the elements of a to-many collection of the row, in the order of the child query.
#[doc(hidden)]
pub fn children<C: ViewDecoder<B>, B: Backend>(
    row: &B::Row,
    node: &Node<B>,
    field_index: usize,
) -> Result<Vec<C>, Error> {
    let Some((child, by_key)) = node.child(field_index) else { return Ok(Vec::new()) };
    let Some(key) = node.key(row, KEY_ALIAS)? else {
        return Ok(Vec::new());
    };
    match by_key.get(&key) {
        Some(indices) => indices.iter().map(|&i| C::decode(&child.rows[i], child)).collect(),
        None => Ok(Vec::new()),
    }
}

/// A map that a collection is decoded into.
#[doc(hidden)]
pub trait MapInsert<K, V>: Default {
    /// Insert the entry, `false` if the key is already in the map.
    fn insert_new(&mut self, key: K, value: V) -> bool;
}

impl<K: Ord, V> MapInsert<K, V> for std::collections::BTreeMap<K, V> {
    fn insert_new(&mut self, key: K, value: V) -> bool {
        match self.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(value);
                true
            }
            std::collections::btree_map::Entry::Occupied(_) => false,
        }
    }
}

impl<K, V, S> MapInsert<K, V> for HashMap<K, V, S>
where
    K: Eq + std::hash::Hash,
    S: std::hash::BuildHasher + Default,
{
    fn insert_new(&mut self, key: K, value: V) -> bool {
        match self.entry(key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(value);
                true
            }
            std::collections::hash_map::Entry::Occupied(_) => false,
        }
    }
}

/// Decode the elements of a map collection of the row, keyed by their `$map_key` column.
#[doc(hidden)]
pub fn map<K, C, M, B: Backend>(row: &B::Row, node: &Node<B>, field_index: usize) -> Result<M, Error>
where
    K: for<'r> Decode<'r, B> + Type<B>,
    C: ViewDecoder<B>,
    M: MapInsert<K, C>,
{
    let mut map = M::default();
    let Some((child, by_key)) = node.child(field_index) else { return Ok(map) };
    let Some(indices) = node.key(row, KEY_ALIAS)?.and_then(|key| by_key.get(&key)) else { return Ok(map) };
    for &i in indices {
        let row = &child.rows[i];
        let key = column::<K, B>(row, child, MAP_KEY_ALIAS)?;
        if !map.insert_new(key, C::decode(row, child)?) {
            let path = child.path.clone();
            return Err(Error::DuplicateMapKey { view: child.view, path });
        }
    }
    Ok(map)
}

/// Decode the column of an `Option` field with the given alias. An override may leave the
/// column out, and then the value is `None`.
#[doc(hidden)]
pub fn optional_column<T, B: Backend>(row: &B::Row, node: &Node<B>, alias: &str) -> Result<Option<T>, Error>
where
    T: for<'r> Decode<'r, B> + Type<B>,
{
    match node.get::<Option<T>>(row, alias) {
        Ok(value) => Ok(value),
        Err(sqlx::Error::ColumnNotFound(_)) => Ok(None),
        Err(source) => Err(Error::Decode { view: node.view, path: node.path_of(alias), source }),
    }
}

/// Read the tag of the enum whose aliases start with `prefix`.
#[doc(hidden)]
pub fn tag<B: Backend>(row: &B::Row, node: &Node<B>, prefix: &str) -> Result<String, Error>
where
    String: for<'r> Decode<'r, B> + Type<B>,
{
    let alias = format!("{prefix}{TAG_ALIAS}");
    match node.get::<Option<String>>(row, alias.as_str()) {
        Ok(Some(tag)) => Ok(tag),
        Ok(None) => Err(Error::NullTag { view: node.view, path: node.path_of(&alias) }),
        Err(source) => Err(Error::Decode { view: node.view, path: node.path_of(&alias), source }),
    }
}

/// Check that the columns of the other variants than `variant` are NULL, for the enum
/// whose aliases start with `prefix`.
#[doc(hidden)]
pub fn strict<B: Backend>(row: &B::Row, node: &Node<B>, prefix: &str, variant: &str, tag: &str) -> Result<(), Error> {
    let Some(sum) = node.sums.iter().find(|s| s.alias_prefix == prefix) else { return Ok(()) };
    let Some((_, columns)) = sum.variants.iter().find(|(name, _)| *name == variant) else { return Ok(()) };
    for (ordinal, alias) in columns {
        let null = B::is_null(row, *ordinal).map_err(|source| Error::Decode {
            view: node.view,
            path: node.path_of(alias),
            source,
        })?;
        if !null {
            return Err(Error::OtherVariantColumn { view: node.view, path: node.path_of(alias), tag: tag.to_string() });
        }
    }
    Ok(())
}

/// The error for a tag that names no variant.
#[doc(hidden)]
pub fn unknown_tag<B: Backend>(node: &Node<B>, prefix: &str, tag: &str, expected: &[&'static str]) -> Error {
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
pub fn variant<'a, B: Backend>(
    row: &B::Row,
    node: &'a Node<B>,
    field_index: usize,
    variant: &str,
) -> Result<(&'a B::Row, &'a Node<B>), Error> {
    let (child, by_key) = node.variant_child(field_index, variant);
    let missing = || Error::MissingVariant { view: node.view, path: child.path.clone() };
    let key = node.key(row, KEY_ALIAS)?.ok_or_else(missing)?;
    match by_key.get(&key).and_then(|indices| indices.first()) {
        Some(&i) => Ok((&child.rows[i], child)),
        None => Err(missing()),
    }
}

/// Decode an optional to-one reference of the row.
#[doc(hidden)]
pub fn to_one<C: ViewDecoder<B>, B: Backend>(
    row: &B::Row,
    node: &Node<B>,
    field_index: usize,
    ref_alias: &str,
) -> Result<Option<C>, Error> {
    match node.referenced(row, field_index, ref_alias)? {
        Some((_, (row, child))) => C::decode(row, child).map(Some),
        None => Ok(None),
    }
}

/// A reference to the entity of a to-one field of a graph view, `None` if the foreign key is
/// NULL.
#[doc(hidden)]
pub fn reference<C: View, B: Backend>(row: &B::Row, node: &Node<B>, ref_alias: &str) -> Result<Option<Ref<C>>, Error> {
    Ok(node.key(row, ref_alias)?.map(|key| node.identity.reference::<C>(key)))
}

/// A reference to the entity of a required to-one field of a graph view.
#[doc(hidden)]
pub fn reference_required<C: View, B: Backend>(row: &B::Row, node: &Node<B>, ref_alias: &str) -> Result<Ref<C>, Error> {
    reference(row, node, ref_alias)?
        .ok_or_else(|| Error::MissingReference { view: node.view, path: node.path_of(ref_alias) })
}

/// References to the elements of a collection of a graph view.
#[doc(hidden)]
pub fn references<C: View, B: Backend>(row: &B::Row, node: &Node<B>, field_index: usize) -> Result<Vec<Ref<C>>, Error> {
    match node.key(row, KEY_ALIAS)? {
        Some(key) => Ok(node.identity.references::<C>(node.shape, field_index, &key)),
        None => Ok(Vec::new()),
    }
}

/// Decode a to-one field shared by all the rows that reference the same entity.
#[doc(hidden)]
pub fn shared_to_one<C: ViewDecoder<B>, B: Backend>(
    row: &B::Row,
    node: &Node<B>,
    field_index: usize,
    ref_alias: &str,
) -> Result<Option<Arc<C>>, Error> {
    match node.referenced(row, field_index, ref_alias)? {
        Some((key, (row, child))) => node.identity.shared(key, || C::decode(row, child)).map(Some),
        None => Ok(None),
    }
}

/// Decode a required to-one field shared by all the rows that reference the same entity.
#[doc(hidden)]
pub fn shared_to_one_required<C: ViewDecoder<B>, B: Backend>(
    row: &B::Row,
    node: &Node<B>,
    field_index: usize,
    ref_alias: &str,
) -> Result<Arc<C>, Error> {
    shared_to_one(row, node, field_index, ref_alias)?
        .ok_or_else(|| Error::MissingReference { view: node.view, path: node.path_of(ref_alias) })
}

/// Decode the elements of a collection, each entity shared by all the collections it is in.
#[doc(hidden)]
pub fn shared_children<C: ViewDecoder<B>, B: Backend>(
    row: &B::Row,
    node: &Node<B>,
    field_index: usize,
) -> Result<Vec<Arc<C>>, Error> {
    let Some((child, by_key)) = node.child(field_index) else { return Ok(Vec::new()) };
    let Some(indices) = node.key(row, KEY_ALIAS)?.and_then(|key| by_key.get(&key)) else { return Ok(Vec::new()) };
    indices
        .iter()
        .map(|&i| {
            let row = &child.rows[i];
            match child.key(row, KEY_ALIAS)? {
                Some(key) => child.identity.shared(key, || C::decode(row, child)),
                None => C::decode(row, child).map(Arc::new),
            }
        })
        .collect()
}

/// Store the entities of the rows in the graph, each once.
#[doc(hidden)]
pub fn graph_store<T: ViewDecoder<B>, B: Backend>(node: &Node<B>, graph: &mut GraphBuilder) -> Result<(), Error> {
    for (i, row) in node.rows.iter().enumerate() {
        if node.fresh.as_ref().is_some_and(|fresh| !fresh[i]) {
            continue;
        }
        if let Some(key) = node.key(row, KEY_ALIAS)? {
            graph.store::<T>(key, || T::decode(row, node))?;
        }
    }
    Ok(())
}

/// Visit the rows of a child field for a graph: store them if they are entities of the
/// graph, and visit their own child fields.
#[doc(hidden)]
pub fn graph_visit<C: ViewDecoder<B>, B: Backend>(
    node: &Node<B>,
    field_index: usize,
    graph: &mut GraphBuilder,
    entity: bool,
) -> Result<(), Error> {
    for entry in node.children.iter().filter(|c| c.field_index == field_index && c.variant.is_none()) {
        // The rows of a recursive collection loaded with one query are this node's rows
        if let Some(child) = &entry.node {
            C::decode_graph(child, graph, entity)?;
        }
    }
    Ok(())
}

impl<B: Backend> Node<B> {
    /// The keys of the rows, in row order.
    pub(crate) fn row_keys(&self) -> Result<Vec<Key>, Error> {
        let keys = self.rows.iter().map(|row| self.key(row, KEY_ALIAS)).collect::<Result<Vec<_>, _>>()?;
        Ok(keys.into_iter().flatten().collect())
    }
}

/// Decode a required to-one reference of the row.
#[doc(hidden)]
pub fn to_one_required<C: ViewDecoder<B>, B: Backend>(
    row: &B::Row,
    node: &Node<B>,
    field_index: usize,
    ref_alias: &str,
) -> Result<C, Error> {
    to_one(row, node, field_index, ref_alias)?.ok_or_else(|| Error::MissingReference {
        view: node.view,
        path: node.child(field_index).map_or_else(String::new, |(child, _)| child.path.clone()),
    })
}

type NodeFuture<'a, B> = Pin<Box<dyn Future<Output = Result<Node<B>, Error>> + Send + 'a>>;

/// Run the query of the plan, then its child queries, on connections of the runner. Queries
/// with an override in `overrides` run the override instead of the generated SQL.
///
/// `path` is the path of the rows in the root view, and `ancestors` the queries above, to
/// run them again for the levels of a recursive collection. `entities` says that the rows
/// are entities of a graph, which are loaded once.
///
/// When the runner is concurrent, the child queries run at the same time. A graph loads
/// them one at a time, in order, so that each entity is fetched by the same query.
#[allow(clippy::too_many_arguments)]
pub(crate) fn load<'a, 'c: 'a, B: Backend>(
    runner: &'a Runner<'c, B>,
    plan: &'a QueryPlan,
    args: &'a QueryArgs,
    keys: Option<KeyList>,
    overrides: Option<&'a Overrides>,
    path: String,
    ancestors: Vec<&'a QueryPlan>,
    identity: Arc<Identity>,
    entities: bool,
) -> NodeFuture<'a, B>
where
    B::Connection: Send,
    B::Row: Send + Sync,
{
    Box::pin(async move {
        let view = plan.shape.name;
        let active = overrides.and_then(|o| o.get(plan.query_name()));
        let (options, values) = args.of(plan);

        // The connection is held for this query only, not while the children load
        let rows = {
            let mut conn = runner.lease().await?;
            match active {
                Some(active) if active.shadow => {
                    let start = Instant::now();
                    let rows = fetch::<B>(&mut conn, plan, options, keys.clone(), values, Some(active)).await?;
                    let override_time = start.elapsed();
                    let start = Instant::now();
                    let expected = fetch::<B>(&mut conn, plan, options, keys, values, None).await?;
                    let generated_time = start.elapsed();
                    active.stats.record(override_time, generated_time);
                    if let Err(difference) = compare::<B>(plan, &rows, &expected) {
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
                _ => fetch::<B>(&mut conn, plan, options, keys, values, active).await?,
            }
        };

        let mut node = Node::new(path, rows, plan, &identity)?;
        if identity.graph && entities {
            let keys = node.rows.iter().map(|row| node.key(row, KEY_ALIAS)).collect::<Result<Vec<_>, _>>()?;
            node.fresh = Some(identity.fetch(plan.shape, keys));
        }
        let mut chain = ancestors;
        chain.push(plan);

        let concurrent = runner.concurrent() && !identity.graph;
        let mut pending = Vec::new();
        for child in &plan.children {
            let Some(next) = next_child(&mut node, plan, child, &chain, &identity)? else { continue };
            if concurrent {
                pending.push(next);
            } else {
                let child_node = load_child(
                    runner,
                    next.target,
                    args,
                    next.keys,
                    overrides,
                    next.path,
                    &chain,
                    &identity,
                    next.graph_edge,
                )
                .await?;
                attach(&mut node, plan, next.field_index, next.variant, next.target, next.graph_edge, child_node)?;
            }
        }
        if !pending.is_empty() {
            let loads = pending.iter_mut().map(|next| {
                let keys = next.keys.take();
                let path = std::mem::take(&mut next.path);
                load_child(runner, next.target, args, keys, overrides, path, &chain, &identity, next.graph_edge)
            });
            let child_nodes = futures_util::future::try_join_all(loads).await?;
            for (next, child_node) in pending.into_iter().zip(child_nodes) {
                attach(&mut node, plan, next.field_index, next.variant, next.target, next.graph_edge, child_node)?;
            }
        }

        Ok(node)
    })
}

/// The arguments of the queries of a load: the options of the root query and of to-many
/// child queries by name, with their filter values.
#[derive(Debug, Default)]
pub(crate) struct QueryArgs {
    root: (RootOptions, Vec<Bound>),
    nested: HashMap<String, (RootOptions, Vec<Bound>)>,
    /// For the other child queries.
    none: (RootOptions, Vec<Bound>),
}

impl QueryArgs {
    pub(crate) fn new(options: RootOptions, values: Vec<Bound>) -> QueryArgs {
        QueryArgs { root: (options, values), ..QueryArgs::default() }
    }

    /// Set the options of the child query named `name`.
    pub(crate) fn nest(&mut self, name: String, options: RootOptions, values: Vec<Bound>) {
        self.nested.insert(name, (RootOptions { by_keys: true, ..options }, values));
    }

    /// The options and filter values of a query.
    fn of(&self, plan: &QueryPlan) -> (&RootOptions, &[Bound]) {
        let (options, values) = match plan.link {
            Link::Root => &self.root,
            _ => self.nested.get(plan.query_name()).unwrap_or(&self.none),
        };
        (options, values)
    }
}

/// A child query to run: its plan, the keys of the rows above, and the path of its rows.
struct NextChild<'a> {
    field_index: usize,
    variant: Option<&'static str>,
    target: &'a QueryPlan,
    keys: Option<KeyList>,
    path: String,
    graph_edge: bool,
}

/// The child query of `child` for the rows of `node`, `None` if there is none to run: the
/// level below the depth limit of a recursive collection, or a level in the rows of this
/// query, which is attached here.
fn next_child<'a, B: Backend>(
    node: &mut Node<B>,
    plan: &'a QueryPlan,
    child: &'a ChildPlan,
    chain: &[&'a QueryPlan],
    identity: &Identity,
) -> Result<Option<NextChild<'a>>, Error> {
    let field = plan.shape.fields[child.field_index].name;
    let path = match child.variant {
        Some(variant) => format!("{}.{variant}", join(&node.path, field)),
        None => join(&node.path, field),
    };
    let target: &'a QueryPlan = match &child.query {
        ChildQuery::Query(target) => target,
        ChildQuery::Repeat { up, depth } => {
            // The next level of a recursive collection, unless it is the last level
            let target = chain[chain.len() - 1 - up];
            let level = chain.iter().filter(|p| std::ptr::eq(**p, target)).count();
            if level >= *depth as usize {
                return Ok(None);
            }
            target
        }
        ChildQuery::Same => {
            // The next level of a recursive collection or reference is in the rows of this query:
            // a collection's rows by the key they reference, a reference's rows by their own key
            let (link, by) = match &plan.shape.fields[child.field_index].kind {
                FieldKind::ToOne { .. } => (format!("{REF_ALIAS_PREFIX}{field}"), KEY_ALIAS),
                _ => (PARENT_ALIAS.to_string(), PARENT_ALIAS),
            };
            node.check_acyclic(&link)?;
            let by_key = node.group(by)?;
            node.children.push(ChildEntry { field_index: child.field_index, variant: None, node: None, by_key });
            return Ok(None);
        }
    };

    // The keys the child rows are selected by
    let (parent_alias, tag) = match &target.link {
        Link::Child { .. } => (KEY_ALIAS, None),
        Link::ToOne { ref_alias } => (ref_alias.as_str(), None),
        Link::Variant { tag_alias, tag_value } => (KEY_ALIAS, Some((tag_alias.as_str(), *tag_value))),
        Link::Root => unreachable!("a child query is never a root query"),
    };
    let mut seen = HashSet::new();
    let mut keys = Vec::new();
    for (i, row) in node.rows.iter().enumerate() {
        // Later rows of an entity of a graph are not loaded again
        if node.fresh.as_ref().is_some_and(|fresh| !fresh[i]) {
            continue;
        }
        // A variant table is only queried for the rows of the variant
        if let Some((tag_alias, tag_value)) = tag {
            let tag = node.tag_text(row, tag_alias)?;
            if tag.as_deref() != Some(tag_value) {
                continue;
            }
        }
        if let Some(key) = node.key(row, parent_alias)?
            && seen.insert(key.clone())
        {
            keys.push(key);
        }
    }

    // A graph fetches each entity and expands each collection of an entity once
    let graph_edge = identity.graph && plan.shape.fields[child.field_index].kind.is_graph_edge();
    if graph_edge {
        match &target.link {
            Link::ToOne { .. } => identity.not_fetched(target.shape, &mut keys),
            Link::Child { .. } => identity.expand(plan.shape, child.field_index, &mut keys),
            _ => {}
        }
    }

    let keys = if keys.is_empty() {
        None
    } else {
        Some(KeyList::new(keys).map_err(|_| Error::MixedKeys { view: target.shape.name })?)
    };
    Ok(Some(NextChild { field_index: child.field_index, variant: child.variant, target, keys, path, graph_edge }))
}

/// Load the rows of a child query, none without keys.
#[allow(clippy::too_many_arguments)]
async fn load_child<'a, 'c: 'a, B: Backend>(
    runner: &'a Runner<'c, B>,
    target: &'a QueryPlan,
    args: &'a QueryArgs,
    keys: Option<KeyList>,
    overrides: Option<&'a Overrides>,
    path: String,
    chain: &[&'a QueryPlan],
    identity: &Arc<Identity>,
    graph_edge: bool,
) -> Result<Node<B>, Error>
where
    B::Connection: Send,
    B::Row: Send + Sync,
{
    match keys {
        None => Node::new(path, Vec::new(), target, identity),
        Some(keys) => {
            let ancestors = chain.to_vec();
            load::<B>(runner, target, args, Some(keys), overrides, path, ancestors, identity.clone(), graph_edge).await
        }
    }
}

/// Attach the rows of a child query to the rows of `node`.
fn attach<B: Backend>(
    node: &mut Node<B>,
    plan: &QueryPlan,
    field_index: usize,
    variant: Option<&'static str>,
    target: &QueryPlan,
    graph_edge: bool,
    child_node: Node<B>,
) -> Result<(), Error> {
    let by_key = child_node.group(attach_alias(&target.link))?;
    if graph_edge && matches!(target.link, Link::Child { .. }) {
        node.record_edges(plan.shape, field_index, &by_key, &child_node)?;
    }
    node.children.push(ChildEntry { field_index, variant, node: Some(child_node), by_key });
    Ok(())
}

/// Child rows are attached by their parent key, referenced rows by their own key.
fn attach_alias(link: &Link) -> &'static str {
    match link {
        Link::Child { .. } => PARENT_ALIAS,
        _ => KEY_ALIAS,
    }
}

impl<B: Backend> Node<B> {
    /// The text of a tag column, to select the rows of a variant.
    fn tag_text(&self, row: &B::Row, tag_alias: &str) -> Result<Option<String>, Error> {
        let Some(ordinal) = B::find_column(row, tag_alias) else {
            let source = sqlx::Error::ColumnNotFound(tag_alias.to_string());
            return Err(Error::Decode { view: self.view, path: self.path_of(tag_alias), source });
        };
        match B::read_key(row, ordinal, crate::backend::KeyKind::Text) {
            Ok(Some(Key::Text(tag))) => Ok(Some(tag)),
            Ok(_) => Ok(None),
            Err(source) => Err(Error::Decode { view: self.view, path: self.path_of(tag_alias), source }),
        }
    }

    /// The indices of the rows by the key in the column with the given alias: in the order
    /// of their `$index` column if the rows have one, otherwise in row order.
    fn group(&self, alias: &str) -> Result<Groups, Error> {
        // The group of each row, then the rows of each group together
        let mut groups: HashMap<Key, usize, _> = HashMap::default();
        let mut of_row = Vec::with_capacity(self.rows.len());
        let mut sizes: Vec<usize> = Vec::new();
        for row in &self.rows {
            let group = self.key(row, alias)?.map(|key| {
                let next = groups.len();
                let group = *groups.entry(key).or_insert(next);
                if group == next {
                    sizes.push(0);
                }
                sizes[group] += 1;
                group
            });
            of_row.push(group);
        }
        let mut starts = Vec::with_capacity(sizes.len() + 1);
        let mut end = 0;
        for size in &sizes {
            starts.push(end);
            end += size;
        }
        starts.push(end);
        let mut filled = starts.clone();
        let mut rows = vec![0; end];
        for (i, group) in of_row.into_iter().enumerate() {
            if let Some(group) = group {
                rows[filled[group]] = i;
                filled[group] += 1;
            }
        }
        if self.key_columns.iter().any(|(name, _)| name == INDEX_ALIAS) {
            for pair in starts.windows(2) {
                self.place(&mut rows[pair[0]..pair[1]])?;
            }
        }
        Ok(Groups { groups, starts, rows })
    }

    /// Record the keys of the elements of a graph collection for each parent key.
    fn record_edges(
        &self,
        shape: &'static ViewShape,
        field_index: usize,
        by_key: &Groups,
        children: &Node<B>,
    ) -> Result<(), Error> {
        for (parent, indices) in by_key.iter() {
            let keys = indices
                .iter()
                .map(|&i| children.key(&children.rows[i], KEY_ALIAS))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect();
            self.identity.edges(shape, field_index, parent.clone(), keys);
        }
        Ok(())
    }

    /// Fail if the keys of the rows and the keys in their `link` column, their parents' or the
    /// keys they reference, form a cycle, which a tree cannot hold: decoding would not end.
    fn check_acyclic(&self, link: &str) -> Result<(), Error> {
        let mut parents = HashMap::new();
        for row in &self.rows {
            if let (Some(key), parent) = (self.key(row, KEY_ALIAS)?, self.key(row, link)?) {
                parents.insert(key, parent);
            }
        }
        // Follow each row's parents; reaching a row of the current walk again is a cycle
        let mut done: HashSet<&Key> = HashSet::new();
        for start in parents.keys() {
            let mut walk = HashSet::new();
            let mut current = Some(start);
            while let Some(key) = current {
                if done.contains(key) {
                    break;
                }
                if !walk.insert(key) {
                    return Err(Error::Cycle { view: self.view, path: self.path.clone() });
                }
                current = parents.get(key).and_then(Option::as_ref);
            }
            done.extend(walk);
        }
        Ok(())
    }

    /// Order the rows of one list by their `$index` column.
    fn place(&self, indices: &mut [usize]) -> Result<(), Error> {
        let mut placed = Vec::with_capacity(indices.len());
        for &i in indices.iter() {
            match self.key(&self.rows[i], INDEX_ALIAS)? {
                Some(Key::Int(index)) => placed.push((index, i)),
                _ => {
                    return Err(Error::ListIndex {
                        view: self.view,
                        path: self.path.clone(),
                        message: "an element has a NULL index".into(),
                    });
                }
            }
        }
        placed.sort_unstable();
        if let Some(pair) = placed.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(Error::ListIndex {
                view: self.view,
                path: self.path.clone(),
                message: format!("two elements of a list have the index {}", pair[0].0),
            });
        }
        for (slot, (_, i)) in indices.iter_mut().zip(placed) {
            *slot = i;
        }
        Ok(())
    }
}

/// The SQL of a query: its override, or the generated SQL, rendered for `render.keys` keys.
fn statement(
    plan: &QueryPlan,
    options: &RootOptions,
    has_keys: bool,
    active: Option<&ActiveOverride>,
    render: &Render,
) -> Result<Arc<str>, Error> {
    let view = plan.shape.name;
    let dialect = render.dialect;
    match (active, &plan.link) {
        (None, _) => Ok(sql::render(plan, &RootOptions { by_keys: has_keys, ..options.clone() }, render).into()),
        (Some(_), Link::Root) => unreachable!("a root override runs through `root_override`"),
        (Some(active), Link::Child { .. }) => {
            let own = sql::expand_keys(&active.sql, dialect, render.keys);
            let sql = sql::wrap_child(&own, plan, options, render)
                .map_err(|column| Error::ColumnNotSelected { view, column: column.to_string() })?;
            Ok(sql.into())
        }
        (Some(active), _) => Ok(sql::expand_keys(&active.sql, dialect, render.keys).into()),
    }
}

/// The most keys of one statement on MySQL and SQLite; more keys are split into several
/// statements, except for the root query.
const MAX_KEYS: usize = 1000;

/// The number of placeholders for a list of keys on MySQL and SQLite: the next power of
/// two, so that lists of similar lengths run the same statement.
fn padded(len: usize) -> usize {
    len.max(1).next_power_of_two()
}

/// Count the rows of the root query, see [`sql::count`].
pub(crate) async fn count<B: Backend>(
    conn: &mut B::Connection,
    plan: &QueryPlan,
    options: &RootOptions,
    keys: Option<KeyList>,
    overrides: Option<&Overrides>,
    values: Vec<Bound>,
) -> Result<i64, Error> {
    let view = plan.shape.name;
    let has_keys = keys.is_some();
    let (keys, render) = root_keys::<B>(keys);
    let options = RootOptions {
        by_keys: has_keys,
        filter_lists: values.iter().skip(options.params.len()).map(Bound::list_len).collect(),
        ..options.clone()
    };
    let (override_sql, keys, values) = match overrides.and_then(|o| o.get(plan.query_name())) {
        Some(active) => {
            let root = root_override::<B>(plan, active, &options, keys, &render, values)?;
            (Some((root.own, root.filter_keys)), root.keys, root.values)
        }
        None => (None, keys, values),
    };
    let sql: Arc<str> = sql::count(plan, &options, override_sql.as_ref().map(|(s, f)| (s.as_str(), *f)), &render)
        .map_err(|column| Error::ColumnNotSelected { view, column: column.to_string() })?
        .into();
    let error = |source| Error::Query { view, path: plan.path.clone(), sql: sql.to_string(), source };
    B::fetch_count(conn, sql.clone(), keys, values).await.map_err(error)
}

/// The query of the keys of the root query's rows, in its order, see [`sql::keys`]: what a
/// stream loads, a batch at a time.
pub(crate) struct KeysQuery {
    pub(crate) sql: Arc<str>,
    pub(crate) keys: Option<KeyList>,
    pub(crate) values: Vec<Bound>,
}

impl KeysQuery {
    pub(crate) fn new<B: Backend>(
        plan: &QueryPlan,
        options: &RootOptions,
        keys: Option<KeyList>,
        overrides: Option<&Overrides>,
        values: Vec<Bound>,
    ) -> Result<KeysQuery, Error> {
        let view = plan.shape.name;
        let has_keys = keys.is_some();
        let (keys, render) = root_keys::<B>(keys);
        let options = RootOptions {
            by_keys: has_keys,
            filter_lists: values.iter().skip(options.params.len()).map(Bound::list_len).collect(),
            ..options.clone()
        };
        let (override_sql, keys, values) = match overrides.and_then(|o| o.get(plan.query_name())) {
            Some(active) => {
                let root = root_override::<B>(plan, active, &options, keys, &render, values)?;
                (Some((root.own, root.filter_keys)), root.keys, root.values)
            }
            None => (None, keys, values),
        };
        let sql = sql::keys(plan, &options, override_sql.as_ref().map(|(s, f)| (s.as_str(), *f)), &render)
            .map_err(|column| Error::ColumnNotSelected { view, column: column.to_string() })?
            .into();
        Ok(KeysQuery { sql, keys, values })
    }

    /// The error of running the query.
    pub(crate) fn error(&self, plan: &QueryPlan, source: sqlx::Error) -> Error {
        Error::Query { view: plan.shape.name, path: plan.path.clone(), sql: self.sql.to_string(), source }
    }
}

/// The keys of rows of a [`KeysQuery`], without NULL ones.
pub(crate) fn read_keys<B: Backend>(plan: &QueryPlan, rows: &[B::Row]) -> Result<Vec<Key>, Error> {
    let Some(first) = rows.first() else { return Ok(Vec::new()) };
    let decode = |source| Error::Decode { view: plan.shape.name, path: plan.key_alias.clone(), source };
    let column = KeyColumn::resolve::<B>(first, &plan.key_alias).map_err(decode)?;
    let mut found = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(key) = column.read::<B>(row).map_err(decode)? {
            found.push(key);
        }
    }
    Ok(found)
}

/// The SQL of a root override, or of [`crate::Load::sql`], with its keys and named parameters
/// as placeholders, and the keys and values to bind for them.
struct RootOverride {
    own: String,
    /// The keys are bound but the SQL does not take them: they filter it from outside.
    filter_keys: bool,
    keys: Option<KeyList>,
    values: Vec<Bound>,
}

/// Expand a root override's keys and named parameters. `options` are those of the root query,
/// with `by_keys` set if keys are bound, and `values` the named parameters' values followed by
/// the filter's. PostgreSQL numbers its placeholders; MySQL and SQLite bind in the order of the
/// text, so with named parameters the keys are bound there among the values.
fn root_override<B: Backend>(
    plan: &QueryPlan,
    active: &ActiveOverride,
    options: &RootOptions,
    keys: Option<KeyList>,
    render: &Render,
    values: Vec<Bound>,
) -> Result<RootOverride, Error> {
    let view = plan.shape.name;
    let has_keys = keys.is_some();
    if active.keys_param && !has_keys {
        return Err(Error::KeysRequired { view, origin: active.origin.to_string() });
    }
    let (own, slots) = sql::expand_params(&active.sql, B::DIALECT, render.keys, &options.params, options.first_param())
        .map_err(|name| Error::Params {
            view,
            message: format!("the root query takes `:{name}`, but no value is bound to it"),
        })?;
    let filter_keys = has_keys && !active.keys_param;
    if B::DIALECT.binds_arrays() || options.params.is_empty() {
        return Ok(RootOverride { own, filter_keys, keys, values });
    }
    // The placeholders of the override, then the keys filtering it, then the filter's values
    let key_values = keys.map(KeyList::into_bound).unwrap_or_default();
    let mut ordered = Vec::with_capacity(values.len() + key_values.len());
    for slot in slots {
        match slot {
            sql::Slot::Keys => ordered.extend(key_values.iter().cloned()),
            sql::Slot::Param(index) => ordered.push(values[index].clone()),
        }
    }
    if filter_keys {
        ordered.extend(key_values);
    }
    ordered.extend(values.into_iter().skip(options.params.len()));
    Ok(RootOverride { own, filter_keys, keys: None, values: ordered })
}

/// The keys of the root query, padded on MySQL and SQLite, and how to render for them.
fn root_keys<B: Backend>(keys: Option<KeyList>) -> (Option<KeyList>, Render) {
    let render = Render::new(B::DIALECT);
    match keys {
        Some(mut keys) if !B::DIALECT.binds_arrays() => {
            let len = padded(keys.len());
            keys.pad(len);
            (Some(keys), render.with_keys(len))
        }
        keys => (keys, render),
    }
}

/// Run the query of the plan: the override if `active` is given, else the generated SQL.
async fn fetch<B: Backend>(
    conn: &mut B::Connection,
    plan: &QueryPlan,
    options: &RootOptions,
    keys: Option<KeyList>,
    values: &[Bound],
    active: Option<&ActiveOverride>,
) -> Result<Vec<B::Row>, Error> {
    let has_keys = keys.is_some();
    let root = matches!(plan.link, Link::Root);
    let mut values = values.to_vec();
    let mut options = options.clone();
    // The generated query takes no named parameters: it runs in their place in shadow mode
    if active.is_none() && !options.params.is_empty() {
        values.drain(..options.params.len());
        options.params.clear();
    }
    let options = RootOptions {
        filter_lists: values.iter().skip(options.params.len()).map(Bound::list_len).collect(),
        ..options
    };

    // MySQL and SQLite bind each key: child queries split many keys into several statements,
    // each padded so that statements are reused
    let batches: Vec<(Option<KeyList>, Render)> = match keys {
        Some(keys) if !B::DIALECT.binds_arrays() && !root => keys
            .chunks(MAX_KEYS)
            .into_iter()
            .map(|mut keys| {
                let len = padded(keys.len());
                keys.pad(len);
                (Some(keys), Render::new(B::DIALECT).with_keys(len))
            })
            .collect(),
        keys => vec![root_keys::<B>(keys)],
    };

    let mut rows = Vec::new();
    for (keys, render) in batches {
        // Generated SQL only contains quoted identifiers from the static shape of the view and
        // integer limits. Override SQL comes from configuration and was checked when the
        // registry was built. All values, including the keys, are bound parameters.
        let (sql, keys, values) = match active {
            Some(active) if root => {
                let options = RootOptions { by_keys: has_keys, ..options.clone() };
                let root = root_override::<B>(plan, active, &options, keys, &render, values.clone())?;
                let sql = sql::wrap_root(&root.own, plan, &options, root.filter_keys, &render)
                    .map_err(|column| Error::ColumnNotSelected { view: plan.shape.name, column: column.to_string() })?;
                (Arc::from(sql), root.keys, root.values)
            }
            _ => (statement(plan, &options, has_keys, active, &render)?, keys, values.clone()),
        };
        let batch = B::fetch(conn, sql.clone(), keys, values).await.map_err(|source| Error::Query {
            view: plan.shape.name,
            path: plan.path.clone(),
            sql: sql.to_string(),
            source,
        })?;
        rows.extend(batch);
    }
    Ok(rows)
}

/// The encoded values of a row, by alias.
type RowValues = Vec<Option<Vec<u8>>>;

/// Compare the rows of an override with the rows of the generated query, as they are
/// used: rows of a to-many query in order per parent, other rows by key.
fn compare<B: Backend>(plan: &QueryPlan, actual: &[B::Row], expected: &[B::Row]) -> Result<(), String> {
    // Columns an override leaves out are optional and not compared
    let aliases: Vec<&str> = plan
        .columns
        .iter()
        .map(|c| c.alias.as_str())
        .filter(|alias| actual.first().is_none_or(|row| B::find_column(row, alias).is_some()))
        .collect();
    let group_alias = match plan.link {
        Link::Child { .. } => PARENT_ALIAS,
        _ => plan.key_alias.as_str(),
    };
    let actual = grouped::<B>(actual, &aliases, group_alias)?;
    let expected = grouped::<B>(expected, &aliases, group_alias)?;
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

fn grouped<B: Backend>(
    rows: &[B::Row],
    aliases: &[&str],
    group_alias: &str,
) -> Result<HashMap<Vec<u8>, Vec<RowValues>>, String> {
    let mut groups: HashMap<Vec<u8>, Vec<RowValues>> = HashMap::new();
    for row in rows {
        let value = |alias: &str| -> Result<Option<Vec<u8>>, String> {
            let ordinal = B::find_column(row, alias).ok_or_else(|| format!("no column {alias}"))?;
            B::encoded(row, ordinal).map_err(|e| e.to_string())
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
