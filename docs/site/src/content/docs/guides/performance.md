---
title: Performance
description: Mabat against hand-written SQLx, SeaORM and Diesel, loading the same nested data.
---

Mabat runs one query for the root rows and one batched query per relationship, then decodes the rows into
nested values. How much does that cost against writing the same queries by hand, or against other Rust database
libraries? The benchmark in
[`benches/orm-comparison`](https://github.com/ddalton/mabat/tree/main/benches/orm-comparison) loads the same data
four ways and measures it with criterion.

## What is measured

Tasks by key, each with its 10 subtasks in order of their position, as typed nested values: 1, 100 and 10,000
tasks out of 10,000. Each library loads them its own idiomatic, batched way, two queries and grouping in Rust,
on one connection:

| | How it loads |
| --- | --- |
| SQLx, by hand | `WHERE id = ANY($1)`, then `WHERE task_id = ANY($1) ORDER BY position`, grouped in a `HashMap` |
| Mabat | `mabat::load::<Task>().by_keys(keys).all(&mut conn)`, with a `child` collection |
| SeaORM 2.0 | `Task::find().filter(id.is_in(keys))`, then `load_many` of the subtasks ordered by position |
| diesel-async 0.9 | `eq_any(keys)`, then `belonging_to(&tasks).order(position)` and `grouped_by` |

## Results

Median of criterion's samples, on an Apple M1 with 8 GB, PostgreSQL 16.9 on the same machine, Rust 1.96, SQLx
0.9.0, SeaORM 2.0.4, Diesel 2.3.14 with diesel-async 0.9.2 and tokio-postgres 0.7.18:

| Tasks (× 10 subtasks) | SQLx, by hand | Mabat | SeaORM | diesel-async |
| --- | ---: | ---: | ---: | ---: |
| 1 | 86.6 µs | 98.9 µs | 191.9 µs | 101.1 µs |
| 100 | 897 µs | 964 µs | 957 µs | 594 µs |
| 10,000 | 74.3 ms | 80.6 ms | 79.3 ms | 47.0 ms |

- **Mabat is within 7–14% of hand-written SQLx** running the same two queries, the cost of planning the load
  and decoding by alias into nested values.
- **Against SeaORM**, Mabat is about as fast for 100 and 10,000 tasks, and twice as fast for one.
- **diesel-async is 35–40% faster than all three for many rows.** It runs on the tokio-postgres driver; the
  others run on SQLx. Hand-written SQLx is as far behind it as Mabat is, so the difference is the driver's, not
  the mapping's.

## Run it

```sh
cd benches/orm-comparison
../../scripts/with-postgres.sh sh -c 'DATABASE_URL=$MABAT_TEST_DATABASE_URL cargo bench'
```

It needs the PostgreSQL server binaries, as the tests do, fills the database of `DATABASE_URL` with 10,000 tasks
and 100,000 subtasks, and checks that every library loads the same counts before measuring. The crate is a
workspace of its own, so building Mabat never compiles SeaORM or Diesel. Results depend on the machine and on
where the database runs: on a network, round trips dominate and the four come closer together.
