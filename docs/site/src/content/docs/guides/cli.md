---
title: The mabat CLI
description: Check, explain and scaffold override SQL from a manifest of the views, with no Rust toolchain.
---

A DBA does not need Rust to work on overrides. The application writes a **manifest** of its views — their queries,
aliases and accepted column types — and the `mabat` tool (crate `mabat-cli`) works from it
([MPA-OVR-8](../../spec/mpa/#mpa-ovr-8)).

```rust
#[test]
fn views_manifest_is_up_to_date() {
    let manifest = Mabat::<sqlx::Postgres>::builder().register::<TaskView>().manifest().unwrap();
    assert!(!manifest.write("mabat/views.json").unwrap(), "mabat/views.json was out of date");
}
```

```sh
# check every query, generated and overridden, against a database
mabat check --manifest mabat/views.json --overrides mabat/overrides --database-url postgres://...

# or against a schema file in any scratch database: created in a transaction that is rolled back
# (a temporary database on MySQL, an in-memory database on SQLite)
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
without a database ([MPA-SCH-1](../../spec/mpa/#mpa-sch-1)). `mabat schema` writes it, `mabat schema --check` tells CI when the database has moved
on ([MPA-SCH-2](../../spec/mpa/#mpa-sch-2)), and `mabat check --snapshot` checks the views against it ([MPA-SCH-4](../../spec/mpa/#mpa-sch-4)):

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

`check` prints the same report as the application's startup check. It exits with 0 when there are no errors, 1
when there are, and 2 for any other problem, so it can gate a DBA's CI.
