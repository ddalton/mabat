//! SQLite.
//!
//! SQLite types are affinities: a column declared `INT` or `BIGINT` is `INTEGER`, one
//! declared `VARCHAR(40)` is `TEXT`. The type of an expression is not known before it runs,
//! so such columns are [`KeyKind::Dynamic`] and their checks are skipped.

use mabat_core::sql::Dialect;
use sqlx::sqlite::{SqliteConnection, SqliteRow, SqliteStatement, SqliteTypeInfo};
use sqlx::{Column, Either, Row, Sqlite, Statement, TypeInfo, ValueRef};
use uuid::Uuid;

use super::{Backend, Inspected, InspectedColumn, InspectedParams, KeyKind};
use crate::key::Key;

connection!(Sqlite, SqliteConnection);
bind_each!(Sqlite);

impl Backend for Sqlite {
    const DIALECT: Dialect = Dialect::Sqlite;

    fn add_keys(
        args: &mut <Sqlite as sqlx::Database>::Arguments,
        keys: &crate::key::KeyList,
    ) -> Result<(), sqlx::error::BoxDynError> {
        add_each_key(args, keys)
    }

    fn read_catalog<'c>(
        conn: &'c mut SqliteConnection,
    ) -> super::BoxFuture<'c, Result<Vec<mabat_check::Table>, sqlx::Error>> {
        Box::pin(async move {
            let mut catalog = super::Catalog::default();
            let tables = sqlx::query(
                "SELECT name, type = 'view' FROM sqlite_master \
                 WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .fetch_all(&mut *conn)
            .await?;
            for row in &tables {
                catalog.tables.push((row.try_get(0)?, row.try_get(1)?));
            }
            let names: Vec<String> = catalog.tables.iter().map(|(name, _)| name.clone()).collect();
            for table in names {
                let columns = sqlx::query(
                    "SELECT name, type, \"notnull\", pk, \
                     (SELECT count(*) FROM pragma_table_info(?1) WHERE pk > 0) FROM pragma_table_info(?1) ORDER BY cid",
                )
                .bind(&table)
                .fetch_all(&mut *conn)
                .await?;
                let mut keys: Vec<(i64, String)> = Vec::new();
                for row in &columns {
                    let name: String = row.try_get(0)?;
                    let declared: String = row.try_get(1)?;
                    let pk: i64 = row.try_get(3)?;
                    let key_columns: i64 = row.try_get(4)?;
                    if pk > 0 {
                        keys.push((pk, name.clone()));
                    }
                    // A single-column INTEGER PRIMARY KEY is the rowid, which SQLite generates
                    let rowid = pk > 0 && key_columns == 1 && declared.eq_ignore_ascii_case("INTEGER");
                    let column = mabat_check::Column {
                        name,
                        r#type: String::new(),
                        declared,
                        nullable: row.try_get::<i64, _>(2)? == 0 && pk == 0,
                        generated: rowid,
                    };
                    catalog.columns.push((table.clone(), column));
                }
                keys.sort();
                catalog.primary_keys.extend(keys.into_iter().map(|(_, column)| (table.clone(), column)));
                let foreign = sqlx::query(
                    "SELECT id, \"table\", \"from\", \"to\" FROM pragma_foreign_key_list(?) ORDER BY id, seq",
                )
                .bind(&table)
                .fetch_all(&mut *conn)
                .await?;
                for row in &foreign {
                    let id: i64 = row.try_get(0)?;
                    let to: Option<String> = row.try_get(3)?;
                    catalog.foreign_keys.push((
                        table.clone(),
                        id.to_string(),
                        row.try_get(2)?,
                        row.try_get(1)?,
                        to.unwrap_or_default(),
                    ));
                }
            }
            // A foreign key without columns references the primary key of its table
            let primary: Vec<(String, String)> = catalog.primary_keys.clone();
            for (_, _, _, referenced, reference) in &mut catalog.foreign_keys {
                if reference.is_empty()
                    && let Some((_, column)) = primary.iter().find(|(t, _)| t == referenced)
                {
                    *reference = column.clone();
                }
            }
            Ok(catalog.tables())
        })
    }

    fn insert_generated<'c>(
        conn: &'c mut SqliteConnection,
        sql: String,
        args: <Sqlite as sqlx::Database>::Arguments,
    ) -> super::BoxFuture<'c, Result<Key, sqlx::Error>> {
        Box::pin(async move {
            let text = sql.clone();
            let run = sqlx::query_with(sqlx::AssertSqlSafe(sql), args).fetch_one(conn);
            let row = crate::trace::statement(<Self as sqlx::Database>::NAME, &text, |_| 1, run).await?;
            super::returned_key::<Self>(&row)
        })
    }

    common_methods!(Sqlite, SqliteConnection, SqliteRow);

    fn key_kind(ty: &SqliteTypeInfo) -> Option<KeyKind> {
        Some(match ty.name() {
            "INTEGER" => KeyKind::I64,
            "TEXT" => KeyKind::Text,
            // How SQLx stores a UUID
            "BLOB" => KeyKind::Uuid,
            // An expression, or a column with numeric affinity that may hold integers
            "NULL" | "NUMERIC" => KeyKind::Dynamic,
            _ => return None,
        })
    }

    fn read_key(row: &SqliteRow, ordinal: usize, kind: KeyKind) -> Result<Option<Key>, sqlx::Error> {
        let raw = row.try_get_raw(ordinal)?;
        if raw.is_null() {
            return Ok(None);
        }
        let kind = match kind {
            // Read by the type of the value
            KeyKind::Dynamic => match raw.type_info().name() {
                "INTEGER" => KeyKind::I64,
                "BLOB" => KeyKind::Uuid,
                _ => KeyKind::Text,
            },
            kind => kind,
        };
        Ok(Some(match kind {
            KeyKind::Uuid => Key::Uuid(row.try_get_unchecked::<Uuid, _>(ordinal)?),
            KeyKind::Text => Key::Text(row.try_get_unchecked::<String, _>(ordinal)?),
            _ => Key::Int(row.try_get_unchecked::<i64, _>(ordinal)?),
        }))
    }

    fn encoded(row: &SqliteRow, ordinal: usize) -> Result<Option<Vec<u8>>, sqlx::Error> {
        let raw = row.try_get_raw(ordinal)?;
        if raw.is_null() {
            return Ok(None);
        }
        // Tagged with the storage class, so that 1 and '1' differ as they do in SQLite
        let (tag, bytes) = match raw.type_info().name() {
            "INTEGER" => (b'i', row.try_get_unchecked::<i64, _>(ordinal)?.to_be_bytes().to_vec()),
            "REAL" => (b'r', row.try_get_unchecked::<f64, _>(ordinal)?.to_bits().to_be_bytes().to_vec()),
            "BLOB" => (b'b', row.try_get_unchecked::<Vec<u8>, _>(ordinal)?),
            _ => (b't', row.try_get_unchecked::<String, _>(ordinal)?.into_bytes()),
        };
        let mut encoded = Vec::with_capacity(bytes.len() + 1);
        encoded.push(tag);
        encoded.extend(bytes);
        Ok(Some(encoded))
    }

    fn type_catalog() -> &'static [SqliteTypeInfo] {
        static TYPES: std::sync::OnceLock<Vec<SqliteTypeInfo>> = std::sync::OnceLock::new();
        TYPES.get_or_init(|| {
            BUILT_IN_TYPES
                .iter()
                .filter_map(|variant| serde_json::from_value::<SqliteTypeInfo>(serde_json::Value::from(*variant)).ok())
                .collect()
        })
    }

    fn type_name(ty: &SqliteTypeInfo) -> String {
        ty.name().to_string()
    }
}

fn inspected(statement: &SqliteStatement) -> Inspected {
    let columns = statement
        .columns()
        .iter()
        .map(|column| {
            let ty = column.type_info();
            InspectedColumn {
                name: column.name().to_string(),
                type_name: (!ty.is_null()).then(|| Sqlite::type_name(ty)),
                key_kind: Sqlite::key_kind(ty),
            }
        })
        .collect();
    let params = match statement.parameters() {
        Some(Either::Right(count)) => InspectedParams::Count(count),
        Some(Either::Left(types)) => InspectedParams::Count(types.len()),
        None => InspectedParams::Count(0),
    };
    Inspected { columns, params }
}

/// SQLx's SQLite types (the variants of its `DataType`), resolved with the `offline` feature
/// of `sqlx-sqlite`. `Int4`, also named `INTEGER`, and `Null`, the type of an expression, are
/// left out.
const BUILT_IN_TYPES: &[&str] = &["Integer", "Float", "Text", "Blob", "Numeric", "Bool", "Date", "Time", "Datetime"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_type_resolves() {
        assert_eq!(Sqlite::type_catalog().len(), BUILT_IN_TYPES.len());
    }
}
