//! Planning the queries that fill a view.
//!
//! A view is filled by a tree of queries. The root query selects the view's table,
//! including embedded structs, enums stored in the row, and the foreign keys of to-one
//! references. Each to-many collection and each to-one reference is loaded by a child query
//! keyed by the keys collected from its parent query, which avoids both N+1 queries and the
//! cartesian product of joining collections. So is each variant of an enum stored in a table
//! per variant.
//!
//! Every selected column is aliased with its path relative to the query's view, e.g.
//! `name`, `address.city` or `status.Blocked.reason`, plus the system aliases [`KEY_ALIAS`],
//! [`PARENT_ALIAS`], [`REF_ALIAS_PREFIX`] and [`TAG_ALIAS`]. Results are decoded by alias,
//! never by position.

use std::collections::HashSet;
use std::fmt::Write;

use crate::selection::Selection;
use crate::shape::{
    Child, EmbeddedKind, EmbeddedShape, Field, FieldKind, OrderBy, Recursion, SumShape, SumStrategy, Through,
    VariantData, ViewShape,
};
use crate::{INDEX_ALIAS, KEY_ALIAS, MAP_KEY_ALIAS, PARENT_ALIAS, REF_ALIAS_PREFIX, ROOT_QUERY, TAG_ALIAS};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error(
        "{view} at `{path}` refers back to {view}; add `depth = n` or `recursive = \"cte\"` to the `child` or \
         `to_one` attribute of a field on the cycle"
    )]
    Recursive { view: &'static str, path: String },
    #[error("{view} at `{path}`: {reason}")]
    UnsupportedRecursion { view: &'static str, path: String, reason: &'static str },
    #[error("{view}: field `{field}` is an embedded value that contains a {kind}, which is not supported")]
    UnsupportedEmbedded { view: &'static str, field: String, kind: &'static str },
    #[error("{view} at `{path}`: cannot select `{field}`: {reason}")]
    Selection { view: &'static str, path: String, field: String, reason: &'static str },
}

/// A query in the plan of a view.
#[derive(Debug)]
pub struct QueryPlan {
    pub shape: &'static ViewShape,
    /// Path of the view within the root view, empty for the root.
    pub path: String,
    pub link: Link,
    /// Alias of the key column: the alias of the field that holds the key, or [`KEY_ALIAS`]
    /// if the view has no such field.
    pub key_alias: String,
    pub columns: Vec<SelectColumn>,
    /// Ordering of the rows of a child query.
    pub order_by: Vec<OrderBy>,
    /// The enums stored in the rows of this query, for strict decoding.
    pub sums: Vec<SumPlan>,
    /// For a recursive collection loaded with one query: the query selects all levels with
    /// `WITH RECURSIVE`, and its recursive field is a [`ChildQuery::Same`].
    pub cte: Option<Cte>,
    pub children: Vec<ChildPlan>,
    /// Whether each field of the view is loaded, by its index in [`ViewShape::fields`]: all
    /// of them, unless the plan is of a [`Selection`].
    pub selected: Vec<bool>,
}

/// A recursive collection or to-one reference loaded with one `WITH RECURSIVE` query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cte {
    /// The most levels to load, all levels if `None`.
    pub depth: Option<u32>,
    /// For a to-one reference, the column of each row that references the row of the next
    /// level; a collection's next level is the rows that reference it.
    pub follow: Option<&'static str>,
}

/// How a query is linked to its parent query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// The root query of the view.
    Root,
    /// A to-many collection: child rows whose `fk` column is one of the parent keys, or
    /// with `through`, rows linked to the parent rows by a link table whose `fk` column is
    /// one of the parent keys.
    Child { fk: &'static str, through: Option<Through> },
    /// A to-one reference: rows whose key is one of the values of the parent's
    /// `ref_alias` column.
    ToOne { ref_alias: String },
    /// A variant of an enum stored in a table per variant: rows whose key is the key of a
    /// parent row whose `tag_alias` column is `tag_value`.
    Variant { tag_alias: String, tag_value: &'static str },
}

#[derive(Debug)]
pub struct ChildPlan {
    /// Index of the field in the parent's [`ViewShape::fields`].
    pub field_index: usize,
    /// The variant, for a query of a variant table.
    pub variant: Option<&'static str>,
    pub query: ChildQuery,
}

/// How the rows of a child field are loaded.
#[derive(Debug)]
pub enum ChildQuery {
    /// By a query of its own.
    Query(Box<QueryPlan>),
    /// By running a query above again, for the next level of a recursive collection: the
    /// query `up` levels above this field's query (0 is that query itself). At most `depth`
    /// levels are loaded.
    Repeat { up: usize, depth: u32 },
    /// By the same query, which selects all levels of a recursive collection, see
    /// [`QueryPlan::cte`].
    Same,
}

impl ChildPlan {
    /// The query of the field, if it has one of its own.
    pub fn plan(&self) -> Option<&QueryPlan> {
        match &self.query {
            ChildQuery::Query(plan) => Some(plan),
            _ => None,
        }
    }
}

/// A selected column and its alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectColumn {
    pub column: String,
    pub alias: String,
    /// Select the column as `text`, for tag columns of any type, e.g. a PostgreSQL enum.
    pub as_text: bool,
    /// A column of the link table of a many-to-many collection, not of the view's table.
    pub from_link: bool,
}

/// An enum stored in the rows of a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SumPlan {
    /// Prefix of the aliases of the enum, e.g. `status.`.
    pub alias_prefix: String,
    pub variants: Vec<VariantPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantPlan {
    pub name: &'static str,
    /// Aliases that must be NULL in a row of this variant: the columns of the other
    /// variants that this variant does not use. Empty for a lenient enum.
    pub exclusive: Vec<String>,
}

impl SelectColumn {
    fn new(column: impl Into<String>, alias: impl Into<String>) -> SelectColumn {
        SelectColumn { column: column.into(), alias: alias.into(), as_text: false, from_link: false }
    }

    fn link(column: impl Into<String>, alias: impl Into<String>) -> SelectColumn {
        SelectColumn { from_link: true, ..SelectColumn::new(column, alias) }
    }
}

/// A query being planned, to find cycles.
struct Frame {
    /// The field leads to entities of a graph.
    graph: bool,
    /// The field or variant that leads to this query, by address; `None` for the root.
    entered_by: Option<usize>,
    /// The recursion of the collection that leads to this query.
    recursion: Option<Recursion>,
}

fn address<T>(value: &'static T) -> usize {
    std::ptr::from_ref(value) as usize
}

