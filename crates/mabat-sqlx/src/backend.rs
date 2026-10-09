//! The databases Mabat runs on.
//!
//! [`Backend`] is implemented for the SQLx databases of the enabled features: `postgres`,
//! `mysql` and `sqlite`. It wraps every SQLx call the executor and the checks make, so the
//! rest of the crate only needs `B: Backend`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use mabat_core::sql::Dialect;
use sqlx::{Database, Decode, Type};

use crate::filter::Bound;
use crate::key::{Key, KeyList};
use crate::pooled::Source;

/// A boxed future, as the async methods of [`Backend`] return.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A database Mabat runs on. Implemented for `sqlx::Postgres`, `sqlx::MySql` and
/// `sqlx::Sqlite`, with the features of the same names.
pub trait Backend: Database + Sized {
    const DIALECT: Dialect;

    /// Run a query and fetch its rows. The keys are bound first: as one array on
    /// PostgreSQL, else one parameter per key. Then the filter values, in order.
    fn fetch<'c>(
        conn: &'c mut Self::Connection,
        sql: Arc<str>,
        keys: Option<KeyList>,
        values: Vec<Bound>,
    ) -> BoxFuture<'c, Result<Vec<Self::Row>, sqlx::Error>>;

    /// Run a `SELECT count(*)` query, bound as [`Backend::fetch`] binds.
    fn fetch_count<'c>(
        conn: &'c mut Self::Connection,
        sql: Arc<str>,
        keys: Option<KeyList>,
        values: Vec<Bound>,
    ) -> BoxFuture<'c, Result<i64, sqlx::Error>>;

    /// Decode the column with the given alias.
    fn get<T>(row: &Self::Row, alias: &str) -> Result<T, sqlx::Error>
    where
        T: for<'r> Decode<'r, Self> + Type<Self>;

    /// Decode the column at a position.
    fn get_at<T>(row: &Self::Row, ordinal: usize) -> Result<T, sqlx::Error>
    where
        T: for<'r> Decode<'r, Self> + Type<Self>;

    /// The position of the column with the given alias, `None` if the row has none.
    fn find_column(row: &Self::Row, alias: &str) -> Option<usize>;

    /// The type of the column at a position.
    fn column_type(row: &Self::Row, ordinal: usize) -> &Self::TypeInfo;

    /// Whether the value at a position is NULL.
    fn is_null(row: &Self::Row, ordinal: usize) -> Result<bool, sqlx::Error>;

    /// How keys are read from a column of the type, `None` if it cannot hold a key.
    fn key_kind(ty: &Self::TypeInfo) -> Option<KeyKind>;

    /// Read the key at a position, `None` if it is NULL.
    fn read_key(row: &Self::Row, ordinal: usize, kind: KeyKind) -> Result<Option<Key>, sqlx::Error>;

    /// The value at a position, encoded so that equal values have equal encodings, for
    /// comparing rows in shadow mode. `None` if it is NULL.
    fn encoded(row: &Self::Row, ordinal: usize) -> Result<Option<Vec<u8>>, sqlx::Error>;

    /// Prepare a statement without running it, in a transaction (or a savepoint) that is
    /// rolled back. The error message of the database if it does not prepare.
    fn inspect<'c>(
        conn: &'c mut Self::Connection,
        sql: String,
    ) -> BoxFuture<'c, Result<Result<Inspected, String>, sqlx::Error>>;

    /// The built-in types, resolved as the driver resolves the columns of a statement, to
    /// find the types a Rust type can be decoded from.
    fn type_catalog() -> &'static [Self::TypeInfo];

    /// The name of a resolved column type as the checks report and compare it. A domain is
    /// named by its base type.
    fn type_name(ty: &Self::TypeInfo) -> String;

    /// The names of types that `T` accepts but that are not in the catalog, because the
    /// driver knows them only by name: custom types and extension types.
    fn named_types<T: Type<Self>>() -> Vec<String> {
        Vec::new()
    }

    /// Run a statement with arguments: the number of rows it affected.
    fn execute_args<'c>(
        conn: &'c mut Self::Connection,
        sql: String,
        args: Self::Arguments,
    ) -> BoxFuture<'c, Result<u64, sqlx::Error>>;

    /// Run an insert whose key the database generates (`insert_generated` of
    /// `mabat_core::write`), and read the key it generated.
    fn insert_generated<'c>(
        conn: &'c mut Self::Connection,
        sql: String,
        args: Self::Arguments,
    ) -> BoxFuture<'c, Result<Key, sqlx::Error>>;

    /// The SQL types of the columns of `table` (as the dialect quotes it), by column name, so
    /// that statements writing many rows can cast their values. Only PostgreSQL needs them.
    fn column_types<'c>(
        conn: &'c mut Self::Connection,
        table: String,
    ) -> BoxFuture<'c, Result<std::collections::HashMap<String, String>, sqlx::Error>> {
        let _ = (conn, table);
        Box::pin(async { Ok(std::collections::HashMap::new()) })
    }

    /// The tables and views of the database's current schema, with their columns as the
    /// catalog declares them, primary keys and foreign keys. Column types as SQLx names them
    /// are left empty, for [`crate::schema::snapshot`] to fill in.
    fn read_catalog<'c>(conn: &'c mut Self::Connection) -> BoxFuture<'c, Result<Vec<mabat_check::Table>, sqlx::Error>>;

    /// Run a query with arguments and fetch its rows.
    fn fetch_args<'c>(
        conn: &'c mut Self::Connection,
        sql: String,
        args: Self::Arguments,
    ) -> BoxFuture<'c, Result<Vec<Self::Row>, sqlx::Error>>;

    /// A copy of arguments, to run a statement again with them.
    fn clone_args(args: &Self::Arguments) -> Self::Arguments;

    /// Add a key to the arguments of a statement.
    fn add_key(args: &mut Self::Arguments, key: &Key) -> Result<(), sqlx::error::BoxDynError>;

    /// Add keys to the arguments of a statement, as [`Backend::fetch`] binds them: one array
    /// on PostgreSQL, else one argument per key.
    fn add_keys(args: &mut Self::Arguments, keys: &KeyList) -> Result<(), sqlx::error::BoxDynError>;

    /// Begin a read-only transaction on a connection of the pool that shares a snapshot:
    /// the snapshot of `import`, or a new one whose id is returned, for
    /// [`crate::Pooled::snapshot`]. Only PostgreSQL shares snapshots.
    fn begin_snapshot(
        pool: &sqlx::Pool<Self>,
        import: Option<String>,
    ) -> BoxFuture<'_, Result<(sqlx::Transaction<'static, Self>, String), sqlx::Error>> {
        let _ = (pool, import);
        Box::pin(async { Err(sqlx::Error::Configuration(format!("{} cannot share snapshots", Self::NAME).into())) })
    }
}

