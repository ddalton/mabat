# MPA: the Mabat Persistence Architecture

**Version 0.1**, describing the `mabat` crates 0.1. Machine-readable index: [`mpa.json`](mpa.json).

MPA is the contract of Mabat, the way JPA is the contract of Java persistence providers. It says what a view
declares, how it is loaded, overridden, served as JSON or GraphQL, and written, and what Mabat does not do. It
is written for people and for tools: each rule has a stable identifier, such as `MPA-WRITE-9`, that code reviews,
issues and AI assistants can cite.

## Contents

0. [Conventions](#0-conventions)
1. [Concepts](#1-concepts)
2. [Databases](#2-databases)
3. [Declaring views](#3-declaring-views)
4. [Enums with data](#4-enums-with-data)
5. [Loading](#5-loading)
6. [Query planning](#6-query-planning)
7. [Overrides](#7-overrides)
8. [JSON and selections](#8-json-and-selections)
9. [GraphQL](#9-graphql)
10. [Writing](#10-writing)
11. [Errors and diagnostics](#11-errors-and-diagnostics)
12. [Not supported](#12-not-supported)
13. [Schema snapshots](#13-schema-snapshots)

## 0. Conventions

- **MPA-DOC-1** The words MUST, MUST NOT, SHOULD and MAY are used as in RFC 2119. A rule describes what Mabat
  does; a rule that says the application MUST do something states a precondition, whose violation is reported as
  described.
- **MPA-DOC-2** Rule identifiers are `MPA-<AREA>-<n>`. They are never reused: a removed rule keeps its number,
  marked removed.
- **MPA-DOC-3** Code is Rust 2024 with the `mabat` facade crate. `conn` is a connection, a transaction or a
  `Pooled` pool (MPA-DB-5). Errors are `mabat::Error` (section 11).
- **MPA-DOC-4** [`mpa.json`](mpa.json) lists every capability, attribute, function, error and diagnostic with the
  rules that define it. A test (`crates/mabat/tests/mpa.rs`) fails when the derive accepts an attribute, or the
  crate has an error variant, that the index does not list.

## 1. Concepts

- **MPA-CORE-1** A **view** is a Rust struct deriving `mabat::View` that declares the shape of the data to load
  from one table: its columns, embedded values, to-one references and to-many collections. The same table MAY
  have many views.
- **MPA-CORE-2** A view's values form an **aggregate**: its row, its embedded values, the rows of its owned
  collections recursively, and the rows of its variant tables. Referenced views (to-one references, many-to-many
  elements) are other aggregates.
- **MPA-CORE-3** `#[derive(View)]` generates a static **shape** (`mabat::shape`), from which Mabat plans the
  queries, and, for each enabled database, a decoder and an encoder. Nothing is computed by reflection at run
  time.
- **MPA-CORE-4** The **path** of a field is its name, joined with `.` below collections, references, embedded
  values and variants, such as `children.notes` or `state.Blocked.reason`. Tuple fields are named `0`, `1`, …
- **MPA-CORE-5** A view is loaded by a tree of queries, one per collection and reference (section 6), each named
  by its path, the root query being `$root`. Overrides (section 7) and nested arguments (MPA-LOAD-9) address
  queries by these names.

## 2. Databases

- **MPA-DB-1** The features `postgres` (default), `mysql` and `sqlite` enable PostgreSQL, MySQL 8 or later, and
  SQLite, through SQLx 0.9 and the Tokio runtime. Any combination MAY be enabled.
- **MPA-DB-2** A view is decoded and encoded on each enabled database. `#[view(databases = "postgres, mysql")]`
  limits it to the listed databases, for views whose field types not every enabled database supports, such as a
  PostgreSQL array or `rust_decimal::Decimal` on SQLite.
- **MPA-DB-3** A load runs on the database of the connection it is given. A registry (section 7) is built for one
  database; using it on another fails with `Error::WrongBackend`.
- **MPA-DB-4** Keys of a batched query are bound as one array on PostgreSQL (`= ANY($1)`) and as one parameter
  per key on MySQL and SQLite (`IN (?, …)`), padded to a power of two so statements are reused, and split into
  statements of at most 1,000 keys for child queries. The root query is never split.
- **MPA-DB-5** Wherever a load or write takes a connection, it accepts a connection, a `sqlx::Transaction`, a
  pooled connection, or a `mabat::Pooled` pool (MPA-LOAD-11). Every query of a load runs on the connection
  given, so it sees the uncommitted writes of its transaction.

## 3. Declaring views

### 3.1 Container attributes

| Attribute | On | Rule |
| --- | --- | --- |
| `#[view(table = "t")]` | struct | MPA-VIEW-1 |
| `#[view(key = "c")]` | struct | MPA-VIEW-2 |
| `#[view(embedded)]` | struct | MPA-VIEW-3 |
| `#[view(tag = "c")]`, `strategy`, `lenient` | enum | section 4 |
| `#[view(databases = "…")]` | any | MPA-DB-2 |

- **MPA-VIEW-1** A view struct MUST have named fields and `#[view(table = "…")]`, the table it is loaded from.
- **MPA-VIEW-2** `key` names the key column, `id` by default. Key fields of type `i16`, `i32`, `i64`, `String` and
  `Uuid` are supported; on MySQL also unsigned integers; on SQLite columns without a declared type hold keys
  read by value. The key MAY be generated by the database (MPA-WRITE-13).
- **MPA-VIEW-3** `#[view(embedded)]` declares a struct stored in columns of the table of the view that contains
  it. It MUST NOT contain collections or references.
- **MPA-VIEW-4** A view MUST NOT have generic parameters.

### 3.2 Field attributes

| Attribute | Field | Rule |
| --- | --- | --- |
| none, or `#[view(column = "c")]` | column | MPA-VIEW-5 |
| `#[view(json)]` | JSON column | MPA-VIEW-6 |
| `#[view(version)]` | version column | MPA-WRITE-9 |
| `#[view(generated)]` | key column | MPA-WRITE-13 |
| `#[view(computed)]` | a value SQL computes | MPA-VIEW-13 |
| `#[view(embed)]`, `#[view(embed(prefix = "p_"))]` | embedded struct or enum | MPA-VIEW-7 |
| `#[view(to_one(fk = "c"))]` | to-one reference | MPA-VIEW-8 |
| `#[view(child(fk = "c", …))]` | to-many collection | MPA-VIEW-9 |

- **MPA-VIEW-5** A field without a relationship attribute is a column of the view's table, named like the field
  unless `column` names it. Its type MUST be decodable by SQLx. `Option<T>` is nullable: a NULL column decodes to
  `None`, and a non-`Option` field fails on NULL with `Error::Decode`.
- **MPA-VIEW-6** A `json` field is decoded from a JSON or JSONB column with `serde::Deserialize`, and written with
  `serde::Serialize`.
- **MPA-VIEW-7** An `embed` field holds an embedded struct (MPA-VIEW-3) or an enum (section 4), whose columns are
  the view's columns with the optional prefix. Embedded values nest; prefixes concatenate.
- **MPA-VIEW-8** A `to_one` field references another view: `fk` is the column of this view's table holding the
  referenced key. `Option<T>` makes it optional (a NULL foreign key is `None`); a non-`Option` reference whose row
  is missing fails with `Error::MissingReference`. The field MAY be `T`, `Box<T>`, `Arc<T>` (MPA-LOAD-13) or
  `Ref<T>` (MPA-LOAD-14). A reference back to its own view, such as a parent, takes `depth = n` (n ≥ 1) or
  `recursive = "cte"` (MPA-PLAN-4), and MUST be an `Option` of a `Box<T>` or `Arc<T>`; `recursive = "cte"` with
  a `depth`, recursion on a `Ref<T>`, and a `Box` of anything but an owned view are compile errors.
- **MPA-VIEW-9** A `child` field is a collection of another view, loaded by a child query whose rows have `fk`
  equal to this row's key. Its arguments:
  - `order_by = "a, b desc"`: the order of the elements; the key is always the last tie-breaker.
  - `through = "link"`, `target = "c"`: a many-to-many collection through a link table, whose `fk` references
    this view and whose `target` references the element. Both MUST be given together.
  - `index = "c"`: a `Vec` whose elements are placed by an integer column (of the link table with `through`),
    whatever the order of the rows. Indexes MUST be non-NULL and distinct, else `Error::ListIndex`; gaps are
    allowed.
  - `key = "c"`: a `BTreeMap` or `HashMap` keyed by a column, whose values MUST be owned views. Two elements with
    the same key fail with `Error::DuplicateMapKey`.
  - `depth = n` (n ≥ 1) or `recursive = "cte"`: a recursive collection (MPA-PLAN-4).
- **MPA-VIEW-10** A collection field MUST be a `Vec<T>`, `Vec<Arc<T>>`, `Vec<Ref<T>>`, `BTreeMap<K, T>` or
  `HashMap<K, T>` of a view; `Vec<Box<T>>` is a compile error.
- **MPA-VIEW-11** Invalid attributes are compile errors with a message naming the attribute, such as a `child`
  without `fk`, `index` on a map, or `version` on a collection.

### 3.3 Column types

- **MPA-VIEW-12** The derive records the Rust type of each column (`shape::ValueType`): nullable for `Option`, a
  list for `Vec` (except `Vec<u8>`), and a scalar named after the type: `Boolean`, `Int` (up to 32 bits), `BigInt`,
  `Float`, `String`, `Uuid`, `Date`, `Time`, `DateTime`, `NaiveDateTime`, `Decimal`, `Json`, `Bytes`, or the name
  of any other type. GraphQL types and filters (section 9) come from it.
- **MPA-VIEW-13** A `computed` field is a value that SQL computes, such as `count(*) AS "invoices"`: not a column of
  the view's table. The generated query does not select it; SQL that selects its alias (the field's name) fills
  it: the load's own root SQL (MPA-LOAD-19) or an override, whose checks require it unless it is an `Option`
  (MPA-OVR-3). Loading a view whose computed field is not an `Option` with a generated query fails with
  `Error::Params`; an `Option` is then `None`. A computed field is never written, MAY be ordered and filtered by
  over root SQL, and is a scalar in GraphQL. It belongs to a view's own fields, not to embedded structs or variants,
  and takes no `column`, `json`, `version` or `generated` (compile errors).

## 4. Enums with data

- **MPA-SUM-1** An enum deriving `View` with `#[view(tag = "c")]` is stored with a tag column whose value names
  the variant, `#[view(tag_value = "…")]` on each variant (the variant's name by default). The tag MAY be of any
  column type, such as a PostgreSQL or MySQL enum; it is read as text.
- **MPA-SUM-2** With `strategy = "tag"` (the default), the fields of the variants are columns of the containing
  view's row. Tuple fields MUST name their column.
- **MPA-SUM-3** With `strategy = "table_per_variant"`, a variant with data is stored in its own table,
  `#[view(table = "…", key = "…")]` on the variant, keyed by the containing view's key. Each variant table is
  loaded by one query with only the keys whose tag names that variant. Variant tables MAY have collections and
  references.
- **MPA-SUM-4** Decoding is strict: a NULL tag fails with `Error::NullTag`, an unknown tag with
  `Error::UnknownTag`, a missing variant row with `Error::MissingVariant`, and, unless the enum is `lenient`, a
  non-NULL column of another variant with `Error::OtherVariantColumn`.

## 5. Loading

```rust
let tasks = mabat::load::<TaskView>()
    .filter(col("parent_id").is_null() & col("name").ilike("%release%"))
    .order_by("name")
    .limit(20)
    .all(&mut conn)
    .await?;
```

- **MPA-LOAD-1** `mabat::load::<T>()` runs the generated queries; `Mabat::load::<T>()` runs them with the
  registry's overrides (section 7).
- **MPA-LOAD-2** Terminals: `all` (every match), `one` (exactly one, else `Error::NotFound` or
  `Error::TooManyRows`), `optional` (at most one), `count` (`SELECT count(*)` of the root query, ignoring order and
  paging), `graph` (MPA-LOAD-14) and `json` (section 8).
- **MPA-LOAD-3** `by_key` and `by_keys` restrict the root rows to keys. Keys of different types in one load fail
  with `Error::MixedKeys`; an empty `by_keys` loads nothing without a query.
- **MPA-LOAD-4** `filter(condition)` restricts the root rows; several calls all apply. Conditions are built with
  `mabat::filter::col("c")`: `eq`, `ne`, `lt`, `le`, `gt`, `ge`, `is_null`, `is_not_null`, `is_in`, `not_in`,
  `like`, `ilike`, combined with `&` (`and`), `|` (`or`), `!`, `Condition::all` and `Condition::any`. Every value is
  a bound parameter.
- **MPA-LOAD-5** `ilike` is `ILIKE` on PostgreSQL and `LOWER(c) LIKE LOWER(?)` elsewhere. `is_in` with an empty
  list matches nothing, `not_in` with an empty list everything; a NULL column matches neither.
- **MPA-LOAD-6** `order_by`, `order_by_desc`, `limit` and `offset` order and page the root rows.
- **MPA-LOAD-7** Filter and order columns are columns of the view's table. With an override of the root query,
  they MUST be selected by the view, else `Error::ColumnNotSelected`.
- **MPA-LOAD-8** Children are decoded in the order of their query: `order_by`, then the key.
- **MPA-LOAD-9** `nested(path, Nested)` filters, orders and pages the elements of the to-many collection whose
  query is named `path`, for each parent, in its one query: the filter follows the parent keys condition,
  `order_by` replaces the collection's order, and `limit`/`offset` apply per parent with `ROW_NUMBER() OVER
  (PARTITION BY …)`. An unknown path, a to-one reference, or a `recursive = "cte"` collection fails with
  `Error::NestedArguments`. The levels of a depth-limited recursive collection share their query name, so they
  share its arguments.
- **MPA-LOAD-10** With an override of a collection's query, nested arguments wrap it as a subquery and refer to
  columns by the aliases the view selects (MPA-LOAD-7).
- **MPA-LOAD-11** With a `Pooled` pool, the child queries of each level run at the same time, each on a pooled
  connection held for that query only, so a load uses at most the `connections` of its `Pooled`. `Pooled::snapshot`
  (PostgreSQL only; other databases do not compile) shares one `REPEATABLE READ READ ONLY` snapshot between the
  connections, imported when the load starts; `Pooled::read_committed` (any database) lets each query see what is
  committed when it runs. Graph loads run their queries one at a time.
- **MPA-LOAD-12** Recursive collections and references load as many levels as their `depth`, or all levels with
  `recursive = "cte"` (MPA-PLAN-4). A cycle in the data of a `cte` collection or chain of references fails with
  `Error::Cycle`; with `depth`, a cycle repeats its rows until the last level.
- **MPA-LOAD-13** An `Arc<T>` reference or element is decoded once per entity of a load and shared by everything
  that references it. With a recursive `depth`, an entity reached at several levels holds the references of the
  level it was first decoded at.
- **MPA-LOAD-14** A view with `Ref<T>` fields is a graph and MUST be loaded with `graph`, else
  `Error::GraphRequired`. `graph` returns a `Graph<T>` holding each entity once, with typed references and
  generated navigation methods (`task.manager(&graph)`); cycles end by themselves and need no `depth`. A graph is
  also built in code: `Graph::new()` makes an empty one, `insert` adds an entity and returns its `Ref`,
  `add_root` makes an entity a root, and `get_mut` changes one; `save_graph` saves it (MPA-WRITE-14), and
  `save_graph_changes` saves the entities that changed (MPA-WRITE-20).
- **MPA-LOAD-15** `stream(conn)` loads the matching values a batch at a time, as a `futures::Stream` of
  `Result<T, Error>`. One query first reads the keys of every matching root, with the keys, filter, order, limit
  and offset of the load, selecting only the key. Then each batch of `batch_size(n)` keys (1,000 by default, at
  least 1) is loaded as `by_keys` loads keys: the root query, with the filter, and every child query, for that
  batch. The values of a batch are yielded in the order of the keys, so the stream yields what `all` returns, in
  the same order, and holds at most one batch of values in memory. On a PostgreSQL connection (not a `Pooled`
  pool), the keys query is declared as a cursor `WITH HOLD`, `mabat_stream_keys`, and each batch's keys are
  fetched from it, so the server holds the keys and the stream holds one batch of them; the keys are the same as
  if read at once, as of when the cursor is declared. Otherwise the list of keys is read at once and held
  throughout.
- **MPA-LOAD-16** Each batch's queries see what the connection sees when they run. Outside a snapshot, a later
  batch sees changes committed after the keys were read: a root deleted in between is skipped, a root that no
  longer matches the filter is skipped, and a root committed after the keys were read is not loaded. In a
  `REPEATABLE READ` transaction (PostgreSQL, MySQL), or with `Pooled::snapshot`, whose snapshot spans the whole
  stream, every batch sees the same data. With a `Pooled` pool, the child queries of a batch run concurrently
  (MPA-LOAD-11).
- **MPA-LOAD-17** A stream decodes each batch on its own: `Arc<T>` values are shared within a batch only
  (MPA-LOAD-13). A view with `Ref<T>` fields cannot be streamed (`Error::GraphRequired`, MPA-LOAD-14).
  `json_stream(conn)` streams `serde_json::Value` objects as `json` writes them, of a `select`ion or of every field;
  a graph view streams as JSON with a selection only. `stream` with a selection fails with
  `Error::SelectionWithoutJson`. An override of the root query is a subquery of the keys query and is restricted
  to each batch's keys, or binds them itself if it takes them (MPA-OVR-3); nested arguments apply per parent, as in
  `all` (MPA-LOAD-9).
- **MPA-LOAD-18** The stream holds the connection until it ends or is dropped, and dropping it stops the load (the
  connections of a `Pooled` stream are given back, and its snapshot rolled back). The cursor of a PostgreSQL
  stream is closed when the stream ends; one left open by a dropped stream stays open on the connection until the
  next stream on it closes it, its transaction rolls back, or the connection closes. Nothing runs until the stream
  is first polled. An error, from preparing the load or from a query, is yielded once and ends the stream.
- **MPA-LOAD-19** `sql(text)` runs `text` as the root query, as an override of it runs (MPA-OVR-3, MPA-OVR-4): for
  reports and other queries that are not a table, such as aggregates and joins. Its rows are decoded as the view,
  and the view's collections and references load as usual, by the keys it selects. `bind(name, value)` binds
  `:name`, a named parameter of the root query's SQL, of `sql` or of a root override; a parameter is a whole
  `:name` token outside literals, quoted identifiers and comments, not a `::` cast, and MAY appear more than once.
  Every parameter MUST have a value and every value a parameter, else `Error::Params`. Filters, order, paging,
  `count` and streams apply to its rows as a subquery. Unlike an override, it is not checked beforehand: a missing
  alias fails when it runs.

## 6. Query planning

- **MPA-PLAN-1** A view is loaded by one root query plus one query per to-one reference, per collection and per
  variant table, at any depth, each batched by the keys of the rows above: never one query per row, and never a
  join of two collections.
- **MPA-PLAN-2** Every selected column has an alias equal to its path. System aliases: `$key` (the key, when no
  field holds it), `$parent` (the parent key of a collection row), `$ref.<field>` (the foreign key of a
  reference), `<prefix>$tag` (the tag of an enum), `$index` (the position in an ordered list), `$map_key`.
- **MPA-PLAN-3** Rows are decoded by alias, never by position, so queries MAY select columns in any order and
  extra columns are ignored.
- **MPA-PLAN-4** A collection with `depth = n` runs its query again for each level, at most `n` levels. With
  `recursive = "cte"`, all levels come from one `WITH RECURSIVE` query whose path guard stops at cycles; it needs a
  collection that contains its own view directly and no `through`. A recursive to-one reference is planned the
  same way: with `depth = n` its query runs again for each level, at most `n` levels below the first row, and the
  references of the last level are `None`; with `recursive = "cte"`, one `WITH RECURSIVE` query follows the
  references from the keys of the level above to the end of every chain. As chains share rows (two employees
  with one manager), a depth in that query would not be each chain's own, so `recursive = "cte"` on a reference
  takes no `depth` (`Error::Plan` for a shape built by hand).
- **MPA-PLAN-5** `mabat::plan::<T>()` returns the plan, and `Mabat::explain::<T>()` its queries with their SQL,
  generated or overridden.

## 7. Overrides

- **MPA-OVR-1** A registry, `Mabat::builder().register::<T>()…build(&mut conn)`, runs views with override SQL for
  any of their queries, from files (`overrides_dir`) or strings (`overrides`, `overrides_sql`). Writes never use
  overrides.
- **MPA-OVR-2** A view has at most one override file, named after it: `TaskView.toml`
  (`[query."children.notes"] sql = "…"`, optional `shadow = true`) or `TaskView.sql` (each query after a
  `-- mabat: query children.notes` line, optionally `, shadow`).
- **MPA-OVR-3** An override MUST select the aliases of section 6 that the view decodes: its columns, `$key` or the
  key field, `$parent` for a collection, `$ref.<field>` for references, and its computed fields (MPA-VIEW-13).
  A root override MAY take named parameters, `:name`, bound by each load (MPA-LOAD-19); the checks prepare them
  as placeholders after the keys. Other overrides take only the keys.
- **MPA-OVR-4** A child query override MUST take the keys of the rows above: `:keys` on any database
  (`= ANY(:keys)` on PostgreSQL, `IN (:keys)` elsewhere), or `$1` on PostgreSQL. `:keys` is replaced as a whole
  token, not inside literals, quoted identifiers or longer names. A root override MAY take keys, and then MUST be
  loaded by key (`Error::KeysRequired`).
- **MPA-OVR-5** `check` and `build` prepare every query, generated and overridden, on the database without
  running it, and compare its columns and parameters with the view (diagnostics M0100–M0301, section 11). `build`
  fails with `Error::Invalid` on errors, unless `on_invalid(OnInvalid::UseGenerated)` replaces invalid overrides by
  the generated queries.
- **MPA-OVR-6** A `shadow` override runs together with the generated query and logs a warning when their rows
  differ; `shadow_stats` counts runs and mismatches.
- **MPA-OVR-7** `reload(&mut conn)` reads the override files again, checks them, and swaps them in atomically;
  invalid files leave the running overrides as they are.
- **MPA-OVR-8** `Builder::manifest()` writes the views as JSON. The `mabat` command line tool checks override
  files against a manifest and a database (`mabat check`, with `--schema` to create a schema in a scratch
  transaction or database), explains (`mabat explain`) and scaffolds override files from the generated SQL of
  every query or of the queries named with `--query` (`mabat scaffold`), with no Rust toolchain. With `--out`,
  `scaffold` writes to the view's file in a directory, adding to an existing file and never replacing a query
  it already overrides.

## 8. JSON and selections

- **MPA-JSON-1** `load::<T>().json(conn)` returns `serde_json::Value` objects: columns through their type's
  `Serialize`, `json` columns as the JSON they hold, collections as arrays, maps as objects, references as objects
  or `null`, and enums as objects whose `__typename` names the variant. Tuple fields are named `_0`, `_1`, …
- **MPA-JSON-2** A column whose type does not implement `Serialize` fails with `Error::Json` only when it is loaded
  as JSON; the view still compiles.
- **MPA-JSON-3** `select(Selection)` loads only the selected fields: only their columns are selected and only
  their child queries run. `Selection::parse("name assignee { name } children { name }")` reads GraphQL-like text.
  The key column is always selected, named after the key field if the view has one, selected or not: the name
  overrides give it (MPA-OVR-3), so overridden queries load selections too.
- **MPA-JSON-4** A view selected without fields loads its columns and embedded values, not its collections or
  references. Embedded structs and enums are loaded whole. Unknown fields fail with `PlanError::Selection`.
- **MPA-JSON-5** A selection has a finite depth, so recursive and graph views load as trees as deep as it asks.
  A graph view loaded as JSON without a selection fails with `Error::GraphRequired`.
- **MPA-JSON-6** A selection is loaded with `json`; typed terminals with a selection fail with
  `Error::SelectionWithoutJson`. Overrides apply to selections.

## 9. GraphQL

- **MPA-GQL-1** `mabat_graphql::schema(&pool).list::<T>("name").by_key::<T>("name").finish()` builds an
  async-graphql dynamic schema from the views and every view they reach. `registry(..)` applies a registry's
  overrides; `connections(n)` runs each level concurrently (MPA-LOAD-11).
- **MPA-GQL-2** Types: views and embedded structs are objects named after their Rust type, with fields named as
  in Rust; enums with data are unions of an object per variant (`{Enum}{Variant}`, with a `_variant` field); enums
  without data are GraphQL enums; maps are lists of `{ key, value }`; columns are `Boolean`, `Int`, `Float`,
  `String` or the custom scalars of MPA-VIEW-12 (`BigInt`, `UUID`, `Date`, `Time`, `DateTime`, `NaiveDateTime`,
  `Decimal`, `JSON`, `Bytes`).
- **MPA-GQL-3** A list field takes `where` (a filter per comparable column with `eq`, `ne`, `lt`, `le`, `gt`, `ge`,
  `in`, `notIn`, `isNull`, plus `like` and `ilike` for strings, combined with `and`, `or`, `not`), `orderBy`
  (`[{ column: ASC | DESC }]`), `limit` and `offset`. A `by_key` field takes `key`, of the key field's type.
- **MPA-GQL-4** Each root field is one load of the selection set (fragments resolved, variables applied). Nested
  list fields take the arguments of MPA-GQL-3 for the elements of each parent (MPA-LOAD-9); selecting one
  collection twice with different arguments is an error.

## 10. Writing

```rust
mabat::save(&mut board, &mut tx).await?;                    // the board and what it owns
mabat::save_changes(&before, &mut after, &mut tx).await?;   // only what changed
mabat::delete::<Board, _>(board.id, &mut tx).await?;        // the board and what it owns
```

- **MPA-WRITE-1** `save`, `save_changes` and `delete` run in a transaction, or a savepoint of the caller's
  transaction. Statements run when they are called: there is no session and no flush.
- **MPA-WRITE-2** Keys are assigned by the application, or generated by the database (MPA-WRITE-13): a saved view
  and the views of its owned collections MUST have a key field, else `Error::Write`.
- **MPA-WRITE-3** `save` creates or replaces the row by key. It updates the row (`UPDATE … WHERE key = ?`), and
  only if no row has the key, inserts it with an upsert, `INSERT … ON CONFLICT (key) DO UPDATE` on PostgreSQL and
  SQLite and `INSERT … AS new ON DUPLICATE KEY UPDATE` on MySQL, which updates a row inserted meanwhile. Columns,
  embedded values and `json` fields are written.
- **MPA-WRITE-4** An owned collection is made equal to the value's: rows of elements that are gone are deleted
  with what they own, deepest first; the others are saved with the parent's key, their position for an `index`
  list (from 0), and their key for a map.
- **MPA-WRITE-5** A many-to-many collection replaces the rows of its link table; the linked views are not written.
  A to-one reference writes its foreign key only; the referenced view is not written.
- **MPA-WRITE-6** An enum stored in columns writes its tag (as a literal, so any tag column type accepts it), its
  variant's columns, and NULL to the other variants' columns. An enum in a table per variant upserts the
  variant's row and deletes the other variants' rows with what they own.
- **MPA-WRITE-7** `delete::<T, _>(key, conn)` deletes the row and what it owns (owned collections, links, variant
  rows), deepest first, and returns whether the row existed. Its links to entities of a graph are deleted with it;
  the entities themselves, and the rows that reference it, are not changed.
- **MPA-WRITE-8** `save_changes(&before, &mut after, conn)` writes what changed from `before` (as loaded) to
  `after`: columns and embedded values that differ by `PartialEq` (types without it count as changed), to-one
  foreign keys that differ, owned collection elements matched by key (changed ones updated, new ones saved whole,
  removed ones deleted with what they own, moved ones of an `index` list given their position), and link tables
  whose keys differ. Rows that did not change produce no statement. The two values MUST have the same key.
- **MPA-WRITE-9** A `#[view(version)]` integer column locks its row optimistically. `save` updates the row only if
  it has the value's version, incrementing it, else inserts it only if no row has its key; `save_changes` updates
  only if the row has the version. Otherwise the write fails with `Error::Conflict`, and the transaction is rolled
  back. A `save_changes` without changed columns does not increment the version.
- **MPA-WRITE-10** New versions and generated keys are written back into the value and into the elements of its
  owned collections (`Vec` and maps of owned views), so the value can be saved again without reloading.
- **MPA-WRITE-11** A column type without `sqlx::Encode`, or a `json` type without `Serialize`, fails with
  `Error::Write` only when saved; the view still compiles.
- **MPA-WRITE-12** A view of some of a table's columns is saved like any view when its row exists (MPA-WRITE-3).
  Saving a new row fails with `Error::Query` when the table's other columns are `NOT NULL` without a default.
- **MPA-WRITE-13** `#[view(generated)]` on the key field, an `Option` of an integer, declares a key the database
  generates: an identity, serial or `AUTO_INCREMENT` column. `save` inserts a value whose key is `None` without its
  key column and reads the key the database generated, with `RETURNING` on PostgreSQL and SQLite and
  `LAST_INSERT_ID()` on MySQL; its owned collections, links and variant rows are then written with that key, and
  the key is written back (MPA-WRITE-10). In `save` and `save_changes`, elements of owned collections without a
  key are new and are inserted the same way. A value with a key is saved as any other (MPA-WRITE-3), so the column
  MUST accept keys given explicitly: on PostgreSQL, `GENERATED BY DEFAULT AS IDENTITY` or `serial`, not
  `GENERATED ALWAYS`. `save_changes` of a value without a key, and a reference or link to one, fail with
  `Error::Write`. `generated` on any other field, or on a key that is not an `Option` of an integer, is a compile
  error.
- **MPA-WRITE-14** `save_graph(&mut graph, conn)` saves every entity of a graph in one transaction (MPA-WRITE-1),
  each as `save` saves a value, with its `Ref<T>` and `Option<Ref<T>>` fields written as the keys of the entities
  they point to. New versions and generated keys are written back into the entities. Every view of the graph's
  entities MUST be reachable from the view of its roots through references, else `Error::Write`. `save` and
  `save_changes` of a value with references into a graph fail with `Error::Write`.
- **MPA-WRITE-15** An entity is saved after the entities whose keys it needs: those its row references, those whose
  collections write its foreign key (MPA-WRITE-16), and those that values it owns reference. In a cycle of such
  dependencies, the optional references (`Option<Ref<T>>`) between entities of the cycle are written NULL first and
  set once every entity of the cycle is saved; if the required ones alone form a cycle, the save fails with
  `Error::Write`.
- **MPA-WRITE-16** A `Vec<Ref<T>>` collection by the foreign key `fk` of `T`'s table is the inverse of a reference
  of `T` by that same column to this view, if `T` has one: the reference is written, and the two MUST agree (each
  element references the entity, and each entity of the graph that references it is an element), else
  `Error::Write`. Otherwise the collection writes its elements' `fk`, and their position for an `index` list; rows
  of `T` with the entity's key in `fk` that are no longer elements get NULL there, and are not deleted. An element
  of two such collections fails with `Error::Write`.
- **MPA-WRITE-17** A `Vec<Ref<T>>` collection through a link table replaces the entity's link rows, with positions
  for an `index` list (MPA-WRITE-5).
- **MPA-WRITE-18** Values owned by an entity MAY have `Ref<T>` fields, written as keys; collections of references
  are saved only on entities, and one in an owned value fails with `Error::Write`.
- **MPA-WRITE-19** `save_all(&mut values, conn)` saves many values in one transaction, each as `save` saves one
  (MPA-WRITE-3 to MPA-WRITE-13), with statements for each table and level of the aggregate rather than for each
  row: the rows are updated by one statement from a table of their values, the ones not updated are inserted by
  another, then the rows of their variant tables, owned collections and links are written the same way. Rows whose
  generated keys are `None` are inserted one by one, to read their keys. Statements are split to hold at most
  30,000 parameters. A version conflict names the keys of the rows of its statement; on MySQL it cannot tell
  which of them changed. A view with a column type that does not implement `Clone` is saved value by value, in
  the same transaction. Values with references into a graph fail with `Error::Write`.
- **MPA-WRITE-20** A graph records its changed entities: those added with `insert` and those handed out by
  `get_mut`, whether or not they were then changed, since it was loaded or last saved; `is_changed(r)` tells.
  `save_graph_changes(&mut graph, conn)` saves as `save_graph` does (MPA-WRITE-14 to MPA-WRITE-18), but writes
  only the rows of the changed entities, and of the elements whose foreign key a changed entity's collection
  writes (MPA-WRITE-16); the other entities' keys are used as loaded. Link rows are replaced (MPA-WRITE-17), and
  rows no longer in a collection set to NULL, only for the collections of changed entities. Collections and the
  references they are the inverse of MUST still agree across the whole graph. A successful `save_graph` or
  `save_graph_changes` leaves no entity changed; a failed one is rolled back and keeps them changed.

## 11. Errors and diagnostics

Every failure is a `mabat::Error`; messages name the view and the path.

| Variant | When | Rules |
| --- | --- | --- |
| `Plan` | the view cannot be planned: a cycle without `depth`, an unsupported recursion or embedding, an unknown selected field | MPA-PLAN-4, MPA-JSON-4 |
| `Query` | a statement failed; carries the SQL | |
| `Decode` | a column cannot be decoded into its field | MPA-VIEW-5 |
| `MissingReference` | a required reference's row is missing | MPA-VIEW-8 |
| `NullTag`, `UnknownTag`, `OtherVariantColumn`, `MissingVariant` | enum decoding | MPA-SUM-4 |
| `ListIndex` | a NULL or repeated list index | MPA-VIEW-9 |
| `DuplicateMapKey` | two map elements with one key | MPA-VIEW-9 |
| `Cycle` | a cycle in the data of a `cte` collection | MPA-LOAD-12 |
| `UnloadedReference` | a graph reference to an entity that was not loaded | MPA-LOAD-14 |
| `GraphRequired` | a graph view loaded without `graph`, or as JSON without a selection | MPA-LOAD-14, MPA-JSON-5 |
| `MixedKeys` | keys of different types in one load | MPA-LOAD-3 |
| `NotFound`, `TooManyRows` | `one` and `optional` | MPA-LOAD-2 |
| `NestedArguments` | nested arguments on a path that takes none | MPA-LOAD-9 |
| `SelectionWithoutJson` | a selection with a typed terminal | MPA-JSON-6 |
| `Json` | a column that cannot be written as JSON | MPA-JSON-2 |
| `ColumnNotSelected` | an order or filter column an override does not select | MPA-LOAD-7, MPA-LOAD-10 |
| `KeysRequired` | a root override that takes keys, loaded without keys | MPA-OVR-4 |
| `Params` | a named parameter without a value, a value without a parameter, or a computed field without SQL | MPA-LOAD-19, MPA-VIEW-13 |
| `Invalid` | `build` found errors; carries the `Report` | MPA-OVR-5 |
| `Check` | the checks could not run | MPA-OVR-5 |
| `NotRegistered` | a registry load of a view that is not registered | MPA-OVR-1 |
| `WrongBackend`, `ManifestBackend` | a registry or manifest used on another database | MPA-DB-3, MPA-OVR-8 |
| `Connection` | a pooled connection could not be opened or join its snapshot | MPA-LOAD-11 |
| `Write` | a value that cannot be written | MPA-WRITE-2, MPA-WRITE-11, MPA-WRITE-13, MPA-WRITE-14, MPA-WRITE-15, MPA-WRITE-16 |
| `Conflict` | a version or row changed since the value was loaded | MPA-WRITE-9 |

Diagnostics of `check`, `build`, `reload` and `mabat check` (M0201–M0207 with `--snapshot` only), each with a severity, the view, the query, the file
and line, and notes:

| Code | Meaning |
| --- | --- |
| M0100 | An override file cannot be read or parsed |
| M0101 | An override names no registered view, or no query of its view |
| M0102 | A query's columns do not match the view (missing, extra or wrongly typed aliases) |
| M0103 | A query does not prepare on the database |
| M0104 | A query has the wrong parameters (keys not taken, or other parameters) |
| M0105 | A query does not select every optional path (warning: the path is always `None`) |
| M0201 | A query reads a table that is not in the schema snapshot |
| M0202 | A query reads a column that is not in the schema snapshot |
| M0203 | A column has a type the field cannot be decoded from |
| M0204 | A column is nullable under a field that is not an `Option` (warning) |
| M0205 | The columns linking a query to its parent hold different kinds of key |
| M0206 | A view's key column is not the primary key of its table (warning) |
| M0207 | A generated key is on a column the database does not generate |
| M0301 | A view cannot be planned |

## 12. Not supported

Mabat 0.1 does not do the following; tools SHOULD NOT generate code that relies on it.

- **MPA-NOT-1** Removed: keys generated by the database are supported (MPA-WRITE-13).
- **MPA-NOT-2** Removed: graphs are saved with `save_graph` (MPA-WRITE-14).
- **MPA-NOT-3** A unit of work that collects writes and flushes them later (MPA-WRITE-1).
- **MPA-NOT-4** Lazy loading: everything a view declares is loaded by the load, or selected (section 8).
- **MPA-NOT-5** Writes through overrides, and override SQL for writes.
- **MPA-NOT-6** Filtering by columns of embedded structs in `filter`, `nested` or GraphQL `where`.
- **MPA-NOT-7** Arguments on map collections in GraphQL, and GraphQL mutations.
- **MPA-NOT-8** Pipelining queries on one connection.
- **MPA-NOT-9** Schema generation or migrations: views describe existing tables.
- **MPA-NOT-10** Removed: `save_graph_changes` saves only the changed entities of a graph (MPA-WRITE-20).

## 13. Schema snapshots

A snapshot of a database's schema, committed next to the manifest of the views, lets views be checked against
the schema without a database.

- **MPA-SCH-1** `mabat schema --database-url <url> --out mabat/schema.json`, or `mabat::schema::snapshot(conn)`,
  writes a snapshot of the current schema of a database as JSON: its tables and views; their columns, with the
  type as SQLx names it and as the database declares it, nullability, and whether the database generates their
  values (an identity, serial or `AUTO_INCREMENT` column, or SQLite's `INTEGER PRIMARY KEY`); and their primary and
  foreign keys. Tables are sorted by name and columns are in the order of their table, so the same schema always
  gives the same file. Without `--out`, the snapshot is printed.
- **MPA-SCH-2** `mabat schema --check mabat/schema.json --database-url <url>` compares a database with a snapshot
  and lists the tables, columns, types, nullability, generated values and keys that differ. It exits with status 1
  when they differ, so that CI can tell when the snapshot is out of date.
- **MPA-SCH-3** The manifest of the views (MPA-OVR-8) records, for each query, its table and key column, the
  column behind each alias, and the foreign key and link table of each collection, so that views can be checked
  against a snapshot. It is format 2; manifests of format 1, without them, are still read.
- **MPA-SCH-4** `mabat check --manifest mabat/views.json --snapshot mabat/schema.json`, or
  `Manifest::check_snapshot`, checks the views against a snapshot with no database, and reports what it finds as
  diagnostics (section 11): a table (M0201) or column (M0202) that a query reads and the schema lacks; a column
  whose type, as SQLx names it, is not one the field can be decoded from (M0203); a column that the schema
  allows to be NULL under a field that is not an `Option` (M0204, a warning; not reported for the fields of
  embedded values and enums, which may be NULL in rows that do not hold them, or for views of the database, whose
  columns are all reported nullable); and columns that link a query to its parent (a foreign key, the columns of a
  link table, or a reference) holding a different kind of key (integer, text or uuid) from the key they match
  (M0205). It exits with status 1 when there are errors. The manifest and the snapshot MUST be of the same
  database, and the manifest MUST be of format 2.
- **MPA-SCH-5** The key of each query is checked against its table: a key column that is not the table's primary
  key is a warning (M0206), as loads assume it is unique and saves update and upsert by it (views of the database
  have no primary key and are not checked); a view whose key is `#[view(generated)]` on a column the database
  does not generate is an error (M0207). The manifest records which views have generated keys.
- **MPA-SCH-6** Against a snapshot, override files are checked for their syntax and names only (M0100, M0101):
  their SQL needs a database to prepare it, with `mabat check --database-url` or the application's startup check
  (MPA-OVR-5).
- **MPA-SCH-7** A build script checks the committed manifest against the committed snapshot with
  `mabat_check::build("mabat/views.json", "mabat/schema.json").overrides("mabat/overrides").run()`, the
  `mabat-check` crate in `[build-dependencies]`, which needs no database driver. Each error is a `cargo::error`,
  which fails the build, and each warning a `cargo::warning`; the build script runs again when either file or an
  override file changes. Before the manifest exists, or with `MABAT_SKIP_CHECK` set, nothing is checked and a
  warning says so. `Build::check` returns what it finds instead of telling Cargo. The manifest's types, its JSON
  form, `explain` and `scaffold`, the override file parser and the report are in `mabat-check`, and
  `mabat::manifest` re-exports them; checking a manifest against a database is `mabat::manifest::check`.
