//! PostgreSQL.

use mabat_core::sql::Dialect;
use sqlx::postgres::{PgArguments, PgConnection, PgRow, PgStatement, PgTypeInfo, PgTypeKind};
use sqlx::query::Query;
use sqlx::{Column, Either, Postgres, Row, Statement, TypeInfo, ValueRef};
use uuid::Uuid;

use super::{Backend, Inspected, InspectedColumn, InspectedParams, KeyKind};
use crate::filter::{Bound, Value, Values};
use crate::key::{Key, KeyList};

connection!(Postgres, PgConnection);

/// The name of the cursor a stream reads its keys from: one per connection, as a stream holds
/// its connection.
const CURSOR: &str = "mabat_stream_keys";

impl Backend for Postgres {
    const DIALECT: Dialect = Dialect::Postgres;

    fn add_keys(args: &mut PgArguments, keys: &KeyList) -> Result<(), sqlx::error::BoxDynError> {
        use sqlx::Arguments;
        match keys {
            KeyList::Int(keys) => args.add(keys),
            KeyList::Text(keys) => args.add(keys),
            KeyList::Uuid(keys) => args.add(keys),
        }
    }

    fn read_catalog<'c>(
        conn: &'c mut PgConnection,
    ) -> super::BoxFuture<'c, Result<Vec<mabat_check::Table>, sqlx::Error>> {
        Box::pin(async move {
            let mut catalog = super::Catalog::default();
            const RELATIONS: &str = "n.nspname = current_schema() AND c.relkind IN ('r', 'p', 'v', 'm')";
            let tables = sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT c.relname::text, c.relkind IN ('v', 'm') FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace WHERE {RELATIONS} ORDER BY 1"
            )))
            .fetch_all(&mut *conn)
            .await?;
            for row in &tables {
                catalog.tables.push((row.try_get(0)?, row.try_get(1)?));
            }
            let columns = sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT c.relname::text, a.attname::text, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
                 a.attidentity IN ('a', 'd') OR coalesce(pg_get_expr(d.adbin, d.adrelid) LIKE 'nextval(%', false) \
                 FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                 WHERE {RELATIONS} AND a.attnum > 0 AND NOT a.attisdropped ORDER BY 1, a.attnum"
            )))
            .fetch_all(&mut *conn)
            .await?;
            for row in &columns {
                let column = mabat_check::Column {
                    name: row.try_get(1)?,
                    r#type: String::new(),
                    declared: row.try_get(2)?,
                    nullable: row.try_get(3)?,
                    generated: row.try_get(4)?,
                };
                catalog.columns.push((row.try_get(0)?, column));
            }
            let keys = sqlx::query(
                "SELECT c.relname::text, a.attname::text FROM pg_index i JOIN pg_class c ON c.oid = i.indrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) \
                 WHERE i.indisprimary AND n.nspname = current_schema() \
                 ORDER BY 1, array_position(i.indkey::int2[], a.attnum)",
            )
            .fetch_all(&mut *conn)
            .await?;
            for row in &keys {
                catalog.primary_keys.push((row.try_get(0)?, row.try_get(1)?));
            }
            let foreign = sqlx::query(
                "SELECT c.relname::text, con.conname::text, a.attname::text, r.relname::text, ra.attname::text \
                 FROM pg_constraint con JOIN pg_class c ON c.oid = con.conrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace JOIN pg_class r ON r.oid = con.confrelid \
                 CROSS JOIN LATERAL unnest(con.conkey, con.confkey) WITH ORDINALITY AS k(col, ref, ord) \
                 JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.col \
                 JOIN pg_attribute ra ON ra.attrelid = con.confrelid AND ra.attnum = k.ref \
                 WHERE con.contype = 'f' AND n.nspname = current_schema() ORDER BY 1, 2, k.ord",
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

    fn column_types<'c>(
        conn: &'c mut PgConnection,
        table: String,
    ) -> super::BoxFuture<'c, Result<std::collections::HashMap<String, String>, sqlx::Error>> {
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod) FROM pg_attribute a \
                 WHERE a.attrelid = to_regclass($1) AND a.attnum > 0 AND NOT a.attisdropped",
            )
            .bind(table)
            .fetch_all(conn)
            .await?;
            rows.iter().map(|row| Ok((row.try_get::<String, _>(0)?, row.try_get::<String, _>(1)?))).collect()
        })
    }

    fn insert_generated<'c>(
        conn: &'c mut PgConnection,
        sql: String,
        args: <Postgres as sqlx::Database>::Arguments,
    ) -> super::BoxFuture<'c, Result<Key, sqlx::Error>> {
        Box::pin(async move {
            let row = sqlx::query_with(sqlx::AssertSqlSafe(sql), args).fetch_one(conn).await?;
            super::returned_key::<Self>(&row)
        })
    }

    common_methods!(Postgres, PgConnection, PgRow);

    fn key_kind(ty: &PgTypeInfo) -> Option<KeyKind> {
        Some(match base(ty).name() {
            "INT2" => KeyKind::I16,
            "INT4" => KeyKind::I32,
            "INT8" => KeyKind::I64,
            "UUID" => KeyKind::Uuid,
            "TEXT" | "VARCHAR" | "CHAR" | "NAME" => KeyKind::Text,
            _ => return None,
        })
    }

    fn read_key(row: &PgRow, ordinal: usize, kind: KeyKind) -> Result<Option<Key>, sqlx::Error> {
        Ok(match kind {
            KeyKind::I16 => row.try_get::<Option<i16>, _>(ordinal)?.map(|v| Key::Int(v.into())),
            KeyKind::I32 => row.try_get::<Option<i32>, _>(ordinal)?.map(|v| Key::Int(v.into())),
            KeyKind::I64 | KeyKind::U64 | KeyKind::Dynamic => row.try_get::<Option<i64>, _>(ordinal)?.map(Key::Int),
            KeyKind::Uuid => row.try_get::<Option<Uuid>, _>(ordinal)?.map(Key::Uuid),
            KeyKind::Text => row.try_get::<Option<String>, _>(ordinal)?.map(Key::Text),
        })
    }

    fn encoded(row: &PgRow, ordinal: usize) -> Result<Option<Vec<u8>>, sqlx::Error> {
        let raw = row.try_get_raw(ordinal)?;
        if raw.is_null() {
            return Ok(None);
        }
        Ok(Some(raw.as_bytes().map_err(sqlx::Error::Decode)?.to_vec()))
    }

    fn type_catalog() -> &'static [PgTypeInfo] {
        static TYPES: std::sync::OnceLock<Vec<PgTypeInfo>> = std::sync::OnceLock::new();
        TYPES.get_or_init(|| {
            BUILT_IN_TYPES
                .iter()
                .filter_map(|variant| serde_json::from_value::<PgTypeInfo>(serde_json::Value::from(*variant)).ok())
                .collect()
        })
    }

    fn type_name(ty: &PgTypeInfo) -> String {
        base(ty).name().to_string()
    }

    fn begin_snapshot(
        pool: &sqlx::Pool<Postgres>,
        import: Option<String>,
    ) -> super::BoxFuture<'_, Result<(sqlx::Transaction<'static, Postgres>, String), sqlx::Error>> {
        Box::pin(async move {
            let mut tx = pool.begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY").await?;
            let id = match import {
                Some(id) => {
                    // An id as pg_export_snapshot returns it, such as 00000003-0000001B-1
                    if !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
                        return Err(sqlx::Error::Protocol(format!("unexpected snapshot id {id:?}")));
                    }
                    let set = format!("SET TRANSACTION SNAPSHOT '{id}'");
                    sqlx::query(sqlx::AssertSqlSafe(set)).execute(&mut *tx).await?;
                    id
                }
                None => sqlx::query_scalar::<_, String>("SELECT pg_export_snapshot()").fetch_one(&mut *tx).await?,
            };
            Ok((tx, id))
        })
    }

    fn open_cursor<'c>(
        conn: &'c mut PgConnection,
        sql: std::sync::Arc<str>,
        keys: Option<KeyList>,
        values: Vec<Bound>,
    ) -> super::BoxFuture<'c, Result<bool, sqlx::Error>> {
        Box::pin(async move {
            // `WITH HOLD`, so that it outlives the transaction it is declared in, or the
            // statement outside one: the keys are then kept by the server until it is closed
            let left: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_cursors WHERE name = $1)")
                .bind(CURSOR)
                .fetch_one(&mut *conn)
                .await?;
            if left {
                sqlx::query(sqlx::AssertSqlSafe(format!("CLOSE {CURSOR}"))).execute(&mut *conn).await?;
            }
            let declare = format!("DECLARE {CURSOR} NO SCROLL CURSOR WITH HOLD FOR {sql}");
            bind(sqlx::query(sqlx::AssertSqlSafe(declare)), keys, values).execute(&mut *conn).await?;
            Ok(true)
        })
    }

    fn fetch_cursor<'c>(conn: &'c mut PgConnection, n: usize) -> super::BoxFuture<'c, Result<Vec<PgRow>, sqlx::Error>> {
        Box::pin(async move {
            sqlx::query(sqlx::AssertSqlSafe(format!("FETCH FORWARD {n} FROM {CURSOR}"))).fetch_all(conn).await
        })
    }

    fn close_cursor<'c>(conn: &'c mut PgConnection) -> super::BoxFuture<'c, Result<(), sqlx::Error>> {
        Box::pin(async move {
            sqlx::query(sqlx::AssertSqlSafe(format!("CLOSE {CURSOR}"))).execute(conn).await?;
            Ok(())
        })
    }

    fn named_types<T: sqlx::Type<Postgres>>() -> Vec<String> {
        let mut names = Vec::new();
        // Text types also accept the citext extension. Only a resolved type may be passed to a
        // compatibility check that could be an array's, which panics on a name-only type.
        let text = Postgres::type_catalog().iter().find(|ty| ty.name() == "TEXT");
        if text.is_some_and(|text| T::compatible(text)) && T::compatible(&PgTypeInfo::with_name("citext")) {
            names.push("citext".to_string());
        }
        // A custom type, such as an enum declared with `#[sqlx(type_name = "...")]`
        let own = T::type_info();
        if own.oid().is_none() {
            names.push(own.name().to_string());
        }
        names
    }
}

