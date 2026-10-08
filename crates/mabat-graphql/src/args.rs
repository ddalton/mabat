//! The arguments of root fields: `where` as a [`Condition`], `orderBy`, and keys.

use async_graphql::dynamic::{ObjectAccessor, ValueAccessor};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use mabat::Key;
use mabat::filter::{Condition, Value, col};
use mabat::shape::{FieldKind, Scalar, ViewShape};
use uuid::Uuid;

use crate::types::field_name;

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

/// The value of an argument for a column of the scalar.
fn value(scalar: Scalar, input: &ValueAccessor<'_>) -> async_graphql::Result<Value> {
    Ok(match scalar {
        Scalar::Boolean => Value::Bool(input.boolean()?),
        Scalar::Int | Scalar::BigInt => Value::I64(input.i64()?),
        // Compared as a double, which every database compares with its decimal type
        Scalar::Float | Scalar::Decimal => match input.f64() {
            Ok(value) => Value::F64(value),
            Err(_) => Value::F64(input.string()?.parse()?),
        },
        Scalar::String => Value::Text(input.string()?.to_string()),
        Scalar::Uuid => Value::Uuid(input.string()?.parse::<Uuid>()?),
        Scalar::Date => Value::Date(input.string()?.parse::<NaiveDate>()?),
        Scalar::DateTime => Value::Timestamptz(input.string()?.parse::<DateTime<Utc>>()?),
        Scalar::NaiveDateTime => {
            let text = input.string()?;
            Value::Timestamp(
                NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
                    .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f"))?,
            )
        }
        other => return Err(format!("columns of type {other:?} cannot be filtered").into()),
    })
}

/// The column and scalar of a filterable field of the view.
fn column(shape: &ViewShape, name: &str) -> async_graphql::Result<(&'static str, Scalar)> {
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
pub(crate) fn condition(shape: &'static ViewShape, input: &ObjectAccessor<'_>) -> async_graphql::Result<Condition> {
    let mut conditions = Vec::new();
    for (name, value) in input.iter() {
        if value.is_null() {
            continue;
        }
        match name.as_str() {
            "and" => {
                let all =
                    value.list()?.iter().map(|v| condition(shape, &v.object()?)).collect::<Result<Vec<_>, _>>()?;
                conditions.push(Condition::all(all));
            }
            "or" => {
                let any =
                    value.list()?.iter().map(|v| condition(shape, &v.object()?)).collect::<Result<Vec<_>, _>>()?;
                conditions.push(Condition::any(any));
            }
            "not" => conditions.push(!condition(shape, &value.object()?)?),
            field => {
                let (column, scalar) = self::column(shape, field)?;
                for (op, operand) in value.object()?.iter() {
                    if operand.is_null() {
                        continue;
                    }
                    let list = || -> async_graphql::Result<Vec<Value>> {
                        operand.list()?.iter().map(|v| self::value(scalar, &v)).collect()
                    };
                    conditions.push(match op.as_str() {
                        "eq" => col(column).eq(self::value(scalar, &operand)?),
                        "ne" => col(column).ne(self::value(scalar, &operand)?),
                        "lt" => col(column).lt(self::value(scalar, &operand)?),
                        "le" => col(column).le(self::value(scalar, &operand)?),
                        "gt" => col(column).gt(self::value(scalar, &operand)?),
                        "ge" => col(column).ge(self::value(scalar, &operand)?),
                        "in" => col(column).is_in(list()?),
                        "notIn" => col(column).not_in(list()?),
                        "isNull" if operand.boolean()? => col(column).is_null(),
                        "isNull" => col(column).is_not_null(),
                        "like" => col(column).like(operand.string()?),
                        "ilike" => col(column).ilike(operand.string()?),
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
pub(crate) fn order(
    shape: &'static ViewShape,
    input: &ValueAccessor<'_>,
) -> async_graphql::Result<Vec<(&'static str, bool)>> {
    let mut order = Vec::new();
    for element in input.list()?.iter() {
        for (name, direction) in element.object()?.iter() {
            if direction.is_null() {
                continue;
            }
            let (column, _) = column(shape, name.as_str())?;
            order.push((column, direction.enum_name()? == "DESC"));
        }
    }
    Ok(order)
}

/// The key of a `key` argument, of the scalar of the view's key field. Without a key field,
/// an `ID` that is an integer, else a UUID, else text.
pub(crate) fn key(scalar: Option<Scalar>, input: &ValueAccessor<'_>) -> async_graphql::Result<Key> {
    Ok(match scalar {
        Some(Scalar::Int | Scalar::BigInt) => Key::from(input.i64()?),
        Some(Scalar::Uuid) => Key::from(input.string()?.parse::<Uuid>()?),
        Some(Scalar::String) => Key::from(input.string()?),
        Some(other) => return Err(format!("keys of type {other:?} are not supported").into()),
        None => {
            let id = match input.i64() {
                Ok(id) => return Ok(Key::from(id)),
                Err(_) => input.string()?,
            };
            match (id.parse::<i64>(), id.parse::<Uuid>()) {
                (Ok(id), _) => Key::from(id),
                (_, Ok(id)) => Key::from(id),
                _ => Key::from(id),
            }
        }
    })
}