/// What a load runs on: a connection, a transaction or pooled connection that derefs to one,
/// or a [`crate::Pooled`] pool.
pub trait Conn: Send {
    type Backend: Backend;

    #[doc(hidden)]
    fn source(&mut self) -> Source<'_, Self::Backend>;
}

impl<DB: Backend> Conn for sqlx::Transaction<'_, DB>
where
    DB::Connection: Send,
{
    type Backend = DB;

    fn source(&mut self) -> Source<'_, DB> {
        Source::Connection(&mut **self)
    }
}

impl<DB: Backend> Conn for sqlx::pool::PoolConnection<DB>
where
    DB::Connection: Send,
{
    type Backend = DB;

    fn source(&mut self) -> Source<'_, DB> {
        Source::Connection(&mut **self)
    }
}

/// How the keys of a column are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    I16,
    I32,
    I64,
    /// An unsigned integer, read as `u64` (MySQL).
    U64,
    Uuid,
    Text,
    /// A column whose type the driver does not know, such as an SQLite expression: read as
    /// an integer if it is one, else as text.
    Dynamic,
}

/// The kind of value a key column holds. Keys of the same class compare equal across
/// column types, e.g. an `INT4` foreign key referencing an `INT8` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyClass {
    Int,
    Text,
    Uuid,
}

