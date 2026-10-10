# Releasing

1. **Version.** Update `version` in the workspace `Cargo.toml` and the `version` of the internal
   dependencies in `[workspace.dependencies]`.
2. **Changelog.** Move the `[Unreleased]` entries of `CHANGELOG.md` under the new version, with the date.
3. **Checks.** Run the tests against PostgreSQL, then the dry run:

   ```sh
   scripts/with-postgres.sh
   cargo publish --workspace --dry-run
   ```

   CI also builds with the minimum Rust version, 1.94, and runs the dry run.
4. **Publish.** `cargo publish --workspace` publishes the crates in dependency order: mabat-core,
   mabat-check, mabat-derive, mabat-sqlx, mabat, then mabat-graphql and mabat-cli. Then tag the release (`git tag v0.1.0`) and
   push the tag.

The derive macro finds the `mabat` crate under whatever name an application depends on it; `FACADE` in
`crates/mabat-derive/src/lib.rs` holds its package name.

A published version can be yanked but never deleted.
