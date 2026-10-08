---
title: Databases and concurrency
description: PostgreSQL, MySQL and SQLite, how keys are bound on each, and concurrent loads on a pool.
---

The features `postgres` (the default), `mysql` and `sqlite` enable each database, in any combination
([MPA-DB-1](../../spec/mpa/#mpa-db-1)). A view is generated for every enabled database, and a load runs on the
database of the connection it is given ([MPA-DB-3](../../spec/mpa/#mpa-db-3)).

## Differences between databases

| | PostgreSQL | MySQL 8 | SQLite |
| --- | --- | --- | --- |
| Keys of a batched query | one array, `= ANY($1)` | `IN (?, …)`, padded to a power of two, at most 1,000 per statement | same as MySQL |
| `ilike` | `ILIKE` | `LOWER() LIKE LOWER()` | `LOWER() LIKE LOWER()` |
| Upsert | `ON CONFLICT … DO UPDATE` | `INSERT … AS new ON DUPLICATE KEY UPDATE` | `ON CONFLICT … DO UPDATE` |
| Shared snapshots for pooled loads | yes | no | no |

See [MPA-DB-4](../../spec/mpa/#mpa-db-4). When a view's field types are not supported by every enabled database —
a PostgreSQL array, `Decimal` on SQLite — `#[view(databases = "postgres")]` limits it
([MPA-DB-2](../../spec/mpa/#mpa-db-2)).

## Concurrent loads

A load runs its root query, then the queries of each level. Given a `Pooled` pool, the queries of a level run at the
same time, each on a connection held for that query only ([MPA-LOAD-11](../../spec/mpa/#mpa-load-11)):

```rust
// PostgreSQL: every query sees one snapshot, exported by the first connection and imported by the others
let mut pooled = mabat::Pooled::snapshot(&pool, 4);
let boards = mabat::load::<BoardView>().all(&mut pooled).await?;

// Any database: each query sees what is committed when it runs
let mut pooled = mabat::Pooled::read_committed(&pool, 4);
```

Loading 20 boards whose lists and labels each take half a second takes one second on one connection and half a
second on a pool. With `read_committed`, a row committed during the load can show up in a collection whose parent
was read before it. Graph loads run their queries one at a time.
