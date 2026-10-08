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
mabat scaffold --manifest mabat/views.json --view TaskView --format sql   # a starting override file
```

`check` prints the same report as the application's startup check. It exits with 0 when there are no errors, 1
when there are, and 2 for any other problem, so it can gate a DBA's CI.
