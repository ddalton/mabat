//! Saving many values at once: [`crate::save_all`].
//!
//! The rows of many values are written as loading reads them: by table and by level, a few
//! statements for all of them rather than statements for each. For the rows of one table:
//!
//! 1. the rows whose keys the database generates and that have none are inserted, one by one,
//!    to read their keys;
//! 2. the other rows are updated by one statement from a table of their values, with their
//!    versions checked and incremented, which returns the keys of the rows it updated (MySQL
//!    cannot return them, so there the rows that exist are first found and locked);
//! 3. the rows it did not update are inserted by one statement, as an upsert like
//!    [`crate::save`]'s, or for versioned rows as an insert, which fails as a conflict if a
//!    row has the key;
//!
//! then the rows of their variant tables, owned collections (after deleting the elements that
//! are gone) and links are written the same way, a level at a time. Statements hold at most
//! [`MAX_PARAMETERS`] parameters, and larger batches are split.

use std::collections::{HashMap, HashSet};

use mabat_core::shape::{EmbeddedKind, FieldKind, VariantData, ViewShape};
use mabat_core::sql::Dialect;
use mabat_core::write::{self as statement, ColumnValue};

use crate::Error;
use crate::backend::{Backend, BoxFuture};
use crate::key::{Key, KeyList};
use crate::write::{Owned, Parent, RowWrite, Written, delete_rows, delete_tree, query_error, save_row, select_keys};

/// The most parameters of one statement: under the limits of PostgreSQL and MySQL (65,535)
/// and SQLite (32,766).
const MAX_PARAMETERS: usize = 30_000;

/// A row to save, with the parent whose collection it is an element of.
struct Item<B: Backend> {
    row: RowWrite<B>,
    parent: Option<Parent>,
}

/// The column types of the tables written, by table, for PostgreSQL's casts.
type Types = HashMap<&'static str, HashMap<String, String>>;

/// Save the rows of many values, each with what it owns: by table and level when every row
/// kept its values, else one value after the other. Returns what saving each produced.
pub(crate) async fn save<B: Backend>(conn: &mut B::Connection, rows: Vec<RowWrite<B>>) -> Result<Vec<Written>, Error>
where
    B::Connection: Send,
{
    if rows.iter().all(RowWrite::is_batchable) {
        let items = rows.into_iter().map(|row| Item { row, parent: None }).collect();
        let mut types = Types::new();
        return save_rows::<B>(conn, items, &mut types).await;
    }
    let mut written = Vec::with_capacity(rows.len());
    for row in rows {
        written.push(save_row::<B>(&mut *conn, row, None).await?);
    }
    Ok(written)
}

/// How a row of a batch is written.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Inserted with a key the database generated: nothing it would own exists yet.
    Inserted,
    Existing,
    New,
}

