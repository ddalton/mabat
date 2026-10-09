# mabat-check

Checks [Mabat](https://github.com/ddalton/mabat) views against a snapshot of a database's schema, without a
database: the manifest of the views, override files and diagnostics, with no database driver.

It is what `mabat check --snapshot` runs, and what a build script runs to fail `cargo build` when the views no
longer match the schema:

```rust,ignore
// build.rs, with mabat-check in [build-dependencies]
fn main() {
    mabat_check::build("mabat/views.json", "mabat/schema.json").overrides("mabat/overrides").run();
}
```

Errors fail the build and warnings are shown by Cargo. Set `MABAT_SKIP_CHECK=1` to skip the check while the
schema and the views change together. See the [CLI guide](https://ddalton.github.io/mabat/guides/cli/) for how
the manifest and the snapshot are kept up to date.

Applications use the [`mabat`](https://crates.io/crates/mabat) crate, which re-exports the manifest and the
report.
