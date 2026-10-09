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
    value_list(dialect, columns).join(", ")
}

/// A value of a row of a multi-row statement: a placeholder numbered after `n` on
/// PostgreSQL, NULL or a literal; cast to `cast` when given.
fn row_value(dialect: Dialect, value: &ColumnValue, n: &mut usize, cast: Option<&str>) -> String {
    let value = match value {
        ColumnValue::Bound => {
            *n += 1;
            dialect.placeholder(*n)
        }
        ColumnValue::Null => "NULL".to_string(),
        ColumnValue::Literal(text) => format!("'{}'", text.replace('\'', "''")),
    };
    match cast {
        Some(ty) => format!("CAST({value} AS {ty})"),
        None => value,
    }
}

/// The `VALUES` tuples of rows, with placeholders numbered across the rows.
fn tuples(dialect: Dialect, rows: &[Vec<ColumnValue>], casts: Option<&[Option<String>]>) -> String {
    let mut n = 0;
    let tuples: Vec<String> = rows
        .iter()
        .map(|row| {
            let values: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(i, value)| row_value(dialect, value, &mut n, casts.and_then(|c| c[i].as_deref())))
                .collect();
            format!("({})", values.join(", "))
        })
        .collect();
    tuples.join(", ")
}

/// Insert rows, whose values follow `names`.
pub fn insert_rows(dialect: Dialect, table: &str, names: &[String], rows: &[Vec<ColumnValue>]) -> String {
    let q = |ident: &str| dialect.quote(ident);
    let quoted: Vec<String> = names.iter().map(|name| q(name)).collect();
    let tuples = tuples(dialect, rows, None);
    format!("INSERT INTO {} ({}) VALUES {tuples}", q(table), quoted.join(", "))
}

