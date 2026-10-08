//! The arguments of fields: `where` as a [`Condition`], `orderBy`, `limit`, `offset`, and
//! keys. Root fields and nested collections take the same arguments.

use async_graphql::{Name, Value};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use mabat::filter::{self, Condition, col};
use mabat::shape::{FieldKind, Scalar, ViewShape};
use mabat::{Key, Nested};
use uuid::Uuid;

use crate::types::field_name;

type Result<T> = async_graphql::Result<T>;

/// `true` if columns of the scalar can be compared with filter values.
pub(crate) fn comparable(scalar: Scalar) -> bool {
    matches!(
        scalar,
        Scalar::Boolean
            | Scalar::Int
            | Scalar::BigInt
            | Scalar::Float
            | Scalar::String
            | Scalar::Uuid
            | Scalar::Date
            | Scalar::DateTime
            | Scalar::NaiveDateTime
            | Scalar::Decimal
    )
}

fn string(input: &Value) -> Result<&str> {
    match input {
        Value::String(text) => Ok(text),
        other => Err(format!("expected a string, found {other}").into()),
    }
}

fn integer(input: &Value) -> Result<i64> {
    match input {
        Value::Number(number) => number.as_i64().ok_or_else(|| format!("expected an integer, found {number}").into()),
        other => Err(format!("expected an integer, found {other}").into()),
    }
}

fn list(input: &Value) -> Result<&[Value]> {
    match input {
        Value::List(values) => Ok(values),
        other => Err(format!("expected a list, found {other}").into()),
    }
}

fn object(input: &Value) -> Result<&async_graphql::indexmap::IndexMap<Name, Value>> {
    match input {
        Value::Object(fields) => Ok(fields),
        other => Err(format!("expected an object, found {other}").into()),
    }
}

/// The value of an argument for a column of the scalar.
fn value(scalar: Scalar, input: &Value) -> Result<filter::Value> {
    Ok(match scalar {
        Scalar::Boolean => match input {
            Value::Boolean(value) => filter::Value::Bool(*value),
            other => return Err(format!("expected a boolean, found {other}").into()),
        },
        Scalar::Int | Scalar::BigInt => filter::Value::I64(integer(input)?),
        // Compared as a double, which every database compares with its decimal type
        Scalar::Float | Scalar::Decimal => match input {
            Value::Number(number) => filter::Value::F64(number.as_f64().unwrap_or_default()),
            other => filter::Value::F64(string(other)?.parse()?),
        },
        Scalar::String => filter::Value::Text(string(input)?.to_string()),
        Scalar::Uuid => filter::Value::Uuid(string(input)?.parse::<Uuid>()?),
        Scalar::Date => filter::Value::Date(string(input)?.parse::<NaiveDate>()?),
        Scalar::DateTime => filter::Value::Timestamptz(string(input)?.parse::<DateTime<Utc>>()?),
        Scalar::NaiveDateTime => {
            let text = string(input)?;
            filter::Value::Timestamp(
                NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
                    .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f"))?,
            )
        }
        other => return Err(format!("columns of type {other:?} cannot be filtered").into()),
    })
}

/// The column and scalar of a filterable field of the view.
fn column(shape: &ViewShape, name: &str) -> Result<(&'static str, Scalar)> {
    shape
        .fields
        .iter()
        .find_map(|field| match field.kind {
            FieldKind::Column { column, ty } if field_name(field) == name && !ty.list => Some((column, ty.scalar)),
            _ => None,
        })
        .ok_or_else(|| format!("{} has no column field {name}", shape.name).into())
}

