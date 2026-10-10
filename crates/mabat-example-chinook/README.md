# Chinook: an example service with Mabat

A web service on [Chinook](https://github.com/lerocha/chinook-database), a digital music store, that shows
what Mabat does in one small application ([`src/lib.rs`](src/lib.rs)):

- **Views as Rust types:** an artist's albums and tracks, a customer's account with invoices and lines, the chain
  of managers above an employee, the organization chart, and playlists.
- **REST** with [axum](https://github.com/tokio-rs/axum): typed values serialized with serde, or JSON straight
  from the rows, of the whole view or a `?select=` of it.
- **Streaming:** every track as NDJSON, 500 at a time, without holding them all in memory.
- **Writes:** playlists created with a key the database generates, changed with `save_changes`, and deleted.
- **GraphQL** generated from the views, with GraphiQL.
- **A DBA's override** in [`mabat/overrides/Discography.sql`](mabat/overrides/Discography.sql), checked at startup,
  and `GET /explain/{view}` to see the SQL that runs.
- **Schema checks at build time:** [`build.rs`](build.rs) checks the views against
  [`mabat/schema.json`](mabat/schema.json), so `cargo build` fails when they no longer match the schema;
  [`tests/files.rs`](tests/files.rs) keeps the manifest and the snapshot up to date.

It runs on SQLite, created from the Chinook script on first run; there is nothing to set up.

## Run it

```sh
cargo run -p mabat-example-chinook
```

It creates `chinook.db` in the current directory (`CHINOOK_DB` to change it) and listens on
`http://127.0.0.1:3000` (`ADDR` to change it).

## A tour

```sh
# An artist's albums, newest first as the override orders them, with their tracks and genres
curl localhost:3000/artists/1

# A filtered page of tracks: rock, by Angus Young
curl 'localhost:3000/tracks?genre_id=1&composer=angus&limit=5'

# Every track, streamed as NDJSON
curl localhost:3000/tracks/export | head -3

# A customer's account, or only the fields of a selection: only their queries run
curl localhost:3000/customers/5
curl -G localhost:3000/customers/5 --data-urlencode 'select=first_name invoices { total lines { quantity } }'

# Up the chain of managers, in one WITH RECURSIVE query, and down the organization chart
curl localhost:3000/employees/8/managers
curl localhost:3000/employees/chart

# A playlist: created with a generated key, renamed, deleted
curl -X POST localhost:3000/playlists -H 'content-type: application/json' \
     -d '{"name": "Road trip", "track_ids": [1, 2, 3]}'
curl -X PUT localhost:3000/playlists/19 -H 'content-type: application/json' -d '{"name": "Long road trip"}'
curl -X DELETE localhost:3000/playlists/19

# The SQL of a view, with the override
curl localhost:3000/explain/Discography

# GraphQL: open http://localhost:3000/graphql in a browser, or
curl localhost:3000/graphql -H 'content-type: application/json' \
     -d '{"query": "{ employee(key: 8) { first_name manager { first_name manager { first_name } } } }"}'
```

## Change the schema, see the build fail

Remove the `title` column of `album` from `mabat/schema.json` and run `cargo build -p mabat-example-chinook`:

```text
error: mabat error[M0202]: Discography.albums: column `album.title` is not in the schema (mabat/schema.json)
```

In an application, `mabat schema --check` in CI keeps the snapshot in step with the database, and the manifest
test keeps `mabat/views.json` in step with the views. `MABAT_SKIP_CHECK=1` skips the check while both change.

## Test it

```sh
cargo test -p mabat-example-chinook
```

[`tests/api.rs`](tests/api.rs) calls every route on a new copy of the store.