/// Save rows of one view's table, then what they own.
fn save_rows<'c, B: Backend>(
    conn: &'c mut B::Connection,
    mut items: Vec<Item<B>>,
    types: &'c mut Types,
) -> BoxFuture<'c, Result<Vec<Written>, Error>>
where
    B::Connection: Send,
{
    Box::pin(async move {
        let Some(first) = items.first() else { return Ok(Vec::new()) };
        let shape = first.row.shape;
        let encode = |e: sqlx::error::BoxDynError| Error::Write { view: shape.name, message: e.to_string() };
        let mut written: Vec<Written> = items.iter().map(|_| Written::default()).collect();

        // The parent's key and the position, for elements of collections
        for item in &mut items {
            if let Some(parent) = &item.parent {
                item.row.bind_key(parent.fk.to_string(), Some(&parent.key))?;
                if let Some((column, index)) = parent.index {
                    item.row.bind_key(column.to_string(), Some(&Key::Int(index)))?;
                }
            }
        }

        // Rows without the keys the database generates are inserted one by one, to read them
        let mut status = vec![Status::New; items.len()];
        for (i, item) in items.iter_mut().enumerate() {
            if item.row.key.is_some() {
                continue;
            }
            if !item.row.generated {
                return Err(Error::Write {
                    view: shape.name,
                    message: "it has no key field, so it cannot be saved".to_string(),
                });
            }
            let row = &mut item.row;
            if let Some((column, current)) = row.version.clone() {
                row.bind_key(column, Some(&Key::Int(current)))?;
                written[i].version = Some(current);
            }
            let sql = statement::insert_generated(B::DIALECT, shape.table, shape.key_column, &row.columns);
            let key = B::insert_generated(&mut *conn, sql.clone(), std::mem::take(&mut row.args))
                .await
                .map_err(|e| query_error(shape, &sql, e))?;
            row.key = Some(key.clone());
            written[i].key = Some(key);
            status[i] = Status::Inserted;
        }

        // The rows that exist: on MySQL, found and locked first; elsewhere, those the update
        // returns, without a lock, which would write to every row it locks
        let returning = B::DIALECT != Dialect::MySql;
        if returning {
            for s in status.iter_mut().filter(|s| **s == Status::New) {
                *s = Status::Existing;
            }
        } else {
            let keys: Vec<Key> = items
                .iter()
                .zip(&status)
                .filter(|(_, s)| **s == Status::New)
                .filter_map(|(item, _)| item.row.key.clone())
                .collect();
            let existing = lock_keys::<B>(&mut *conn, shape, keys).await?;
            for (item, status) in items.iter().zip(status.iter_mut()) {
                if *status == Status::New && item.row.key.as_ref().is_some_and(|k| existing.contains(k)) {
                    *status = Status::Existing;
                }
            }
        }

        // On PostgreSQL, the types of the columns, to cast the values of the rows updated
        if status.contains(&Status::Existing) && B::DIALECT == Dialect::Postgres && !types.contains_key(shape.table) {
            let found = B::column_types(&mut *conn, B::DIALECT.quote(shape.table))
                .await
                .map_err(|e| query_error(shape, "", e))?;
            types.insert(shape.table, found);
        }

        // The rows that exist are updated, the others inserted, by their columns
        for target in [Status::Existing, Status::New] {
            for (names, rows) in groups(&items, &status, target) {
                if target == Status::Existing {
                    let statements = updates(shape, &names, &rows, &items, types)?;
                    let updated = run_updates::<B>(&mut *conn, shape, statements, returning).await?;
                    for &i in &rows {
                        match updated.contains(&i) {
                            true => {
                                if let Some((_, current)) = &items[i].row.version {
                                    written[i].version = Some(current + 1);
                                }
                            }
                            // Not there, or with another version: inserted, or a conflict
                            false => status[i] = Status::New,
                        }
                    }
                } else {
                    let statements = inserts(shape, &names, &rows, &items)?;
                    let versioned = items[rows[0]].row.version.is_some();
                    run_inserts::<B>(&mut *conn, shape, statements, versioned).await?;
                    for &i in &rows {
                        if let Some((_, current)) = &items[i].row.version {
                            written[i].version = Some(*current);
                        }
                    }
                }
            }
        }

        // Variant tables: the rows of the chosen variants, and none in the others
        let mut variant_fields: Vec<usize> =
            items.iter().flat_map(|item| item.row.variants.iter().map(|(field, _, _)| *field)).collect();
        variant_fields.sort_unstable();
        variant_fields.dedup();
        for field_index in variant_fields {
            let FieldKind::Embedded { shape: embedded, .. } = &shape.fields[field_index].kind else { continue };
            let EmbeddedKind::Sum(sum) = &embedded().kind else { continue };
            let mut chosen: Vec<Item<B>> = Vec::new();
            let mut not: HashMap<&'static str, Vec<Key>> = HashMap::new();
            for (i, item) in items.iter_mut().enumerate() {
                let key = item.row.key.clone().expect("saved rows have keys");
                let Some(position) = item.row.variants.iter().position(|(f, _, _)| *f == field_index) else { continue };
                let (_, variant, row) = item.row.variants.remove(position);
                for other in sum.variants.iter().filter(|v| v.name != variant) {
                    if status[i] != Status::Inserted && matches!(other.data, VariantData::Table { .. }) {
                        not.entry(other.name).or_default().push(key.clone());
                    }
                }
                if let Some(mut row) = row {
                    let table = row.shape;
                    row.bind_key(table.key_column.to_string(), Some(&key))?;
                    row.key = Some(key);
                    chosen.push(Item { row, parent: None });
                }
            }
            for variant in sum.variants {
                let VariantData::Table { shape: table } = &variant.data else { continue };
                if let Some(keys) = not.remove(variant.name) {
                    delete_tree::<B>(&mut *conn, table(), keys).await?;
                }
                let (these, rest): (Vec<_>, Vec<_>) =
                    chosen.into_iter().partition(|item| std::ptr::eq(item.row.shape, table()));
                chosen = rest;
                save_rows::<B>(&mut *conn, these, types).await?;
            }
        }

        // Owned collections: delete the elements that are gone, then save the others
        let mut collection_fields: Vec<usize> =
            items.iter().flat_map(|item| item.row.collections.iter().map(|(field, _)| *field)).collect();
        collection_fields.sort_unstable();
        collection_fields.dedup();
        for field_index in collection_fields {
            let FieldKind::Child(child) = &shape.fields[field_index].kind else { continue };
            let target = (child.shape)();
            let mut parents = Vec::new();
            let mut elements: Vec<Item<B>> = Vec::new();
            let mut counts = Vec::with_capacity(items.len());
            for (i, item) in items.iter_mut().enumerate() {
                let key = item.row.key.clone().expect("saved rows have keys");
                if status[i] != Status::Inserted {
                    parents.push(key.clone());
                }
                let rows = match item.row.collections.iter().position(|(f, _)| *f == field_index) {
                    Some(position) => item.row.collections.remove(position).1,
                    None => Vec::new(),
                };
                counts.push(rows.len());
                for (position, row) in rows.into_iter().enumerate() {
                    let index = child.index.map(|column| (column, position as i64));
                    elements.push(Item { row, parent: Some(Parent { fk: child.fk, key: key.clone(), index }) });
                }
            }
            let kept: HashSet<Key> = elements.iter().filter_map(|e| e.row.key.clone()).collect();
            let existing = select_keys::<B>(&mut *conn, target, target.key_column, child.fk, parents).await?;
            let gone: Vec<Key> = existing.into_iter().filter(|k| !kept.contains(k)).collect();
            delete_tree::<B>(&mut *conn, target, gone).await?;
            let mut saved = save_rows::<B>(&mut *conn, elements, types).await?.into_iter();
            for (i, count) in counts.into_iter().enumerate() {
                written[i].collections.push((field_index, saved.by_ref().take(count).collect()));
            }
        }

        // Many-to-many collections: the links of the rows
        let mut link_fields: Vec<usize> =
            items.iter().flat_map(|item| item.row.links.iter().map(|(field, _)| *field)).collect();
        link_fields.sort_unstable();
        link_fields.dedup();
        for field_index in link_fields {
            let FieldKind::Child(child) = &shape.fields[field_index].kind else { continue };
            let Some(through) = child.through else { continue };
            let parents: Vec<Key> = items
                .iter()
                .zip(&status)
                .filter(|(_, s)| **s != Status::Inserted)
                .filter_map(|(item, _)| item.row.key.clone())
                .collect();
            delete_rows::<B>(&mut *conn, shape, through.table, child.fk, &parents).await?;
            let mut links: Vec<(Vec<String>, LinkRow<B>)> = Vec::new();
            for item in &mut items {
                let owner = item.row.key.clone().expect("saved rows have keys");
                let rows = match item.row.links.iter().position(|(f, _)| *f == field_index) {
                    Some(position) => item.row.links.remove(position).1,
                    None => Vec::new(),
                };
                for (position, link) in rows.into_iter().enumerate() {
                    let linked = link.key.clone().ok_or_else(|| encode("a linked value has no key".into()))?;
                    let mut names = vec![child.fk.to_string(), through.target.to_string()];
                    let mut values = vec![Owned::Key(owner.clone()), Owned::Key(linked)];
                    if let Some(column) = child.index {
                        names.push(column.to_string());
                        values.push(Owned::Key(Key::Int(position as i64)));
                    }
                    let mut columns = vec![ColumnValue::Bound; names.len()];
                    for (name, value) in &link.columns {
                        names.push(name.clone());
                        columns.push(value.clone());
                    }
                    values.extend(link.owned.unwrap_or_default());
                    links.push((names, LinkRow { columns, values }));
                }
            }
            let mut groups: Vec<(Vec<String>, Vec<LinkRow<B>>)> = Vec::new();
            for (names, link) in links {
                match groups.iter_mut().find(|(n, _)| *n == names) {
                    Some((_, rows)) => rows.push(link),
                    None => groups.push((names, vec![link])),
                }
            }
            for (names, rows) in groups {
                let per_row = names.len().max(1);
                for chunk in rows.chunks((MAX_PARAMETERS / per_row).max(1)) {
                    let values: Vec<Vec<ColumnValue>> = chunk.iter().map(|link| link.columns.clone()).collect();
                    let sql = statement::insert_rows(B::DIALECT, through.table, &names, &values);
                    let mut args = B::Arguments::default();
                    for link in chunk {
                        for value in &link.values {
                            value.bind(&mut args).map_err(encode)?;
                        }
                    }
                    B::execute_args(&mut *conn, sql.clone(), args).await.map_err(|e| query_error(shape, &sql, e))?;
                }
            }
        }
        Ok(written)
    })
}

