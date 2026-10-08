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

/// Render the SELECT statement of a query in the plan.
///
/// A child or to-one query always filters on the keys bound as `$1`. The root query does
/// so only when [`RootOptions::by_keys`] is set.
pub fn select(plan: &QueryPlan, root: &RootOptions) -> String {
    let mut sql = String::from("SELECT ");
    for (i, column) in plan.columns.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        let _ = write!(sql, "{TABLE_ALIAS}.{} AS {}", quote_ident(&column.column), quote_ident(&column.alias));
    }
    let _ = write!(sql, " FROM {} AS {TABLE_ALIAS}", quote_ident(plan.shape.table));

    let key = quote_ident(plan.shape.key_column);
    let order_by: &[OrderBy] = match &plan.link {
        Link::Root => {
            if root.by_keys {
                let _ = write!(sql, " WHERE {TABLE_ALIAS}.{key} = ANY($1)");
            }
            &root.order_by
        }
        Link::Child { fk } => {
            let _ = write!(sql, " WHERE {TABLE_ALIAS}.{} = ANY($1)", quote_ident(fk));
            &plan.order_by
        }
        Link::ToOne { .. } => {
            let _ = write!(sql, " WHERE {TABLE_ALIAS}.{key} = ANY($1)");
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
        let _ = write!(sql, " ORDER BY {}", terms.join(", "));
    }

    if let Link::Root = plan.link {
        if let Some(limit) = root.limit {
            let _ = write!(sql, " LIMIT {limit}");
        }
        if let Some(offset) = root.offset {
            let _ = write!(sql, " OFFSET {offset}");
        }
    }

    sql
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
