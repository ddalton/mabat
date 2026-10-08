//! Entity keys.

use uuid::Uuid;

use crate::backend::{Backend, KeyKind};

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

/// A key column of a query result, resolved once so that reading keys does not look up
/// the column by name for every row. All rows of a result share the same columns.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KeyColumn {
    ordinal: usize,
    kind: KeyKind,
}

impl KeyColumn {
    /// Resolve the key column with the given alias from a row of the result.
    pub(crate) fn resolve<B: Backend>(row: &B::Row, alias: &str) -> Result<KeyColumn, sqlx::Error> {
        let Some(ordinal) = B::find_column(row, alias) else {
            return Err(sqlx::Error::ColumnNotFound(alias.to_string()));
        };
        let ty = B::column_type(row, ordinal);
        let Some(kind) = B::key_kind(ty) else {
            return Err(sqlx::Error::ColumnDecode {
                index: alias.to_string(),
                source: format!("unsupported key type {}, expected an integer, text or uuid column", B::type_name(ty))
                    .into(),
            });
        };
        Ok(KeyColumn { ordinal, kind })
    }

    /// Read the key of a row, `None` if the value is NULL.
    pub(crate) fn read<B: Backend>(&self, row: &B::Row) -> Result<Option<Key>, sqlx::Error> {
        B::read_key(row, self.ordinal, self.kind)
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

/// Keys of one type: bound as one array parameter on PostgreSQL, one parameter per key
/// elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyList {
    Int(Vec<i64>),
    Text(Vec<String>),
    Uuid(Vec<Uuid>),
}

impl KeyList {
    /// Group keys into an array. All keys need to be of the same type.
    pub(crate) fn new(keys: Vec<Key>) -> Result<KeyList, MixedKeys> {
        let mut iter = keys.into_iter();
        let Some(first) = iter.next() else {
            return Ok(KeyList::Int(Vec::new()));
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
                Ok(KeyList::Int(values))
            }
            Key::Text(v) => {
                let mut values = vec![v];
                for key in iter {
                    match key {
                        Key::Text(v) => values.push(v),
                        _ => return Err(MixedKeys),
                    }
                }
                Ok(KeyList::Text(values))
            }
            Key::Uuid(v) => {
                let mut values = vec![v];
                for key in iter {
                    match key {
                        Key::Uuid(v) => values.push(v),
                        _ => return Err(MixedKeys),
                    }
                }
                Ok(KeyList::Uuid(values))
            }
        }
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            KeyList::Int(keys) => keys.len(),
            KeyList::Text(keys) => keys.len(),
            KeyList::Uuid(keys) => keys.len(),
        }
    }

    /// Pad the list to `len` keys by repeating the last key, so that MySQL and SQLite run
    /// the same statement for lists of similar lengths. Repeated keys select nothing more.
    pub(crate) fn pad(&mut self, len: usize) {
        fn pad<T: Clone>(keys: &mut Vec<T>, len: usize) {
            if let Some(last) = keys.last().cloned() {
                keys.resize(len.max(keys.len()), last);
            }
        }
        match self {
            KeyList::Int(keys) => pad(keys, len),
            KeyList::Text(keys) => pad(keys, len),
            KeyList::Uuid(keys) => pad(keys, len),
        }
    }

    /// Split the list into lists of at most `size` keys.
    pub(crate) fn chunks(self, size: usize) -> Vec<KeyList> {
        fn split<T>(keys: Vec<T>, size: usize, wrap: fn(Vec<T>) -> KeyList) -> Vec<KeyList> {
            let mut out = Vec::new();
            let mut keys = keys.into_iter().peekable();
            while keys.peek().is_some() {
                out.push(wrap(keys.by_ref().take(size).collect()));
            }
            out
        }
        match self {
            KeyList::Int(keys) => split(keys, size, KeyList::Int),
            KeyList::Text(keys) => split(keys, size, KeyList::Text),
            KeyList::Uuid(keys) => split(keys, size, KeyList::Uuid),
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
        assert_eq!(KeyList::new(vec![Key::Int(1), Key::Int(2)]), Ok(KeyList::Int(vec![1, 2])));
        assert_eq!(KeyList::new(vec![Key::Int(1), Key::from("a")]), Err(MixedKeys));
        assert_eq!(KeyList::new(vec![]), Ok(KeyList::Int(vec![])));
    }
}
