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
| Columns, embedded values, JSON | the row, upserted by key | [MPA-WRITE-3](../../spec/mpa/#mpa-write-3) |
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

## Not yet

Database-generated keys and saving `Ref<T>` graphs are not supported
([MPA-NOT-1](../../spec/mpa/#mpa-not-1), [MPA-NOT-2](../../spec/mpa/#mpa-not-2)).