/// Insert rows, or update the rows with the same keys, as [`upsert`] does one.
pub fn upsert_rows(dialect: Dialect, table: &str, key: &str, names: &[String], rows: &[Vec<ColumnValue>]) -> String {
    let q = |ident: &str| dialect.quote(ident);
    let mut sql = insert_rows(dialect, table, names, rows);
    let others: Vec<&String> = names.iter().filter(|name| *name != key).collect();
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

/// The alias of the key of each row in [`update_rows`].
pub const KEY_VALUE: &str = "$key";
/// The alias of the version of each row in [`update_rows`].
pub const VERSION_VALUE: &str = "$version";

/// Update rows by key from a table of their values: each row has the values of `names`, then
/// its key, then with `version` its version, which the row must have and which is
/// incremented. On PostgreSQL, `casts` gives the type of each value, so that NULLs, literals
/// and parameters of other types take the types of their columns. On PostgreSQL and SQLite,
/// the statement returns the keys of the rows it updated. On PostgreSQL and SQLite,
/// the statement returns the keys of the rows it updated.
pub fn update_rows(
    dialect: Dialect,
    table: &str,
    key: &str,
    names: &[String],
    rows: &[Vec<ColumnValue>],
    version: Option<&str>,
    casts: Option<&[Option<String>]>,
) -> String {
    let q = |ident: &str| dialect.quote(ident);
    let mut aliases: Vec<String> = names.iter().map(|name| q(name)).collect();
    aliases.push(q(KEY_VALUE));
    if version.is_some() {
        aliases.push(q(VERSION_VALUE));
    }
    // The name of the i-th value in the table of values
    let value = |i: usize| match dialect {
        Dialect::Sqlite => format!("v.column{}", i + 1),
        _ => format!("v.{}", aliases[i]),
    };
    let mut set: Vec<String> =
        names.iter().enumerate().map(|(i, name)| format!("{} = {}", q(name), value(i))).collect();
    let mut on = format!("{}.{} = {}", q(table), q(key), value(names.len()));
    if let Some(version) = version {
        set.push(format!("{0} = {1}.{0} + 1", q(version), q(table)));
        let _ = write!(on, " AND {}.{} = {}", q(table), q(version), value(names.len() + 1));
    }
    match dialect {
        Dialect::Postgres => format!(
            "UPDATE {} SET {} FROM (VALUES {}) AS v ({}) WHERE {on} RETURNING {}.{}",
            q(table),
            set.join(", "),
            tuples(dialect, rows, casts),
            aliases.join(", "),
            q(table),
            q(key)
        ),
        Dialect::Sqlite => format!(
            "UPDATE {} SET {} FROM (VALUES {}) AS v WHERE {on} RETURNING {}.{}",
            q(table),
            set.join(", "),
            tuples(dialect, rows, None),
            q(table),
            q(key)
        ),
        Dialect::MySql => {
            // A derived table of SELECTs, whose column types MySQL takes from all the rows
            let mut n = 0;
            let selects: Vec<String> = rows
                .iter()
                .enumerate()
                .map(|(r, row)| {
                    let values: Vec<String> = row
                        .iter()
                        .enumerate()
                        .map(|(i, value)| {
                            let value = row_value(dialect, value, &mut n, None);
                            if r == 0 { format!("{value} AS {}", aliases[i]) } else { value }
                        })
                        .collect();
                    format!("SELECT {}", values.join(", "))
                })
                .collect();
            let set: Vec<String> = set.iter().map(|s| format!("{}.{s}", q(table))).collect();
            format!("UPDATE {} JOIN ({}) AS v ON {on} SET {}", q(table), selects.join(" UNION ALL "), set.join(", "))
        }
    }
}

/// Lock the rows of `table` whose `key` is one of `keys` keys, and select their keys; on
/// SQLite, which locks the whole database for a write, only select them.
pub fn lock_keys(dialect: Dialect, table: &str, key: &str, keys: usize) -> String {
    let q = |ident: &str| dialect.quote(ident);
    let select = format!("SELECT {} FROM {} WHERE {}", q(key), q(table), dialect.keys_condition(&q(key), keys));
    match dialect {
        Dialect::Postgres | Dialect::MySql => format!("{select} FOR UPDATE"),
        Dialect::Sqlite => select,
    }
}

fn value_list(dialect: Dialect, columns: &[(String, ColumnValue)]) -> Vec<String> {
    let mut n = 0;
    columns
        .iter()
        .map(|(_, value)| match value {
            ColumnValue::Bound => {
                n += 1;
                dialect.placeholder(n)
            }
            ColumnValue::Null => "NULL".to_string(),
            ColumnValue::Literal(text) => format!("'{}'", text.replace('\'', "''")),
        })
        .collect()
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

/// Update the columns of the row with the key, bound after the columns, and with a
/// `version` column, only if it has the version bound after the key, incrementing it.
pub fn update(
    dialect: Dialect,
    table: &str,
    key: &str,
    columns: &[(String, ColumnValue)],
    version: Option<&str>,
) -> String {
    let q = |ident: &str| dialect.quote(ident);
    let values = value_list(dialect, columns);
    let mut set: Vec<String> =
        columns.iter().zip(&values).map(|((name, _), value)| format!("{} = {value}", q(name))).collect();
    if let Some(version) = version {
        set.push(format!("{0} = {0} + 1", q(version)));
    }
    let bound = columns.iter().filter(|(_, value)| *value == ColumnValue::Bound).count();
    let mut sql =
        format!("UPDATE {} SET {} WHERE {} = {}", q(table), set.join(", "), q(key), dialect.placeholder(bound + 1));
    if let Some(version) = version {
        let _ = write!(sql, " AND {} = {}", q(version), dialect.placeholder(bound + 2));
    }
    sql
}

/// Insert a row unless a row has its key, so that the number of rows affected says which
/// happened. On MySQL, which counts an existing row as affected by `ON DUPLICATE KEY`, the
/// key is bound once more after the columns.
pub fn insert_if_absent(dialect: Dialect, table: &str, key: &str, columns: &[(String, ColumnValue)]) -> String {
    let q = |ident: &str| dialect.quote(ident);
    let names: Vec<String> = columns.iter().map(|(name, _)| q(name)).collect();
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => {
            format!("{} ON CONFLICT ({}) DO NOTHING", insert(dialect, table, columns), q(key))
        }
        Dialect::MySql => format!(
            "INSERT INTO {table} ({}) SELECT {} FROM DUAL WHERE NOT EXISTS (SELECT 1 FROM {table} WHERE {} = ?)",
            names.join(", "),
            values(dialect, columns),
            q(key),
            table = q(table),
        ),
    }
}

/// Insert a row whose key the database generates, returning the key: with `RETURNING` on
/// PostgreSQL and SQLite; MySQL reports it as the last insert id of the statement.
pub fn insert_generated(dialect: Dialect, table: &str, key: &str, columns: &[(String, ColumnValue)]) -> String {
    let insert = match dialect {
        Dialect::Postgres | Dialect::Sqlite if columns.is_empty() => {
            format!("INSERT INTO {} DEFAULT VALUES", dialect.quote(table))
        }
        _ => insert(dialect, table, columns),
    };
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => format!("{insert} RETURNING {}", dialect.quote(key)),
        Dialect::MySql => insert,
    }
}