/// The keys of the rows of `shape` that exist among `keys`, locking the rows.
async fn lock_keys<B: Backend>(
    conn: &mut B::Connection,
    shape: &'static ViewShape,
    keys: Vec<Key>,
) -> Result<HashSet<Key>, Error> {
    let mut found = HashSet::new();
    let Ok(keys) = KeyList::new(keys) else { return Ok(found) };
    for keys in keys.chunks(crate::write::MAX_KEYS) {
        let sql = statement::lock_keys(B::DIALECT, shape.table, shape.key_column, keys.len());
        let mut args = B::Arguments::default();
        B::add_keys(&mut args, &keys).map_err(|e| Error::Write { view: shape.name, message: e.to_string() })?;
        let rows = B::fetch_args(&mut *conn, sql.clone(), args).await.map_err(|e| query_error(shape, &sql, e))?;
        for row in &rows {
            let kind = B::key_kind(B::column_type(row, 0)).ok_or_else(|| Error::Write {
                view: shape.name,
                message: format!("the key column `{}` cannot hold keys", shape.key_column),
            })?;
            if let Some(key) = B::read_key(row, 0, kind).map_err(|e| query_error(shape, &sql, e))? {
                found.insert(key);
            }
        }
    }
    Ok(found)
}

/// The values of the bound columns of rows, in order.
fn bind_owned<B: Backend>(row: &RowWrite<B>, args: &mut B::Arguments) -> Result<(), sqlx::error::BoxDynError> {
    for value in row.owned.as_deref().unwrap_or_default() {
        value.bind(args)?;
    }
    Ok(())
}

