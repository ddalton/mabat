---
title: Why Mabat
description: How Mabat compares with Diesel, SeaORM and hand-written SQLx, and when to use it.
---

Mabat is not an ORM in the Hibernate sense, and not a query builder. It sits between them: you declare the
**shape of the data a use case reads** — an aggregate — and Mabat owns the queries that fill it.

## What it does that others do not

| | Mabat | Diesel / SeaORM | Hand-written SQLx |
| --- | --- | --- | --- |
| Nested aggregates in one call | Declared once, loaded with one batched query per relationship | Assembled by hand from several queries, or joins that duplicate rows | Assembled by hand |
| Enums with data (sum types) | Native: tag column or table per variant, strict decoding | Not supported; flatten into columns | By hand |
| Recursive trees and cyclic graphs | `depth`, `WITH RECURSIVE`, `Ref<T>` graphs, no `Rc`/`RefCell` | By hand | By hand |
| Tuning a query in production | Override file, checked at startup, shadow-compared, hot-reloaded | Change code and redeploy | Change code and redeploy |
| JSON and GraphQL from the same views | Selections and a generated schema | Separate layer | Separate layer |
| Writing back | Whole aggregates, or only what changed, with version locking | Row by row | Row by row |

## When to use it

- Read-heavy services whose responses are nested documents: an API's detail pages, a GraphQL backend, exports.
- Teams where DBAs tune SQL: overrides let them change a query without a Rust release, and the startup checks
  make sure a change matches the code.
- Domains with sum types and trees, which relational ORMs flatten.

## When not to

- Ad-hoc analytical queries: write them with SQLx directly; Mabat composes with it on the same connection.
- Schema management: Mabat reads existing tables and does not generate or migrate schemas
  ([MPA-NOT-9](../../spec/mpa/#mpa-not-9)).
- Workloads that save large graphs often: `save_graph` writes every entity, not only the changed ones
  ([MPA-NOT-10](../../spec/mpa/#mpa-not-10)).
