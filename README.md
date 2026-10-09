# Mabat

Typed aggregate reads for Rust, with SQL you can tune without changing code.

**Documentation: [ddalton.github.io/mabat](https://ddalton.github.io/mabat/)** — guides, the
[MPA specification](https://ddalton.github.io/mabat/spec/mpa/) and the
[architecture](https://ddalton.github.io/mabat/architecture/).

*Mabat* (מבט) is Hebrew for "view": you declare the view of the data you want, and the queries that fill it
can be tuned separately.

> **Status:** early development, not published yet. Loading is complete on PostgreSQL, MySQL and SQLite:
> struct views, enums with data, collections and recursive views, shared and cyclic graphs, overrides checked
> at startup, concurrent loads on a pool, JSON, selections and a generated GraphQL schema. Saving is complete
> (M8): `save`, `save_changes`, `delete`, optimistic locking, database-generated keys and saving graphs. The
> [status table](#status) has the details.

Mabat loads nested, typed data from PostgreSQL, MySQL or SQLite. The shape of the result is declared with
ordinary Rust structs, and Mabat plans and runs the queries that fill it: one query for the root rows, plus one
batched query (`WHERE fk = ANY($1)` on PostgreSQL, `WHERE fk IN (?, …)` on MySQL and SQLite) per collection and
per reference. It never runs one query per row.

```rust
use mabat::View;
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
let task = mabat::load::<TaskView>().by_key(id).one(&mut *conn).await?;

// Many, filtered, ordered and paged
use mabat::filter::col;
let page = mabat::load::<TaskView>()
    .filter(col("status").eq("open") & col("assignee_id").is_in([1_i64, 2, 3]))
    .order_by("name")
    .limit(20)
    .offset(40)
    .all(&mut *conn)
    .await?;
let total = mabat::load::<TaskView>().filter(col("status").eq("open")).count(&mut *conn).await?;

// The queries Mabat runs
println!("{}", mabat::plan::<TaskView>()?.explain());
```

## Collections and recursive views

```rust
#[derive(View)]
#[view(table = "playlist")]
struct PlaylistView {
    name: String,
    // many-to-many through a link table, placed by its index column whatever the row order
    #[view(child(through = "playlist_song", fk = "playlist_id", target = "song_id", index = "seq"))]
    songs: Vec<SongView>,
    // a map keyed by a column (BTreeMap or HashMap)
    #[view(child(fk = "playlist_id", key = "name"))]
    settings: BTreeMap<String, SettingView>,
}

#[derive(View)]
#[view(table = "category")]
struct CategoryTree {
    name: String,
    // a tree loaded level by level: one batched query per level, at most 5 levels
    #[view(child(fk = "parent_id", index = "position", depth = 5))]
    children: Vec<CategoryTree>,
}

#[derive(View)]
#[view(table = "category")]
struct CategoryCte {
    name: String,
    // a tree of any depth loaded with one WITH RECURSIVE query
    #[view(child(fk = "parent_id", order_by = "name", recursive = "cte"))]
    children: Vec<CategoryCte>,
}
```

Recursive views are owned trees, so they need no `Rc` or `RefCell`.

- **Overrides:** a recursive collection has one query name, such as `children`, so one override tunes every
  level.
- **Nested collections:** with `recursive = "cte"`, collections under the recursive view (each category's
  products, say) load with one query for all levels.
- **Cycles:** rows whose parents form a cycle can't be a tree, and loading them is an error rather than an
  endless loop.

## Shared values and graphs, without `Rc` or `RefCell`

A field of type `Arc<T>` is decoded once per entity and shared: every task with the same assignee holds the
same `Arc`.

Cycles, such as a manager and their reports or a team and its members, use `Ref<T>` fields. A `Ref<T>` is a
typed, `Copy` index into a `Graph`. The derive macro generates a method per reference to follow it:

```rust
#[derive(View)]
#[view(table = "employee")]
pub struct Employee {
    pub name: String,
    #[view(to_one(fk = "manager_id"))]
    pub manager: Option<Ref<Employee>>,
    #[view(child(fk = "manager_id", order_by = "name"))]
    pub reports: Vec<Ref<Employee>>,
    #[view(to_one(fk = "team_id"))]
    pub team: Ref<Team>,
}

let graph = mabat::load::<Employee>().by_key(id).graph(&mut *conn).await?;
let me = graph.root().unwrap();
for colleague in me.manager(&graph).unwrap().reports(&graph) {
    println!("{} in {}", colleague.name, colleague.team(&graph).name);
}
```

- **What a graph load fetches:** every entity reachable through the `Ref` fields. Each entity is fetched once,
  and each relationship is loaded once, with batched queries. Cycles end by themselves, with no `depth`.
- **Navigation:** borrows the `Graph`, so there are no runtime borrow checks. Changes go through
  `graph.get_mut(r)`.
- **Threads:** `Graph` is `Send + Sync`.

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
containing view. Mabat loads each variant table with one batched query, using only the keys whose tag
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

## Saving aggregates

A view also saves: `mabat::save` writes a value and everything it owns, in one transaction (a savepoint inside
yours), on any of the three databases:

```rust
mabat::save(&mut board, &mut tx).await?;             // the board, then its lists, cards and links made to match
mabat::delete::<Board, _>(board.id, &mut tx).await?; // the board and all it owns

// Load, change, save only what changed: updated columns, new and removed elements, nothing else
let before = mabat::load::<Board>().by_key(id).one(&mut tx).await?;
let mut after = before.clone();
after.name = "Roadmap 2".into();
mabat::save_changes(&before, &mut after, &mut tx).await?;  // UPDATE "board" SET "name" = $1 WHERE "id" = $2
```

- **Rows** are created or replaced by key: an `UPDATE`, and only for a new row an upsert (`ON CONFLICT … DO UPDATE`
  on PostgreSQL and SQLite, `ON DUPLICATE KEY UPDATE` on MySQL). A view of some of a table's columns saves them in
  a row that exists. Keys come from the application, or from the database.
- **Owned collections** are made equal to the value's: elements that are gone are deleted with what they own,
  the others are saved, with their position for ordered lists and their key for maps.
- **References and many-to-many links** write foreign keys and link rows only: the referenced values are
  aggregates of their own.
- **Enums** write their tag and their variant's columns, NULL to the other variants', or the variant's table row,
  deleting the other variants' rows.

`save_changes` compares the two values with `PartialEq`, field by field and element by element (matched by key), so
rows that did not change produce no statement. It also updates views of some of a table's columns, which a whole
`save` cannot insert.

A `#[view(version)]` integer field locks a row optimistically: it is updated only if it still has the value's
version, which it increments, and otherwise the write fails with `Error::Conflict`. New versions are written back
into the value, so it can be saved again.

Keys can come from the database: mark an `Option` integer key field `#[view(generated)]`, and `save` inserts a
value whose key is `None`, then writes back the key the database generated, into the value and into the new
elements of its collections:

```rust
#[derive(View)]
#[view(table = "project")]
struct Project {
    #[view(generated)]
    id: Option<i64>,         // an identity, serial or AUTO_INCREMENT column
    name: String,
    #[view(child(fk = "project_id"))]
    tasks: Vec<Task>,        // Task's own key can be generated too
}

let mut project = Project { id: None, name: "Alpha".into(), tasks: vec![] };
mabat::save(&mut project, &mut tx).await?;   // INSERT … RETURNING "id"
assert!(project.id.is_some());
```

A graph is saved whole with `mabat::save_graph`: every entity, each after the entities it references, so that
their keys exist; a cycle of optional references is written NULL first and set afterwards. New entities are added
with `Graph::insert`:

```rust
#[derive(View)]
#[view(table = "employee")]
struct Person {
    #[view(generated)]
    id: Option<i64>,
    name: String,
    #[view(to_one(fk = "manager_id"))]
    manager: Option<Ref<Person>>,
}

let mut graph = Graph::<Person>::new();
let ada = graph.insert(Person { id: None, name: "Ada".into(), manager: None });
let grace = graph.insert(Person { id: None, name: "Grace".into(), manager: Some(ada) });
graph.get_mut(ada).manager = Some(grace);          // a cycle: saved with NULL, then set
graph.add_root(ada);
mabat::save_graph(&mut graph, &mut tx).await?;     // keys generated and written back
```

Many values are saved at once with `mabat::save_all`, which writes them as loads read them: a few statements for
each table and level of the aggregate, not for each row. Saving 1,000 tasks with 10 subtasks each takes about
80 ms instead of 1.6 s with one `save` per task, on a local PostgreSQL; on a remote one the difference is
thousands of round trips.

```rust
mabat::save_all(&mut tasks, &mut tx).await?;   // one UPDATE and one INSERT per table, then the subtasks
```

Writes never use overrides.

## Arguments of nested collections

A collection can be filtered, ordered and paged for each parent, still with one query for all parents:

```rust
// Every task with its three most recent open subtasks
let tasks = mabat::load::<TaskView>()
    .nested("children", Nested::new().filter(col("done").eq(false)).order_by_desc("created_at").limit(3))
    .all(&mut conn)
    .await?;
```

Paging per parent uses `ROW_NUMBER() OVER (PARTITION BY …)`, on PostgreSQL, MySQL 8 and SQLite alike. With an
override of the collection's query, the arguments apply to it as a subquery.

## JSON and selections

Any view also loads as JSON, whole or a selection of its fields. A selection is written like a GraphQL
selection set, and only its columns are selected and only its child queries run:

```rust
let selection = Selection::parse("name assignee { name } children { name }")?;
let tasks: Vec<serde_json::Value> = mabat::load::<TaskView>().select(selection).json(&mut conn).await?;
// [{ "name": "Release", "assignee": { "name": "Ada" }, "children": [{ "name": "Write docs" }] }]
```

Columns are written with the `Serialize` implementation of their Rust type, enums as objects whose `__typename`
names the variant. A collection or reference selected by name alone loads the columns of its view. A selection
has a finite depth, so recursive views and graph views load as trees as deep as it asks. Overrides apply as for
typed loads.

## GraphQL

The `mabat-graphql` crate generates an [async-graphql](https://crates.io/crates/async-graphql) schema from the
views. Each root field is one load of the fields the query selects:

```rust
let schema = mabat_graphql::schema(&pool)
    .list::<TaskView>("tasks")   // tasks(where: TaskViewWhere, orderBy: [TaskViewOrderBy!], limit: Int, offset: Int)
    .by_key::<TaskView>("task")  // task(key: UUID!): TaskView
    .finish()?;
```

```graphql
{
  tasks(where: { name: { ilike: "%release%" } }, orderBy: [{ name: ASC }], limit: 10) {
    name
    assignee { name }          # one batched query for the assignees, none for the other collections
  }
}
```

Views and embedded structs become object types, enums with data become unions (`... on StateBlocked { reason }`),
enums without data become GraphQL enums, and maps become lists of `{ key, value }` entries. A `where` argument
filters the columns of the view with `eq`, `ne`, `lt`, `le`, `gt`, `ge`, `in`, `notIn`, `isNull`, `like` and
`ilike`, combined with `and`, `or` and `not`. Nested lists take the same arguments for the elements of each
parent, such as `tasks { subtasks(orderBy: [{ position: ASC }], limit: 3) { name } }`. Overrides of a registry apply with `.registry(..)`, and
`.connections(n)` runs the queries of a level concurrently. `cargo run -p mabat-graphql --example chinook` serves
the Chinook music store with GraphiQL.

## Streaming many values

`stream` loads values a batch at a time, so exporting a million rows holds one batch in memory, not a million.
It reads the keys of every match, with the filter, order and page, then loads each batch of keys with its
collections and references, and yields the values in order. On PostgreSQL the keys stay on the server, in a
cursor, and are fetched a batch at a time:

```rust
use futures_util::TryStreamExt;

let mut tasks = mabat::load::<TaskView>().order_by("id").batch_size(500).stream(&mut conn);
while let Some(task) = tasks.try_next().await? {
    export(&task)?;
}
```

Outside a transaction, each batch sees what is committed when it runs, so a value deleted after the keys were
read is skipped; in a `REPEATABLE READ` transaction or with `Pooled::snapshot`, every batch sees one snapshot.
`json_stream` streams JSON objects, of a selection or of every field. Graph views (`Ref<T>`) cannot be streamed.

## Concurrent loads on a pool

A load runs its root query, then the queries of each level: the collections and references of the rows above.
On one connection they run one after the other. Given a pool, the queries of a level run at the same time, each
on a connection of its own:

```rust
// PostgreSQL: every query sees one snapshot, exported by the first connection and imported by the others
let mut pooled = mabat::Pooled::snapshot(&pool, 4);
let boards = mabat::load::<BoardView>().all(&mut pooled).await?;

// Any database: each query sees what is committed when it runs
let mut pooled = mabat::Pooled::read_committed(&pool, 4);
```

Loading 20 boards whose lists and labels each take half a second takes one second on one connection and half a
second on a pool. Only PostgreSQL can share a snapshot between connections, so `snapshot` takes a PostgreSQL
pool. With `read_committed`, a row committed during the load can show up in a collection whose parent was read
before it. Graph loads run their queries one at a time on one connection of the pool.

## Tuning without code changes

Any query of a view can be replaced with SQL from an override file. A DBA can change joins, ordering, hints, or
the tables themselves, and can read from a materialized view, without touching the Rust code. The rows are
decoded by column alias, so the override only has to keep the aliases:

```toml
# mabat/overrides/TaskView.toml
[query."children.notes"]
sql = '''
SELECT n.task_id AS "$parent", n.id AS "$key", n.body AS "body", n.tag_code AS "$ref.tag"
FROM task_note n
WHERE n.task_id = ANY($1)
ORDER BY n.id
'''
```

```rust
let mabat = Mabat::builder()
    .register::<TaskView>()
    .overrides_dir("mabat/overrides")
    .build(&mut conn)                       // checks every query against the database
    .await?;

let task = mabat.load::<TaskView>().by_key(id).one(&mut *tx).await?;
```

At startup, every query, generated or overridden, is prepared on the database without being run. Its columns
and parameters are then compared with the view, so a broken override or a schema that drifted from the view
fails before any request is served:

```text
error[M0102]: override for TaskView.children.notes does not match the view
  --> mabat/overrides/TaskView.toml:2
   | column 3 "body" has type INT4, expected TEXT for String
   | column 4 "$ref.tga" is not a path of NoteView in this query (did you mean "$ref.tag"?)
```

- `Mabat::builder()...check(&mut conn)` returns the same report without building, for a test in CI.
- `mabat::scaffold::<TaskView, sqlx::Postgres>()` returns an override file with the generated SQL of every
  query, as a starting point for tuning.
- `shadow = true` runs the override and the generated query, compares their rows, and counts mismatches and
  timings, so a tuned query can be shown to be equivalent before it is relied on.
- `OnInvalid::UseGenerated` starts with the generated queries in place of invalid overrides instead of
  refusing to start.
- `mabat.reload(&mut conn)` reads the files again and, if they changed and pass the checks, puts them in use
  atomically. An invalid change never replaces a working query.

### Checking overrides without the application

A DBA doesn't need Rust to check an override. The application writes a manifest of its views: their queries,
aliases and accepted column types. A test can write it and fail when the committed copy is out of date:

```rust
#[test]
fn views_manifest_is_up_to_date() {
    let manifest = Mabat::builder().register::<TaskView>().manifest().unwrap();
    assert!(!manifest.write("mabat/views.json").unwrap(), "mabat/views.json was out of date");
}
```

The `mabat` command line tool (crate `mabat-cli`) reads the manifest:

```sh
# check every query, generated and overridden, against a database
mabat check --manifest mabat/views.json --overrides mabat/overrides --database-url postgres://...

# or against a schema file, in any scratch database: created in a transaction that is rolled back
mabat check --manifest mabat/views.json --overrides mabat/overrides --schema schema.sql

mabat explain  --manifest mabat/views.json --overrides mabat/overrides   # the SQL each query runs
mabat scaffold --manifest mabat/views.json --view TaskView --format sql   # every query's generated SQL
```

To tune a query, start from the SQL Mabat generates for it. `explain` lists the queries of a view by name;
`scaffold --query` writes the generated SQL of the ones you name into the view's override file, ready to edit:
change the joins, add hints, use another index. Every query in the file replaces the generated one, so scaffold
only the queries you are tuning, and the others keep following the view as it changes.

```sh
mabat scaffold --manifest mabat/views.json --view TaskView --format sql \
    --query children.notes --out mabat/overrides
# wrote mabat/overrides/TaskView.sql; run it again with another --query to add that query to the file
```

An existing file is added to, never overwritten: scaffolding a query the file already overrides is an error.

A snapshot of the database's schema, committed next to the manifest, lets views be checked against the schema
without a database. `mabat schema` writes it, `mabat schema --check` tells CI when the database has moved
on, and `mabat check --snapshot` checks the views against it:

```sh
mabat schema --database-url postgres://... --out mabat/schema.json          # tables, columns, keys, as JSON
mabat schema --check mabat/schema.json --database-url postgres://...        # exit status 1 if they differ
mabat check --manifest mabat/views.json --snapshot mabat/schema.json        # no database needed
```

Against a snapshot, `check` finds the tables and columns the views read that are missing (M0201, M0202), columns
of types the fields cannot be decoded from (M0203), nullable columns under fields that are not `Option` (M0204, a
warning), foreign keys and link tables holding another kind of key than the key they match (M0205), keys that are
not their table's primary key (M0206, a warning), and `#[view(generated)]` keys the database does not generate
(M0207). Override files are checked for their names only: their SQL needs a database to prepare it.

```text
error[M0202]: TaskView.$root: column `task.due` is not in the schema
  --> mabat/schema.json
   | did you mean `due_on`?
```

In a build script, the same check makes `cargo build` fail when the views no longer match the schema. The
`mabat-check` crate needs no database driver:

```toml
[build-dependencies]
mabat-check = "0.1"
```

```rust
// build.rs
fn main() {
    mabat_check::build("mabat/views.json", "mabat/schema.json").overrides("mabat/overrides").run();
}
```

```text
error: app@0.1.0: mabat error[M0202]: ProjectView.tasks: column `task.title` is not in the schema; did you mean `titel`? (mabat/schema.json)
error: build script logged errors
```

The build script checks the committed files: the manifest test keeps `mabat/views.json` current, and
`mabat schema --check` in CI keeps `mabat/schema.json` current. Warnings show as Cargo warnings. Before the
manifest exists, or with `MABAT_SKIP_CHECK=1`, the check is skipped with a warning.
`crates/mabat-example-build` is a complete example.

`check` prints the same report as the application's startup check. Its exit status is 0 when there are no
errors, 1 when there are, and 2 for any other problem, so it can run in a DBA's CI.

Override files can also be plain SQL, which SQL editors and `psql` understand, with a marker line before each
query:

```sql
-- mabat/overrides/TaskView.sql
-- mabat: query children.notes, shadow
SELECT n.task_id AS "$parent", n.id AS "$key", n.body AS "body", n.tag_code AS "$ref.tag"
FROM task_note n
WHERE n.task_id = ANY($1)
ORDER BY n.id;
```

## Specification

[MPA, the Mabat Persistence Architecture](docs/mpa.md), is Mabat's contract, the way JPA is Java persistence's:
every attribute, loading mode, override rule and write semantic as a numbered rule, such as `MPA-WRITE-9` for
optimistic locking with `#[view(version)]`, and what Mabat does not do. [`docs/mpa.json`](docs/mpa.json) indexes it
for tools, and [`llms.txt`](llms.txt) points AI assistants to both.

## Status

| Feature | Status |
| --- | --- |
| Struct views, embedded structs, `Option` columns | Done (M1) |
| To-many children and to-one references, nested, batched | Done (M1) |
| Decoding by column alias, errors that name the view and path | Done (M1) |
| Enums with data: `tag` and `table_per_variant` strategies, nested enums, JSON fields | Done (M2) |
| Filters on the root query, counting | Done |
| SQL overrides checked at startup, shadow mode, scaffolding, reloading | Done (M3) |
| Ordered lists, maps, many-to-many, recursive views | Done (M4) |
| Shared (`Arc`) and graph (`Ref<T>`) representations for cyclic data | Done (M5) |
| DBA tooling: view manifest and `mabat check` / `explain` / `scaffold` CLI | Done |
| SQLite and MySQL | Done (M6) |
| Concurrent loads on a pool, with a shared snapshot on PostgreSQL | Done (M6) |
| JSON loads and selections of fields | Done (M7) |
| GraphQL schema generated from views (`mabat-graphql`) | Done (M7) |
| Arguments of nested collections: filters, order and paging per parent, also in GraphQL | Done |
| Saving and deleting aggregates, saving changes only, optimistic locking | Done (M8) |
| Keys generated by the database | Done (M8) |
| Saving graphs | Done (M8) |
| Saving many values at once, batched by table (`save_all`) | Done (M8) |
| Checking views against a schema snapshot, from the CLI or a build script | Done |
| Streaming loads a batch at a time (`stream`, `json_stream`) | Done |

Loading 1,000 tasks with 10 subtasks each takes about 8% longer than hand-written SQLx code running the same two
queries (`crates/mabat/examples/parity.rs`).

## Installation

```toml
[dependencies]
mabat = "0.1"
sqlx = { version = "0.9", default-features = false, features = ["postgres", "runtime-tokio"] }
```

For MySQL or SQLite, enable its feature, with or without `postgres`:

```toml
mabat = { version = "0.1", default-features = false, features = ["mysql"] }
sqlx = { version = "0.9", default-features = false, features = ["mysql", "runtime-tokio"] }
```

A view is decoded on each enabled database, and a load runs on the database of the connection it is given.
With several enabled, `#[view(databases = "postgres, mysql")]` limits a view whose field types not every
database decodes, such as a PostgreSQL array or `Decimal` on SQLite. In override SQL, `:keys` stands for the keys
of a batched query on any database: `= ANY(:keys)` on PostgreSQL, `IN (:keys)` on MySQL and SQLite.

The derive macro also works when the crate is renamed in `Cargo.toml`, for example
`views = { package = "mabat", version = "0.1" }`.

## Requirements

- **Rust:** 1.94 or later.
- **Database:** PostgreSQL, MySQL 8 or later, or SQLite, through SQLx 0.9 with the Tokio runtime.
- **Changes:** see [CHANGELOG.md](CHANGELOG.md) for what each version adds.

## Development

The tests need PostgreSQL and MySQL. `scripts/with-postgres.sh` starts a throwaway PostgreSQL cluster on port
55432, and `scripts/with-mysql.sh` a throwaway MySQL server on port 53306. Each runs a command against it and
removes it afterwards, and they nest:

```sh
scripts/with-mysql.sh scripts/with-postgres.sh                             # cargo test --workspace
scripts/with-postgres.sh cargo run --release -p mabat --example parity   # benchmark
```

Without a server (`MABAT_TEST_DATABASE_URL` or `MABAT_TEST_MYSQL_URL` unset), its tests are skipped. The SQLite
tests need no server and always run, in in-memory databases.

### End-to-end tests

The `mabat-e2e` crate tests Mabat against two well-known sample databases, both MIT licensed, and compares
every load with an independent answer computed in SQL:

- **[Pagila](https://github.com/devrimgunduz/pagila)** (a DVD rental store) covers:
  - a PostgreSQL enum and a domain
  - `text[]` arrays and `numeric` money
  - payments in a partitioned table
  - many-to-many links between films and actors, loaded as a graph of 200 actors and 997 films
  - a reference cycle between stores and their staff
  - overrides that derive an enum from legacy columns and call a stored function
- **[Chinook](https://github.com/lerocha/chinook-database)** (a music store) covers:
  - invoices whose totals must equal the sum of their lines exactly
  - the recursive employee hierarchy, loaded as trees in both modes
  - employees, customers and invoices as a graph
  - albums in maps and playlists of tracks
  - all of the above on MySQL and SQLite too, with a playlist of 3,290 tracks that needs several statements of
    keys

Each dataset is loaded once per database into a schema named after a hash of its SQL (`mabat_e2e_pagila_…`,
`mabat_e2e_chinook_…`), and the tests only read it. `scripts/with-postgres.sh` starts with an empty database
each time. With a database of your own, drop these schemas when you no longer need them.

Mabat carries forward the ideas of [XOR](https://github.com/ddalton/xor), a Java library built around the same
"view as contract" concept, along with the lessons learned building it.

## License

[MIT](LICENSE)
