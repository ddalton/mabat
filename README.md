# Refract

Typed aggregate reads for Rust, with SQL you can tune without changing code.

> **Status:** design phase. Nothing is published yet. See the [design document](docs/design.md).

Refract loads nested, typed data from a relational database. The shape of the result is declared with ordinary
Rust types, and Refract plans and runs the queries that fill it.

- **Views as contracts.** A struct deriving `View` defines the shape of the data. The query behind it can be
  generated, or replaced in configuration with hand-tuned SQL written by a DBA. Every replacement is checked
  against the Rust type at startup and in CI.
- **Algebraic data types.** Enums with data map to the database through a tag column, a table per variant or
  JSON. Decoding is exhaustive and strict.
- **Cycles without `Rc`, `Weak` or `RefCell`.** Each view chooses a tree, a shared DAG (`Arc`), or an
  arena-backed graph with typed `Ref<T>` handles and generated navigation methods.
- **No N+1 queries.** Collections are loaded with batched child queries (`WHERE fk = ANY($1)`) and stitched
  together by key. Ordered lists are placed by their index column, whatever order the rows arrive in.
- **Async, on SQLx.** PostgreSQL first.

```rust
#[derive(View)]
#[view(table = "task")]
struct TaskView {
    id: Uuid,
    name: String,
    status: Status,
    #[view(child(fk = "parent_id"))]
    children: Vec<TaskSummary>,
}

#[derive(View)]
#[view(tag = "status_kind")]
enum Status {
    Open,
    Assigned { assignee: String },
    Blocked { reason: String },
}

let task: TaskView = refract.load::<TaskView>().by_key(id).one(&mut tx).await?;
```

Refract carries forward the ideas of [XOR](https://github.com/ddalton/xor), a Java library built around the same
"view as contract" concept, along with the lessons learned building it.

## License

[MIT](LICENSE)