/// A child query to plan.
struct Entry<'s> {
    shape: &'static ViewShape,
    path: String,
    link: Link,
    order_by: Vec<OrderBy>,
    child: Option<&'static Child>,
    entered_by: usize,
    recursion: Option<Recursion>,
    graph: bool,
    /// The fields to load, all if `None`.
    selection: Option<&'s Selection>,
}

/// A query of a variant table found while adding the columns of a view.
struct VariantQuery {
    field_index: usize,
    path: String,
    tag_alias: String,
    variant: &'static str,
    tag_value: &'static str,
    shape: &'static ViewShape,
}

/// Collects the columns of the row of a query.
struct Row<'a> {
    view: &'static ViewShape,
    columns: &'a mut Vec<SelectColumn>,
    sums: &'a mut Vec<SumPlan>,
    variants: &'a mut Vec<VariantQuery>,
}

impl QueryPlan {
    /// Plan the queries for a view.
    pub fn build(shape: &'static ViewShape) -> Result<QueryPlan, PlanError> {
        let mut stack = vec![Frame { graph: false, entered_by: None, recursion: None }];
        Self::build_inner(shape, String::new(), Link::Root, Vec::new(), None, None, &mut stack)
    }

    /// Plan the queries for the selected fields of a view: only their columns are selected
    /// and only their child queries run. The key column is always selected.
    ///
    /// A selection is a tree of finite depth, so it is loaded as one: recursive collections
    /// get a query per selected level, and references into a graph are loaded as values.
    pub fn build_selected(shape: &'static ViewShape, selection: &Selection) -> Result<QueryPlan, PlanError> {
        let mut stack = vec![Frame { graph: false, entered_by: None, recursion: None }];
        Self::build_inner(shape, String::new(), Link::Root, Vec::new(), None, Some(selection), &mut stack)
    }

    /// Plan a query; its frame is the top of `stack`.
    fn build_inner(
        shape: &'static ViewShape,
        path: String,
        link: Link,
        order_by: Vec<OrderBy>,
        child: Option<&'static Child>,
        selection: Option<&Selection>,
        stack: &mut Vec<Frame>,
    ) -> Result<QueryPlan, PlanError> {
        // Every selected field is a field of the view
        if let Some(selection) = selection {
            for (name, nested) in selection.fields() {
                let error = |reason| PlanError::Selection {
                    view: shape.name,
                    path: path.clone(),
                    field: name.to_string(),
                    reason,
                };
                match shape.fields.iter().find(|f| f.name == name) {
                    // The GraphQL meta field, answered without a column
                    None if name == "__typename" => {}
                    None => return Err(error("no such field")),
                    Some(Field { kind: FieldKind::Column { .. }, .. }) if !nested.is_empty() => {
                        return Err(error("a column has no fields to select"));
                    }
                    Some(_) => {}
                }
            }
        }

        let mut columns = vec![SelectColumn::new(shape.key_column, KEY_ALIAS)];
        if let Link::Child { fk, through } = &link {
            columns.push(if through.is_some() {
                SelectColumn::link(*fk, PARENT_ALIAS)
            } else {
                SelectColumn::new(*fk, PARENT_ALIAS)
            });
        }
        if let Some(child) = child {
            let column = |name: &'static str, alias| {
                if child.through.is_some() { SelectColumn::link(name, alias) } else { SelectColumn::new(name, alias) }
            };
            if let Some(index) = child.index {
                columns.push(column(index, INDEX_ALIAS));
            }
            if let Some(key) = child.map_key {
                columns.push(column(key, MAP_KEY_ALIAS));
            }
        }

        let mut sums = Vec::new();
        let mut variants = Vec::new();
        let mut children = Vec::new();
        let mut selected = Vec::with_capacity(shape.fields.len());
        for (field_index, field) in shape.fields.iter().enumerate() {
            // The selection of the field's view, `None` for all of its fields. A view selected
            // without fields loads its columns and embedded values
            let nested = match selection {
                None => None,
                Some(selection) if selection.is_empty() => match field.kind {
                    FieldKind::Column { .. } | FieldKind::Embedded { .. } => Some(selection),
                    FieldKind::Child(_) | FieldKind::ToOne { .. } => {
                        selected.push(false);
                        continue;
                    }
                },
                Some(selection) => match selection.get(field.name) {
                    None => {
                        selected.push(false);
                        continue;
                    }
                    Some(nested) => Some(nested),
                },
            };
            selected.push(true);
            let field_path = join_path(&path, field.name);
            match &field.kind {
                FieldKind::Column { column, .. } => columns.push(SelectColumn::new(*column, field.name)),
                FieldKind::Embedded { column_prefix, shape: embedded } => {
                    let mut row = Row { view: shape, columns: &mut columns, sums: &mut sums, variants: &mut variants };
                    let top = Some((field_index, field_path.as_str()));
                    row.add_embedded(embedded(), column_prefix, &format!("{}.", field.name), top)?;
                }
                FieldKind::Child(spec) => {
                    let link = Link::Child { fk: spec.fk, through: spec.through };
                    let entry = Entry {
                        shape: (spec.shape)(),
                        path: field_path,
                        link,
                        order_by: spec.order_by.to_vec(),
                        child: Some(spec),
                        entered_by: address(field),
                        recursion: spec.recursion,
                        graph: spec.graph,
                        selection: nested,
                    };
                    children.push(ChildPlan { field_index, variant: None, query: Self::child_query(entry, stack)? });
                }
                FieldKind::ToOne { fk, shape: target, graph, recursion, .. } => {
                    let ref_alias = format!("{REF_ALIAS_PREFIX}{}", field.name);
                    columns.push(SelectColumn::new(*fk, ref_alias.clone()));
                    let entry = Entry {
                        shape: target(),
                        path: field_path,
                        link: Link::ToOne { ref_alias },
                        order_by: Vec::new(),
                        child: None,
                        entered_by: address(field),
                        recursion: *recursion,
                        graph: *graph,
                        selection: nested,
                    };
                    children.push(ChildPlan { field_index, variant: None, query: Self::child_query(entry, stack)? });
                }
            }
        }

        for query in variants {
            let entry = Entry {
                shape: query.shape,
                path: query.path,
                link: Link::Variant { tag_alias: query.tag_alias, tag_value: query.tag_value },
                order_by: Vec::new(),
                child: None,
                entered_by: address(query.shape),
                recursion: None,
                graph: false,
                selection: None,
            };
            let query_plan = Self::child_query(entry, stack)?;
            children.push(ChildPlan {
                field_index: query.field_index,
                variant: Some(query.variant),
                query: query_plan,
            });
        }

