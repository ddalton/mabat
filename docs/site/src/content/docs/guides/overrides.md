---
title: Tuning with overrides
description: Replace any query of a view with SQL from a file, checked against the database at startup, shadowed and hot-reloaded.
---

Any query of a view can be replaced with SQL from an override file. A DBA can change joins, hints, ordering or the
tables themselves — read from a materialized view, call a stored function — without touching the Rust code. Rows
are decoded by alias, so an override only has to keep the aliases ([MPA-OVR-1](../../spec/mpa/#mpa-ovr-1),
[MPA-OVR-3](../../spec/mpa/#mpa-ovr-3)).

```rust
use mabat::Mabat;

let mabat = Mabat::builder()
    .register::<TaskView>()
    .overrides_dir("mabat/overrides")
    .build(&mut conn)                         // checks every query against the database
    .await?;

let task = mabat.load::<TaskView>().by_key(id).one(&mut tx).await?;
```

## Override files

A view has at most one file, named after it, in TOML or plain SQL ([MPA-OVR-2](../../spec/mpa/#mpa-ovr-2)):

```sql
-- mabat/overrides/TaskView.sql
-- mabat: query children.notes, shadow
SELECT n.task_id AS "$parent", n.id AS "$key", n.body AS "body", n.tag_code AS "$ref.tag"
FROM task_note n
WHERE n.task_id IN (:keys)
ORDER BY n.id;
```

```toml
# mabat/overrides/TaskView.toml
[query."children.notes"]
sql = '''
SELECT n.task_id AS "$parent", n.id AS "$key", n.body AS "body", n.tag_code AS "$ref.tag"
FROM task_note n WHERE n.task_id = ANY(:keys) ORDER BY n.id
'''
shadow = true
```

- Queries are named `$root` or by the path they fill, such as `children.notes`.
- A child query takes the keys of the rows above as `:keys` — `= ANY(:keys)` on PostgreSQL, `IN (:keys)` on MySQL
  and SQLite ([MPA-OVR-4](../../spec/mpa/#mpa-ovr-4)).
- System aliases: `$key`, `$parent`, `$ref.<field>`, `<prefix>$tag`, `$index`, `$map_key`
  ([MPA-PLAN-2](../../spec/mpa/#mpa-plan-2)).
- `mabat::scaffold::<TaskView, sqlx::Postgres>()` returns a file with the generated SQL of every query, as a
  starting point.

## Checked at startup

`build` prepares every query, generated and overridden, without running it, and compares its columns and
parameters with the view, so a broken override or a schema that drifted fails before any request is served
([MPA-OVR-5](../../spec/mpa/#mpa-ovr-5)):

```text
error[M0102]: override for TaskView.children.notes does not match the view
  --> mabat/overrides/TaskView.toml:2
   | column 3 "body" has type INT4, expected TEXT for String
   | column 4 "$ref.tga" is not a path of NoteView in this query (did you mean "$ref.tag"?)
```

`check` returns the same report without building, for a CI test, and `OnInvalid::UseGenerated` starts with the
generated queries in place of invalid overrides. The codes are listed on the
[errors and diagnostics](../../reference/errors/#diagnostics) page.

## Shadow mode and reloading

- `shadow` runs the override and the generated query, compares their rows, and counts mismatches and timings, so a
  tuned query is shown equivalent before it is relied on ([MPA-OVR-6](../../spec/mpa/#mpa-ovr-6)).
- `mabat.reload(&mut conn)` reads the files again and swaps them in atomically if they pass the checks; an invalid
  change never replaces a working query ([MPA-OVR-7](../../spec/mpa/#mpa-ovr-7)).

Writes never use overrides ([MPA-NOT-5](../../spec/mpa/#mpa-not-5)).

## Finding the query to tune

Mabat traces each load with [`tracing`](https://docs.rs/tracing) at the `debug` level: a span for the load, one
for each of its queries, with the query's name, whether an override ran and the rows it returned, and one for each
statement, with its SQL and time ([MPA-DB-6](../../spec/mpa/#mpa-db-6)). With `tracing-subscriber` and
`RUST_LOG=mabat=debug`:

```text
DEBUG mabat.load{view="Project" keys=1}:mabat.query{view="Task" query="tasks" overridden=true keys=1}:
      mabat.statement{db.system.name="SQLite" db.query.text="SELECT t.id AS \"id\", …" elapsed_ms=0.173 rows=2}:
      statement ran rows=2 elapsed_ms=0.173
```

The query name is the name an override file uses, so the slow query of a trace is the one to override. With
`tracing-opentelemetry`, the spans are part of the application's traces; their `db.*` fields follow
OpenTelemetry's names. Bound values are never traced.
