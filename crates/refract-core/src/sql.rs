//! Rendering a query plan as PostgreSQL.
//!
//! The SQL is built from the plan, never by editing SQL text. Every identifier is quoted,
//! and keys are always bound as the array parameter `$1`.

use std::fmt::Write;

use crate::filter::Filter;
use crate::plan::{Link, QueryPlan};
use crate::shape::OrderBy;

/// Table alias of the entity selected by a query.
const TABLE_ALIAS: &str = "t0";

/// Options of the root query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootOptions {
    /// Restrict the root rows to the keys bound as `$1`.
    pub by_keys: bool,
    /// Restrict the root rows to the rows that match. Its values are bound after the keys.
    pub filter: Option<Filter>,
    pub order_by: Vec<OrderBy>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

impl RootOptions {
    /// The placeholder number of the first filter value: `$2` after the keys, else `$1`.
    pub fn first_filter_param(&self) -> usize {
        if self.by_keys { 2 } else { 1 }
    }

    /// The WHERE clause of the root query, without `WHERE`, or `None` if there are no
    /// conditions. `key` is the key column reference.
    fn conditions<'a>(
        &'a self,
        key: &str,
        filter_keys: bool,
        column: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Option<String>, &'a str> {
        let mut conditions = Vec::new();
        if filter_keys {
            conditions.push(format!("{key} = ANY($1)"));
        }
        if let Some(filter) = &self.filter {
            let mut rendered = String::new();
            filter.render(&mut rendered, column, self.first_filter_param())?;
            conditions.push(if conditions.is_empty() { rendered } else { format!("({rendered})") });
        }
        Ok((!conditions.is_empty()).then(|| conditions.join(" AND ")))
    }
}

fn table_column(column: &str) -> Option<String> {
    Some(format!("{TABLE_ALIAS}.{}", quote_ident(column)))
}

/// Quote an identifier for PostgreSQL.
pub fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// How a statement is laid out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Layout {
    /// On one line, for running and logging.
    #[default]
    Line,
    /// One column per line and one clause per line, for reading and editing.
    Multiline,
}

/// Render the SELECT statement of a query in the plan.
///
/// A child or to-one query always filters on the keys bound as `$1`. The root query does
/// so only when [`RootOptions::by_keys`] is set.
pub fn select(plan: &QueryPlan, root: &RootOptions) -> String {
    select_with(plan, root, Layout::Line)
}

/// Render the SELECT statement of a query in the plan with the given layout.
pub fn select_with(plan: &QueryPlan, root: &RootOptions, layout: Layout) -> String {
    let (column_separator, clause) = match layout {
        Layout::Line => (", ", " "),
        Layout::Multiline => (",\n       ", "\n"),
    };
    let mut sql = String::from("SELECT ");
    for (i, column) in plan.columns.iter().enumerate() {
        if i > 0 {
            sql.push_str(column_separator);
        }
        let cast = if column.as_text { "::text" } else { "" };
        let _ = write!(sql, "{TABLE_ALIAS}.{}{cast} AS {}", quote_ident(&column.column), quote_ident(&column.alias));
    }
    let _ = write!(sql, "{clause}FROM {} AS {TABLE_ALIAS}", quote_ident(plan.shape.table));

    let key = quote_ident(plan.shape.key_column);
    let order_by: &[OrderBy] = match &plan.link {
        Link::Root => {
            let key = format!("{TABLE_ALIAS}.{key}");
            if let Ok(Some(conditions)) = root.conditions(&key, root.by_keys, &table_column) {
                let _ = write!(sql, "{clause}WHERE {conditions}");
            }
            &root.order_by
        }
        Link::Child { fk } => {
            let _ = write!(sql, "{clause}WHERE {TABLE_ALIAS}.{} = ANY($1)", quote_ident(fk));
            &plan.order_by
        }
        Link::ToOne { .. } | Link::Variant { .. } => {
            let _ = write!(sql, "{clause}WHERE {TABLE_ALIAS}.{key} = ANY($1)");
            &[]
        }
    };

    let mut terms: Vec<String> = order_by
        .iter()
        .map(|o| format!("{TABLE_ALIAS}.{}{}", quote_ident(o.column), if o.descending { " DESC" } else { "" }))
        .collect();
    // Child rows are ordered deterministically, ending with the key
    if !matches!(plan.link, Link::Root) && !order_by.iter().any(|o| o.column == plan.shape.key_column) {
        terms.push(format!("{TABLE_ALIAS}.{key}"));
    }
    if !terms.is_empty() {
        let _ = write!(sql, "{clause}ORDER BY {}", terms.join(", "));
    }

    if let Link::Root = plan.link {
        if let Some(limit) = root.limit {
            let _ = write!(sql, "{clause}LIMIT {limit}");
        }
        if let Some(offset) = root.offset {
            let _ = write!(sql, "{clause}OFFSET {offset}");
        }
    }

    sql
}

/// Table alias of an override used as a subquery.
const OVERRIDE_ALIAS: &str = "o";

