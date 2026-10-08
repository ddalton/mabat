//! The MPA specification (`docs/mpa.md`) and its index (`docs/mpa.json`) cover the crate:
//! every attribute the derive accepts and every error variant is listed, and the index is
//! up to date with the specification. Regenerate the index with `scripts/mpa-index.py`.

use std::collections::BTreeSet;
use std::path::Path;

fn read(path: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn index() -> serde_json::Value {
    serde_json::from_str(&read("docs/mpa.json")).expect("docs/mpa.json is JSON")
}

/// The values of `field` of the objects of the array `list` of the index.
fn listed(index: &serde_json::Value, list: &str, field: &str) -> BTreeSet<String> {
    index[list].as_array().unwrap().iter().map(|item| item[field].as_str().unwrap().to_string()).collect()
}

/// The quoted strings that follow `marker` in `source`.
fn quoted_after(source: &str, marker: &str) -> BTreeSet<String> {
    source.split(marker).skip(1).filter_map(|rest| rest.split('"').next()).map(str::to_string).collect()
}

#[test]
fn every_attribute_is_listed() {
    let derive = read("crates/mabat-derive/src/lib.rs");
    let mut accepted = quoted_after(&derive, "is_ident(\"");
    accepted.remove("view");
    let listed = listed(&index(), "attributes", "name");
    let missing: Vec<&String> = accepted.difference(&listed).collect();
    assert!(missing.is_empty(), "attributes of the derive missing from docs/mpa.json: {missing:?}");
}

#[test]
fn every_error_is_listed() {
    let errors = read("crates/mabat-sqlx/src/error.rs");
    // The variants: the identifiers that start the lines after each #[error(..)] attribute
    let mut variants = BTreeSet::new();
    let mut after_attribute = false;
    for line in errors.lines().map(str::trim) {
        if line.starts_with("#[error") {
            after_attribute = true;
        } else if after_attribute && line.starts_with(|c: char| c.is_ascii_uppercase()) {
            let name: String = line.chars().take_while(char::is_ascii_alphanumeric).collect();
            variants.insert(format!("mabat::Error::{name}"));
            after_attribute = false;
        }
    }
    assert!(variants.len() > 20, "found {variants:?}");
    let listed = listed(&index(), "errors", "variant");
    let missing: Vec<&String> = variants.difference(&listed).collect();
    assert!(missing.is_empty(), "errors missing from docs/mpa.json: {missing:?}");
    let extra: Vec<&String> = listed.difference(&variants).collect();
    assert!(extra.is_empty(), "errors in docs/mpa.json that the crate does not have: {extra:?}");
}

#[test]
fn the_index_is_up_to_date() {
    let spec = read("docs/mpa.md");
    let in_spec: BTreeSet<String> =
        quoted_after(&spec.replace("- **MPA-", "- \"MPA-").replace("** ", "\" "), "- \"").into_iter().collect();
    let in_index = listed(&index(), "rules", "id");
    assert_eq!(in_spec, in_index, "docs/mpa.json is stale: run scripts/mpa-index.py");

    // Every rule a rule or a table refers to exists
    let referred: BTreeSet<String> = spec
        .split("MPA-")
        .skip(1)
        .map(|rest| {
            let id: String =
                rest.chars().take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '-').collect();
            format!("MPA-{}", id.trim_end_matches('-'))
        })
        .filter(|id| id.chars().last().is_some_and(|c| c.is_ascii_digit()))
        .collect();
    let unknown: Vec<&String> = referred.difference(&in_index).collect();
    assert!(unknown.is_empty(), "docs/mpa.md refers to rules it does not define: {unknown:?}");
}