impl KeyKind {
    /// The class of the keys, `None` if it is only known per value.
    pub fn class(self) -> Option<KeyClass> {
        match self {
            KeyKind::I16 | KeyKind::I32 | KeyKind::I64 | KeyKind::U64 => Some(KeyClass::Int),
            KeyKind::Uuid => Some(KeyClass::Uuid),
            KeyKind::Text => Some(KeyClass::Text),
            KeyKind::Dynamic => None,
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

/// A prepared statement, as the checks see it.
#[derive(Debug, Clone)]
pub struct Inspected {
    pub columns: Vec<InspectedColumn>,
    pub params: InspectedParams,
}

#[derive(Debug, Clone)]
pub struct InspectedColumn {
    pub name: String,
    /// The type name, `None` if the driver does not know it (SQLite expressions).
    pub type_name: Option<String>,
    /// How keys would be read from the column, `None` if it cannot hold a key. Columns of
    /// unknown type are [`KeyKind::Dynamic`].
    pub key_kind: Option<KeyKind>,
}

#[derive(Debug, Clone)]
pub enum InspectedParams {
    /// The parameter types: for each, the class of the keys if it is an array of keys, and
    /// the type name. PostgreSQL.
    Types(Vec<(Option<KeyClass>, String)>),
    /// The number of parameters. MySQL and SQLite.
    Count(usize),
}

/// Implements the parts of [`Backend`] that are the same code for every driver, on its
/// concrete types.
#[allow(unused_macros)] // when no database is enabled
macro_rules! common_methods {
    ($db:ty, $conn:ty, $row:ty) => {
        fn get<T>(row: &$row, alias: &str) -> Result<T, sqlx::Error>
        where
            T: for<'r> sqlx::Decode<'r, $db> + sqlx::Type<$db>,
        {
            sqlx::Row::try_get::<T, _>(row, alias)
        }

        fn get_at<T>(row: &$row, ordinal: usize) -> Result<T, sqlx::Error>
        where
            T: for<'r> sqlx::Decode<'r, $db> + sqlx::Type<$db>,
        {
            sqlx::Row::try_get::<T, _>(row, ordinal)
        }

        fn find_column(row: &$row, alias: &str) -> Option<usize> {
            sqlx::Row::try_column(row, alias).ok().map(sqlx::Column::ordinal)
        }

        fn column_type(row: &$row, ordinal: usize) -> &<$db as sqlx::Database>::TypeInfo {
            sqlx::Column::type_info(sqlx::Row::column(row, ordinal))
        }

        fn is_null(row: &$row, ordinal: usize) -> Result<bool, sqlx::Error> {
            Ok(sqlx::ValueRef::is_null(&sqlx::Row::try_get_raw(row, ordinal)?))
        }

        fn fetch<'c>(
            conn: &'c mut $conn,
            sql: std::sync::Arc<str>,
            keys: Option<crate::key::KeyList>,
            values: Vec<crate::filter::Bound>,
        ) -> crate::backend::BoxFuture<'c, Result<Vec<$row>, sqlx::Error>> {
            Box::pin(async move { bind(sqlx::query(sqlx::AssertSqlSafe(sql)), keys, values).fetch_all(conn).await })
        }

        fn fetch_count<'c>(
            conn: &'c mut $conn,
            sql: std::sync::Arc<str>,
            keys: Option<crate::key::KeyList>,
            values: Vec<crate::filter::Bound>,
        ) -> crate::backend::BoxFuture<'c, Result<i64, sqlx::Error>> {
            Box::pin(async move {
                let row = bind(sqlx::query(sqlx::AssertSqlSafe(sql)), keys, values).fetch_one(conn).await?;
                sqlx::Row::try_get::<i64, _>(&row, 0)
            })
        }

