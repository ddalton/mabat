//! The statements that write aggregates: upserting a row, inserting link rows, and finding
//! and deleting rows by keys.

use std::fmt::Write;

use crate::sql::Dialect;

/// The value of a column in a statement that writes a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnValue {
    /// A bound parameter.
    Bound,
    /// `NULL`.
    Null,
    /// A string literal, such as the tag of an enum, which any column type of the tag
    /// accepts. Quotes are escaped.
    Literal(String),
}

/// The values of the columns, with placeholders numbered from 1 on PostgreSQL.
fn values(dialect: Dialect, columns: &[(String, ColumnValue)]) -> String {
    let mut n = 0;
    let values: Vec<String> = columns
        .iter()
        .map(|(_, value)| match value {
            ColumnValue::Bound => {
                n += 1;
                dialect.placeholder(n)
            }
            ColumnValue::Null => "NULL".to_string(),
            ColumnValue::Literal(text) => format!("'{}'", text.replace('\'', "''")),
        })
        .collect();
    values.join(", ")
}

/// Insert a row, or update the row with the same key: `ON CONFLICT … DO UPDATE` on
/// PostgreSQL and SQLite, `ON DUPLICATE KEY UPDATE` on MySQL. `columns` include the key.
pub fn upsert(dialect: Dialect, table: &str, key: &str, columns: &[(String, ColumnValue)]) -> String {
    let q = |ident: &str| dialect.quote(ident);
    let names: Vec<String> = columns.iter().map(|(name, _)| q(name)).collect();
    let mut sql = format!("INSERT INTO {} ({}) VALUES ({})", q(table), names.join(", "), values(dialect, columns));
    let others: Vec<&String> = columns.iter().map(|(name, _)| name).filter(|name| *name != key).collect();
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => {
            let excluded = if dialect == Dialect::Postgres { "EXCLUDED" } else { "excluded" };
            if others.is_empty() {
                let _ = write!(sql, " ON CONFLICT ({}) DO NOTHING", q(key));
            } else {
                let set: Vec<String> = others.iter().map(|name| format!("{0} = {excluded}.{0}", q(name))).collect();
                let _ = write!(sql, " ON CONFLICT ({}) DO UPDATE SET {}", q(key), set.join(", "));
            }
        }
        Dialect::MySql => {
            let set: Vec<String> = if others.is_empty() {
                vec![format!("{0} = {0}", q(key))]
            } else {
                others.iter().map(|name| format!("{0} = {1}.{0}", q(name), q("new"))).collect()
            };
            let _ = write!(sql, " AS {} ON DUPLICATE KEY UPDATE {}", q("new"), set.join(", "));
        }
    }
    sql
}

/// Insert a row.
pub fn insert(dialect: Dialect, table: &str, columns: &[(String, ColumnValue)]) -> String {
    let names: Vec<String> = columns.iter().map(|(name, _)| dialect.quote(name)).collect();
    format!("INSERT INTO {} ({}) VALUES ({})", dialect.quote(table), names.join(", "), values(dialect, columns))
}

/// The `select` column of the rows whose `column` is one of `keys` keys.
pub fn select_by(dialect: Dialect, table: &str, select: &str, column: &str, keys: usize) -> String {
    let q = |ident: &str| dialect.quote(ident);
    format!("SELECT {} FROM {} WHERE {}", q(select), q(table), dialect.keys_condition(&q(column), keys))
}

/// Delete the rows whose `column` is one of `keys` keys.
pub fn delete_by(dialect: Dialect, table: &str, column: &str, keys: usize) -> String {
    let q = |ident: &str| dialect.quote(ident);
    format!("DELETE FROM {} WHERE {}", q(table), dialect.keys_condition(&q(column), keys))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns() -> Vec<(String, ColumnValue)> {
        vec![
            ("id".into(), ColumnValue::Bound),
            ("name".into(), ColumnValue::Bound),
            ("state".into(), ColumnValue::Literal("it's".into())),
            ("reason".into(), ColumnValue::Null),
        ]
    }

    #[test]
    fn upserts_per_dialect() {
        assert_eq!(
            upsert(Dialect::Postgres, "task", "id", &columns()),
            "INSERT INTO \"task\" (\"id\", \"name\", \"state\", \"reason\") VALUES ($1, $2, 'it''s', NULL) \
             ON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\", \"state\" = EXCLUDED.\"state\", \
             \"reason\" = EXCLUDED.\"reason\""
        );
        assert_eq!(
            upsert(Dialect::MySql, "task", "id", &columns()),
            "INSERT INTO `task` (`id`, `name`, `state`, `reason`) VALUES (?, ?, 'it''s', NULL) AS `new` \
             ON DUPLICATE KEY UPDATE `name` = `new`.`name`, `state` = `new`.`state`, `reason` = `new`.`reason`"
        );
        let sqlite = upsert(Dialect::Sqlite, "task", "id", &columns());
        assert!(sqlite.ends_with("ON CONFLICT (\"id\") DO UPDATE SET \"name\" = excluded.\"name\", \"state\" = excluded.\"state\", \"reason\" = excluded.\"reason\""), "{sqlite}");

        // Only the key: nothing to update
        let key = vec![("id".to_string(), ColumnValue::Bound)];
        assert_eq!(
            upsert(Dialect::Postgres, "tag", "id", &key),
            "INSERT INTO \"tag\" (\"id\") VALUES ($1) ON CONFLICT (\"id\") DO NOTHING"
        );
        assert!(upsert(Dialect::MySql, "tag", "id", &key).ends_with("ON DUPLICATE KEY UPDATE `id` = `id`"));
    }

    #[test]
    fn keys_and_links() {
        assert_eq!(
            select_by(Dialect::Postgres, "task", "id", "parent_id", 1),
            "SELECT \"id\" FROM \"task\" WHERE \"parent_id\" = ANY($1)"
        );
        assert_eq!(delete_by(Dialect::Sqlite, "task", "id", 3), "DELETE FROM \"task\" WHERE \"id\" IN (?, ?, ?)");
        let link = vec![("film_id".to_string(), ColumnValue::Bound), ("actor_id".to_string(), ColumnValue::Bound)];
        assert_eq!(
            insert(Dialect::MySql, "film_actor", &link),
            "INSERT INTO `film_actor` (`film_id`, `actor_id`) VALUES (?, ?)"
        );
    }
}
