# mabat-graphql

A GraphQL schema generated from [Mabat](https://github.com/ddalton/mabat) views, with
[async-graphql](https://crates.io/crates/async-graphql). Each root field is one Mabat load of the fields the
query selects: only their columns are selected, and only the queries of the selected collections and references
run, on PostgreSQL, MySQL or SQLite.

```rust
let schema = mabat_graphql::schema(&pool)
    .list::<TaskView>("tasks")   // tasks(where, orderBy, limit, offset): [TaskView!]!
    .by_key::<TaskView>("task")  // task(key): TaskView
    .finish()?;
```

See the crate documentation for the types and arguments, and `examples/chinook.rs` for a server of the Chinook
music store with GraphiQL:

```sh
cargo run -p mabat-graphql --example chinook
```