        // A recursive collection selects all levels in one query when it loads itself with it
        let same = children.iter().find(|c| matches!(c.query, ChildQuery::Same));
        let cte = match (stack.last().and_then(|f| f.recursion), same) {
            (Some(Recursion::Cte { depth }), Some(same)) => {
                let follow = match shape.fields[same.field_index].kind {
                    FieldKind::ToOne { fk, .. } => Some(fk),
                    _ => None,
                };
                Some(Cte { depth, follow })
            }
            _ => None,
        };

        // Select the key column only once when a selected field holds it
        let mut key_alias = KEY_ALIAS.to_string();
        let key_field = shape.fields.iter().zip(&selected).find(|(f, selected)| {
            **selected && matches!(f.kind, FieldKind::Column { column, .. } if column == shape.key_column)
        });
        if let Some((field, _)) = key_field {
            columns.remove(0);
            key_alias = field.name.to_string();
        }

        Ok(QueryPlan { shape, path, link, key_alias, columns, order_by, sums, cte, children, selected })
    }

    /// Plan the query of a child field, or find that it repeats a query above.
    fn child_query(mut entry: Entry<'_>, stack: &mut Vec<Frame>) -> Result<ChildQuery, PlanError> {
        // A selection has a finite depth: its levels are planned as they are selected, and
        // references into a graph are loaded as values
        if entry.selection.is_some() {
            entry.recursion = None;
            entry.graph = false;
        }
        // The same field leading to the same view again is a cycle
        let cycle = stack.iter().rposition(|f| f.entered_by == Some(entry.entered_by));
        if let Some(position) = cycle.filter(|_| entry.selection.is_none()) {
            let up = stack.len() - 1 - position;
            let recursion = entry.recursion.or_else(|| stack[position + 1..].iter().rev().find_map(|f| f.recursion));
            // A cycle through a reference into a graph ends by itself: the load never fetches an
            // entity or expands a relationship of it twice
            let graph = entry.graph || stack[position + 1..].iter().any(|f| f.graph);
            return match recursion {
                None if graph => Ok(ChildQuery::Repeat { up, depth: u32::MAX }),
                None => Err(PlanError::Recursive { view: entry.shape.name, path: entry.path }),
                Some(Recursion::Depth(depth)) => Ok(ChildQuery::Repeat { up, depth }),
                Some(Recursion::Cte { depth }) if up == 0 && entry.recursion.is_some() => {
                    if let (Link::ToOne { .. }, Some(_)) = (&entry.link, depth) {
                        return Err(PlanError::UnsupportedRecursion {
                            view: entry.shape.name,
                            path: entry.path,
                            reason: "`recursive = \"cte\"` on a reference loads the whole chain, as chains can share \
                                     rows; use `depth = n` to limit it",
                        });
                    }
                    if let Link::Child { through: Some(_), .. } = entry.link {
                        return Err(PlanError::UnsupportedRecursion {
                            view: entry.shape.name,
                            path: entry.path,
                            reason: "`recursive = \"cte\"` does not support `through`; use `depth = n`",
                        });
                    }
                    Ok(ChildQuery::Same)
                }
                Some(Recursion::Cte { .. }) => Err(PlanError::UnsupportedRecursion {
                    view: entry.shape.name,
                    path: entry.path,
                    reason: "`recursive = \"cte\"` needs a collection that contains its own view directly; \
                             use `depth = n` for a cycle through other views",
                }),
            };
        }

        stack.push(Frame { graph: entry.graph, entered_by: Some(entry.entered_by), recursion: entry.recursion });
        let plan =
            Self::build_inner(entry.shape, entry.path, entry.link, entry.order_by, entry.child, entry.selection, stack);
        stack.pop();
        Ok(ChildQuery::Query(Box::new(plan?)))
    }

    /// Name of the query in its plan: its path, or `$root` for the root query. Overrides
    /// address queries by this name.
    pub fn query_name(&self) -> &str {
        if self.path.is_empty() { ROOT_QUERY } else { &self.path }
    }

    /// Visit this query and all queries below it, parents before children.
    pub fn walk<'a>(&'a self, visit: &mut impl FnMut(&'a QueryPlan)) {
        visit(self);
        for child in &self.children {
            if let Some(plan) = child.plan() {
                plan.walk(visit);
            }
        }
    }

    /// `true` if the view or a view below it has references into a graph, so it needs to be
    /// loaded as a graph.
    pub fn has_graph_edges(&self) -> bool {
        let mut found = false;
        self.walk(&mut |plan| found |= plan.shape.fields.iter().any(|f| f.kind.is_graph_edge()));
        found
    }

    /// Number of queries in the plan, including this one.
    pub fn query_count(&self) -> usize {
        1 + self.children.iter().filter_map(ChildPlan::plan).map(QueryPlan::query_count).sum::<usize>()
    }

    /// A readable description of the plan, with the SQL of every query.
    pub fn explain(&self) -> String {
        let mut out = String::new();
        self.explain_inner(&mut out, 0);
        out
    }

    fn explain_inner(&self, out: &mut String, depth: usize) {
        let indent = "  ".repeat(depth);
        let name = self.query_name();
        let link = self.link.describe();
        let _ = writeln!(out, "{indent}{name}: {}{link}", self.shape.name);
        let _ = writeln!(out, "{indent}  {}", crate::sql::select(self, &crate::sql::RootOptions::default()));
        for child in &self.children {
            match &child.query {
                ChildQuery::Query(plan) => plan.explain_inner(out, depth + 1),
                ChildQuery::Repeat { up, depth: levels } => {
                    let field = self.shape.fields[child.field_index].name;
                    let _ =
                        writeln!(out, "{indent}  {field}: repeats the query {up} level(s) up, at most {levels} levels");
                }
                ChildQuery::Same => {
                    let field = self.shape.fields[child.field_index].name;
                    let _ = writeln!(out, "{indent}  {field}: all levels in this query");
                }
            }
        }
    }
}

impl Link {
    /// How the query is linked, for `explain`, e.g. ` (to-many by parent_id)`.
    pub fn describe(&self) -> String {
        match self {
            Link::Root => String::new(),
            Link::Child { fk, through: None } => format!(" (to-many by {fk})"),
            Link::Child { fk, through: Some(through) } => format!(" (to-many through {}.{fk})", through.table),
            Link::ToOne { ref_alias } => format!(" (to-one by {ref_alias})"),
            Link::Variant { tag_alias, tag_value } => format!(" (variant where {tag_alias} = '{tag_value}')"),
        }
    }
}

