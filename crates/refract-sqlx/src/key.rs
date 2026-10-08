//! Entity keys.

use sqlx::postgres::{PgArguments, PgRow, PgTypeInfo, PgTypeKind};
use sqlx::query::Query;
use sqlx::{Column, Postgres, Row, TypeInfo};
use uuid::Uuid;

/// The value of a key or foreign key column.
///
/// All integer types are normalized to `i64`, so a foreign key of a different integer
/// width than the key it references still matches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    Int(i64),
    Text(String),
    Uuid(Uuid),
}

/// The type of a key column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyKind {
    Int2,
    Int4,
    Int8,
    Uuid,
    Text,
}

impl KeyKind {
    fn from_type_name(name: &str) -> Option<KeyKind> {
        Some(match name {
            "INT2" => KeyKind::Int2,
            "INT4" => KeyKind::Int4,
            "INT8" => KeyKind::Int8,
            "UUID" => KeyKind::Uuid,
            "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => KeyKind::Text,
            _ => return None,
        })
    }
}

/// The kind of value a key column holds. Keys of the same class compare equal across
/// column types, e.g. an `INT4` foreign key referencing an `INT8` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyClass {
    Int,
    Text,
    Uuid,
}

impl KeyClass {
    /// The class of a column type, `None` if it cannot hold a key.
    pub(crate) fn of(ty: &PgTypeInfo) -> Option<KeyClass> {
        KeyKind::from_type_name(ty.name()).map(|kind| match kind {
            KeyKind::Int2 | KeyKind::Int4 | KeyKind::Int8 => KeyClass::Int,
            KeyKind::Uuid => KeyClass::Uuid,
            KeyKind::Text => KeyClass::Text,
        })
    }

    /// The class of the elements of an array type, `None` if it is not an array of keys.
    pub(crate) fn of_array(ty: &PgTypeInfo) -> Option<KeyClass> {
        match ty.kind() {
            PgTypeKind::Array(element) => KeyClass::of(element),
            _ => None,
        }
    }
}

impl std::fmt::Display for KeyClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            KeyClass::Int => "an integer",
            KeyClass::Text => "a text",
            KeyClass::Uuid => "a uuid",
        })
    }
}

/// A key column of a query result, resolved once so that reading keys does not look up
/// the column by name for every row. All rows of a result share the same columns.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KeyColumn {
    ordinal: usize,
    kind: KeyKind,
}

impl KeyColumn {
    /// Resolve the key column with the given alias from a row of the result.
    pub(crate) fn resolve(row: &PgRow, alias: &str) -> Result<KeyColumn, sqlx::Error> {
        let column = row.try_column(alias)?;
        let name = column.type_info().name();
        let Some(kind) = KeyKind::from_type_name(name) else {
            return Err(sqlx::Error::ColumnDecode {
                index: alias.to_string(),
                source: format!("unsupported key type {name}, expected an integer, text or uuid column").into(),
            });
        };
        Ok(KeyColumn { ordinal: column.ordinal(), kind })
    }

    /// Read the key of a row, `None` if the value is NULL.
    pub(crate) fn read(&self, row: &PgRow) -> Result<Option<Key>, sqlx::Error> {
        let index = self.ordinal;
        Ok(match self.kind {
            KeyKind::Int2 => row.try_get::<Option<i16>, _>(index)?.map(|v| Key::Int(v.into())),
            KeyKind::Int4 => row.try_get::<Option<i32>, _>(index)?.map(|v| Key::Int(v.into())),
            KeyKind::Int8 => row.try_get::<Option<i64>, _>(index)?.map(Key::Int),
            KeyKind::Uuid => row.try_get::<Option<Uuid>, _>(index)?.map(Key::Uuid),
            KeyKind::Text => row.try_get::<Option<String>, _>(index)?.map(Key::Text),
        })
    }
}

impl From<i16> for Key {
    fn from(value: i16) -> Self {
        Key::Int(value.into())
    }
}

impl From<i32> for Key {
    fn from(value: i32) -> Self {
        Key::Int(value.into())
    }
}

impl From<i64> for Key {
    fn from(value: i64) -> Self {
        Key::Int(value)
    }
}

impl From<String> for Key {
    fn from(value: String) -> Self {
        Key::Text(value)
    }
}

impl From<&str> for Key {
    fn from(value: &str) -> Self {
        Key::Text(value.to_string())
    }
}

impl From<Uuid> for Key {
    fn from(value: Uuid) -> Self {
        Key::Uuid(value)
    }
}

/// Keys of one type, bound as an array parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeyArray {
    Int(Vec<i64>),
    Text(Vec<String>),
    Uuid(Vec<Uuid>),
}

impl KeyArray {
    /// Group keys into an array. All keys need to be of the same type.
    pub(crate) fn new(keys: Vec<Key>) -> Result<KeyArray, MixedKeys> {
        let mut iter = keys.into_iter();
        let Some(first) = iter.next() else {
            return Ok(KeyArray::Int(Vec::new()));
        };
        match first {
            Key::Int(v) => {
                let mut values = vec![v];
                for key in iter {
                    match key {
                        Key::Int(v) => values.push(v),
                        _ => return Err(MixedKeys),
                    }
                }
                Ok(KeyArray::Int(values))
            }
            Key::Text(v) => {
                let mut values = vec![v];
                for key in iter {
                    match key {
                        Key::Text(v) => values.push(v),
                        _ => return Err(MixedKeys),
                    }
                }
                Ok(KeyArray::Text(values))
            }
            Key::Uuid(v) => {
                let mut values = vec![v];
                for key in iter {
                    match key {
                        Key::Uuid(v) => values.push(v),
                        _ => return Err(MixedKeys),
                    }
                }
                Ok(KeyArray::Uuid(values))
            }
        }
    }

    pub(crate) fn bind<'q>(self, query: Query<'q, Postgres, PgArguments>) -> Query<'q, Postgres, PgArguments> {
        match self {
            KeyArray::Int(values) => query.bind(values),
            KeyArray::Text(values) => query.bind(values),
            KeyArray::Uuid(values) => query.bind(values),
        }
    }
}

/// Keys of different types were combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MixedKeys;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_widths_compare_equal() {
        assert_eq!(Key::from(7_i16), Key::from(7_i64));
        assert_eq!(Key::from(7_i32), Key::Int(7));
    }

    #[test]
    fn arrays_need_one_type() {
        assert_eq!(KeyArray::new(vec![Key::Int(1), Key::Int(2)]), Ok(KeyArray::Int(vec![1, 2])));
        assert_eq!(KeyArray::new(vec![Key::Int(1), Key::from("a")]), Err(MixedKeys));
        assert_eq!(KeyArray::new(vec![]), Ok(KeyArray::Int(vec![])));
    }
}
