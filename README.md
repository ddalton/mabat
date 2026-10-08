# Refract

Typed aggregate reads for Rust, with SQL you can tune without changing code.

> **Status:** early development, not published yet. Milestones 1 (struct views, PostgreSQL, reads), 2 (enums
> with data) and 3 (overrides checked at startup) are implemented. See the [design document](docs/design.md) for the
> plan.

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

## Enums with data

Enums map to tables in one of two ways. With the `tag` strategy (the default), a tag column names the variant
and the variants' fields are columns of the same row:

```rust
#[derive(View)]
#[view(tag = "state")]                       // any column type, e.g. a PostgreSQL enum
enum State {
    #[view(tag_value = "open")]
    Open,
    #[view(tag_value = "assigned")]
    Assigned { assignee: String },
    #[view(tag_value = "blocked")]
    Blocked {
        #[view(embed(prefix = "blocked_reason_"))]
        reason: Reason,                      // enums nest
        #[view(column = "blocked_since")]
        since: Option<DateTime<Utc>>,
    },
    #[view(tag_value = "closed")]
    Closed(#[view(column = "closed_resolution")] String),
}
```

With `strategy = "table_per_variant"`, each variant's data is in its own table, keyed by the key of the
containing view. Refract loads each variant table with one batched query, using only the keys whose tag
names that variant:

```rust
#[derive(View)]
#[view(tag = "kind", strategy = "table_per_variant")]
enum Payment {
    #[view(tag_value = "none")]
    Unpaid,
    #[view(tag_value = "card", table = "card_payment", key = "issue_id")]
    Card { last4: String, #[view(to_one(fk = "holder_id"))] holder: Option<PersonView> },
    #[view(tag_value = "bank", table = "bank_payment", key = "issue_id")]
    Bank { iban: String, #[view(child(fk = "bank_payment_id"))] notes: Vec<PaymentNote> },
}

#[derive(View)]
#[view(table = "issue")]
struct IssueView {
    id: i64,
    #[view(embed)]
    state: State,
    #[view(embed(prefix = "payment_"))]
    payment: Payment,
    #[view(json)]
    metadata: Option<Metadata>,              // any serde type, from a JSON or JSONB column
}
```

Decoding is strict. An unknown tag is an error, and so is a missing variant row. A column of another variant
that isn't NULL is also an error, unless the enum is marked `lenient`. Variant fields have paths that name
the variant, such as `state.Blocked.reason.$tag` or `state.Closed.0`, and overrides use them like any other
path.

## Tuning without code changes

Any query of a view can be replaced with SQL from an override file. A DBA can change joins, ordering, hints, or
the tables themselves, and can read from a materialized view, without touching the Rust code. The rows are
decoded by column alias, so the override only has to keep the aliases:

```toml
# refract/overrides/TaskView.toml
[query."children.notes"]
sql = '''
SELECT n.task_id AS "$parent", n.id AS "$key", n.body AS "body", n.tag_code AS "$ref.tag"
FROM task_note n
WHERE n.task_id = ANY($1)
ORDER BY n.id
'''
```

```rust
let refract = Refract::builder()
    .register::<TaskView>()
    .overrides_dir("refract/overrides")
    .build(&mut conn)                       // checks every query against the database
    .await?;

let task = refract.load::<TaskView>().by_key(id).one(&mut *tx).await?;
```

At startup, every query, generated or overridden, is prepared on the database without being run. Its columns
and parameters are then compared with the view, so a broken override or a schema that drifted from the view
fails before any request is served:

```text
error[R0102]: override for TaskView.children.notes does not match the view
  --> refract/overrides/TaskView.toml:2
   | column 3 "body" has type INT4, expected TEXT for String
   | column 4 "$ref.tga" is not a path of NoteView in this query (did you mean "$ref.tag"?)
```

- `Refract::builder()...check(&mut conn)` returns the same report without building, for a test in CI.
- `refract::scaffold::<TaskView>()` writes an override file with the generated SQL of every query, as a
  starting point for tuning.
- `shadow = true` runs the override and the generated query, compares their rows, and counts mismatches and
  timings, so a tuned query can be shown to be equivalent before it is relied on.
- `OnInvalid::UseGenerated` starts with the generated queries in place of invalid overrides instead of
  refusing to start.
- `refract.reload(&mut conn)` reads the files again and, if they changed and pass the checks, puts them in use
  atomically. An invalid change never replaces a working query.

Override files can also be plain SQL, which SQL editors and `psql` understand, with a marker line before each
query:

```sql
-- refract/overrides/TaskView.sql
-- refract: query children.notes, shadow
SELECT n.task_id AS "$parent", n.id AS "$key", n.body AS "body", n.tag_code AS "$ref.tag"
FROM task_note n
WHERE n.task_id = ANY($1)
ORDER BY n.id;
```

## Status

| Feature | Status |
| --- | --- |
| Struct views, embedded structs, `Option` columns | Done (M1) |
| To-many children and to-one references, nested, batched | Done (M1) |
| Decoding by column alias, errors that name the view and path | Done (M1) |
| Enums with data: `tag` and `table_per_variant` strategies, nested enums, JSON fields | Done (M2) |
| Filters on the root query | Planned |
| SQL overrides checked at startup, shadow mode, scaffolding, reloading | Done (M3) |
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
