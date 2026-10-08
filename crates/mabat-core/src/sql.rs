//! Rendering a query plan as SQL, for PostgreSQL, MySQL and SQLite.
//!
//! The SQL is built from the plan, never by editing SQL text. Every identifier is quoted,
//! and every value is bound. On PostgreSQL, keys are bound as one array parameter
//! (`= ANY($1)`); on MySQL and SQLite, which have no array parameters, as a list of
//! parameters (`IN (?, ?, …)`).

use std::fmt::Write;

use crate::filter::{Filter, Params};
use crate::plan::{Link, QueryPlan};
use crate::shape::OrderBy;

/// Table alias of the entity selected by a query.
const TABLE_ALIAS: &str = "t0";

/// Table alias of the link table of a many-to-many collection.
const LINK_ALIAS: &str = "j";

/// Name and alias of the recursive CTE of a recursive collection.
const TREE: &str = "$tree";
const TREE_ALIAS: &str = "r";

/// The SQL dialect of a database.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Dialect {
    #[default]
    Postgres,
    MySql,
    Sqlite,
}

impl Dialect {
    /// Quote an identifier.
    pub fn quote(self, ident: &str) -> String {
        match self {
            Dialect::MySql => format!("`{}`", ident.replace('`', "``")),
            Dialect::Postgres | Dialect::Sqlite => format!("\"{}\"", ident.replace('"', "\"\"")),
        }
    }

    /// Whether keys and lists are bound as one array parameter.
    pub fn binds_arrays(self) -> bool {
        self == Dialect::Postgres
    }

    /// The placeholder of parameter `n`, counted from 1. MySQL and SQLite bind parameters in
    /// the order of their placeholders.
    pub fn placeholder(self, n: usize) -> String {
        match self {
            Dialect::Postgres => format!("${n}"),
            Dialect::MySql | Dialect::Sqlite => "?".to_string(),
        }
    }

    /// `expr` is one of the keys: `expr = ANY($1)` with an array, or `expr IN (?, …)` with
    /// `count` parameters (at least one).
    pub fn keys_condition(self, expr: &str, count: usize) -> String {
        match self {
            Dialect::Postgres => format!("{expr} = ANY($1)"),
            Dialect::MySql | Dialect::Sqlite => format!("{expr} IN ({})", list(count.max(1))),
        }
    }

    /// `expr` as text, for tag columns of any type, e.g. a PostgreSQL or MySQL enum.
    fn text(self, expr: &str) -> String {
        match self {
            Dialect::Postgres => format!("{expr}::text"),
            Dialect::MySql => format!("CAST({expr} AS CHAR)"),
            Dialect::Sqlite => format!("CAST({expr} AS TEXT)"),
        }
    }

    /// `OFFSET` without `LIMIT`, which MySQL and SQLite do not allow on their own.
    fn offset_only(self, offset: u64) -> String {
        match self {
            Dialect::Postgres => format!("OFFSET {offset}"),
            Dialect::MySql => format!("LIMIT 18446744073709551615 OFFSET {offset}"),
            Dialect::Sqlite => format!("LIMIT -1 OFFSET {offset}"),
        }
    }

    /// The path of keys of the anchor of a recursive CTE, its extension by the next key, and
    /// the condition that the next key is not on the path, which stops cycles.
    fn path(self, key: &str, path: &str) -> (String, String, String) {
        match self {
            Dialect::Postgres => (format!("ARRAY[{key}]"), format!("{path} || {key}"), format!("{key} <> ALL({path})")),
            Dialect::Sqlite => (
                format!("',' || {key} || ','"),
                format!("{path} || {key} || ','"),
                format!("instr({path}, ',' || {key} || ',') = 0"),
            ),
            Dialect::MySql => (
                format!("CAST(CONCAT(',', {key}, ',') AS CHAR(8000))"),
                format!("CONCAT({path}, {key}, ',')"),
                format!("LOCATE(CONCAT(',', {key}, ','), {path}) = 0"),
            ),
        }
    }
}

/// `count` placeholders for MySQL and SQLite: `?, ?, ?`.
fn list(count: usize) -> String {
    vec!["?"; count].join(", ")
}