/// The condition of a `where` argument: all of its entries.
pub(crate) fn condition(shape: &'static ViewShape, input: &Value) -> Result<Condition> {
    let mut conditions = Vec::new();
    for (name, value) in object(input)? {
        if matches!(value, Value::Null) {
            continue;
        }
        match name.as_str() {
            "and" => {
                let all = list(value)?.iter().map(|v| condition(shape, v)).collect::<Result<Vec<_>>>()?;
                conditions.push(Condition::all(all));
            }
            "or" => {
                let any = list(value)?.iter().map(|v| condition(shape, v)).collect::<Result<Vec<_>>>()?;
                conditions.push(Condition::any(any));
            }
            "not" => conditions.push(!condition(shape, value)?),
            field => {
                let (column, scalar) = self::column(shape, field)?;
                for (op, operand) in object(value)? {
                    if matches!(operand, Value::Null) {
                        continue;
                    }
                    let one = || self::value(scalar, operand);
                    let values = || list(operand)?.iter().map(|v| self::value(scalar, v)).collect::<Result<Vec<_>>>();
                    conditions.push(match op.as_str() {
                        "eq" => col(column).eq(one()?),
                        "ne" => col(column).ne(one()?),
                        "lt" => col(column).lt(one()?),
                        "le" => col(column).le(one()?),
                        "gt" => col(column).gt(one()?),
                        "ge" => col(column).ge(one()?),
                        "in" => col(column).is_in(values()?),
                        "notIn" => col(column).not_in(values()?),
                        "isNull" if matches!(operand, Value::Boolean(true)) => col(column).is_null(),
                        "isNull" => col(column).is_not_null(),
                        "like" => col(column).like(string(operand)?),
                        "ilike" => col(column).ilike(string(operand)?),
                        other => return Err(format!("unknown filter operator {other}").into()),
                    });
                }
            }
        }
    }
    Ok(Condition::all(conditions))
}

/// The columns of an `orderBy` argument, with `true` for descending. Each element orders by
/// its fields, in order.
pub(crate) fn order(shape: &'static ViewShape, input: &Value) -> Result<Vec<(&'static str, bool)>> {
    let mut order = Vec::new();
    for element in list(input)? {
        for (name, direction) in object(element)? {
            let descending = match direction {
                Value::Null => continue,
                Value::Enum(direction) => direction.as_str() == "DESC",
                other => return Err(format!("expected ASC or DESC, found {other}").into()),
            };
            order.push((column(shape, name.as_str())?.0, descending));
        }
    }
    Ok(order)
}

/// A `limit` or `offset` argument, which cannot be negative.
pub(crate) fn count(name: &str, input: Option<&Value>) -> Result<Option<u64>> {
    match input {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            u64::try_from(integer(value)?).map(Some).map_err(|_| format!("{name} cannot be negative").into())
        }
    }
}

/// The arguments of a field: its `where`, `orderBy`, `limit` and `offset`.
pub(crate) struct Arguments {
    pub(crate) condition: Option<Condition>,
    pub(crate) order: Vec<(&'static str, bool)>,
    pub(crate) limit: Option<u64>,
    pub(crate) offset: Option<u64>,
}

impl Arguments {
    pub(crate) fn parse(shape: &'static ViewShape, get: impl Fn(&str) -> Option<Value>) -> Result<Arguments> {
        let present = |name: &str| get(name).filter(|value| !matches!(value, Value::Null));
        Ok(Arguments {
            condition: present("where").map(|filter| condition(shape, &filter)).transpose()?,
            order: present("orderBy").map(|order| self::order(shape, &order)).transpose()?.unwrap_or_default(),
            limit: count("limit", get("limit").as_ref())?,
            offset: count("offset", get("offset").as_ref())?,
        })
    }

    /// The arguments of a nested collection.
    pub(crate) fn nested(self) -> Nested {
        let mut nested = Nested::new();
        if let Some(condition) = self.condition {
            nested = nested.filter(condition);
        }
        for (column, descending) in self.order {
            nested = if descending { nested.order_by_desc(column) } else { nested.order_by(column) };
        }
        if let Some(limit) = self.limit {
            nested = nested.limit(limit);
        }
        if let Some(offset) = self.offset {
            nested = nested.offset(offset);
        }
        nested
    }
}

/// The key of a `key` argument, of the scalar of the view's key field. Without a key field,
/// an `ID` that is an integer, else a UUID, else text.
pub(crate) fn key(scalar: Option<Scalar>, input: &Value) -> Result<Key> {
    Ok(match scalar {
        Some(Scalar::Int | Scalar::BigInt) => Key::from(integer(input)?),
        Some(Scalar::Uuid) => Key::from(string(input)?.parse::<Uuid>()?),
        Some(Scalar::String) => Key::from(string(input)?),
        Some(other) => return Err(format!("keys of type {other:?} are not supported").into()),
        None => {
            if let Ok(id) = integer(input) {
                return Ok(Key::from(id));
            }
            let id = string(input)?;
            match (id.parse::<i64>(), id.parse::<Uuid>()) {
                (Ok(id), _) => Key::from(id),
                (_, Ok(id)) => Key::from(id),
                _ => Key::from(id),
            }
        }
    })
}
