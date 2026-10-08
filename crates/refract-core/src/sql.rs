//! Rendering a query plan as PostgreSQL.
//!
//! The SQL is built from the plan, never by editing SQL text. Every identifier is quoted,
//! and keys are always bound as the array parameter `$1`.

use std::fmt::Write;

use crate::plan::{Link, QueryPlan};
use crate::shape::OrderBy;

/// Table alias of the entity selected by a query.
const TABLE_ALIAS: &str = "t0";

/// Options of the root query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootOptions {
    /// Restrict the root rows to the keys bound as `$1`.
    pub by_keys: bool,
    pub order_by: Vec<OrderBy>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
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
            if root.by_keys {
                let _ = write!(sql, "{clause}WHERE {TABLE_ALIAS}.{key} = ANY($1)");
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
/// The override becomes a subquery, so ordering refers to its column aliases: each
/// [`RootOptions::order_by`] column is mapped to the alias it is selected as. With
/// `filter_keys`, the rows are restricted to the keys bound as `$1`; this is for an
/// override that does not take the keys as a parameter itself. When there is nothing to
/// apply, the override is returned as is.
///
/// Returns the order by column that is not selected by the plan as the error.
pub fn wrap_root<'a>(sql: &str, plan: &QueryPlan, root: &'a RootOptions, filter_keys: bool) -> Result<String, &'a str> {
    let sql = sql.trim_end().trim_end_matches(';').trim_end();
    if !filter_keys && root.order_by.is_empty() && root.limit.is_none() && root.offset.is_none() {
        return Ok(sql.to_string());
    }

    let mut wrapped = format!("SELECT * FROM ({sql}\n) AS {OVERRIDE_ALIAS}");
    if filter_keys {
        let _ = write!(wrapped, " WHERE {OVERRIDE_ALIAS}.{} = ANY($1)", quote_ident(&plan.key_alias));
    }
    let mut terms = Vec::new();
    for order in &root.order_by {
        let alias = plan.columns.iter().find(|c| c.column == order.column).ok_or(order.column)?;
        terms.push(format!(
            "{OVERRIDE_ALIAS}.{}{}",
            quote_ident(&alias.alias),
            if order.descending { " DESC" } else { "" }
        ));
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
        let options =
            RootOptions { by_keys: true, order_by: vec![OrderBy::desc("id")], limit: Some(10), offset: Some(20) };
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

        let options =
            RootOptions { by_keys: true, order_by: vec![OrderBy::desc("my \"label\"")], limit: Some(5), offset: None };
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
    fn child_select() {
        let plan = QueryPlan::build(&LIST).unwrap();
        let child = &plan.children[0].plan;
        // root options do not apply to child queries
        let options = RootOptions { by_keys: false, order_by: vec![], limit: Some(1), offset: None };
        assert_eq!(
            select(child, &options),
            "SELECT t0.\"id\" AS \"$key\", t0.\"list_id\" AS \"$parent\", t0.\"my \"\"label\"\"\" AS \"label\" \
             FROM \"item\" AS t0 WHERE t0.\"list_id\" = ANY($1) ORDER BY t0.\"position\", t0.\"id\""
        );
    }
}