impl Row<'_> {
    /// Add the columns of an embedded struct or enum. `top` is the field index and path of
    /// the view's field when the value is not nested in another embedded value.
    fn add_embedded(
        &mut self,
        shape: &'static EmbeddedShape,
        column_prefix: &str,
        alias_prefix: &str,
        top: Option<(usize, &str)>,
    ) -> Result<(), PlanError> {
        match &shape.kind {
            EmbeddedKind::Product { fields } => {
                for field in *fields {
                    self.add_field(field, column_prefix, alias_prefix)?;
                }
                Ok(())
            }
            EmbeddedKind::Sum(sum) => self.add_sum(sum, column_prefix, alias_prefix, top),
        }
    }

    fn add_sum(
        &mut self,
        sum: &'static SumShape,
        column_prefix: &str,
        alias_prefix: &str,
        top: Option<(usize, &str)>,
    ) -> Result<(), PlanError> {
        let tag_alias = format!("{alias_prefix}{TAG_ALIAS}");
        self.columns.push(SelectColumn {
            column: format!("{column_prefix}{}", sum.tag_column),
            alias: tag_alias.clone(),
            as_text: true,
            from_link: false,
        });

        if sum.strategy == SumStrategy::TablePerVariant {
            let Some((field_index, path)) = top else {
                return Err(PlanError::UnsupportedEmbedded {
                    view: self.view.name,
                    field: alias_prefix.trim_end_matches('.').to_string(),
                    kind: "enum stored in a table per variant",
                });
            };
            for variant in sum.variants {
                if let VariantData::Table { shape } = &variant.data {
                    self.variants.push(VariantQuery {
                        field_index,
                        path: format!("{path}.{}", variant.name),
                        tag_alias: tag_alias.clone(),
                        variant: variant.name,
                        tag_value: variant.tag_value,
                        shape: shape(),
                    });
                }
            }
            return Ok(());
        }

        // The columns of each variant, to find the columns that must be NULL for the others
        let mut ranges = Vec::new();
        for variant in sum.variants {
            let start = self.columns.len();
            match &variant.data {
                VariantData::Unit => {}
                VariantData::Columns { fields } => {
                    let prefix = format!("{alias_prefix}{}.", variant.name);
                    for field in *fields {
                        self.add_field(field, column_prefix, &prefix)?;
                    }
                }
                VariantData::Table { .. } => {
                    return Err(PlanError::UnsupportedEmbedded {
                        view: self.view.name,
                        field: format!("{alias_prefix}{}", variant.name),
                        kind: "variant table in an enum stored in columns",
                    });
                }
            }
            ranges.push(start..self.columns.len());
        }

        let variants = sum
            .variants
            .iter()
            .zip(&ranges)
            .map(|(variant, own)| {
                let mut exclusive = Vec::new();
                if !sum.lenient {
                    let used: HashSet<&str> = self.columns[own.clone()].iter().map(|c| c.column.as_str()).collect();
                    for other in ranges.iter().filter(|r| *r != own) {
                        for column in &self.columns[other.clone()] {
                            if !used.contains(column.column.as_str()) && !exclusive.contains(&column.alias) {
                                exclusive.push(column.alias.clone());
                            }
                        }
                    }
                }
                VariantPlan { name: variant.name, exclusive }
            })
            .collect();
        self.sums.push(SumPlan { alias_prefix: alias_prefix.to_string(), variants });
        Ok(())
    }

    fn add_field(&mut self, field: &Field, column_prefix: &str, alias_prefix: &str) -> Result<(), PlanError> {
        match &field.kind {
            FieldKind::Column { column, .. } => {
                self.columns.push(SelectColumn::new(
                    format!("{column_prefix}{column}"),
                    format!("{alias_prefix}{}", field.name),
                ));
                Ok(())
            }
            FieldKind::Embedded { column_prefix: inner, shape } => self.add_embedded(
                shape(),
                &format!("{column_prefix}{inner}"),
                &format!("{alias_prefix}{}.", field.name),
                None,
            ),
            FieldKind::Child { .. } => Err(PlanError::UnsupportedEmbedded {
                view: self.view.name,
                field: format!("{alias_prefix}{}", field.name),
                kind: "child collection",
            }),
            FieldKind::ToOne { .. } => Err(PlanError::UnsupportedEmbedded {
                view: self.view.name,
                field: format!("{alias_prefix}{}", field.name),
                kind: "to-one reference",
            }),
        }
    }
}

