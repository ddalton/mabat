---
title: Saving aggregates
description: Save a value and what it owns, save only what changed, delete, and lock rows optimistically.
---

A view also writes. The same declaration that loads an aggregate saves it back, on any of the three databases, in
a transaction — or a savepoint of yours ([MPA-WRITE-1](../../spec/mpa/#mpa-write-1)).

```rust
mabat::save(&mut board, &mut tx).await?;                     // the board and what it owns
mabat::save_changes(&before, &mut after, &mut tx).await?;    // only what changed
mabat::delete::<Board, _>(board.id, &mut tx).await?;         // the board and what it owns
```

Statements run when they are called; there is no session to flush. Keys come from the application, so a saved view
and the views of its owned collections need a key field ([MPA-WRITE-2](../../spec/mpa/#mpa-write-2)).

## What `save` writes

| Part | Written as | Rule |
| --- | --- | --- |
| Columns, embedded values, JSON | the row, updated by key, or inserted if new | [MPA-WRITE-3](../../spec/mpa/#mpa-write-3) |
| An owned collection | made equal to the value's: gone elements deleted with what they own, the rest saved with their position or map key | [MPA-WRITE-4](../../spec/mpa/#mpa-write-4) |
| Many-to-many | the link rows, replaced | [MPA-WRITE-5](../../spec/mpa/#mpa-write-5) |
| A to-one reference | the foreign key only | [MPA-WRITE-5](../../spec/mpa/#mpa-write-5) |
| An enum | the tag and variant columns, or the variant's table row | [MPA-WRITE-6](../../spec/mpa/#mpa-write-6) |

## Saving only what changed

```rust
let before = mabat::load::<Board>().by_key(id).one(&mut tx).await?;
let mut after = before.clone();
after.name = "Roadmap 2".into();
mabat::save_changes(&before, &mut after, &mut tx).await?;   // UPDATE "board" SET "name" = $1 WHERE "id" = $2
```

Fields are compared with `PartialEq` and elements matched by key: changed rows get `UPDATE … SET <changed>`, new
elements are saved, removed ones deleted, and unchanged rows produce no statement
([MPA-WRITE-8](../../spec/mpa/#mpa-write-8)). It also updates views of some of a table's columns, which a whole
`save` cannot insert ([MPA-WRITE-12](../../spec/mpa/#mpa-write-12)).

There is no session watching your values, so nothing is tracked behind your back: `before` is the snapshot, and
the comparison runs when you call `save_changes`. [Change tracking, against an
ORM](../../architecture/09-change-tracking-against-an-orm/) sets this beside the proxies, snapshots and flushes
of an ORM's session.

## Optimistic locking

```rust
#[derive(View, Clone, PartialEq)]
#[view(table = "doc")]
struct Doc {
    id: i64,
    title: String,
    #[view(version)]
    version: i32,
}
```

A row with a version column is written only if it still has the value's version, which the write increments;
otherwise the write fails with `Error::Conflict` and rolls back ([MPA-WRITE-9](../../spec/mpa/#mpa-write-9)). The new
versions are written back into the value and its owned elements, so it can be saved again without reloading
([MPA-WRITE-10](../../spec/mpa/#mpa-write-10)).

## Many values at once

`mabat::save_all` saves many values in one transaction, each as `save` saves one, but with statements for each
table and level of the aggregate instead of for each row ([MPA-WRITE-19](../../spec/mpa/#mpa-write-19)):

```rust
mabat::save_all(&mut tasks, &mut tx).await?;
```

For the rows of each table, one statement updates those that exist from a table of their values, and one inserts
the others; then the rows of their collections, variant tables and links follow the same way. Statements are split
to stay under the databases' limits on parameters.

| Saving 1,000 tasks of 10 subtasks (local PostgreSQL) | New rows | Rows that exist |
| --- | --- | --- |
| `save`, one task at a time | 1.0 s | 1.6 s |
| `save_all` | 0.12 s | 0.08 s |

Over a network, the gap grows with the round-trip time: one task at a time takes thousands of round trips, and
`save_all` a few dozen. Run `cargo run --release -p mabat --example save_all` to measure your own. Rows whose keys
the database generates are inserted one at a time, to read their keys; their collections are still batched.

## Keys the database generates

Mark the key field `#[view(generated)]` and make it an `Option` of an integer. `save` inserts a value whose key
is `None` without its key column, reads the key the database generated, writes what the value owns under it, and
writes the key back into the value and into its new elements
([MPA-WRITE-13](../../spec/mpa/#mpa-write-13)):

```rust
#[derive(View)]
#[view(table = "project")]
struct Project {
    #[view(generated)]
    id: Option<i64>,
    name: String,
    #[view(child(fk = "project_id", index = "position"))]
    tasks: Vec<Task>,
}

let mut project = Project { id: None, name: "Alpha".into(), tasks: vec![task("Design")] };
mabat::save(&mut project, &mut tx).await?;   // INSERT INTO "project" ("name") VALUES ($1) RETURNING "id"
let id = project.id.unwrap();                // and project.tasks[0].id, if Task's key is generated too
```

| Database | The column | How the key is read |
| --- | --- | --- |
| PostgreSQL | `BIGINT GENERATED BY DEFAULT AS IDENTITY`, or `bigserial` | `RETURNING` |
| MySQL | `BIGINT AUTO_INCREMENT` | `LAST_INSERT_ID()` |
| SQLite | `INTEGER PRIMARY KEY` | `RETURNING` |

A value that has a key is saved like any other, so the column must accept keys given explicitly: on PostgreSQL
use `GENERATED BY DEFAULT`, not `GENERATED ALWAYS`. With `save_changes`, elements without a key are new and are
inserted the same way. A value that was never saved has no changes to save: `save_changes` refuses it, and so
does a reference or a link to it, with `Error::Write`.

## Graphs

Views with `Ref<T>` fields are saved as a whole graph with `save_graph`; see [Saving a
graph](../graphs/#saving-a-graph).
