//! Snapshots of database schemas.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// The version of the snapshot format.
pub const FORMAT: u32 = 1;

/// The tables of a database, with their columns and keys, as `mabat schema` writes them.
///
/// Tables are sorted by name, columns are in the order of the table, and keys in the order of
/// their columns, so that the same schema always gives the same file and a change of schema a
/// readable diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub format: u32,
    /// The database: `PostgreSQL`, `MySQL` or `SQLite`, as the manifest names it.
    pub backend: String,
    pub tables: Vec<Table>,
}

/// A table, or a view of the database, which Mabat views may read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Table {
    pub name: String,
    /// A view of the database, not a table: it has no keys of its own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub view: bool,
    pub columns: Vec<Column>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primary_key: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign_keys: Vec<ForeignKey>,
}

/// A column of a table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    /// The type as SQLx names it, the names the manifest's accepted types use, e.g. `INT8`.
    pub r#type: String,
    /// The type as the database declares it, e.g. `bigint` or `varchar(100)`.
    pub declared: String,
    pub nullable: bool,
    /// The database generates its values: an identity, serial or `AUTO_INCREMENT` column, or
    /// SQLite's `INTEGER PRIMARY KEY`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub generated: bool,
}

/// A foreign key: `columns` of the table reference `references` of `table`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ForeignKey {
    pub columns: Vec<String>,
    pub table: String,
    pub references: Vec<String>,
}

impl Snapshot {
    /// A snapshot of the tables, sorted.
    pub fn new(backend: impl Into<String>, mut tables: Vec<Table>) -> Snapshot {
        tables.sort_by(|a, b| a.name.cmp(&b.name));
        for table in &mut tables {
            table.foreign_keys.sort();
        }
        Snapshot { format: FORMAT, backend: backend.into(), tables }
    }

    /// The snapshot as JSON, as `mabat schema` writes it.
    pub fn to_json(&self) -> String {
        let mut json = serde_json::to_string_pretty(self).expect("a snapshot is JSON");
        json.push('\n');
        json
    }

    pub fn from_json(json: &str) -> Result<Snapshot, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// The table or view with the name: as written, else ignoring case, as databases that
    /// fold unquoted names do.
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables
            .iter()
            .find(|t| t.name == name)
            .or_else(|| self.tables.iter().find(|t| t.name.eq_ignore_ascii_case(name)))
    }

    /// How the schema `actual` differs from the snapshot, one line each; empty if it does not.
    pub fn differences(&self, actual: &Snapshot) -> Vec<String> {
        let mut out = Vec::new();
        if self.backend != actual.backend {
            out.push(format!("the snapshot is of {}, the database is {}", self.backend, actual.backend));
            return out;
        }
        for table in &self.tables {
            if !actual.tables.iter().any(|t| t.name == table.name) {
                out.push(format!("table `{}` is in the snapshot but not in the database", table.name));
            }
        }
        for table in &actual.tables {
            match self.tables.iter().find(|t| t.name == table.name) {
                None => out.push(format!("table `{}` is in the database but not in the snapshot", table.name)),
                Some(snapshot) => table_differences(snapshot, table, &mut out),
            }
        }
        out
    }
}

fn table_differences(snapshot: &Table, actual: &Table, out: &mut Vec<String>) {
    let name = &actual.name;
    if snapshot.view != actual.view {
        let what = |view: bool| if view { "a view" } else { "a table" };
        out.push(format!("`{name}` is {} in the snapshot, {} in the database", what(snapshot.view), what(actual.view)));
    }
    for column in &snapshot.columns {
        if !actual.columns.iter().any(|c| c.name == column.name) {
            out.push(format!("column `{name}.{}` is in the snapshot but not in the database", column.name));
        }
    }
    for column in &actual.columns {
        let Some(before) = snapshot.columns.iter().find(|c| c.name == column.name) else {
            out.push(format!("column `{name}.{}` is in the database but not in the snapshot", column.name));
            continue;
        };
        let mut changes = String::new();
        if before.declared != column.declared || before.r#type != column.r#type {
            let _ = write!(changes, "; type {} in the snapshot, {} in the database", before.declared, column.declared);
        }
        if before.nullable != column.nullable {
            let null = |nullable: bool| if nullable { "nullable" } else { "NOT NULL" };
            let _ = write!(
                changes,
                "; {} in the snapshot, {} in the database",
                null(before.nullable),
                null(column.nullable)
            );
        }
        if before.generated != column.generated {
            let generated = |generated: bool| if generated { "generated" } else { "not generated" };
            let _ = write!(
                changes,
                "; {} in the snapshot, {} in the database",
                generated(before.generated),
                generated(column.generated)
            );
        }
        if !changes.is_empty() {
            out.push(format!("column `{name}.{}`:{}", column.name, changes.trim_start_matches(';')));
        }
    }
    if snapshot.primary_key != actual.primary_key {
        out.push(format!(
            "the primary key of `{name}` is ({}) in the snapshot, ({}) in the database",
            snapshot.primary_key.join(", "),
            actual.primary_key.join(", ")
        ));
    }
    let describe =
        |fk: &ForeignKey| format!("({}) → {}({})", fk.columns.join(", "), fk.table, fk.references.join(", "));
    for fk in &snapshot.foreign_keys {
        if !actual.foreign_keys.contains(fk) {
            out.push(format!("foreign key `{name}` {} is in the snapshot but not in the database", describe(fk)));
        }
    }
    for fk in &actual.foreign_keys {
        if !snapshot.foreign_keys.contains(fk) {
            out.push(format!("foreign key `{name}` {} is in the database but not in the snapshot", describe(fk)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, ty: &str, nullable: bool) -> Column {
        Column { name: name.into(), r#type: ty.into(), declared: ty.to_lowercase(), nullable, generated: false }
    }

    fn task() -> Table {
        Table {
            name: "task".into(),
            view: false,
            columns: vec![
                column("id", "INT8", false),
                column("name", "TEXT", false),
                column("parent_id", "INT8", true),
            ],
            primary_key: vec!["id".into()],
            foreign_keys: vec![ForeignKey {
                columns: vec!["parent_id".into()],
                table: "task".into(),
                references: vec!["id".into()],
            }],
        }
    }

    #[test]
    fn round_trips_sorted() {
        let person = Table { name: "person".into(), columns: vec![column("id", "INT8", false)], ..task() };
        let snapshot = Snapshot::new("PostgreSQL", vec![task(), person]);
        assert_eq!(snapshot.tables.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["person", "task"]);
        assert_eq!(Snapshot::from_json(&snapshot.to_json()).unwrap(), snapshot);
        assert!(snapshot.to_json().contains("\"type\": \"INT8\""));
        assert_eq!(snapshot.table("TASK").map(|t| t.name.as_str()), Some("task"));
    }

    #[test]
    fn lists_differences() {
        let before = Snapshot::new("PostgreSQL", vec![task()]);
        assert!(before.differences(&before).is_empty());
        let mut changed = task();
        changed.columns[1] = column("name", "VARCHAR", true);
        changed.columns.push(column("due", "DATE", true));
        changed.foreign_keys.clear();
        let after = Snapshot::new("PostgreSQL", vec![changed, Table { name: "note".into(), ..task() }]);
        assert_eq!(
            before.differences(&after),
            [
                "table `note` is in the database but not in the snapshot",
                "column `task.name`: type text in the snapshot, varchar in the database; NOT NULL in the snapshot, \
             nullable in the database",
                "column `task.due` is in the database but not in the snapshot",
                "foreign key `task` (parent_id) → task(id) is in the snapshot but not in the database",
            ]
        );
    }
}