        fn execute_args<'c>(
            conn: &'c mut $conn,
            sql: String,
            args: <$db as sqlx::Database>::Arguments,
        ) -> crate::backend::BoxFuture<'c, Result<u64, sqlx::Error>> {
            Box::pin(async move {
                let done = sqlx::query_with(sqlx::AssertSqlSafe(sql), args).execute(conn).await?;
                Ok(done.rows_affected())
            })
        }

        fn fetch_args<'c>(
            conn: &'c mut $conn,
            sql: String,
            args: <$db as sqlx::Database>::Arguments,
        ) -> crate::backend::BoxFuture<'c, Result<Vec<$row>, sqlx::Error>> {
            Box::pin(async move { sqlx::query_with(sqlx::AssertSqlSafe(sql), args).fetch_all(conn).await })
        }

        fn clone_args(args: &<$db as sqlx::Database>::Arguments) -> <$db as sqlx::Database>::Arguments {
            args.clone()
        }

        fn add_key(
            args: &mut <$db as sqlx::Database>::Arguments,
            key: &crate::key::Key,
        ) -> Result<(), sqlx::error::BoxDynError> {
            use sqlx::Arguments;
            match key {
                crate::key::Key::Int(key) => args.add(*key),
                crate::key::Key::Text(key) => args.add(key.as_str()),
                crate::key::Key::Uuid(key) => args.add(*key),
            }
        }

        fn inspect<'c>(
            conn: &'c mut $conn,
            sql: String,
        ) -> crate::backend::BoxFuture<'c, Result<Result<crate::backend::Inspected, String>, sqlx::Error>> {
            Box::pin(async move {
                use sqlx::{Connection, Executor, SqlSafeStr};
                let mut tx = conn.begin().await?;
                let result = (&mut *tx).prepare(sqlx::AssertSqlSafe(sql).into_sql_str()).await;
                tx.rollback().await?;
                match result {
                    Ok(statement) => Ok(Ok(inspected(&statement))),
                    Err(sqlx::Error::Database(e)) => Ok(Err(e.message().to_string())),
                    Err(e) => Err(e),
                }
            })
        }
    };
}

/// Implements [`Conn`] for the connection type of a driver.
#[allow(unused_macros)] // when no database is enabled
macro_rules! connection {
    ($db:ty, $conn:ty) => {
        impl crate::backend::Conn for $conn {
            type Backend = $db;

            fn source(&mut self) -> crate::pooled::Source<'_, $db> {
                crate::pooled::Source::Connection(self)
            }
        }
    };
}

/// Binds keys and values to a query of a driver that binds each value of a list (MySQL and
/// SQLite).
#[allow(unused_macros)]
macro_rules! bind_each {
    ($db:ty) => {
        /// Keys as one argument per key.
        fn add_each_key(
            args: &mut <$db as sqlx::Database>::Arguments,
            keys: &crate::key::KeyList,
        ) -> Result<(), sqlx::error::BoxDynError> {
            use sqlx::Arguments;
            match keys {
                crate::key::KeyList::Int(keys) => keys.iter().try_for_each(|key| args.add(*key)),
                crate::key::KeyList::Text(keys) => keys.iter().try_for_each(|key| args.add(key.as_str())),
                crate::key::KeyList::Uuid(keys) => keys.iter().try_for_each(|key| args.add(*key)),
            }
        }

        fn bind<'q>(
            mut query: sqlx::query::Query<'q, $db, <$db as sqlx::Database>::Arguments>,
            keys: Option<crate::key::KeyList>,
            values: Vec<crate::filter::Bound>,
        ) -> sqlx::query::Query<'q, $db, <$db as sqlx::Database>::Arguments> {
            use crate::filter::{Bound, Value};
            use crate::key::KeyList;
            match keys {
                Some(KeyList::Int(keys)) => {
                    for key in keys {
                        query = query.bind(key);
                    }
                }
                Some(KeyList::Text(keys)) => {
                    for key in keys {
                        query = query.bind(key);
                    }
                }
                Some(KeyList::Uuid(keys)) => {
                    for key in keys {
                        query = query.bind(key);
                    }
                }
                None => {}
            }
            fn one<'q>(
                query: sqlx::query::Query<'q, $db, <$db as sqlx::Database>::Arguments>,
                value: Value,
            ) -> sqlx::query::Query<'q, $db, <$db as sqlx::Database>::Arguments> {
                match value {
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
                }
            }
            for value in values {
                match value {
                    Bound::One(value) => query = one(query, value),
                    Bound::List(values) => {
                        for value in values.into_values() {
                            query = one(query, value);
                        }
                    }
                }
            }
            query
        }
    };
}