/// How to render a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Render {
    pub dialect: Dialect,
    pub layout: Layout,
    /// The number of keys bound: of the root query when it is loaded by keys, or the keys a
    /// child query is selected by. Only MySQL and SQLite need it, for their `IN` lists.
    pub keys: usize,
    /// On MySQL and SQLite, write the keys as the [`KEYS_TOKEN`] placeholder of override
    /// files, for scaffolding and the manifest.
    pub keys_token: bool,
}

impl Render {
    pub const fn new(dialect: Dialect) -> Render {
        Render { dialect, layout: Layout::Line, keys: 1, keys_token: false }
    }

    /// Write the keys as the [`KEYS_TOKEN`] placeholder on MySQL and SQLite.
    pub const fn with_keys_token(self) -> Render {
        Render { keys_token: true, ..self }
    }

    /// `expr` is one of the keys, see [`Dialect::keys_condition`].
    fn keys_condition(&self, expr: &str) -> String {
        if self.keys_token && !self.dialect.binds_arrays() {
            format!("{expr} IN ({KEYS_TOKEN})")
        } else {
            self.dialect.keys_condition(expr, self.keys)
        }
    }

    pub const fn with_layout(self, layout: Layout) -> Render {
        Render { layout, ..self }
    }

    pub const fn with_keys(self, keys: usize) -> Render {
        Render { keys, ..self }
    }

    fn quote(&self, ident: &str) -> String {
        self.dialect.quote(ident)
    }
}

impl Default for Render {
    fn default() -> Self {
        Render::new(Dialect::Postgres)
    }
}

/// Options of the root query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootOptions {
    /// Restrict the root rows to the bound keys.
    pub by_keys: bool,
    /// Restrict the root rows to the rows that match. Its values are bound after the keys.
    pub filter: Option<Filter>,
    /// The number of values of each list parameter slot of the filter (0 for other slots),
    /// for MySQL and SQLite, which bind each value of a list.
    pub filter_lists: Vec<usize>,
    pub order_by: Vec<OrderBy>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

impl RootOptions {
    /// The placeholder number of the first filter value on PostgreSQL: `$2` after the keys,
    /// else `$1`.
    pub fn first_filter_param(&self) -> usize {
        if self.by_keys { 2 } else { 1 }
    }

    /// The WHERE clause of the root query, without `WHERE`, or `None` if there are no
    /// conditions. `key` is the key column reference.
    fn conditions<'a>(
        &'a self,
        render: &Render,
        key: &str,
        filter_keys: bool,
        column: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Option<String>, &'a str> {
        let mut conditions = Vec::new();
        if filter_keys {
            conditions.push(render.keys_condition(key));
        }
        if let Some(filter) = &self.filter {
            let mut rendered = String::new();
            let params =
                Params { dialect: render.dialect, first: self.first_filter_param(), lists: &self.filter_lists };
            filter.render(&mut rendered, column, &params)?;
            conditions.push(if conditions.is_empty() { rendered } else { format!("({rendered})") });
        }
        Ok((!conditions.is_empty()).then(|| conditions.join(" AND ")))
    }
}

