//! Filters on the root query.
//!
//! A filter is a tree of conditions on columns of the view's table. Values are not part of
//! the filter: each condition refers to a parameter slot, and the executor binds the values
//! in slot order after the keys.

use std::fmt::Write;

use crate::sql::Dialect;

/// How the parameters of a filter are written.
#[derive(Debug, Clone, Copy)]
pub struct Params<'a> {
    pub dialect: Dialect,
    /// The PostgreSQL placeholder number of slot 0.
    pub first: usize,
    /// The number of values of each list slot, for MySQL and SQLite, which bind each value.
    pub lists: &'a [usize],
}

impl Params<'_> {
    fn one(&self, slot: usize) -> String {
        self.dialect.placeholder(self.first + slot)
    }

    /// `column` is one of the values of a list slot.
    fn list(&self, column: &str, slot: usize) -> String {
        match self.dialect {
            Dialect::Postgres => format!("{column} = ANY(${})", self.first + slot),
            Dialect::MySql | Dialect::Sqlite => {
                let count = self.lists.get(slot).copied().unwrap_or(1).max(1);
                format!("{column} IN ({})", vec!["?"; count].join(", "))
            }
        }
    }
}

/// A comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl CompareOp {
    fn sql(self) -> &'static str {
        match self {
            CompareOp::Eq => "=",
            CompareOp::Ne => "<>",
            CompareOp::Lt => "<",
            CompareOp::Le => "<=",
            CompareOp::Gt => ">",
            CompareOp::Ge => ">=",
        }
    }
}

/// A condition on the rows of the root query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// `column <op> $param`
    Compare {
        column: String,
        op: CompareOp,
        param: usize,
    },
    /// `column = ANY($param)`, or `NOT (column = ANY($param))` when negated. The parameter
    /// is an array.
    In {
        column: String,
        param: usize,
        negated: bool,
    },
    /// `column IS NULL`, or `IS NOT NULL` when negated.
    Null {
        column: String,
        negated: bool,
    },
    /// `column LIKE $param`, or `ILIKE` when case insensitive.
    Like {
        column: String,
        param: usize,
        case_insensitive: bool,
    },
    /// All of the conditions; true when empty.
    And(Vec<Filter>),
    /// Any of the conditions; false when empty.
    Or(Vec<Filter>),
    Not(Box<Filter>),
}

impl Filter {
    /// Add `by` to every parameter slot, to combine filters whose slots are numbered from 0.
    pub fn shift(&mut self, by: usize) {
        match self {
            Filter::Compare { param, .. } | Filter::In { param, .. } | Filter::Like { param, .. } => *param += by,
            Filter::Null { .. } => {}
            Filter::And(filters) | Filter::Or(filters) => filters.iter_mut().for_each(|f| f.shift(by)),
            Filter::Not(filter) => filter.shift(by),
        }
    }

    /// The columns the filter refers to.
    pub fn columns(&self) -> Vec<&str> {
        let mut columns = Vec::new();
        self.collect_columns(&mut columns);
        columns
    }

    fn collect_columns<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Filter::Compare { column, .. }
            | Filter::In { column, .. }
            | Filter::Null { column, .. }
            | Filter::Like { column, .. } => out.push(column),
            Filter::And(filters) | Filter::Or(filters) => filters.iter().for_each(|f| f.collect_columns(out)),
            Filter::Not(filter) => filter.collect_columns(out),
        }
    }

    /// Render the filter. `column` renders a column reference, or returns `None` for a
    /// column that cannot be referred to, which is then the error. Slots are rendered in
    /// order, which is the order MySQL and SQLite bind their values in.
    pub fn render<'a>(
        &'a self,
        out: &mut String,
        column: &dyn Fn(&str) -> Option<String>,
        params: &Params<'_>,
    ) -> Result<(), &'a str> {
        let col = |name: &'a str| column(name).ok_or(name);
        match self {
            Filter::Compare { column, op, param } => {
                let _ = write!(out, "{} {} {}", col(column)?, op.sql(), params.one(*param));
            }
            Filter::In { column, param, negated } => {
                let condition = params.list(&col(column)?, *param);
                let _ = if *negated { write!(out, "NOT ({condition})") } else { write!(out, "{condition}") };
            }
            Filter::Null { column, negated } => {
                let not = if *negated { " NOT" } else { "" };
                let _ = write!(out, "{} IS{not} NULL", col(column)?);
            }
            Filter::Like { column, param, case_insensitive } => {
                let column = col(column)?;
                let placeholder = params.one(*param);
                let _ = match (case_insensitive, params.dialect) {
                    (false, _) => write!(out, "{column} LIKE {placeholder}"),
                    (true, Dialect::Postgres) => write!(out, "{column} ILIKE {placeholder}"),
                    (true, _) => write!(out, "LOWER({column}) LIKE LOWER({placeholder})"),
                };
            }
            Filter::And(filters) | Filter::Or(filters) => {
                let (separator, empty) =
                    if matches!(self, Filter::And(_)) { (" AND ", "TRUE") } else { (" OR ", "FALSE") };
                if filters.is_empty() {
                    out.push_str(empty);
                }
                for (i, filter) in filters.iter().enumerate() {
                    if i > 0 {
                        out.push_str(separator);
                    }
                    out.push('(');
                    filter.render(out, column, params)?;
                    out.push(')');
                }
            }
            Filter::Not(filter) => {
                out.push_str("NOT (");
                filter.render(out, column, params)?;
                out.push(')');
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(filter: &Filter, first_param: usize) -> String {
        let mut out = String::new();
        let params = Params { dialect: Dialect::Postgres, first: first_param, lists: &[] };
        filter.render(&mut out, &|c| Some(format!("t.\"{c}\"")), &params).unwrap();
        out
    }

    #[test]
    fn renders_nested_conditions() {
        let filter = Filter::And(vec![
            Filter::Compare { column: "status".into(), op: CompareOp::Eq, param: 0 },
            Filter::Or(vec![
                Filter::In { column: "id".into(), param: 1, negated: false },
                Filter::Not(Box::new(Filter::Null { column: "owner".into(), negated: false })),
            ]),
            Filter::Like { column: "name".into(), param: 2, case_insensitive: true },
        ]);
        assert_eq!(
            render(&filter, 2),
            "(t.\"status\" = $2) AND ((t.\"id\" = ANY($3)) OR (NOT (t.\"owner\" IS NULL))) AND (t.\"name\" ILIKE $4)"
        );
        assert_eq!(filter.columns(), ["status", "id", "owner", "name"]);
    }

    #[test]
    fn empty_groups_and_shifting() {
        assert_eq!(render(&Filter::And(vec![]), 1), "TRUE");
        assert_eq!(render(&Filter::Or(vec![]), 1), "FALSE");

        let mut filter = Filter::Not(Box::new(Filter::In { column: "id".into(), param: 0, negated: true }));
        filter.shift(3);
        assert_eq!(render(&filter, 1), "NOT (NOT (t.\"id\" = ANY($4)))");
    }

    #[test]
    fn unknown_columns_are_the_error() {
        let filter = Filter::Null { column: "missing".into(), negated: true };
        let mut out = String::new();
        let params = Params { dialect: Dialect::Postgres, first: 1, lists: &[] };
        assert_eq!(filter.render(&mut out, &|_| None, &params), Err("missing"));
    }
}