/// Set `column` to NULL in the rows whose `key` is one of `keys` keys.
pub fn set_null_by(dialect: Dialect, table: &str, column: &str, key: &str, keys: usize) -> String {
    let q = |ident: &str| dialect.quote(ident);
    format!("UPDATE {} SET {} = NULL WHERE {}", q(table), q(column), dialect.keys_condition(&q(key), keys))
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
    fn updates_and_inserts_if_absent() {
        let changed = vec![("name".to_string(), ColumnValue::Bound), ("reason".to_string(), ColumnValue::Null)];
        let literal = vec![("state".to_string(), ColumnValue::Literal("a, b".into()))];
        assert_eq!(
            update(Dialect::Sqlite, "t", "id", &literal, None),
            "UPDATE \"t\" SET \"state\" = 'a, b' WHERE \"id\" = ?"
        );
        assert_eq!(
            update(Dialect::Postgres, "task", "id", &changed, Some("version")),
            "UPDATE \"task\" SET \"name\" = $1, \"reason\" = NULL, \"version\" = \"version\" + 1 \
             WHERE \"id\" = $2 AND \"version\" = $3"
        );
        assert_eq!(
            update(Dialect::Sqlite, "task", "id", &changed, None),
            "UPDATE \"task\" SET \"name\" = ?, \"reason\" = NULL WHERE \"id\" = ?"
        );
        assert_eq!(
            update(Dialect::MySql, "task", "id", &[], Some("version")),
            "UPDATE `task` SET `version` = `version` + 1 WHERE `id` = ? AND `version` = ?"
        );
        let row = vec![("id".to_string(), ColumnValue::Bound), ("name".to_string(), ColumnValue::Bound)];
        assert_eq!(
            insert_if_absent(Dialect::Postgres, "task", "id", &row),
            "INSERT INTO \"task\" (\"id\", \"name\") VALUES ($1, $2) ON CONFLICT (\"id\") DO NOTHING"
        );
        assert_eq!(
            insert_if_absent(Dialect::MySql, "task", "id", &row),
            "INSERT INTO `task` (`id`, `name`) SELECT ?, ? FROM DUAL WHERE NOT EXISTS (SELECT 1 FROM `task` WHERE `id` = ?)"
        );
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

    #[test]
    fn rows_of_a_batch() {
        let names = vec!["id".to_string(), "name".to_string(), "state".to_string()];
        let row = |state: &str| vec![ColumnValue::Bound, ColumnValue::Bound, ColumnValue::Literal(state.into())];
        let rows = vec![row("open"), vec![ColumnValue::Bound, ColumnValue::Null, ColumnValue::Literal("it's".into())]];
        assert_eq!(
            insert_rows(Dialect::Postgres, "task", &names, &rows),
            "INSERT INTO \"task\" (\"id\", \"name\", \"state\") VALUES ($1, $2, 'open'), ($3, NULL, 'it''s')"
        );
        assert_eq!(
            upsert_rows(Dialect::MySql, "task", "id", &names, &rows[..1]),
            "INSERT INTO `task` (`id`, `name`, `state`) VALUES (?, ?, 'open') AS `new` ON DUPLICATE KEY UPDATE \
             `name` = `new`.`name`, `state` = `new`.`state`"
        );

        // The values of `name`, then the key and the version
        let names = vec!["name".to_string()];
        let rows = vec![vec![ColumnValue::Bound; 3], vec![ColumnValue::Null, ColumnValue::Bound, ColumnValue::Bound]];
        let casts = [Some("text".to_string()), Some("bigint".to_string()), Some("integer".to_string())];
        assert_eq!(
            update_rows(Dialect::Postgres, "task", "id", &names, &rows, Some("version"), Some(&casts)),
            "UPDATE \"task\" SET \"name\" = v.\"name\", \"version\" = \"task\".\"version\" + 1 FROM (VALUES \
             (CAST($1 AS text), CAST($2 AS bigint), CAST($3 AS integer)), \
             (CAST(NULL AS text), CAST($4 AS bigint), CAST($5 AS integer))) AS v (\"name\", \"$key\", \"$version\") \
             WHERE \"task\".\"id\" = v.\"$key\" AND \"task\".\"version\" = v.\"$version\" RETURNING \"task\".\"id\""
        );
        assert_eq!(
            update_rows(Dialect::Sqlite, "task", "id", &names, &rows, None, None),
            "UPDATE \"task\" SET \"name\" = v.column1 FROM (VALUES (?, ?, ?), (NULL, ?, ?)) AS v \
             WHERE \"task\".\"id\" = v.column2 RETURNING \"task\".\"id\""
        );
        let rows = vec![vec![ColumnValue::Bound; 2], vec![ColumnValue::Null, ColumnValue::Bound]];
        assert_eq!(
            update_rows(Dialect::MySql, "task", "id", &names, &rows, None, None),
            "UPDATE `task` JOIN (SELECT ? AS `name`, ? AS `$key` UNION ALL SELECT NULL, ?) AS v \
             ON `task`.`id` = v.`$key` SET `task`.`name` = v.`name`"
        );
        assert_eq!(
            lock_keys(Dialect::Postgres, "task", "id", 1),
            "SELECT \"id\" FROM \"task\" WHERE \"id\" = ANY($1) FOR UPDATE"
        );
    }
}