/// Quote an identifier for PostgreSQL.
pub fn quote_ident(ident: &str) -> String {
    Dialect::Postgres.quote(ident)
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

/// Render the SELECT statement of a query in the plan for PostgreSQL.
///
/// A child or to-one query always filters on the keys bound as `$1`. The root query does
/// so only when [`RootOptions::by_keys`] is set.
pub fn select(plan: &QueryPlan, root: &RootOptions) -> String {
    render(plan, root, &Render::default())
}

/// Render the SELECT statement of a query in the plan for PostgreSQL with the given layout.
pub fn select_with(plan: &QueryPlan, root: &RootOptions, layout: Layout) -> String {
    render(plan, root, &Render::default().with_layout(layout))
}

/// Render the SELECT statement of a query in the plan.
pub fn render(plan: &QueryPlan, root: &RootOptions, render: &Render) -> String {
    let (column_separator, clause) = match render.layout {
        Layout::Line => (", ", " "),
        Layout::Multiline => (",\n       ", "\n"),
    };
    let dialect = render.dialect;
    let q = |ident: &str| render.quote(ident);
    let table = q(plan.shape.table);
    let key = q(plan.shape.key_column);
    let mut sql = String::new();
    if let (Some(cte), Link::Child { fk, .. }) = (&plan.cte, &plan.link) {
        // All levels of a recursive collection: the keys of the rows, then their columns, each
        // row once even if it is below several of the parent keys. The path of keys stops the
        // recursion at cycles in the data.
        let fk = q(fk);
        let tree = q(TREE);
        let (k, path, depth) = (q("k"), q("path"), q("depth"));
        let row_key = format!("{TABLE_ALIAS}.{key}");
        let (anchor_path, next_path, not_on_path) = dialect.path(&row_key, &format!("{TREE_ALIAS}.{path}"));
        let limit = cte.depth.map(|limit| format!(" AND {TREE_ALIAS}.{depth} < {limit}")).unwrap_or_default();
        let anchor_keys = render.keys_condition(&format!("{TABLE_ALIAS}.{fk}"));
        let _ = write!(
            sql,
            "WITH RECURSIVE {tree} AS ({clause}SELECT {row_key} AS {k}, {anchor_path} AS {path}, \
             1 AS {depth} FROM {table} AS {TABLE_ALIAS} WHERE {anchor_keys}{clause}UNION ALL{clause}\
             SELECT {row_key}, {next_path}, {TREE_ALIAS}.{depth} + 1 \
             FROM {table} AS {TABLE_ALIAS} JOIN {tree} AS {TREE_ALIAS} ON {TABLE_ALIAS}.{fk} = {TREE_ALIAS}.{k} \
             WHERE {not_on_path}{limit}{clause}){clause}"
        );
    }
    sql.push_str("SELECT ");
    for (i, column) in plan.columns.iter().enumerate() {
        if i > 0 {
            sql.push_str(column_separator);
        }
        let source = if column.from_link { LINK_ALIAS } else { TABLE_ALIAS };
        let expr = format!("{source}.{}", q(&column.column));
        let expr = if column.as_text { dialect.text(&expr) } else { expr };
        let _ = write!(sql, "{expr} AS {}", q(&column.alias));
    }
    if plan.cte.is_some() {
        let _ = write!(
            sql,
            "{clause}FROM (SELECT DISTINCT {k} FROM {tree}) AS {TREE_ALIAS} \
             JOIN {table} AS {TABLE_ALIAS} ON {TABLE_ALIAS}.{key} = {TREE_ALIAS}.{k}",
            k = q("k"),
            tree = q(TREE),
        );
    } else {
        let _ = write!(sql, "{clause}FROM {table} AS {TABLE_ALIAS}");
    }
    if let Link::Child { through: Some(through), .. } = &plan.link {
        let _ = write!(
            sql,
            " JOIN {} AS {LINK_ALIAS} ON {LINK_ALIAS}.{} = {TABLE_ALIAS}.{key}",
            q(through.table),
            q(through.target)
        );
    }

    let order_by: &[OrderBy] = match &plan.link {
        Link::Root => {
            let key = format!("{TABLE_ALIAS}.{key}");
            let column = |name: &str| Some(format!("{TABLE_ALIAS}.{}", q(name)));
            if let Ok(Some(conditions)) = root.conditions(render, &key, root.by_keys, &column) {
                let _ = write!(sql, "{clause}WHERE {conditions}");
            }
            &root.order_by
        }
        Link::Child { .. } if plan.cte.is_some() => &plan.order_by,
        Link::Child { fk, through } => {
            let source = if through.is_some() { LINK_ALIAS } else { TABLE_ALIAS };
            let condition = render.keys_condition(&format!("{source}.{}", q(fk)));
            let _ = write!(sql, "{clause}WHERE {condition}");
            &plan.order_by
        }
        Link::ToOne { .. } | Link::Variant { .. } => {
            let condition = render.keys_condition(&format!("{TABLE_ALIAS}.{key}"));
            let _ = write!(sql, "{clause}WHERE {condition}");
            &[]
        }
    };

    let mut terms: Vec<String> = order_by
        .iter()
        .map(|o| format!("{TABLE_ALIAS}.{}{}", q(o.column), if o.descending { " DESC" } else { "" }))
        .collect();
    // Child rows are ordered deterministically, ending with the key
    if !matches!(plan.link, Link::Root) && !order_by.iter().any(|o| o.column == plan.shape.key_column) {
        terms.push(format!("{TABLE_ALIAS}.{key}"));
    }
    if !terms.is_empty() {
        let _ = write!(sql, "{clause}ORDER BY {}", terms.join(", "));
    }

    if let Link::Root = plan.link {
        paging(&mut sql, clause, root, dialect);
    }

    sql
}

/// Append `LIMIT` and `OFFSET`.
fn paging(sql: &mut String, clause: &str, root: &RootOptions, dialect: Dialect) {
    match (root.limit, root.offset) {
        (Some(limit), offset) => {
            let _ = write!(sql, "{clause}LIMIT {limit}");
            if let Some(offset) = offset {
                let _ = write!(sql, "{clause}OFFSET {offset}");
            }
        }
        (None, Some(offset)) => {
            let _ = write!(sql, "{clause}{}", dialect.offset_only(offset));
        }
        (None, None) => {}
    }
}

/// Table alias of an override used as a subquery.
const OVERRIDE_ALIAS: &str = "o";

/// The placeholder of the keys in override SQL, on any database: `WHERE n.task_id IN (:keys)`
/// on MySQL and SQLite, where it becomes one placeholder per key, and
/// `WHERE n.task_id = ANY(:keys)` on PostgreSQL, where it becomes `$1`, the array of keys.
pub const KEYS_TOKEN: &str = ":keys";

/// Replace [`KEYS_TOKEN`] in override SQL: by `$1` on PostgreSQL, by `count` placeholders on
/// MySQL and SQLite. Only the token itself is replaced, not `::keys` or `:keys_x`, and
/// not in string literals or quoted identifiers.
pub fn expand_keys(sql: &str, dialect: Dialect, count: usize) -> String {
    let replacement = if dialect.binds_arrays() { dialect.placeholder(1) } else { list(count.max(1)) };
    let mut out = String::with_capacity(sql.len());
    let mut quote = None;
    let mut rest = sql;
    while let Some(c) = rest.chars().next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if matches!(c, '\'' | '"' | '`') => quote = Some(c),
            None if rest.starts_with(KEYS_TOKEN) => {
                let before = out.chars().next_back();
                let after = rest[KEYS_TOKEN.len()..].chars().next();
                let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                if before != Some(':') && !word(after) {
                    out.push_str(&replacement);
                    rest = &rest[KEYS_TOKEN.len()..];
                    continue;
                }
            }
            None => {}
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Apply the root options to the SQL of an override of the root query.
///
/// The override becomes a subquery, so ordering and filters refer to its column aliases:
/// each column of [`RootOptions::order_by`] and [`RootOptions::filter`] is mapped to the
/// alias it is selected as. With `filter_keys`, the rows are restricted to the bound keys;
/// this is for an override that does not take the keys as a parameter itself. When there is
/// nothing to apply, the override is returned as is.
///
/// Returns a column that the plan does not select as the error.
pub fn wrap_root<'a>(
    sql: &str,
    plan: &QueryPlan,
    root: &'a RootOptions,
    filter_keys: bool,
    render: &Render,
) -> Result<String, &'a str> {
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
    let column = |name: &str| override_column(plan, name, render.dialect);
    let key = format!("{OVERRIDE_ALIAS}.{}", render.quote(&plan.key_alias));
    if let Some(conditions) = root.conditions(render, &key, filter_keys, &column)? {
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
    paging(&mut wrapped, " ", root, render.dialect);
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
    render: &Render,
) -> Result<String, &'a str> {
    match override_sql {
        None => {
            let mut sql = format!("SELECT count(*) FROM {} AS {TABLE_ALIAS}", render.quote(plan.shape.table));
            let key = format!("{TABLE_ALIAS}.{}", render.quote(plan.shape.key_column));
            let column = |name: &str| Some(format!("{TABLE_ALIAS}.{}", render.quote(name)));
            if let Some(conditions) = root.conditions(render, &key, root.by_keys, &column)? {
                let _ = write!(sql, " WHERE {conditions}");
            }
            Ok(sql)
        }
        Some((override_sql, filter_keys)) => {
            let mut sql = format!("SELECT count(*) FROM ({}\n) AS {OVERRIDE_ALIAS}", trim_statement(override_sql));
            let key = format!("{OVERRIDE_ALIAS}.{}", render.quote(&plan.key_alias));
            let column = |name: &str| override_column(plan, name, render.dialect);
            if let Some(conditions) = root.conditions(render, &key, filter_keys, &column)? {
                let _ = write!(sql, " WHERE {conditions}");
            }
            Ok(sql)
        }
    }
}

