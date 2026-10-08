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

impl Backend for Postgres {
    const DIALECT: Dialect = Dialect::Postgres;

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
