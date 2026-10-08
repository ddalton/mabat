---
title: GraphQL
description: A GraphQL schema generated from the views with mabat-graphql, each root field one planned load.
---

The `mabat-graphql` crate generates an [async-graphql](https://crates.io/crates/async-graphql) schema from the views
and every view they reach. Each root field runs one load of exactly the fields the query selects
([MPA-GQL-1](../../spec/mpa/#mpa-gql-1), [MPA-GQL-4](../../spec/mpa/#mpa-gql-4)).

```rust
let schema = mabat_graphql::schema(&pool)
    .list::<TaskView>("tasks")   // tasks(where, orderBy, limit, offset): [TaskView!]!
    .by_key::<TaskView>("task")  // task(key: UUID!): TaskView
    .registry(mabat)             // optional: the registry's overrides
    .connections(4)              // optional: each level's queries concurrently
    .finish()?;
```

```graphql
{
  tasks(where: { name: { ilike: "%release%" } }, orderBy: [{ name: ASC }], limit: 10) {
    name
    assignee { name }                                    # one batched query
    children(orderBy: [{ position: ASC }], limit: 3) {   # the first three of each task, one query
      name
    }
  }
}
```

## Types

| Rust | GraphQL |
| --- | --- |
| A view, an embedded struct | An object type named after the Rust type, fields named as in Rust |
| An enum with data | A union of `{Enum}{Variant}` objects, each with a `_variant` field |
| An enum without data | A GraphQL enum |
| A map collection | A list of `{ key: String!, value }` |
| Columns | `Boolean`, `Int`, `Float`, `String`, or the scalars `BigInt`, `UUID`, `Date`, `Time`, `DateTime`, `NaiveDateTime`, `Decimal`, `JSON`, `Bytes` |

See [MPA-GQL-2](../../spec/mpa/#mpa-gql-2).

## Arguments

List fields — at the root and nested — take `where` (`eq`, `ne`, `lt`, `le`, `gt`, `ge`, `in`, `notIn`, `isNull`,
and `like`/`ilike` for strings, combined with `and`, `or`, `not`), `orderBy`, `limit` and `offset`; nested ones apply
to the elements of each parent. `by_key` fields take `key` ([MPA-GQL-3](../../spec/mpa/#mpa-gql-3)).

## Example server

```sh
cargo run -p mabat-graphql --example chinook   # Chinook on SQLite, GraphiQL at http://127.0.0.1:8000
```
