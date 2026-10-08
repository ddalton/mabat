# Changelog

All notable changes to Mabat are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). Until 1.0, minor versions may change the API.

## [Unreleased]

### Added

The first release, with milestones 1 to 5 of the [design](docs/design.md), the DBA tooling, MySQL and SQLite.

- **Views (M1).**
  - `#[derive(View)]` on structs: columns, `Option` columns, embedded structs with column prefixes,
    to-many collections and to-one references.
  - Loading with one root query plus one batched `WHERE fk = ANY($1)` query per relationship.
  - Results decoded by column alias.
  - Errors that name the view and the field path.
- **Enums with data (M2).**
  - The `tag` strategy: variant columns in the row.
  - The `table_per_variant` strategy: a batched query per variant table, sent only the keys of rows of that
    variant.
  - Nested enums, tuple variants and PostgreSQL enum tags.
  - Strict decoding of unknown and NULL tags, columns of other variants and missing variant rows, with
    `lenient`.
  - `#[view(json)]` fields decoded with `serde`.
- **Filters and counting.** `mabat::filter::col(..)` conditions (`eq` … `ilike`, `is_in`, groups and `!`)
  on the root query, and `Load::count`.
- **Overrides (M3).**
  - Override files (TOML or `.sql`) replace any query by name, with no code changes.
  - Every generated and overridden query is checked against the views and the database at startup:
    aliases, types, keys and parameters, reported like compiler errors (`M0100`–`M0105`).
  - Schema drift in generated queries is reported too.
  - `OnInvalid::UseGenerated`, shadow mode with mismatch and timing statistics, runtime reloading with
    `Mabat::reload`, and `mabat::scaffold`.
- **Collections and recursion (M4).**
  - Ordered lists placed by an `index` column.
  - `BTreeMap`/`HashMap` collections keyed by a column.
  - Many-to-many collections `through` a link table.
  - Recursive views loaded level by level (`depth = n`) or with one `WITH RECURSIVE` query
    (`recursive = "cte"`), with cycle detection.
- **Shared values and graphs (M5).**
  - `Arc<T>` fields shared per entity.
  - `Ref<T>` fields loaded with `Load::graph` into a `Graph` of arenas, with generated navigation methods.
  - Cycles without `Rc`, `Weak` or `RefCell`. Each entity is fetched once and each relationship is loaded
    once.
- **DBA tooling.**
  - `Builder::manifest` writes the views' queries, aliases and accepted types as JSON.
  - The `mabat` command line tool (crate `mabat-cli`) runs `check` (against a database or a schema file,
    in a transaction that is rolled back), `explain` and `scaffold`, with no Rust toolchain.
- **MySQL and SQLite (M6).**
  - The `postgres` (default), `mysql` and `sqlite` features. A view is decoded on each enabled database, and a load
    runs on the database of its connection; `#[view(databases = "...")]` limits a view to some of them.
  - Keys bound as `IN (?, …)` lists, padded to a power of two so statements are reused, and split into
    statements of at most 1,000 keys for child queries.
  - `:keys` in override SQL for the keys of a batched query on any database.
  - Checks, manifests and `mabat check` on both. With `--schema`, MySQL checks in a temporary database that
    is dropped afterwards, and SQLite in an in-memory database.
  - On MySQL: unsigned integer keys, `BINARY(16)` UUID keys and `ENUM` tags.
- **JSON and selections (M7).** `Load::json` loads views as JSON, and `Load::select` with a `Selection`
  (built in code or parsed from GraphQL-like text) loads only the selected fields: only their columns are
  selected and only their child queries run. Recursive and graph views load as trees as deep as the selection.
- **Concurrent loads (M6).** `Pooled::snapshot` (PostgreSQL) and `Pooled::read_committed` (any database) run
  the queries of each level of a load at the same time on connections of a pool, in a snapshot that the
  connections share or reading what is committed. Loads, counts, checks and registries take a `Pooled` where
  they take a connection.
- **Platform.** PostgreSQL, MySQL 8 or later, or SQLite, with SQLx 0.9, and Rust 1.94 or later.
- **End-to-end tests** against the Pagila and Chinook sample databases, and Chinook on MySQL and SQLite.