/// The base type of a domain, or the type itself.
fn base(ty: &PgTypeInfo) -> &PgTypeInfo {
    match ty.kind() {
        PgTypeKind::Domain(base) => base,
        _ => ty,
    }
}

/// Keys and lists are bound as arrays.
fn bind(
    mut query: Query<'_, Postgres, PgArguments>,
    keys: Option<KeyList>,
    values: Vec<Bound>,
) -> Query<'_, Postgres, PgArguments> {
    query = match keys {
        Some(KeyList::Int(keys)) => query.bind(keys),
        Some(KeyList::Text(keys)) => query.bind(keys),
        Some(KeyList::Uuid(keys)) => query.bind(keys),
        None => query,
    };
    for value in values {
        query = match value {
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
        };
    }
    query
}

fn inspected(statement: &PgStatement) -> Inspected {
    let columns = statement
        .columns()
        .iter()
        .map(|column| InspectedColumn {
            name: column.name().to_string(),
            type_name: Some(Postgres::type_name(column.type_info())),
            key_kind: Postgres::key_kind(column.type_info()),
        })
        .collect();
    let params = match statement.parameters() {
        Some(Either::Left(types)) => InspectedParams::Types(
            types
                .iter()
                .map(|ty| {
                    let class = match ty.kind() {
                        PgTypeKind::Array(element) => Postgres::key_kind(element).and_then(KeyKind::class),
                        _ => None,
                    };
                    (class, ty.name().to_string())
                })
                .collect(),
        ),
        Some(Either::Right(count)) => InspectedParams::Count(count),
        None => InspectedParams::Count(0),
    };
    Inspected { columns, params }
}

