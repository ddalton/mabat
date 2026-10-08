//! Filters on the rows of the root query of a load.
//!
//! ```ignore
//! use refract::filter::col;
//!
//! let open = refract::load::<TaskView>()
//!     .filter(col("status").eq("open").and(col("created_at").gt(since)))
//!     .filter(col("assignee_id").is_in([1_i64, 2, 3]))
//!     .all(&mut conn)
//!     .await?;
//! ```
//!
//! Filters name columns of the view's table, like [`Load::order_by`](crate::Load::order_by).
//! Values are always bound as parameters. A list of values is bound as one array, so the
//! statement is the same whatever the length of the list.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use refract_core::filter::{CompareOp, Filter};
use sqlx::Postgres;
use sqlx::postgres::PgArguments;
use sqlx::query::Query;
use uuid::Uuid;

/// A value to compare a column with.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    I16(i16),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    Text(String),
    Uuid(Uuid),
    Timestamptz(DateTime<Utc>),
    Timestamp(NaiveDateTime),
    Date(NaiveDate),
}

macro_rules! value_from {
    ($($ty:ty => $variant:ident),* $(,)?) => {
        $(impl From<$ty> for Value {
            fn from(value: $ty) -> Self {
                Value::$variant(value)
            }
        })*
    };
}

value_from! {
    bool => Bool,
    i16 => I16,
    i32 => I32,
    i64 => I64,
    f32 => F32,
    f64 => F64,
    String => Text,
    Uuid => Uuid,
    DateTime<Utc> => Timestamptz,
    NaiveDateTime => Timestamp,
    NaiveDate => Date,
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Value::Text(value.to_string())
    }
}

impl From<&String> for Value {
    fn from(value: &String) -> Self {
        Value::Text(value.clone())
    }
}

/// The values of a list, bound as one array. All values of a list have the same Rust type,
/// so they have the same variant.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Values {
    Bool(Vec<bool>),
    I16(Vec<i16>),
    I32(Vec<i32>),
    I64(Vec<i64>),
    F32(Vec<f32>),
    F64(Vec<f64>),
    Text(Vec<String>),
    Uuid(Vec<Uuid>),
    Timestamptz(Vec<DateTime<Utc>>),
    Timestamp(Vec<NaiveDateTime>),
    Date(Vec<NaiveDate>),
}

impl Values {
    /// `None` if the list is empty.
    fn new(values: Vec<Value>) -> Option<Values> {
        macro_rules! collect {
            ($variant:ident) => {
                Values::$variant(
                    values
                        .into_iter()
                        .filter_map(|v| match v {
                            Value::$variant(v) => Some(v),
                            _ => None,
                        })
                        .collect(),
                )
            };
        }
        Some(match values.first()? {
            Value::Bool(_) => collect!(Bool),
            Value::I16(_) => collect!(I16),
            Value::I32(_) => collect!(I32),
            Value::I64(_) => collect!(I64),
            Value::F32(_) => collect!(F32),
            Value::F64(_) => collect!(F64),
            Value::Text(_) => collect!(Text),
            Value::Uuid(_) => collect!(Uuid),
            Value::Timestamptz(_) => collect!(Timestamptz),
            Value::Timestamp(_) => collect!(Timestamp),
            Value::Date(_) => collect!(Date),
        })
    }
}

/// A value bound for a parameter slot of a filter.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Bound {
    One(Value),
    List(Values),
}

impl Bound {
    pub(crate) fn bind<'q>(self, query: Query<'q, Postgres, PgArguments>) -> Query<'q, Postgres, PgArguments> {
        match self {
            Bound::One(value) => match value {
                Value::Bool(v) => query.bind(v),
                Value::I16(v) => query.bind(v),
                Value::I32(v) => query.bind(v),
                Value::I64(v) => query.bind(v),
                Value::F32(v) => query.bind(v),
                Value::F64(v) => query.bind(v),
                Value::Text(v) => query.bind(v),
                Value::Uuid(v) => query.bind(v),
                Value::Timestamptz(v) => query.bind(v),
                Value::Timestamp(v) => query.bind(v),
                Value::Date(v) => query.bind(v),
            },
            Bound::List(values) => match values {
                Values::Bool(v) => query.bind(v),
                Values::I16(v) => query.bind(v),
                Values::I32(v) => query.bind(v),
                Values::I64(v) => query.bind(v),
                Values::F32(v) => query.bind(v),
                Values::F64(v) => query.bind(v),
                Values::Text(v) => query.bind(v),
                Values::Uuid(v) => query.bind(v),
                Values::Timestamptz(v) => query.bind(v),
                Values::Timestamp(v) => query.bind(v),
                Values::Date(v) => query.bind(v),
            },
        }
    }
}

/// A column of the view's table, to build a [`Condition`] on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Col(String);

/// A column of the view's table, to build a [`Condition`] on.
pub fn col(column: impl Into<String>) -> Col {
    Col(column.into())
}

impl Col {
    fn compare(self, op: CompareOp, value: impl Into<Value>) -> Condition {
        Condition { filter: Filter::Compare { column: self.0, op, param: 0 }, values: vec![Bound::One(value.into())] }
    }

    pub fn eq(self, value: impl Into<Value>) -> Condition {
        self.compare(CompareOp::Eq, value)
    }

    pub fn ne(self, value: impl Into<Value>) -> Condition {
        self.compare(CompareOp::Ne, value)
    }

    pub fn lt(self, value: impl Into<Value>) -> Condition {
        self.compare(CompareOp::Lt, value)
    }

    pub fn le(self, value: impl Into<Value>) -> Condition {
        self.compare(CompareOp::Le, value)
    }

