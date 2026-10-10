//! MySQL.
//!
//! A column type is a type code, flags (`UNSIGNED`, `BINARY`, …) and a collation: text and
//! binary strings share their type codes and differ by collation, `63` being binary.

use mabat_core::sql::Dialect;
use sqlx::mysql::{MySqlConnection, MySqlRow, MySqlStatement, MySqlTypeInfo};
use sqlx::{Column, Either, MySql, Row, Statement, TypeInfo, ValueRef};
use uuid::Uuid;

use super::{Backend, Inspected, InspectedColumn, InspectedParams, KeyKind};
use crate::key::Key;

connection!(MySql, MySqlConnection);
bind_each!(MySql);

impl Backend for MySql {
    const DIALECT: Dialect = Dialect::MySql;

    fn add_keys(
        args: &mut <MySql as sqlx::Database>::Arguments,
        keys: &crate::key::KeyList,
    ) -> Result<(), sqlx::error::BoxDynError> {
        add_each_key(args, keys)
    }

    fn read_catalog<'c>(
        conn: &'c mut MySqlConnection,
    ) -> super::BoxFuture<'c, Result<Vec<mabat_check::Table>, sqlx::Error>> {
        Box::pin(async move {
            let mut catalog = super::Catalog::default();
            let tables = sqlx::query(
                "SELECT CAST(TABLE_NAME AS CHAR), CAST(TABLE_TYPE = 'VIEW' AS SIGNED) FROM information_schema.TABLES \
                 WHERE TABLE_SCHEMA = DATABASE() ORDER BY 1",
            )
            .fetch_all(&mut *conn)
            .await?;
            for row in &tables {
                catalog.tables.push((row.try_get(0)?, row.try_get::<i64, _>(1)? != 0));
            }
            let columns = sqlx::query(
                "SELECT CAST(TABLE_NAME AS CHAR), CAST(COLUMN_NAME AS CHAR), CAST(COLUMN_TYPE AS CHAR), \
                 CAST(IS_NULLABLE = 'YES' AS SIGNED), CAST(EXTRA LIKE '%auto_increment%' AS SIGNED) \
                 FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() ORDER BY 1, ORDINAL_POSITION",
            )
            .fetch_all(&mut *conn)
            .await?;
            for row in &columns {
                let column = mabat_check::Column {
                    name: row.try_get(1)?,
                    r#type: String::new(),
                    declared: row.try_get(2)?,
                    nullable: row.try_get::<i64, _>(3)? != 0,
                    generated: row.try_get::<i64, _>(4)? != 0,
                };
                catalog.columns.push((row.try_get(0)?, column));
            }
            let keys = sqlx::query(
                "SELECT CAST(TABLE_NAME AS CHAR), CAST(COLUMN_NAME AS CHAR) FROM information_schema.KEY_COLUMN_USAGE \
                 WHERE TABLE_SCHEMA = DATABASE() AND CONSTRAINT_NAME = 'PRIMARY' ORDER BY 1, ORDINAL_POSITION",
            )
            .fetch_all(&mut *conn)
            .await?;
            for row in &keys {
                catalog.primary_keys.push((row.try_get(0)?, row.try_get(1)?));
            }
            let foreign = sqlx::query(
                "SELECT CAST(TABLE_NAME AS CHAR), CAST(CONSTRAINT_NAME AS CHAR), CAST(COLUMN_NAME AS CHAR), \
                 CAST(REFERENCED_TABLE_NAME AS CHAR), CAST(REFERENCED_COLUMN_NAME AS CHAR) \
                 FROM information_schema.KEY_COLUMN_USAGE \
                 WHERE TABLE_SCHEMA = DATABASE() AND REFERENCED_TABLE_NAME IS NOT NULL ORDER BY 1, 2, ORDINAL_POSITION",
            )
            .fetch_all(&mut *conn)
            .await?;
            for row in &foreign {
                catalog.foreign_keys.push((
                    row.try_get(0)?,
                    row.try_get(1)?,
                    row.try_get(2)?,
                    row.try_get(3)?,
                    row.try_get(4)?,
                ));
            }
            Ok(catalog.tables())
        })
    }

    fn insert_generated<'c>(
        conn: &'c mut MySqlConnection,
        sql: String,
        args: <MySql as sqlx::Database>::Arguments,
    ) -> super::BoxFuture<'c, Result<Key, sqlx::Error>> {
        Box::pin(async move {
            let text = sql.clone();
            let run = sqlx::query_with(sqlx::AssertSqlSafe(sql), args).execute(conn);
            let done = crate::trace::statement(<Self as sqlx::Database>::NAME, &text, |done| done.rows_affected(), run)
                .await?;
            let key = i64::try_from(done.last_insert_id()).map_err(|e| sqlx::Error::ColumnDecode {
                index: "LAST_INSERT_ID()".to_string(),
                source: Box::new(e),
            })?;
            Ok(Key::Int(key))
        })
    }

    common_methods!(MySql, MySqlConnection, MySqlRow);

    fn key_kind(ty: &MySqlTypeInfo) -> Option<KeyKind> {
        Some(match ty.name() {
            "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" => KeyKind::I64,
            "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED" | "BIGINT UNSIGNED" => {
                KeyKind::U64
            }
            "CHAR" | "VARCHAR" | "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" => KeyKind::Text,
            // How SQLx stores a UUID
            "BINARY" | "VARBINARY" => KeyKind::Uuid,
            "NULL" => KeyKind::Dynamic,
            _ => return None,
        })
    }

    fn read_key(row: &MySqlRow, ordinal: usize, kind: KeyKind) -> Result<Option<Key>, sqlx::Error> {
        if row.try_get_raw(ordinal)?.is_null() {
            return Ok(None);
        }
        Ok(Some(match kind {
            KeyKind::U64 => {
                let key = row.try_get_unchecked::<u64, _>(ordinal)?;
                Key::Int(
                    i64::try_from(key)
                        .map_err(|e| sqlx::Error::ColumnDecode { index: ordinal.to_string(), source: Box::new(e) })?,
                )
            }
            KeyKind::Uuid => Key::Uuid(row.try_get_unchecked::<Uuid, _>(ordinal)?),
            KeyKind::Text => Key::Text(row.try_get_unchecked::<String, _>(ordinal)?),
            _ => Key::Int(row.try_get_unchecked::<i64, _>(ordinal)?),
        }))
    }

    fn encoded(row: &MySqlRow, ordinal: usize) -> Result<Option<Vec<u8>>, sqlx::Error> {
        if row.try_get_raw(ordinal)?.is_null() {
            return Ok(None);
        }
        // The bytes of the value as the server sent it, whatever its type
        Ok(Some(row.try_get_unchecked::<&[u8], _>(ordinal)?.to_vec()))
    }

    fn type_catalog() -> &'static [MySqlTypeInfo] {
        static TYPES: std::sync::OnceLock<Vec<MySqlTypeInfo>> = std::sync::OnceLock::new();
        TYPES.get_or_init(|| {
            BUILT_IN_TYPES
                .iter()
                .filter_map(|(ty, flags, collation, max_size)| {
                    let info = serde_json::json!({
                        "type": ty,
                        "flags": flags,
                        "collation": collation,
                        "max_size": max_size,
                    });
                    serde_json::from_value::<MySqlTypeInfo>(info).ok()
                })
                .collect()
        })
    }

    fn type_name(ty: &MySqlTypeInfo) -> String {
        ty.name().to_string()
    }
}