/// The rows of a status, by the columns they write: rows with the same columns are written
/// by the same statements.
fn groups<B: Backend>(items: &[Item<B>], status: &[Status], target: Status) -> Vec<(Vec<String>, Vec<usize>)> {
    let mut groups: Vec<(Vec<String>, Vec<usize>)> = Vec::new();
    for (i, item) in items.iter().enumerate().filter(|(i, _)| status[*i] == target) {
        let names: Vec<String> = item.row.columns.iter().map(|(name, _)| name.clone()).collect();
        match groups.iter_mut().find(|(n, _)| *n == names) {
            Some((_, rows)) => rows.push(i),
            None => groups.push((names, vec![i])),
        }
    }
    groups
}

/// A link row of a many-to-many collection: its values, and the values bound.
struct LinkRow<B: Backend> {
    columns: Vec<ColumnValue>,
    values: Vec<Owned<B>>,
}

/// A statement that writes some rows of a batch, with its arguments.
struct Statement<B: Backend> {
    sql: String,
    args: B::Arguments,
    /// The rows it writes, by their index in the batch, and their keys.
    rows: Vec<usize>,
    keys: Vec<Key>,
    /// The keys, for a conflict that cannot tell which of the rows changed.
    described: String,
}

/// A statement that writes the rows `rows` of `items`.
fn statement_of<B: Backend>(sql: String, args: B::Arguments, rows: &[usize], items: &[Item<B>]) -> Statement<B> {
    let keys: Vec<Key> = rows.iter().filter_map(|&i| items[i].row.key.clone()).collect();
    let described: Vec<String> = keys.iter().map(|k| format!("{k:?}")).collect();
    Statement { sql, args, rows: rows.to_vec(), keys, described: format!("one of {}", described.join(", ")) }
}

/// The statements that update rows that exist with the columns `names`, from a table of their
/// values.
fn updates<B: Backend>(
    shape: &'static ViewShape,
    names: &[String],
    rows: &[usize],
    items: &[Item<B>],
    types: &Types,
) -> Result<Vec<Statement<B>>, Error> {
    let encode = |e: sqlx::error::BoxDynError| Error::Write { view: shape.name, message: e.to_string() };
    let version = items[rows[0]].row.version.as_ref().map(|(column, _)| column.clone());
    // A row of a view whose only column is its key has nothing to update: it is inserted,
    // unless it exists
    if names.iter().all(|n| n == shape.key_column) && version.is_none() {
        return Ok(Vec::new());
    }
    let casts: Option<Vec<Option<String>>> = types.get(shape.table).map(|columns| {
        let mut casts: Vec<Option<String>> = names.iter().map(|n| columns.get(n).cloned()).collect();
        casts.push(columns.get(shape.key_column).cloned());
        if let Some(version) = &version {
            casts.push(columns.get(version).cloned());
        }
        casts
    });
    let mut statements = Vec::new();
    for chunk in rows.chunks((MAX_PARAMETERS / (names.len() + 2)).max(1)) {
        let mut values = Vec::with_capacity(chunk.len());
        let mut args = B::Arguments::default();
        for &i in chunk {
            let row = &items[i].row;
            let mut row_values: Vec<ColumnValue> = row.columns.iter().map(|(_, v)| v.clone()).collect();
            bind_owned(row, &mut args).map_err(encode)?;
            row_values.push(ColumnValue::Bound);
            B::add_key(&mut args, row.key.as_ref().expect("existing rows have keys")).map_err(encode)?;
            if let Some((_, current)) = &row.version {
                row_values.push(ColumnValue::Bound);
                B::add_key(&mut args, &Key::Int(*current)).map_err(encode)?;
            }
            values.push(row_values);
        }
        let sql = statement::update_rows(
            B::DIALECT,
            shape.table,
            shape.key_column,
            names,
            &values,
            version.as_deref(),
            casts.as_deref(),
        );
        statements.push(statement_of(sql, args, chunk, items));
    }
    Ok(statements)
}