/// The reference to a column of the view's table in an override used as a subquery: the
/// alias it is selected as.
fn override_column(plan: &QueryPlan, column: &str, dialect: Dialect) -> Option<String> {
    let selected = plan.columns.iter().find(|c| c.column == column)?;
    Some(format!("{OVERRIDE_ALIAS}.{}", dialect.quote(&selected.alias)))
}

fn trim_statement(sql: &str) -> &str {
    sql.trim_end().trim_end_matches(';').trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::{Child, Field, FieldKind, ValueType, ViewShape};

    static ITEM_FIELDS: [Field; 1] =
        [Field { name: "label", kind: FieldKind::Column { column: "my \"label\"", ty: ValueType::TEXT } }];
    static ITEM: ViewShape = ViewShape { name: "Item", table: "item", key_column: "id", fields: &ITEM_FIELDS };

    static ORDER: [OrderBy; 1] = [OrderBy::asc("position")];
    static LIST_FIELDS: [Field; 1] = [Field {
        name: "items",
        kind: FieldKind::Child(Child { order_by: &ORDER, ..Child::new("list_id", || &ITEM) }),
    }];
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
            select_with(plan.children[0].plan().unwrap(), &RootOptions::default(), Layout::Multiline),
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
            wrap_root(sql, &plan, &RootOptions::default(), false, &Render::default()),
            Ok(sql.trim_end().trim_end_matches(';').into())
        );

        let options = RootOptions {
            by_keys: true,
            order_by: vec![OrderBy::desc("my \"label\"")],
            limit: Some(5),
            ..RootOptions::default()
        };
        assert_eq!(
            wrap_root(sql, &plan, &options, true, &Render::default()),
            Ok("SELECT * FROM (SELECT id AS \"$key\", label AS \"label\" FROM item\n) AS o \
                WHERE o.\"$key\" = ANY($1) ORDER BY o.\"label\" DESC LIMIT 5"
                .into())
        );

        let options = RootOptions { order_by: vec![OrderBy::asc("nope")], ..RootOptions::default() };
        assert_eq!(wrap_root(sql, &plan, &options, false, &Render::default()), Err("nope"));
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
            count(&plan, &options, None, &Render::default()),
            Ok("SELECT count(*) FROM \"item\" AS t0 \
                WHERE t0.\"id\" = ANY($1) AND ((t0.\"my \"\"label\"\"\" = $2) OR (t0.\"id\" IS NULL))"
                .into())
        );

        // Without keys, the filter values start at $1; in an override, columns are aliases
        let options = RootOptions { filter: Some(filter), ..RootOptions::default() };
        assert_eq!(
            wrap_root("SELECT 1;", &plan, &options, false, &Render::default()),
            Ok("SELECT * FROM (SELECT 1\n) AS o WHERE (o.\"label\" = $1) OR (o.\"$key\" IS NULL)".into())
        );
        assert_eq!(
            count(&plan, &options, Some(("SELECT 1", false)), &Render::default()),
            Ok("SELECT count(*) FROM (SELECT 1\n) AS o WHERE (o.\"label\" = $1) OR (o.\"$key\" IS NULL)".into())
        );

        let options = RootOptions {
            filter: Some(Filter::Null { column: "nope".into(), negated: false }),
            ..RootOptions::default()
        };
        assert_eq!(wrap_root("SELECT 1", &plan, &options, false, &Render::default()), Err("nope"));
    }

    #[test]
    fn child_select() {
        let plan = QueryPlan::build(&LIST).unwrap();
        let child = plan.children[0].plan().unwrap();
        // root options do not apply to child queries
        let options = RootOptions { limit: Some(1), ..RootOptions::default() };
        assert_eq!(
            select(child, &options),
            "SELECT t0.\"id\" AS \"$key\", t0.\"list_id\" AS \"$parent\", t0.\"my \"\"label\"\"\" AS \"label\" \
             FROM \"item\" AS t0 WHERE t0.\"list_id\" = ANY($1) ORDER BY t0.\"position\", t0.\"id\""
        );
    }
    #[test]
    fn mysql_and_sqlite_bind_keys_as_lists() {
        let plan = QueryPlan::build(&LIST).unwrap();
        let child = plan.children[0].plan().unwrap();
        assert_eq!(
            render(child, &RootOptions::default(), &Render::new(Dialect::MySql).with_keys(3)),
            "SELECT t0.`id` AS `$key`, t0.`list_id` AS `$parent`, t0.`my \"label\"` AS `label` FROM `item` AS t0 \
             WHERE t0.`list_id` IN (?, ?, ?) ORDER BY t0.`position`, t0.`id`"
        );
        assert_eq!(
            render(child, &RootOptions::default(), &Render::new(Dialect::Sqlite).with_keys(2)),
            "SELECT t0.\"id\" AS \"$key\", t0.\"list_id\" AS \"$parent\", t0.\"my \"\"label\"\"\" AS \"label\" \
             FROM \"item\" AS t0 WHERE t0.\"list_id\" IN (?, ?) ORDER BY t0.\"position\", t0.\"id\""
        );
        assert_eq!(expand_keys("WHERE x IN (:keys)", Dialect::Sqlite, 3), "WHERE x IN (?, ?, ?)");
        assert_eq!(expand_keys("WHERE x = ANY($1)", Dialect::Postgres, 3), "WHERE x = ANY($1)");
        assert_eq!(expand_keys("WHERE x = ANY(:keys)", Dialect::Postgres, 3), "WHERE x = ANY($1)");
        assert_eq!(
            expand_keys("SELECT ':keys', x::keys, :keys_x FROM t WHERE x IN (:keys)", Dialect::MySql, 2),
            "SELECT ':keys', x::keys, :keys_x FROM t WHERE x IN (?, ?)"
        );
    }

    #[test]
    fn mysql_and_sqlite_filters_and_paging() {
        use crate::filter::{CompareOp, Filter};
        let plan = QueryPlan::build(&ITEM).unwrap();
        let filter = Filter::And(vec![
            Filter::In { column: "id".into(), param: 0, negated: false },
            Filter::Like { column: "my \"label\"".into(), param: 1, case_insensitive: true },
            Filter::Compare { column: "id".into(), op: CompareOp::Gt, param: 2 },
        ]);
        let options = RootOptions {
            by_keys: true,
            filter: Some(filter),
            filter_lists: vec![2, 0, 0],
            offset: Some(5),
            ..RootOptions::default()
        };
        assert_eq!(
            render(&plan, &options, &Render::new(Dialect::MySql).with_keys(1)),
            "SELECT t0.`id` AS `$key`, t0.`my \"label\"` AS `label` FROM `item` AS t0 WHERE t0.`id` IN (?) AND \
             ((t0.`id` IN (?, ?)) AND (LOWER(t0.`my \"label\"`) LIKE LOWER(?)) AND (t0.`id` > ?)) \
             LIMIT 18446744073709551615 OFFSET 5"
        );
        let sqlite = render(&plan, &options, &Render::new(Dialect::Sqlite).with_keys(1));
        assert!(sqlite.ends_with("LIMIT -1 OFFSET 5"), "{sqlite}");
    }

    #[test]
    fn mysql_and_sqlite_recursive_paths() {
        use crate::shape::Recursion;
        static TREE_FIELDS: [Field; 1] = [Field {
            name: "children",
            kind: FieldKind::Child(Child {
                recursion: Some(Recursion::Cte { depth: None }),
                ..Child::new("parent_id", || &TREE)
            }),
        }];
        static TREE: ViewShape = ViewShape { name: "Tree", table: "node", key_column: "id", fields: &TREE_FIELDS };
        let plan = QueryPlan::build(&TREE).unwrap();
        let level = plan.children[0].plan().unwrap();
        let mysql = render(level, &RootOptions::default(), &Render::new(Dialect::MySql).with_keys(2));
        assert!(mysql.starts_with("WITH RECURSIVE `$tree` AS ( SELECT t0.`id` AS `k`, CAST(CONCAT(',', t0.`id`, ',') AS CHAR(8000)) AS `path`"), "{mysql}");
        assert!(mysql.contains("WHERE t0.`parent_id` IN (?, ?) UNION ALL"), "{mysql}");
        assert!(mysql.contains("WHERE LOCATE(CONCAT(',', t0.`id`, ','), r.`path`) = 0"), "{mysql}");
        let sqlite = render(level, &RootOptions::default(), &Render::new(Dialect::Sqlite).with_keys(1));
        assert!(sqlite.contains("instr(r.\"path\", ',' || t0.\"id\" || ',') = 0"), "{sqlite}");
    }
}