/// SQLx's built-in PostgreSQL types (the variants of its `PgType`), resolved by name with
/// the `offline` feature of `sqlx-postgres`. A type declared only by name or OID cannot be
/// used to find compatible types: SQLx compares a declared type as equal to any type of the
/// other kind of declaration, and its array checks need a resolved type.
const BUILT_IN_TYPES: &[&str] = &[
    "Bool",
    "Bytea",
    "Char",
    "Name",
    "Int8",
    "Int2",
    "Int4",
    "Text",
    "Oid",
    "Json",
    "JsonArray",
    "Point",
    "Lseg",
    "Path",
    "Box",
    "Polygon",
    "Line",
    "LineArray",
    "Cidr",
    "CidrArray",
    "Float4",
    "Float8",
    "Unknown",
    "Circle",
    "CircleArray",
    "Macaddr8",
    "Macaddr8Array",
    "Macaddr",
    "Inet",
    "BoolArray",
    "ByteaArray",
    "CharArray",
    "NameArray",
    "Int2Array",
    "Int4Array",
    "TextArray",
    "BpcharArray",
    "VarcharArray",
    "Int8Array",
    "PointArray",
    "LsegArray",
    "PathArray",
    "BoxArray",
    "Float4Array",
    "Float8Array",
    "PolygonArray",
    "OidArray",
    "MacaddrArray",
    "InetArray",
    "Bpchar",
    "Varchar",
    "Date",
    "Time",
    "Timestamp",
    "TimestampArray",
    "DateArray",
    "TimeArray",
    "Timestamptz",
    "TimestamptzArray",
    "Interval",
    "IntervalArray",
    "NumericArray",
    "Timetz",
    "TimetzArray",
    "Bit",
    "BitArray",
    "Varbit",
    "VarbitArray",
    "Numeric",
    "Record",
    "RecordArray",
    "Uuid",
    "UuidArray",
    "Jsonb",
    "JsonbArray",
    "Int4Range",
    "Int4RangeArray",
    "NumRange",
    "NumRangeArray",
    "TsRange",
    "TsRangeArray",
    "TstzRange",
    "TstzRangeArray",
    "DateRange",
    "DateRangeArray",
    "Int8Range",
    "Int8RangeArray",
    "Jsonpath",
    "JsonpathArray",
    "Money",
    "MoneyArray",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_type_resolves() {
        assert_eq!(Postgres::type_catalog().len(), BUILT_IN_TYPES.len());
    }
}
