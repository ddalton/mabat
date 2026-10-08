# Refract

Typed aggregate reads for Rust, with SQL you can tune without changing code.

> **Status:** early development, not published yet. Milestone 1 (struct views, PostgreSQL, reads) is
> implemented. See the [design document](docs/design.md) for the plan.

Refract loads nested, typed data from PostgreSQL. The shape of the result is declared with ordinary Rust structs,
and Refract plans and runs the queries that fill it: one query for the root rows, plus one batched query
(`WHERE fk = ANY($1)`) per collection and per reference. It never runs one query per row.

```rust
use refract::View;
use uuid::Uuid;

#[derive(View)]
#[view(table = "task")]
struct TaskView {
    id: Uuid,
    name: String,
    description: Option<String>,
    #[view(embed(prefix = "addr_"))]
    address: Address,                          // columns addr_street, addr_city
    #[view(to_one(fk = "assignee_id"))]
    assignee: Option<PersonView>,              // loaded by one batched query
    #[view(child(fk = "parent_id", order_by = "position, name"))]
    children: Vec<SubtaskView>,                // loaded by one batched query
}

#[derive(View)]
#[view(embedded)]
struct Address {
    street: String,
    city: String,
}

#[derive(View)]
#[view(table = "person")]
struct PersonView {
    id: i64,
    #[view(column = "full_name")]
    name: String,
}

#[derive(View)]
#[view(table = "task")]
struct SubtaskView {
    name: String,
    #[view(child(fk = "task_id"))]
    notes: Vec<NoteView>,                      // nested collections work the same way
}

#[derive(View)]
#[view(table = "task_note")]
struct NoteView {
    body: String,
}

// One aggregate, on a pooled connection or inside a transaction
let task = refract::load::<TaskView>().by_key(id).one(&mut *conn).await?;

// Many, with ordering and paging of the root rows
let page = refract::load::<TaskView>().order_by("name").limit(20).offset(40).all(&mut *conn).await?;

// The queries Refract runs
println!("{}", refract::plan::<TaskView>()?.explain());
```

## Status

| Feature | Status |
| --- | --- |
| Struct views, embedded structs, `Option` columns | Done (M1) |
| To-many children and to-one references, nested, batched | Done (M1) |
| Decoding by column alias, errors that name the view and path | Done (M1) |
| Enums with data (sum types) | Planned (M2) |
| SQL overrides checked at startup, `refract check` | Planned (M3) |
| Ordered lists, maps, many-to-many, recursive views | Planned (M4) |
| Shared (`Arc`) and graph (`Ref<T>`) representations for cyclic data | Planned (M5) |
| GraphQL selection sets | Planned (M7) |

Loading 1,000 tasks with 10 subtasks each takes about 8% longer than hand-written SQLx code running the same two
queries (`crates/refract/examples/parity.rs`).

## Development

The tests need PostgreSQL. `scripts/with-postgres.sh` starts a throwaway cluster on port 55432, runs a command
against it, and removes it afterwards:

```sh
scripts/with-postgres.sh                                                   # cargo test --workspace
scripts/with-postgres.sh cargo run --release -p refract --example parity   # benchmark
```

Without a database (`REFRACT_TEST_DATABASE_URL` unset), the database tests are skipped.

Refract carries forward the ideas of [XOR](https://github.com/ddalton/xor), a Java library built around the same
"view as contract" concept, along with the lessons learned building it.

## License

[MIT](LICENSE)