/// The statements that insert new rows with the columns `names`: an upsert, which updates a
/// row inserted meanwhile, or for versioned rows an insert, which then fails with a conflict.
fn inserts<B: Backend>(
    shape: &'static ViewShape,
    names: &[String],
    rows: &[usize],
    items: &[Item<B>],
) -> Result<Vec<Statement<B>>, Error> {
    let encode = |e: sqlx::error::BoxDynError| Error::Write { view: shape.name, message: e.to_string() };
    let version = items[rows[0]].row.version.as_ref().map(|(column, _)| column.clone());
    let mut names = names.to_vec();
    if let Some(version) = &version {
        names.push(version.clone());
    }
    let mut statements = Vec::new();
    for chunk in rows.chunks((MAX_PARAMETERS / names.len().max(1)).max(1)) {
        let mut values = Vec::with_capacity(chunk.len());
        let mut args = B::Arguments::default();
        for &i in chunk {
            let row = &items[i].row;
            let mut row_values: Vec<ColumnValue> = row.columns.iter().map(|(_, v)| v.clone()).collect();
            bind_owned(row, &mut args).map_err(encode)?;
            if let Some((_, current)) = &row.version {
                row_values.push(ColumnValue::Bound);
                B::add_key(&mut args, &Key::Int(*current)).map_err(encode)?;
            }
            values.push(row_values);
        }
        let sql = match version {
            Some(_) => statement::insert_rows(B::DIALECT, shape.table, &names, &values),
            None => statement::upsert_rows(B::DIALECT, shape.table, shape.key_column, &names, &values),
        };
        statements.push(statement_of(sql, args, chunk, items));
    }
    Ok(statements)
}

/// Run the statements that update rows, and return the rows they updated. With `returning`,
/// the statements return the keys of those rows, and a row that is missing, or has another
/// version, is left for an insert. Otherwise the rows are locked and known to exist, so each
/// statement must update all its rows, and a row it does not had another version.
async fn run_updates<B: Backend>(
    conn: &mut B::Connection,
    shape: &'static ViewShape,
    statements: Vec<Statement<B>>,
    returning: bool,
) -> Result<HashSet<usize>, Error> {
    let mut updated = HashSet::new();
    for Statement { sql, args, rows, keys, described } in statements {
        if returning {
            let returned =
                B::fetch_args(&mut *conn, sql.clone(), args).await.map_err(|e| query_error(shape, &sql, e))?;
            let mut found = HashSet::new();
            for row in &returned {
                let kind = B::key_kind(B::column_type(row, 0)).ok_or_else(|| Error::Write {
                    view: shape.name,
                    message: format!("the key column `{}` cannot hold keys", shape.key_column),
                })?;
                if let Some(key) = B::read_key(row, 0, kind).map_err(|e| query_error(shape, &sql, e))? {
                    found.insert(key);
                }
            }
            updated.extend(rows.into_iter().zip(keys).filter(|(_, key)| found.contains(key)).map(|(i, _)| i));
        } else {
            let count =
                B::execute_args(&mut *conn, sql.clone(), args).await.map_err(|e| query_error(shape, &sql, e))?;
            if count < rows.len() as u64 {
                return Err(Error::Conflict { view: shape.name, key: described });
            }
            updated.extend(rows);
        }
    }
    Ok(updated)
}

/// Run the statements that insert rows; a versioned row inserted meanwhile is a conflict.
async fn run_inserts<B: Backend>(
    conn: &mut B::Connection,
    shape: &'static ViewShape,
    statements: Vec<Statement<B>>,
    versioned: bool,
) -> Result<(), Error> {
    for Statement { sql, args, described, .. } in statements {
        B::execute_args(&mut *conn, sql.clone(), args).await.map_err(|e| match e {
            sqlx::Error::Database(db) if versioned && db.is_unique_violation() => {
                Error::Conflict { view: shape.name, key: described }
            }
            e => query_error(shape, &sql, e),
        })?;
    }
    Ok(())
}
