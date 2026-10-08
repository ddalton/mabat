# Pagila

The Pagila sample database (a PostgreSQL port of MySQL's Sakila: a DVD rental store), release
[`pagila-v3.1.0`](https://github.com/devrimgunduz/pagila/tree/pagila-v3.1.0), used unchanged by the end-to-end
tests:

| File | SHA-256 |
| --- | --- |
| `pagila-schema.sql` | `8ce358e4c8014087b85296694a0893887bd7a4190e3ce407f2721b86b98e5707` |
| `pagila-insert-data.sql` | `136f3105263a1338a9805da4c06b6b37b60f1abc15ce7dbc8d6f5501f506aa22` |

The files are a `pg_dump` of the `public` schema. Before running them, the tests:

- remove the `search_path` reset and the `OWNER TO` and `ON SCHEMA public` statements
- remove the `public.` qualifiers

This way each test database gets the dataset in a schema of its own (`mabat_e2e::Dataset`).

This release needs no extensions and runs on PostgreSQL 14 to 16. Later releases need PostgreSQL 18 and pgvector.

Copyright (c) Devrim Gündüz, MIT License; see [LICENSE.txt](LICENSE.txt).