#[allow(unused_imports)]
pub(crate) use {bind_each, common_methods, connection};

/// The rows a catalog reader found: tables and whether each is a view, their columns, the
/// columns of their primary keys in order, and the columns of their foreign keys in order, by
/// constraint name.
#[derive(Default)]
pub(crate) struct Catalog {
    pub(crate) tables: Vec<(String, bool)>,
    pub(crate) columns: Vec<(String, mabat_check::Column)>,
    pub(crate) primary_keys: Vec<(String, String)>,
    /// (table, constraint, column, referenced table, referenced column)
    pub(crate) foreign_keys: Vec<(String, String, String, String, String)>,
}

impl Catalog {
    pub(crate) fn tables(self) -> Vec<mabat_check::Table> {
        let mut tables: Vec<mabat_check::Table> = self
            .tables
            .into_iter()
            .map(|(name, view)| mabat_check::Table {
                name,
                view,
                columns: Vec::new(),
                primary_key: Vec::new(),
                foreign_keys: Vec::new(),
            })
            .collect();
        fn table<'t>(tables: &'t mut [mabat_check::Table], name: &str) -> Option<&'t mut mabat_check::Table> {
            tables.iter_mut().find(|t| t.name == name)
        }
        for (name, column) in self.columns {
            if let Some(t) = table(&mut tables, &name) {
                t.columns.push(column);
            }
        }
        for (name, column) in self.primary_keys {
            if let Some(t) = table(&mut tables, &name) {
                t.primary_key.push(column);
            }
        }
        let mut constraints: Vec<(String, String, mabat_check::ForeignKey)> = Vec::new();
        for (name, constraint, column, referenced, reference) in self.foreign_keys {
            match constraints.iter_mut().find(|(t, c, _)| *t == name && *c == constraint) {
                Some((_, _, fk)) => {
                    fk.columns.push(column);
                    fk.references.push(reference);
                }
                None => constraints.push((
                    name,
                    constraint,
                    mabat_check::ForeignKey { columns: vec![column], table: referenced, references: vec![reference] },
                )),
            }
        }
        for (name, _, fk) in constraints {
            if let Some(t) = table(&mut tables, &name) {
                t.foreign_keys.push(fk);
            }
        }
        tables
    }
}

/// The key in the first column of a row an insert returned.
#[cfg(any(feature = "postgres", feature = "sqlite"))]
pub(crate) fn returned_key<B: Backend>(row: &B::Row) -> Result<Key, sqlx::Error> {
    let ty = B::column_type(row, 0);
    let kind = B::key_kind(ty).ok_or_else(|| sqlx::Error::ColumnDecode {
        index: "0".to_string(),
        source: format!("the database generated a key of type {}, expected an integer", B::type_name(ty)).into(),
    })?;
    B::read_key(row, 0, kind)?.ok_or_else(|| sqlx::Error::ColumnDecode {
        index: "0".to_string(),
        source: "the database generated a NULL key".into(),
    })
}

#[cfg(feature = "mysql")]
mod mysql;
#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "sqlite")]
mod sqlite;