/// Apply the root options to the SQL of an override of the root query.
///
/// The override becomes a subquery, so ordering and filters refer to its column aliases:
/// each column of [`RootOptions::order_by`] and [`RootOptions::filter`] is mapped to the
/// alias it is selected as. With `filter_keys`, the rows are restricted to the keys bound
/// as `$1`; this is for an override that does not take the keys as a parameter itself.
/// When there is nothing to apply, the override is returned as is.
///
/// Returns a column that the plan does not select as the error.
pub fn wrap_root<'a>(sql: &str, plan: &QueryPlan, root: &'a RootOptions, filter_keys: bool) -> Result<String, &'a str> {
    let sql = trim_statement(sql);
    if !filter_keys
        && root.filter.is_none()
        && root.order_by.is_empty()
        && root.limit.is_none()
        && root.offset.is_none()
    {
        return Ok(sql.to_string());
    }

    let mut wrapped = format!("SELECT * FROM ({sql}\n) AS {OVERRIDE_ALIAS}");
    let column = |name: &str| override_column(plan, name);
    let key = format!("{OVERRIDE_ALIAS}.{}", quote_ident(&plan.key_alias));
    if let Some(conditions) = root.conditions(&key, filter_keys, &column)? {
        let _ = write!(wrapped, " WHERE {conditions}");
    }
    let mut terms = Vec::new();
    for order in &root.order_by {
        let column = column(order.column).ok_or(order.column)?;
        terms.push(format!("{column}{}", if order.descending { " DESC" } else { "" }));
    }
    if !terms.is_empty() {
        let _ = write!(wrapped, " ORDER BY {}", terms.join(", "));
    }
    if let Some(limit) = root.limit {
        let _ = write!(wrapped, " LIMIT {limit}");
    }
    if let Some(offset) = root.offset {
        let _ = write!(wrapped, " OFFSET {offset}");
    }
    Ok(wrapped)
}

/// Count the rows of the root query that match the keys and the filter. Ordering and
/// paging are ignored. With `override_sql`, the override is counted as a subquery, and
/// `filter_keys` says whether the keys are applied to it, as in [`wrap_root`].
///
/// Returns a column that cannot be referred to as the error.
pub fn count<'a>(
    plan: &QueryPlan,
    root: &'a RootOptions,
    override_sql: Option<(&str, bool)>,
) -> Result<String, &'a str> {
    match override_sql {
        None => {
            let mut sql = format!("SELECT count(*) FROM {} AS {TABLE_ALIAS}", quote_ident(plan.shape.table));
            let key = format!("{TABLE_ALIAS}.{}", quote_ident(plan.shape.key_column));
            if let Some(conditions) = root.conditions(&key, root.by_keys, &table_column)? {
                let _ = write!(sql, " WHERE {conditions}");
            }
            Ok(sql)
        }
        Some((override_sql, filter_keys)) => {
            let mut sql = format!("SELECT count(*) FROM ({}\n) AS {OVERRIDE_ALIAS}", trim_statement(override_sql));
            let key = format!("{OVERRIDE_ALIAS}.{}", quote_ident(&plan.key_alias));
            if let Some(conditions) = root.conditions(&key, filter_keys, &|name| override_column(plan, name))? {
                let _ = write!(sql, " WHERE {conditions}");
            }
            Ok(sql)
        }
    }
}

/// The reference to a column of the view's table in an override used as a subquery: the
/// alias it is selected as.
fn override_column(plan: &QueryPlan, column: &str) -> Option<String> {
    let selected = plan.columns.iter().find(|c| c.column == column)?;
    Some(format!("{OVERRIDE_ALIAS}.{}", quote_ident(&selected.alias)))
}