    pub fn gt(self, value: impl Into<Value>) -> Condition {
        self.compare(CompareOp::Gt, value)
    }

    pub fn ge(self, value: impl Into<Value>) -> Condition {
        self.compare(CompareOp::Ge, value)
    }

    pub fn is_null(self) -> Condition {
        Condition { filter: Filter::Null { column: self.0, negated: false }, values: Vec::new() }
    }

    pub fn is_not_null(self) -> Condition {
        Condition { filter: Filter::Null { column: self.0, negated: true }, values: Vec::new() }
    }

    /// The column is one of the values. Always false for an empty list.
    pub fn is_in<T: Into<Value>>(self, values: impl IntoIterator<Item = T>) -> Condition {
        self.list(values, false)
    }

    /// The column is none of the values. Always true for an empty list. Like `NOT IN` in
    /// SQL, a NULL column matches neither `is_in` nor `not_in`.
    pub fn not_in<T: Into<Value>>(self, values: impl IntoIterator<Item = T>) -> Condition {
        self.list(values, true)
    }

    fn list<T: Into<Value>>(self, values: impl IntoIterator<Item = T>, negated: bool) -> Condition {
        match Values::new(values.into_iter().map(Into::into).collect()) {
            Some(values) => Condition {
                filter: Filter::In { column: self.0, param: 0, negated },
                values: vec![Bound::List(values)],
            },
            None if negated => Condition::all([]),
            None => Condition::any([]),
        }
    }

    /// The column matches a `LIKE` pattern, with `%` and `_` as wildcards.
    pub fn like(self, pattern: impl Into<String>) -> Condition {
        self.pattern(pattern, false)
    }

    /// The column matches a `LIKE` pattern, ignoring case.
    pub fn ilike(self, pattern: impl Into<String>) -> Condition {
        self.pattern(pattern, true)
    }

    fn pattern(self, pattern: impl Into<String>, case_insensitive: bool) -> Condition {
        Condition {
            filter: Filter::Like { column: self.0, param: 0, case_insensitive },
            values: vec![Bound::One(Value::Text(pattern.into()))],
        }
    }
}

/// A condition on the rows of the root query, built from [`col`] and combined with
/// [`Condition::and`] (`&`), [`Condition::or`] (`|`) and `!`.
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    filter: Filter,
    /// The values of the parameter slots of `filter`, in slot order.
    values: Vec<Bound>,
}

impl Condition {
    /// All of the conditions; true when there are none.
    pub fn all(conditions: impl IntoIterator<Item = Condition>) -> Condition {
        Condition::group(conditions, true)
    }

    /// Any of the conditions; false when there are none.
    pub fn any(conditions: impl IntoIterator<Item = Condition>) -> Condition {
        Condition::group(conditions, false)
    }

    fn group(conditions: impl IntoIterator<Item = Condition>, all: bool) -> Condition {
        let mut filters = Vec::new();
        let mut values = Vec::new();
        for condition in conditions {
            let mut filter = condition.filter;
            filter.shift(values.len());
            values.extend(condition.values);
            // Flatten groups of the same kind
            match filter {
                Filter::And(inner) if all => filters.extend(inner),
                Filter::Or(inner) if !all => filters.extend(inner),
                filter => filters.push(filter),
            }
        }
        let filter = if all { Filter::And(filters) } else { Filter::Or(filters) };
        Condition { filter, values }
    }

    /// Both conditions.
    pub fn and(self, other: Condition) -> Condition {
        Condition::all([self, other])
    }

    /// Either condition.
    pub fn or(self, other: Condition) -> Condition {
        Condition::any([self, other])
    }

    pub(crate) fn into_parts(self) -> (Filter, Vec<Bound>) {
        (self.filter, self.values)
    }
}

impl std::ops::BitAnd for Condition {
    type Output = Condition;

    fn bitand(self, other: Condition) -> Condition {
        self.and(other)
    }
}

impl std::ops::BitOr for Condition {
    type Output = Condition;

    fn bitor(self, other: Condition) -> Condition {
        self.or(other)
    }
}

impl std::ops::Not for Condition {
    type Output = Condition;

    fn not(self) -> Condition {
        Condition { filter: Filter::Not(Box::new(self.filter)), values: self.values }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(condition: &Condition) -> String {
        let mut out = String::new();
        condition.filter.render(&mut out, &|c| Some(c.to_string()), 1).unwrap();
        out
    }

    #[test]
    fn conditions_number_their_values_in_order() {
        let condition = col("a").eq(1_i64).and(col("b").is_in(["x", "y"]).or(!col("c").like("%z")));
        assert_eq!(render(&condition), "(a = $1) AND ((b = ANY($2)) OR (NOT (c LIKE $3)))");
        assert_eq!(
            condition.values,
            [
                Bound::One(Value::I64(1)),
                Bound::List(Values::Text(vec!["x".into(), "y".into()])),
                Bound::One(Value::Text("%z".into())),
            ]
        );
    }

    #[test]
    fn groups_are_flattened() {
        let condition = col("a").is_null().and(col("b").is_not_null()).and(col("c").ge(2_i32));
        assert_eq!(render(&condition), "(a IS NULL) AND (b IS NOT NULL) AND (c >= $1)");
    }

    #[test]
    fn empty_lists_are_constants() {
        assert_eq!(render(&col("a").is_in(Vec::<i64>::new())), "FALSE");
        assert_eq!(render(&col("a").not_in(Vec::<i64>::new())), "TRUE");
        assert!(col("a").is_in(Vec::<i64>::new()).values.is_empty());
    }
}