fn join_path(parent: &str, field: &str) -> String {
    if parent.is_empty() { field.to_string() } else { format!("{parent}.{field}") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::{SumShape, ValueType, Variant};

    static PERSON_FIELDS: [Field; 1] =
        [Field { name: "name", kind: FieldKind::Column { column: "full_name", ty: ValueType::TEXT } }];
    static PERSON: ViewShape = ViewShape { name: "Person", table: "person", key_column: "id", fields: &PERSON_FIELDS };

    static ADDRESS_FIELDS: [Field; 2] = [
        Field { name: "street", kind: FieldKind::Column { column: "street", ty: ValueType::TEXT } },
        Field { name: "city", kind: FieldKind::Column { column: "city", ty: ValueType::TEXT } },
    ];
    static ADDRESS: EmbeddedShape =
        EmbeddedShape { name: "Address", kind: EmbeddedKind::Product { fields: &ADDRESS_FIELDS } };

    static CHILD_ORDER: [OrderBy; 1] = [OrderBy::desc("name")];
    static TASK_FIELDS: [Field; 4] = [
        Field { name: "name", kind: FieldKind::Column { column: "name", ty: ValueType::TEXT } },
        Field { name: "address", kind: FieldKind::Embedded { column_prefix: "addr_", shape: || &ADDRESS } },
        Field {
            name: "assignee",
            kind: FieldKind::ToOne {
                fk: "assignee_id",
                optional: true,
                shape: || &PERSON,
                graph: false,
                recursion: None,
            },
        },
        Field {
            name: "children",
            kind: FieldKind::Child(Child { order_by: &CHILD_ORDER, ..Child::new("parent_id", || &SUBTASK) }),
        },
    ];
    static TASK: ViewShape = ViewShape { name: "Task", table: "task", key_column: "id", fields: &TASK_FIELDS };

    static SUBTASK_FIELDS: [Field; 1] =
        [Field { name: "name", kind: FieldKind::Column { column: "name", ty: ValueType::TEXT } }];
    static SUBTASK: ViewShape = ViewShape { name: "Subtask", table: "task", key_column: "id", fields: &SUBTASK_FIELDS };

    static LOOP_FIELDS: [Field; 1] =
        [Field { name: "children", kind: FieldKind::Child(Child::new("parent_id", || &LOOP)) }];
    static LOOP: ViewShape = ViewShape { name: "Loop", table: "task", key_column: "id", fields: &LOOP_FIELDS };

    fn aliases(plan: &QueryPlan) -> Vec<&str> {
        plan.columns.iter().map(|c| c.alias.as_str()).collect()
    }

    #[test]
    fn root_columns() {
        let plan = QueryPlan::build(&TASK).unwrap();
        assert_eq!(plan.link, Link::Root);
        assert_eq!(aliases(&plan), ["$key", "name", "address.street", "address.city", "$ref.assignee"]);
        assert_eq!(plan.columns[2].column, "addr_street");
        assert_eq!(plan.query_count(), 3);
    }

    #[test]
    fn child_queries() {
        let plan = QueryPlan::build(&TASK).unwrap();
        let assignee = &plan.children[0];
        assert_eq!(assignee.field_index, 2);
        let assignee = assignee.plan().unwrap();
        assert_eq!(assignee.path, "assignee");
        assert_eq!(assignee.link, Link::ToOne { ref_alias: "$ref.assignee".into() });
        assert_eq!(aliases(assignee), ["$key", "name"]);

        let children = &plan.children[1];
        assert_eq!(children.field_index, 3);
        let children = children.plan().unwrap();
        assert_eq!(children.link, Link::Child { fk: "parent_id", through: None });
        assert_eq!(aliases(children), ["$key", "$parent", "name"]);
        assert_eq!(children.order_by, [OrderBy::desc("name")]);
    }

    static KEYED_FIELDS: [Field; 2] = [
        Field { name: "code", kind: FieldKind::Column { column: "code", ty: ValueType::TEXT } },
        Field { name: "label", kind: FieldKind::Column { column: "label", ty: ValueType::TEXT } },
    ];
    static KEYED: ViewShape = ViewShape { name: "Keyed", table: "tag", key_column: "code", fields: &KEYED_FIELDS };

    #[test]
    fn key_field_is_selected_once() {
        let plan = QueryPlan::build(&KEYED).unwrap();
        assert_eq!(plan.key_alias, "code");
        assert_eq!(aliases(&plan), ["code", "label"]);

        let plan = QueryPlan::build(&TASK).unwrap();
        assert_eq!(plan.key_alias, "$key");
    }

    #[test]
    fn selections_select_only_their_fields() {
        let selection = Selection::parse("name assignee { name }").unwrap();
        let plan = QueryPlan::build_selected(&TASK, &selection).unwrap();
        assert_eq!(aliases(&plan), ["$key", "name", "$ref.assignee"]);
        assert_eq!(plan.selected, [true, false, true, false]);
        assert_eq!(plan.query_count(), 2);

        // A view selected without fields loads its columns and embedded values
        let plan = QueryPlan::build_selected(&TASK, &Selection::parse("children").unwrap()).unwrap();
        assert_eq!(aliases(&plan), ["$key"]);
        assert_eq!(aliases(plan.children[0].plan().unwrap()), ["$key", "$parent", "name"]);
        let plan = QueryPlan::build_selected(&TASK, &Selection::new()).unwrap();
        assert_eq!(aliases(&plan), ["$key", "name", "address.street", "address.city"]);
        assert_eq!(plan.query_count(), 1);
    }

    #[test]
    fn selections_unroll_recursion() {
        // A cycle without `depth` is rejected, but a selection has a depth of its own
        let selection = Selection::parse("children { children { children } }").unwrap();
        let plan = QueryPlan::build_selected(&LOOP, &selection).unwrap();
        assert_eq!(plan.query_count(), 4);
        let names: Vec<String> = {
            let mut names = Vec::new();
            plan.walk(&mut |p| names.push(p.query_name().to_string()));
            names
        };
        assert_eq!(names, ["$root", "children", "children.children", "children.children.children"]);
    }

    #[test]
    fn selections_name_fields_of_the_view() {
        let error = QueryPlan::build_selected(&TASK, &Selection::parse("nope").unwrap()).unwrap_err();
        assert_eq!(error.to_string(), "Task at ``: cannot select `nope`: no such field");
        let error = QueryPlan::build_selected(&TASK, &Selection::parse("assignee { age }").unwrap()).unwrap_err();
        assert_eq!(error.to_string(), "Person at `assignee`: cannot select `age`: no such field");
        let error = QueryPlan::build_selected(&TASK, &Selection::parse("name { x }").unwrap()).unwrap_err();
        assert!(error.to_string().contains("a column has no fields to select"), "{error}");
        assert!(QueryPlan::build_selected(&TASK, &Selection::parse("__typename name").unwrap()).is_ok());
    }

    #[test]
    fn recursion_is_rejected() {
        let err = QueryPlan::build(&LOOP).unwrap_err();
        assert_eq!(err, PlanError::Recursive { view: "Loop", path: "children.children".into() });
    }

    #[test]
    fn explain_lists_every_query() {
        let explain = QueryPlan::build(&TASK).unwrap().explain();
        assert!(explain.contains("$root: Task"), "{explain}");
        assert!(explain.contains("assignee: Person (to-one by $ref.assignee)"), "{explain}");
        assert!(explain.contains("children: Subtask (to-many by parent_id)"), "{explain}");
    }

    #[test]
    fn query_names() {
        let plan = QueryPlan::build(&TASK).unwrap();
        let mut names = Vec::new();
        plan.walk(&mut |p| names.push(p.query_name().to_string()));
        assert_eq!(names, ["$root", "assignee", "children"]);
    }
    // enum Status { Open, Assigned { assignee: String }, Reassigned { assignee: String, by: String },
    //               Blocked { reason: Reason } } with a nested enum Reason { Waiting { on: String }, Other }
    static ASSIGNED_FIELDS: [Field; 1] =
        [Field { name: "assignee", kind: FieldKind::Column { column: "assignee", ty: ValueType::TEXT } }];
    static REASSIGNED_FIELDS: [Field; 2] = [
        Field { name: "assignee", kind: FieldKind::Column { column: "assignee", ty: ValueType::TEXT } },
        Field { name: "by", kind: FieldKind::Column { column: "reassigned_by", ty: ValueType::TEXT } },
    ];
    static WAITING_FIELDS: [Field; 1] =
        [Field { name: "on", kind: FieldKind::Column { column: "waiting_on", ty: ValueType::TEXT } }];
    static REASON_VARIANTS: [Variant; 2] = [
        Variant { name: "Waiting", tag_value: "waiting", data: VariantData::Columns { fields: &WAITING_FIELDS } },
        Variant { name: "Other", tag_value: "other", data: VariantData::Unit },
    ];
    static REASON: EmbeddedShape = EmbeddedShape {
        name: "Reason",
        kind: EmbeddedKind::Sum(SumShape {
            tag_column: "reason",
            strategy: SumStrategy::Tag,
            lenient: false,
            variants: &REASON_VARIANTS,
        }),
    };
    static BLOCKED_FIELDS: [Field; 1] =
        [Field { name: "reason", kind: FieldKind::Embedded { column_prefix: "", shape: || &REASON } }];
    static STATUS_VARIANTS: [Variant; 4] = [
        Variant { name: "Open", tag_value: "open", data: VariantData::Unit },
        Variant { name: "Assigned", tag_value: "assigned", data: VariantData::Columns { fields: &ASSIGNED_FIELDS } },
        Variant {
            name: "Reassigned",
            tag_value: "reassigned",
            data: VariantData::Columns { fields: &REASSIGNED_FIELDS },
        },
        Variant { name: "Blocked", tag_value: "blocked", data: VariantData::Columns { fields: &BLOCKED_FIELDS } },
    ];
    static STATUS: EmbeddedShape = EmbeddedShape {
        name: "Status",
        kind: EmbeddedKind::Sum(SumShape {
            tag_column: "kind",
            strategy: SumStrategy::Tag,
            lenient: false,
            variants: &STATUS_VARIANTS,
        }),
    };
    static ISSUE_FIELDS: [Field; 1] =
        [Field { name: "status", kind: FieldKind::Embedded { column_prefix: "status_", shape: || &STATUS } }];
    static ISSUE: ViewShape = ViewShape { name: "Issue", table: "issue", key_column: "id", fields: &ISSUE_FIELDS };

    #[test]
    fn enums_in_the_row() {
        let plan = QueryPlan::build(&ISSUE).unwrap();
        assert_eq!(
            aliases(&plan),
            [
                "$key",
                "status.$tag",
                "status.Assigned.assignee",
                "status.Reassigned.assignee",
                "status.Reassigned.by",
                "status.Blocked.reason.$tag",
                "status.Blocked.reason.Waiting.on",
            ]
        );
        let tag = &plan.columns[1];
        assert_eq!((tag.column.as_str(), tag.as_text), ("status_kind", true));
        assert_eq!(plan.columns[6].column, "status_waiting_on");
        assert_eq!(plan.query_count(), 1);

        let sql = crate::sql::select(&plan, &crate::sql::RootOptions::default());
        assert!(sql.contains("t0.\"status_kind\"::text AS \"status.$tag\""), "{sql}");
    }

    #[test]
    fn columns_of_other_variants_must_be_null() {
        let plan = QueryPlan::build(&ISSUE).unwrap();
        // Nested enums are planned before the enum that contains them
        let [reason, status] = plan.sums.as_slice() else { panic!("{:?}", plan.sums) };
        assert_eq!(reason.alias_prefix, "status.Blocked.reason.");
        assert_eq!(reason.variants[1].exclusive, ["status.Blocked.reason.Waiting.on"]);

        assert_eq!(status.alias_prefix, "status.");
        let exclusive = |name: &str| &status.variants.iter().find(|v| v.name == name).unwrap().exclusive;
        assert_eq!(
            exclusive("Open"),
            &[
                "status.Assigned.assignee",
                "status.Reassigned.assignee",
                "status.Reassigned.by",
                "status.Blocked.reason.$tag",
                "status.Blocked.reason.Waiting.on",
            ]
        );
        // The assignee column is shared by Assigned and Reassigned
        assert_eq!(
            exclusive("Assigned"),
            &["status.Reassigned.by", "status.Blocked.reason.$tag", "status.Blocked.reason.Waiting.on"]
        );
        assert_eq!(
            exclusive("Blocked"),
            &["status.Assigned.assignee", "status.Reassigned.assignee", "status.Reassigned.by"]
        );
    }

    // enum Payment { Card { last4 } in card_payment, Cash } stored in a table per variant
    static CARD_FIELDS: [Field; 1] =
        [Field { name: "last4", kind: FieldKind::Column { column: "last4", ty: ValueType::TEXT } }];
    static CARD: ViewShape =
        ViewShape { name: "Payment::Card", table: "card_payment", key_column: "payment_id", fields: &CARD_FIELDS };
    static PAYMENT_VARIANTS: [Variant; 2] = [
        Variant { name: "Card", tag_value: "card", data: VariantData::Table { shape: || &CARD } },
        Variant { name: "Cash", tag_value: "cash", data: VariantData::Unit },
    ];
    static PAYMENT: EmbeddedShape = EmbeddedShape {
        name: "Payment",
        kind: EmbeddedKind::Sum(SumShape {
            tag_column: "kind",
            strategy: SumStrategy::TablePerVariant,
            lenient: false,
            variants: &PAYMENT_VARIANTS,
        }),
    };
    static ORDER_FIELDS: [Field; 1] =
        [Field { name: "payment", kind: FieldKind::Embedded { column_prefix: "payment_", shape: || &PAYMENT } }];
    static ORDER_VIEW: ViewShape =
        ViewShape { name: "Order", table: "orders", key_column: "id", fields: &ORDER_FIELDS };

    #[test]
    fn variant_tables_are_child_queries() {
        let plan = QueryPlan::build(&ORDER_VIEW).unwrap();
        assert_eq!(aliases(&plan), ["$key", "payment.$tag"]);
        assert!(plan.sums.is_empty());
        let [card] = plan.children.as_slice() else { panic!() };
        assert_eq!((card.field_index, card.variant), (0, Some("Card")));
        let card = card.plan().unwrap();
        assert_eq!(card.query_name(), "payment.Card");
        assert_eq!(card.link, Link::Variant { tag_alias: "payment.$tag".into(), tag_value: "card" });
        assert_eq!(aliases(card), ["$key", "last4"]);
        assert_eq!(
            crate::sql::select(card, &crate::sql::RootOptions::default()),
            "SELECT t0.\"payment_id\" AS \"$key\", t0.\"last4\" AS \"last4\" FROM \"card_payment\" AS t0 \
             WHERE t0.\"payment_id\" = ANY($1) ORDER BY t0.\"payment_id\""
        );
        assert!(plan.explain().contains("payment.Card: Payment::Card (variant where payment.$tag = 'card')"));
    }

    static WRAPPER_FIELDS: [Field; 1] =
        [Field { name: "payment", kind: FieldKind::Embedded { column_prefix: "", shape: || &PAYMENT } }];
    static WRAPPER: EmbeddedShape =
        EmbeddedShape { name: "Wrapper", kind: EmbeddedKind::Product { fields: &WRAPPER_FIELDS } };
    static WRAPPED_FIELDS: [Field; 1] =
        [Field { name: "wrapper", kind: FieldKind::Embedded { column_prefix: "", shape: || &WRAPPER } }];
    static WRAPPED: ViewShape =
        ViewShape { name: "Wrapped", table: "orders", key_column: "id", fields: &WRAPPED_FIELDS };

    #[test]
    fn variant_tables_need_to_be_fields_of_a_view() {
        let err = QueryPlan::build(&WRAPPED).unwrap_err();
        assert_eq!(
            err,
            PlanError::UnsupportedEmbedded {
                view: "Wrapped",
                field: "wrapper.payment".into(),
                kind: "enum stored in a table per variant"
            }
        );
    }
    // Lists placed by an index on a link table, and maps keyed by a column
    static DEPENDANT_FIELDS: [Field; 1] =
        [Field { name: "name", kind: FieldKind::Column { column: "name", ty: ValueType::TEXT } }];
    static DEPENDANT: ViewShape =
        ViewShape { name: "Dependant", table: "task", key_column: "id", fields: &DEPENDANT_FIELDS };
    static PROJECT_FIELDS: [Field; 2] = [
        Field {
            name: "dependants",
            kind: FieldKind::Child(Child {
                through: Some(Through { table: "task_dependant", target: "dependant_id" }),
                index: Some("seq"),
                ..Child::new("task_id", || &DEPENDANT)
            }),
        },
        Field {
            name: "by_name",
            kind: FieldKind::Child(Child { map_key: Some("name"), ..Child::new("project_id", || &DEPENDANT) }),
        },
    ];
    static PROJECT: ViewShape = ViewShape { name: "Project", table: "task", key_column: "id", fields: &PROJECT_FIELDS };

    #[test]
    fn link_tables_indices_and_map_keys() {
        let plan = QueryPlan::build(&PROJECT).unwrap();
        let dependants = plan.children[0].plan().unwrap();
        assert_eq!(aliases(dependants), ["$key", "$parent", "$index", "name"]);
        let from_link: Vec<bool> = dependants.columns.iter().map(|c| c.from_link).collect();
        assert_eq!(from_link, [false, true, true, false]);
        assert_eq!(
            crate::sql::select(dependants, &crate::sql::RootOptions::default()),
            "SELECT t0.\"id\" AS \"$key\", j.\"task_id\" AS \"$parent\", j.\"seq\" AS \"$index\", t0.\"name\" AS \"name\" \
             FROM \"task\" AS t0 JOIN \"task_dependant\" AS j ON j.\"dependant_id\" = t0.\"id\" \
             WHERE j.\"task_id\" = ANY($1) ORDER BY t0.\"id\""
        );
        assert!(plan.explain().contains("dependants: Dependant (to-many through task_dependant.task_id)"));

        let by_name = plan.children[1].plan().unwrap();
        assert_eq!(aliases(by_name), ["$key", "$parent", "$map_key", "name"]);
        assert_eq!(by_name.columns[2].column, "name");
    }

    // struct Tree { children: Vec<Tree> } loaded level by level, and with one query
    static TREE_FIELDS: [Field; 2] = [
        Field { name: "name", kind: FieldKind::Column { column: "name", ty: ValueType::TEXT } },
        Field {
            name: "children",
            kind: FieldKind::Child(Child { recursion: Some(Recursion::Depth(3)), ..Child::new("parent_id", || &TREE) }),
        },
    ];
    static TREE: ViewShape = ViewShape { name: "Tree", table: "task", key_column: "id", fields: &TREE_FIELDS };
    static CTE_TREE_FIELDS: [Field; 1] = [Field {
        name: "children",
        kind: FieldKind::Child(Child {
            recursion: Some(Recursion::Cte { depth: Some(10) }),
            ..Child::new("parent_id", || &CTE_TREE)
        }),
    }];
    static CTE_TREE: ViewShape =
        ViewShape { name: "CteTree", table: "task", key_column: "id", fields: &CTE_TREE_FIELDS };

    // struct Crumb { parent: Option<Box<Crumb>> }, level by level, in one query, and both
    static CRUMB_FIELDS: [Field; 1] = [Field {
        name: "parent",
        kind: FieldKind::ToOne {
            fk: "parent_id",
            optional: true,
            shape: || &CRUMB,
            graph: false,
            recursion: Some(Recursion::Depth(2)),
        },
    }];
    static CRUMB: ViewShape = ViewShape { name: "Crumb", table: "category", key_column: "id", fields: &CRUMB_FIELDS };
    static CTE_CRUMB_FIELDS: [Field; 1] = [Field {
        name: "parent",
        kind: FieldKind::ToOne {
            fk: "parent_id",
            optional: true,
            shape: || &CTE_CRUMB,
            graph: false,
            recursion: Some(Recursion::Cte { depth: None }),
        },
    }];
    static CTE_CRUMB: ViewShape =
        ViewShape { name: "CteCrumb", table: "category", key_column: "id", fields: &CTE_CRUMB_FIELDS };
    static LIMITED_CRUMB_FIELDS: [Field; 1] = [Field {
        name: "parent",
        kind: FieldKind::ToOne {
            fk: "parent_id",
            optional: true,
            shape: || &LIMITED_CRUMB,
            graph: false,
            recursion: Some(Recursion::Cte { depth: Some(3) }),
        },
    }];
    static LIMITED_CRUMB: ViewShape =
        ViewShape { name: "LimitedCrumb", table: "category", key_column: "id", fields: &LIMITED_CRUMB_FIELDS };

    #[test]
    fn recursive_references() {
        let plan = QueryPlan::build(&CRUMB).unwrap();
        let level = plan.children[0].plan().unwrap();
        assert_eq!(level.link, Link::ToOne { ref_alias: "$ref.parent".into() });
        assert!(matches!(level.children[0].query, ChildQuery::Repeat { up: 0, depth: 2 }));

        let plan = QueryPlan::build(&CTE_CRUMB).unwrap();
        let level = plan.children[0].plan().unwrap();
        assert_eq!(level.cte, Some(Cte { depth: None, follow: Some("parent_id") }));
        assert!(matches!(level.children[0].query, ChildQuery::Same));
        assert_eq!(
            crate::sql::select(level, &crate::sql::RootOptions::default()),
            "WITH RECURSIVE \"$tree\" AS ( SELECT t0.\"id\" AS \"k\", t0.\"parent_id\" AS \"n\", ARRAY[t0.\"id\"] AS \"path\", \
             1 AS \"depth\" FROM \"category\" AS t0 WHERE t0.\"id\" = ANY($1) UNION ALL SELECT t0.\"id\", \
             t0.\"parent_id\", r.\"path\" || t0.\"id\", r.\"depth\" + 1 FROM \"category\" AS t0 JOIN \"$tree\" AS r \
             ON t0.\"id\" = r.\"n\" WHERE t0.\"id\" <> ALL(r.\"path\") ) SELECT t0.\"id\" AS \"$key\", \
             t0.\"parent_id\" AS \"$ref.parent\" FROM (SELECT DISTINCT \"k\" FROM \"$tree\") AS r JOIN \"category\" AS t0 \
             ON t0.\"id\" = r.\"k\" ORDER BY t0.\"id\""
        );

        // Chains share rows, so a depth in one query would not be each chain's
        let error = QueryPlan::build(&LIMITED_CRUMB).unwrap_err();
        assert!(matches!(error, PlanError::UnsupportedRecursion { .. }), "{error}");
    }

    #[test]
    fn recursive_collections_by_level() {
        let plan = QueryPlan::build(&TREE).unwrap();
        assert_eq!(plan.query_count(), 2);
        let level = plan.children[0].plan().unwrap();
        assert_eq!(level.query_name(), "children");
        assert!(level.cte.is_none());
        assert!(matches!(level.children[0].query, ChildQuery::Repeat { up: 0, depth: 3 }));
        assert!(plan.explain().contains("children: repeats the query 0 level(s) up, at most 3 levels"));
    }

    #[test]
    fn recursive_collections_in_one_query() {
        let plan = QueryPlan::build(&CTE_TREE).unwrap();
        let level = plan.children[0].plan().unwrap();
        assert_eq!(level.cte, Some(Cte { depth: Some(10), follow: None }));
        assert!(matches!(level.children[0].query, ChildQuery::Same));
        assert_eq!(
            crate::sql::select(level, &crate::sql::RootOptions::default()),
            "WITH RECURSIVE \"$tree\" AS ( SELECT t0.\"id\" AS \"k\", ARRAY[t0.\"id\"] AS \"path\", 1 AS \"depth\" \
             FROM \"task\" AS t0 WHERE t0.\"parent_id\" = ANY($1) UNION ALL SELECT t0.\"id\", r.\"path\" || t0.\"id\", \
             r.\"depth\" + 1 FROM \"task\" AS t0 JOIN \"$tree\" AS r ON t0.\"parent_id\" = r.\"k\" \
             WHERE t0.\"id\" <> ALL(r.\"path\") AND r.\"depth\" < 10 ) \
             SELECT t0.\"id\" AS \"$key\", t0.\"parent_id\" AS \"$parent\" FROM (SELECT DISTINCT \"k\" FROM \"$tree\") AS r \
             JOIN \"task\" AS t0 ON t0.\"id\" = r.\"k\" ORDER BY t0.\"id\""
        );
    }

    // A -> bs -> B -> as (depth 4) -> A: a cycle through two views
    static A_FIELDS: [Field; 1] = [Field { name: "bs", kind: FieldKind::Child(Child::new("a_id", || &B)) }];
    static A: ViewShape = ViewShape { name: "A", table: "a", key_column: "id", fields: &A_FIELDS };
    static B_FIELDS: [Field; 1] = [Field {
        name: "as",
        kind: FieldKind::Child(Child { recursion: Some(Recursion::Depth(4)), ..Child::new("b_id", || &A) }),
    }];
    static B: ViewShape = ViewShape { name: "B", table: "b", key_column: "id", fields: &B_FIELDS };
    static C_FIELDS: [Field; 1] = [Field {
        name: "ds",
        kind: FieldKind::Child(Child { recursion: Some(Recursion::Cte { depth: None }), ..Child::new("c_id", || &D) }),
    }];
    static C: ViewShape = ViewShape { name: "C", table: "c", key_column: "id", fields: &C_FIELDS };
    static D_FIELDS: [Field; 1] = [Field { name: "cs", kind: FieldKind::Child(Child::new("d_id", || &C)) }];
    static D: ViewShape = ViewShape { name: "D", table: "d", key_column: "id", fields: &D_FIELDS };

    #[test]
    fn cycles_through_several_views() {
        let plan = QueryPlan::build(&A).unwrap();
        let b = plan.children[0].plan().unwrap();
        let a = b.children[0].plan().unwrap();
        assert_eq!(a.query_name(), "bs.as");
        // bs is entered again: repeat the query of B, one level up from A
        assert!(matches!(a.children[0].query, ChildQuery::Repeat { up: 1, depth: 4 }));

        let err = QueryPlan::build(&C).unwrap_err();
        assert!(matches!(err, PlanError::UnsupportedRecursion { view: "D", .. }), "{err}");
    }
    // struct Node { parent: Option<Ref<Node>>, children: Vec<Ref<Node>> }: cycles of a graph
    static NODE_FIELDS: [Field; 2] = [
        Field {
            name: "parent",
            kind: FieldKind::ToOne { fk: "parent_id", optional: true, shape: || &NODE, graph: true, recursion: None },
        },
        Field { name: "children", kind: FieldKind::Child(Child { graph: true, ..Child::new("parent_id", || &NODE) }) },
    ];
    static NODE: ViewShape = ViewShape { name: "Node", table: "task", key_column: "id", fields: &NODE_FIELDS };

    #[test]
    fn graph_cycles_need_no_annotation() {
        let plan = QueryPlan::build(&NODE).unwrap();
        assert!(plan.has_graph_edges());
        assert!(!QueryPlan::build(&TASK).unwrap().has_graph_edges());
        let parent = plan.children[0].plan().unwrap();
        assert!(matches!(parent.children[0].query, ChildQuery::Repeat { up: 0, depth: u32::MAX }));
        // parent.children enters children for the first time, children.children repeats it
        let children = parent.children[1].plan().unwrap();
        assert_eq!(children.query_name(), "parent.children");
        assert!(matches!(children.children[1].query, ChildQuery::Repeat { up: 0, depth: u32::MAX }));
        // the root, parent, parent.children, children and children.parent; deeper levels repeat these
        assert_eq!(plan.query_count(), 5);
    }
}