fn trim_statement(sql: &str) -> &str {
    sql.trim_end().trim_end_matches(';').trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::{Field, FieldKind, ViewShape};

    static ITEM_FIELDS: [Field; 1] = [Field { name: "label", kind: FieldKind::Column { column: "my \"label\"" } }];
    static ITEM: ViewShape = ViewShape { name: "Item", table: "item", key_column: "id", fields: &ITEM_FIELDS };

    static ORDER: [OrderBy; 1] = [OrderBy::asc("position")];
    static LIST_FIELDS: [Field; 1] =
        [Field { name: "items", kind: FieldKind::Child { fk: "list_id", order_by: &ORDER, shape: || &ITEM } }];
    static LIST: ViewShape = ViewShape { name: "List", table: "list", key_column: "id", fields: &LIST_FIELDS };

    #[test]
    fn quoting() {
        assert_eq!(quote_ident("name"), "\"name\"");
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn root_select() {
        let plan = QueryPlan::build(&ITEM).unwrap();
        assert_eq!(
            select(&plan, &RootOptions::default()),
            "SELECT t0.\"id\" AS \"$key\", t0.\"my \"\"label\"\"\" AS \"label\" FROM \"item\" AS t0"
        );
    }

    #[test]
    fn root_by_keys_with_paging() {
        let plan = QueryPlan::build(&ITEM).unwrap();
        let options = RootOptions {
            by_keys: true,
            order_by: vec![OrderBy::desc("id")],
            limit: Some(10),
            offset: Some(20),
            ..RootOptions::default()
        };
        assert_eq!(
            select(&plan, &options),
            "SELECT t0.\"id\" AS \"$key\", t0.\"my \"\"label\"\"\" AS \"label\" FROM \"item\" AS t0 \
             WHERE t0.\"id\" = ANY($1) ORDER BY t0.\"id\" DESC LIMIT 10 OFFSET 20"
        );
    }

    #[test]
    fn multiline_layout() {
        let plan = QueryPlan::build(&LIST).unwrap();
        assert_eq!(
            select_with(&plan.children[0].plan, &RootOptions::default(), Layout::Multiline),
            "SELECT t0.\"id\" AS \"$key\",\n       t0.\"list_id\" AS \"$parent\",\n       \
             t0.\"my \"\"label\"\"\" AS \"label\"\nFROM \"item\" AS t0\nWHERE t0.\"list_id\" = ANY($1)\n\
             ORDER BY t0.\"position\", t0.\"id\""
        );
    }

    #[test]
    fn wrapped_root_override() {
        let plan = QueryPlan::build(&ITEM).unwrap();
        let sql = "SELECT id AS \"$key\", label AS \"label\" FROM item;\n";
        assert_eq!(
            wrap_root(sql, &plan, &RootOptions::default(), false),
            Ok(sql.trim_end().trim_end_matches(';').into())
        );

        let options = RootOptions {
            by_keys: true,
            order_by: vec![OrderBy::desc("my \"label\"")],
            limit: Some(5),
            ..RootOptions::default()
        };
        assert_eq!(
            wrap_root(sql, &plan, &options, true),
            Ok("SELECT * FROM (SELECT id AS \"$key\", label AS \"label\" FROM item\n) AS o \
                WHERE o.\"$key\" = ANY($1) ORDER BY o.\"label\" DESC LIMIT 5"
                .into())
        );

        let options = RootOptions { order_by: vec![OrderBy::asc("nope")], ..RootOptions::default() };
        assert_eq!(wrap_root(sql, &plan, &options, false), Err("nope"));
    }

    #[test]
    fn root_filters() {
        use crate::filter::{CompareOp, Filter};
        let plan = QueryPlan::build(&ITEM).unwrap();
        let filter = Filter::Or(vec![
            Filter::Compare { column: "my \"label\"".into(), op: CompareOp::Eq, param: 0 },
            Filter::Null { column: "id".into(), negated: false },
        ]);
        let options = RootOptions { by_keys: true, filter: Some(filter.clone()), ..RootOptions::default() };
        assert_eq!(
            select(&plan, &options),
            "SELECT t0.\"id\" AS \"$key\", t0.\"my \"\"label\"\"\" AS \"label\" FROM \"item\" AS t0 \
             WHERE t0.\"id\" = ANY($1) AND ((t0.\"my \"\"label\"\"\" = $2) OR (t0.\"id\" IS NULL))"
        );
        assert_eq!(
            count(&plan, &options, None),
            Ok("SELECT count(*) FROM \"item\" AS t0 \
                WHERE t0.\"id\" = ANY($1) AND ((t0.\"my \"\"label\"\"\" = $2) OR (t0.\"id\" IS NULL))"
                .into())
        );

        // Without keys, the filter values start at $1; in an override, columns are aliases
        let options = RootOptions { filter: Some(filter), ..RootOptions::default() };
        assert_eq!(
            wrap_root("SELECT 1;", &plan, &options, false),
            Ok("SELECT * FROM (SELECT 1\n) AS o WHERE (o.\"label\" = $1) OR (o.\"$key\" IS NULL)".into())
        );
        assert_eq!(
            count(&plan, &options, Some(("SELECT 1", false))),
            Ok("SELECT count(*) FROM (SELECT 1\n) AS o WHERE (o.\"label\" = $1) OR (o.\"$key\" IS NULL)".into())
        );

        let options = RootOptions {
            filter: Some(Filter::Null { column: "nope".into(), negated: false }),
            ..RootOptions::default()
        };
        assert_eq!(wrap_root("SELECT 1", &plan, &options, false), Err("nope"));
    }

    #[test]
    fn child_select() {
        let plan = QueryPlan::build(&LIST).unwrap();
        let child = &plan.children[0].plan;
        // root options do not apply to child queries
        let options = RootOptions { limit: Some(1), ..RootOptions::default() };
        assert_eq!(
            select(child, &options),
            "SELECT t0.\"id\" AS \"$key\", t0.\"list_id\" AS \"$parent\", t0.\"my \"\"label\"\"\" AS \"label\" \
             FROM \"item\" AS t0 WHERE t0.\"list_id\" = ANY($1) ORDER BY t0.\"position\", t0.\"id\""
        );
    }
}
