---
title: Loading
description: Keys, filters, ordering, paging, counting, nested collection arguments and the terminals of a load.
---

`mabat::load::<T>()` starts a load of the generated queries; `Mabat::load::<T>()` does the same with a registry's
[overrides](../overrides/) ([MPA-LOAD-1](../../spec/mpa/#mpa-load-1)).

```rust
use mabat::filter::col;

let page = mabat::load::<TaskView>()
    .filter(col("status").eq("open") & col("assignee_id").is_in([1_i64, 2, 3]))
    .order_by("name")
    .limit(20)
    .offset(40)
    .all(&mut conn)
    .await?;
```

## Terminals

| Terminal | Returns | Rule |
| --- | --- | --- |
| `all` | every match | [MPA-LOAD-2](../../spec/mpa/#mpa-load-2) |
| `one` | exactly one, else `NotFound` or `TooManyRows` | [MPA-LOAD-2](../../spec/mpa/#mpa-load-2) |
| `optional` | at most one | [MPA-LOAD-2](../../spec/mpa/#mpa-load-2) |
| `count` | `SELECT count(*)` of the root query | [MPA-LOAD-2](../../spec/mpa/#mpa-load-2) |
| `graph` | a `Graph`, for views with `Ref<T>` | [MPA-LOAD-14](../../spec/mpa/#mpa-load-14) |
| `json` | `serde_json::Value` objects | [MPA-JSON-1](../../spec/mpa/#mpa-json-1) |
| `stream` | a `Stream` of values, a batch at a time | [MPA-LOAD-15](../../spec/mpa/#mpa-load-15) |
| `json_stream` | a `Stream` of `serde_json::Value` objects | [MPA-LOAD-17](../../spec/mpa/#mpa-load-17) |

A load takes a connection, a transaction, a pooled connection or a [`Pooled` pool](../databases/#concurrent-loads),
and every query runs on it, so it sees the uncommitted writes of its transaction
([MPA-DB-5](../../spec/mpa/#mpa-db-5)).

## Streaming

`stream` loads many values without holding them all in memory. It reads the keys of every match, with the filter,
order and page, and then loads `batch_size` values at a time (1,000 by default), each batch with its collections
and references. On a PostgreSQL connection the keys stay on the server in a cursor and are fetched a batch at a
time; elsewhere they are read at once. It yields the same values as `all`, in the same order
([MPA-LOAD-15](../../spec/mpa/#mpa-load-15)):

```rust
use futures_util::TryStreamExt;

let mut tasks = mabat::load::<TaskView>().order_by("id").batch_size(500).stream(&mut conn);
while let Some(task) = tasks.try_next().await? {
    export(&task)?;
}
```

Each batch sees what the connection sees when it runs. Outside a transaction, a value deleted after the keys were
read is skipped. In a `REPEATABLE READ` transaction, or with `Pooled::snapshot`, every batch sees one snapshot
([MPA-LOAD-16](../../spec/mpa/#mpa-load-16)). `Arc<T>` values are shared within a batch, and graph views
(`Ref<T>`) cannot be streamed. `json_stream` streams JSON, of a selection or of every field
([MPA-LOAD-17](../../spec/mpa/#mpa-load-17)). The stream holds the connection until it ends; dropping it stops the
load, and an error ends it ([MPA-LOAD-18](../../spec/mpa/#mpa-load-18)).

## Keys

`by_key` and `by_keys` restrict the root rows to keys: integers, strings or UUIDs, all of one type
([MPA-LOAD-3](../../spec/mpa/#mpa-load-3)).

## Filters

`mabat::filter::col("c")` builds conditions on columns of the view's table: `eq`, `ne`, `lt`, `le`, `gt`, `ge`,
`is_null`, `is_not_null`, `is_in`, `not_in`, `like` and `ilike`, combined with `&`, `|`, `!`, `Condition::all` and
`Condition::any`. Every value is a bound parameter ([MPA-LOAD-4](../../spec/mpa/#mpa-load-4)). `ilike` is
`LOWER() LIKE LOWER()` outside PostgreSQL, and an empty `is_in` matches nothing
([MPA-LOAD-5](../../spec/mpa/#mpa-load-5)).

## Ordering and paging

`order_by`, `order_by_desc`, `limit` and `offset` apply to the root rows. With an override of the root query, the
columns they use must be selected by the view ([MPA-LOAD-7](../../spec/mpa/#mpa-load-7)).

## Nested collections

`nested(path, Nested)` filters, orders and pages the elements of a collection **for each parent**, in the
collection's one query ([MPA-LOAD-9](../../spec/mpa/#mpa-load-9)):

```rust
use mabat::Nested;

// Every task with its three most recent open subtasks
let tasks = mabat::load::<TaskView>()
    .nested("children", Nested::new().filter(col("done").eq(false)).order_by_desc("created_at").limit(3))
    .nested("children.notes", Nested::new().limit(1))
    .all(&mut conn)
    .await?;
```

Paging per parent numbers the rows with `ROW_NUMBER() OVER (PARTITION BY …)`; with an override of the collection's
query, the arguments wrap it as a subquery ([MPA-LOAD-10](../../spec/mpa/#mpa-load-10)).

## Reports

A report's rows are not a table: they come from aggregates, joins or window functions. `sql` runs your SQL as the
root query, `bind` gives its named parameters (`:name`) their values, and `#[view(computed)]` fields hold what it
computes. The view's collections and references still load by the keys the SQL selects, so a report comes out as
typed, nested values ([MPA-LOAD-19](../../spec/mpa/#mpa-load-19)):

```rust
#[derive(View)]
#[view(table = "customer", key = "customer_id")]
struct TopCustomer {
    customer_id: i32,
    last_name: String,
    #[view(computed)]                      // not a column: the SQL computes it
    invoices: i64,
    #[view(child(fk = "customer_id", order_by = "invoice_date"))]
    period: Vec<InvoiceDate>,              // loaded as usual, by the keys the SQL selects
}

let top = mabat::load::<TopCustomer>()
    .sql(r#"SELECT c.customer_id AS "customer_id", c.last_name AS "last_name", count(*) AS "invoices"
            FROM customer c JOIN invoice i ON i.customer_id = c.customer_id
            WHERE i.invoice_date >= :from AND i.invoice_date < :to
            GROUP BY c.customer_id, c.last_name"#)
    .bind("from", from)
    .bind("to", to)
    .nested("period", Nested::new().filter(col("invoice_date").ge(from) & col("invoice_date").lt(to)))
    .order_by_desc("invoices")
    .limit(10)
    .all(&mut conn)
    .await?;
```

- The SQL selects the aliases of the view's fields, its key, and its computed fields, as an override does.
- Filters, order, paging, `count` and `stream` apply to its rows, computed fields included.
- Every `:name` needs a value and every value a `:name`; `:keys` takes the keys of `by_keys`. A mismatch, or a
  computed field (not an `Option`) loaded without SQL, is `Error::Params`.
- `sql` is not checked at startup. For a report a DBA maintains, put the same SQL in an override of `$root`: it is
  checked at startup, computed fields included, and each load binds its parameters
  ([MPA-OVR-3](../../spec/mpa/#mpa-ovr-3)).
