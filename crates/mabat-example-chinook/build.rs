// `cargo build` fails when the views no longer match the schema: the manifest of the views,
// kept up to date by `tests/files.rs`, is checked against the snapshot of the schema.
fn main() {
    mabat_check::build("mabat/views.json", "mabat/schema.json").overrides("mabat/overrides").run();
}
