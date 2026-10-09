//! Snapshots of a database's schema, for checking views without a database: see
//! [`mabat_check::Snapshot`].

pub use mabat_check::{Column, ForeignKey, Snapshot, Table};

use crate::Error;
use crate::backend::{Backend, Conn};

/// A snapshot of the tables and views of the database's current schema: their columns, with
/// their types as the database declares them and as SQLx names them, nullability and generated
/// values, and their primary and foreign keys.
pub async fn snapshot<C: Conn>(conn: &mut C) -> Result<Snapshot, Error> {
    let mut conn = conn.source().single().await?;
    let mut tables = C::Backend::read_catalog(&mut conn).await.map_err(Error::Check)?;
    for table in &mut tables {
        // The types SQLx reads the columns as, which the manifest's accepted types name
        let sql = format!("SELECT * FROM {}", C::Backend::DIALECT.quote(&table.name));
        let inspected = C::Backend::inspect(&mut conn, sql).await.map_err(Error::Check)?;
        for column in &mut table.columns {
            let found = inspected.as_ref().ok().and_then(|i| i.columns.iter().find(|c| c.name == column.name));
            column.r#type = found.and_then(|c| c.type_name.clone()).unwrap_or_else(|| column.declared.to_uppercase());
        }
    }
    Ok(Snapshot::new(<C::Backend as sqlx::Database>::NAME, tables))
}