fn inspected(statement: &MySqlStatement) -> Inspected {
    let columns = statement
        .columns()
        .iter()
        .map(|column| {
            let ty = column.type_info();
            InspectedColumn {
                name: column.name().to_string(),
                type_name: (!ty.is_null()).then(|| MySql::type_name(ty)),
                key_kind: MySql::key_kind(ty),
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

const BINARY: u16 = 63;
const TEXT: u16 = 255; // utf8mb4_0900_ai_ci

/// SQLx's MySQL column types as the server describes them: the type code, the flags, the
/// collation and the display size, resolved with the `offline` feature of `sqlx-mysql`.
const BUILT_IN_TYPES: &[(&str, &str, u16, Option<u32>)] = &[
    ("Tiny", "", BINARY, Some(1)),
    ("Tiny", "", BINARY, Some(4)),
    ("Short", "", BINARY, None),
    ("Int24", "", BINARY, None),
    ("Long", "", BINARY, None),
    ("LongLong", "", BINARY, None),
    ("Tiny", "UNSIGNED", BINARY, Some(3)),
    ("Short", "UNSIGNED", BINARY, None),
    ("Int24", "UNSIGNED", BINARY, None),
    ("Long", "UNSIGNED", BINARY, None),
    ("LongLong", "UNSIGNED", BINARY, None),
    ("Float", "", BINARY, None),
    ("Double", "", BINARY, None),
    ("NewDecimal", "", BINARY, None),
    ("Date", "", BINARY, None),
    ("Time", "", BINARY, None),
    ("Datetime", "", BINARY, None),
    ("Timestamp", "", BINARY, None),
    ("Year", "UNSIGNED", BINARY, None),
    ("Bit", "UNSIGNED", BINARY, None),
    ("Json", "BLOB | BINARY", BINARY, None),
    ("VarString", "", TEXT, None),
    ("String", "", TEXT, None),
    ("Blob", "BLOB", TEXT, None),
    ("String", "ENUM", TEXT, None),
    ("VarString", "BINARY", BINARY, None),
    ("String", "BINARY", BINARY, None),
    ("Blob", "BLOB | BINARY", BINARY, None),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_type_resolves() {
        let names: Vec<&str> = MySql::type_catalog().iter().map(|ty| ty.name()).collect();
        assert_eq!(names.len(), BUILT_IN_TYPES.len(), "{names:?}");
        for name in ["BOOLEAN", "INT", "BIGINT UNSIGNED", "VARCHAR", "TEXT", "ENUM", "VARBINARY", "DECIMAL"] {
            assert!(names.contains(&name), "{name} in {names:?}");
        }
    }
}
