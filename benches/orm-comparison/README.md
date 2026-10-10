# Mabat, SQLx, SeaORM and Diesel, loading the same data

Loads 1, 100 and 10,000 tasks with their 10 subtasks each with Mabat, hand-written SQLx, SeaORM and diesel-async,
each its own idiomatic batched way (two queries), and measures them with criterion. The loaders are in
[`src/lib.rs`](src/lib.rs); every library's result is checked before it is measured.

```sh
../../scripts/with-postgres.sh sh -c 'DATABASE_URL=$MABAT_TEST_DATABASE_URL cargo bench'
```

This crate is a workspace of its own, so that Mabat's builds and CI never compile SeaORM or Diesel. The results are
in the [performance guide](https://ddalton.github.io/mabat/guides/performance/).
