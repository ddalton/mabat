# mabat-cli

The `mabat` command line tool for [Mabat](https://github.com/ddalton/mabat). It lets a DBA check,
explain and scaffold override SQL against the manifest an application writes of its views, with no Rust
toolchain.

```sh
cargo install mabat-cli

mabat check    --manifest mabat/views.json --overrides mabat/overrides --database-url postgres://...
mabat check    --manifest mabat/views.json --overrides mabat/overrides --schema schema.sql
mabat explain  --manifest mabat/views.json --overrides mabat/overrides
mabat scaffold --manifest mabat/views.json --view TaskView --format sql
```

`check` exits with 0 when there are no errors, 1 when the checks find errors, and 2 for any other problem.
With `--schema`, the schema is created in a transaction that is rolled back, so any scratch database works.
