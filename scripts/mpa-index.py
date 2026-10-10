#!/usr/bin/env python3
"""Write docs/mpa.json, the machine-readable index of the MPA specification, from docs/mpa.md
and the capabilities, attributes, functions and errors listed here. Run it after changing either:

    python3 scripts/mpa-index.py

crates/mabat/tests/mpa.rs checks that the index covers the crate.
"""
import json
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parent.parent
SPEC = ROOT / "docs" / "mpa.md"

# Each rule, with the section it is in and its text
rules = []
section = None
text = SPEC.read_text()
for block in re.split(r"\n(?=- \*\*MPA-|## |### )", text):
    heading = re.match(r"#{2,3} (.+)", block)
    if heading:
        section = heading.group(1).strip()
    rule = re.match(r"- \*\*(MPA-[A-Z]+-\d+)\*\* (.+)", block, re.S)
    if rule:
        body = rule.group(2).split("\n\n")[0]
        body = re.sub(r"\s*\n\s*", " ", body).strip()
        rules.append({"id": rule.group(1), "section": section, "text": body})
ids = {r["id"] for r in rules}

capabilities = [
    ("typed-views", "Declare the shape of nested data with Rust structs deriving View", ["#[derive(View)]", "#[view(table, key)]"], ["MPA-CORE-1", "MPA-VIEW-1"]),
    ("batched-loading", "Load a view with one batched query per relationship, never one per row", ["mabat::load"], ["MPA-PLAN-1"]),
    ("multiple-databases", "PostgreSQL, MySQL 8+ and SQLite, chosen by features and by the connection", ["features: postgres, mysql, sqlite", "#[view(databases)]"], ["MPA-DB-1", "MPA-DB-2"]),
    ("embedded-values", "Structs stored in columns of the containing view, generic or not", ["#[view(embedded)]", "#[view(embed(prefix))]", "mabat::GenericColumn"], ["MPA-VIEW-3", "MPA-VIEW-4", "MPA-VIEW-7"]),
    ("enums-with-data", "Rust enums with data, stored in columns or in a table per variant, decoded strictly", ["#[view(tag, strategy, lenient)]", "#[view(tag_value)]"], ["MPA-SUM-1", "MPA-SUM-2", "MPA-SUM-3", "MPA-SUM-4"]),
    ("json-columns", "Columns decoded and written as JSON with serde", ["#[view(json)]"], ["MPA-VIEW-6"]),
    ("to-one-references", "References to other views by foreign key, owned, boxed, shared or into a graph", ["#[view(to_one(fk))]", "Box<T>"], ["MPA-VIEW-8"]),
    ("collections", "To-many collections, ordered, placed by index, or as maps", ["#[view(child(fk, order_by, index, key))]"], ["MPA-VIEW-9", "MPA-VIEW-10"]),
    ("many-to-many", "Collections through a link table", ["#[view(child(through, target))]"], ["MPA-VIEW-9"]),
    ("recursive-views", "Trees and chains of parents by depth-limited levels or one WITH RECURSIVE query", ["#[view(child(depth))]", "#[view(child(recursive = \"cte\"))]", "#[view(to_one(depth))]", "#[view(to_one(recursive = \"cte\"))]"], ["MPA-PLAN-4", "MPA-LOAD-12", "MPA-VIEW-8"]),
    ("shared-values", "Arc<T> values decoded once per entity and shared", ["Arc<T>"], ["MPA-LOAD-13"]),
    ("graphs", "Cyclic data as a Graph of entities with typed references", ["Ref<T>", "Load::graph", "Graph"], ["MPA-LOAD-14"]),
    ("filters-and-paging", "Filters, ordering, paging and counting of the root rows", ["mabat::filter::col", "Load::filter", "Load::order_by", "Load::limit", "Load::offset", "Load::count"], ["MPA-LOAD-4", "MPA-LOAD-5", "MPA-LOAD-6"]),
    ("nested-arguments", "Filter, order and page the elements of a collection per parent, in one query", ["Load::nested", "Nested"], ["MPA-LOAD-9", "MPA-LOAD-10"]),
    ("streaming", "Load many values a batch at a time, as a stream, holding one batch in memory", ["Load::stream", "Load::json_stream", "Load::batch_size"], ["MPA-LOAD-15", "MPA-LOAD-16", "MPA-LOAD-17", "MPA-LOAD-18"]),
    ("concurrent-loads", "Run the queries of each level concurrently on a pool, optionally in one snapshot", ["Pooled::snapshot", "Pooled::read_committed"], ["MPA-LOAD-11"]),
    ("sql-overrides", "Replace any query of a view with tuned SQL from a file, checked at startup", ["Mabat::builder", "overrides_dir", "overrides_sql", "-- mabat: query <name>"], ["MPA-OVR-1", "MPA-OVR-2", "MPA-OVR-3", "MPA-OVR-4", "MPA-OVR-5"]),
    ("shadow-mode", "Run an override next to the generated query and log differences", ["shadow", "Mabat::shadow_stats"], ["MPA-OVR-6"]),
    ("override-reload", "Reload override files without restarting", ["Mabat::reload"], ["MPA-OVR-7"]),
    ("schema-snapshots", "A snapshot of the database's schema, its drift, and views checked against it without a database", ["mabat schema", "mabat::schema::snapshot", "mabat check --snapshot", "Manifest::check_snapshot", "mabat_check::build"], ["MPA-SCH-1", "MPA-SCH-2", "MPA-SCH-3", "MPA-SCH-4", "MPA-SCH-5", "MPA-SCH-6", "MPA-SCH-7"]),
    ("dba-tooling", "A manifest of the views and the mabat command line tool: check, explain, scaffold", ["Builder::manifest", "mabat check", "mabat explain", "mabat scaffold"], ["MPA-OVR-8"]),
    ("report-queries", "Reports: root SQL with named parameters and computed fields", ["Load::sql", "Load::bind", "#[view(computed)]"], ["MPA-LOAD-19", "MPA-VIEW-13", "MPA-OVR-3"]),
    ("tracing", "tracing spans for each operation, query and statement, with names, rows and times", ["tracing", "RUST_LOG=mabat=debug"], ["MPA-DB-6"]),
    ("json-loading", "Load views as JSON, whole or a selection of their fields", ["Load::json", "Load::select", "Selection::parse", "Load::graph_json"], ["MPA-JSON-1", "MPA-JSON-3", "MPA-JSON-4", "MPA-JSON-5", "MPA-JSON-7"]),
    ("graphql", "A GraphQL schema generated from views, each root field one load", ["mabat_graphql::schema"], ["MPA-GQL-1", "MPA-GQL-2", "MPA-GQL-3", "MPA-GQL-4"]),
    ("save-aggregates", "Save a value and everything it owns, creating or replacing rows by key", ["mabat::save"], ["MPA-WRITE-1", "MPA-WRITE-3", "MPA-WRITE-4", "MPA-WRITE-5", "MPA-WRITE-6"]),
    ("save-changes", "Save only what changed between two values of an aggregate", ["mabat::save_changes"], ["MPA-WRITE-8"]),
    ("optimistic-locking", "Version columns that make stale writes fail", ["#[view(version)]", "Error::Conflict"], ["MPA-WRITE-9", "MPA-WRITE-10"]),
    ("save-many", "Save many values with statements per table and level, not per row", ["mabat::save_all"], ["MPA-WRITE-19"]),
    ("save-graphs", "Save every entity of a graph, ordered by its references, cycles included", ["mabat::save_graph", "Graph::new", "Graph::insert"], ["MPA-WRITE-14", "MPA-WRITE-15", "MPA-WRITE-16", "MPA-WRITE-17", "MPA-WRITE-18"]),
    ("save-graph-changes", "Save only the entities of a graph that were inserted or handed out by get_mut", ["mabat::save_graph_changes", "Graph::is_changed"], ["MPA-WRITE-20"]),
    ("generated-keys", "Keys generated by the database on insert, written back into the value", ["#[view(generated)]", "mabat::save"], ["MPA-WRITE-13", "MPA-WRITE-10"]),
    ("delete-aggregates", "Delete a value and everything it owns", ["mabat::delete"], ["MPA-WRITE-7"]),
]

attributes = [
    ("table", "struct, variant", '#[view(table = "t")]', "The table of a view, or of a variant", ["MPA-VIEW-1", "MPA-SUM-3"]),
    ("key", "struct, variant", '#[view(key = "c")]', "The key column, id by default", ["MPA-VIEW-2", "MPA-SUM-3"]),
    ("embedded", "struct", "#[view(embedded)]", "A struct stored in columns of the containing view, generic over types or not", ["MPA-VIEW-3", "MPA-VIEW-4"]),
    ("databases", "struct, enum", '#[view(databases = "postgres, mysql")]', "Limit the databases a view is decoded and encoded on", ["MPA-DB-2"]),
    ("tag", "enum", '#[view(tag = "c")]', "The column naming the variant", ["MPA-SUM-1"]),
    ("strategy", "enum", '#[view(strategy = "tag" | "table_per_variant")]', "Where the variants' data is stored", ["MPA-SUM-2", "MPA-SUM-3"]),
    ("lenient", "enum", "#[view(lenient)]", "Allow non-NULL columns of other variants", ["MPA-SUM-4"]),
    ("tag_value", "variant", '#[view(tag_value = "v")]', "The tag of a variant, its name by default", ["MPA-SUM-1"]),
    ("column", "field", '#[view(column = "c")]', "The column of a field, its name by default", ["MPA-VIEW-5"]),
    ("json", "field", "#[view(json)]", "A column decoded and written as JSON", ["MPA-VIEW-6"]),
    ("version", "field", "#[view(version)]", "An integer version column for optimistic locking", ["MPA-WRITE-9", "MPA-WRITE-10"]),
    ("generated", "field", "#[view(generated)]", "A key the database generates when the row is inserted", ["MPA-WRITE-13"]),
    ("computed", "field", "#[view(computed)]", "A value SQL computes, not a column of the table", ["MPA-VIEW-13"]),
    ("embed", "field", "#[view(embed)]", "An embedded struct or enum", ["MPA-VIEW-7"]),
    ("prefix", "field", '#[view(embed(prefix = "p_"))]', "The column prefix of an embedded value", ["MPA-VIEW-7"]),
    ("to_one", "field", '#[view(to_one(fk = "c"))]', "A reference to another view", ["MPA-VIEW-8"]),
    ("child", "field", '#[view(child(fk = "c"))]', "A to-many collection", ["MPA-VIEW-9", "MPA-VIEW-10"]),
    ("fk", "to_one, child", 'fk = "c"', "The foreign key column", ["MPA-VIEW-8", "MPA-VIEW-9"]),
    ("order_by", "child", 'order_by = "a, b desc"', "The order of the elements", ["MPA-VIEW-9"]),
    ("through", "child", 'through = "link"', "The link table of a many-to-many collection", ["MPA-VIEW-9"]),
    ("target", "child", 'target = "c"', "The link table column referencing the element", ["MPA-VIEW-9"]),
    ("index", "child", 'index = "c"', "The column placing the elements of a Vec", ["MPA-VIEW-9"]),
    ("key", "child", 'key = "c"', "The column keying the elements of a map", ["MPA-VIEW-9"]),
    ("depth", "child, to_one", "depth = n", "The most levels of a recursive collection or reference", ["MPA-VIEW-8", "MPA-VIEW-9", "MPA-PLAN-4"]),
    ("recursive", "child, to_one", 'recursive = "cte"', "Load all levels with one WITH RECURSIVE query", ["MPA-VIEW-8", "MPA-VIEW-9", "MPA-PLAN-4"]),
]

functions = [
    ("mabat::load::<T>()", "Start a load of the generated queries", ["MPA-LOAD-1"]),
    ("Load::by_key / by_keys", "Load by keys", ["MPA-LOAD-3"]),
    ("Load::filter", "Filter the root rows", ["MPA-LOAD-4"]),
    ("Load::order_by / order_by_desc / limit / offset", "Order and page the root rows", ["MPA-LOAD-6"]),
    ("Load::nested", "Arguments of a nested collection", ["MPA-LOAD-9"]),
    ("Load::select", "Load a selection of fields, as JSON", ["MPA-JSON-3"]),
    ("Load::sql", "Run SQL as the root query, for reports", ["MPA-LOAD-19"]),
    ("Load::bind", "Bind a named parameter of the root query's SQL", ["MPA-LOAD-19"]),
    ("Load::all / one / optional / count", "Run a load", ["MPA-LOAD-2"]),
    ("Load::graph", "Load a graph view", ["MPA-LOAD-14"]),
    ("Load::json", "Load as JSON", ["MPA-JSON-1"]),
    ("Load::graph_json", "Load a graph as JSON, each entity once with $id and $ref", ["MPA-JSON-7"]),
    ("Load::stream", "Load a batch at a time, as a stream", ["MPA-LOAD-15", "MPA-LOAD-16", "MPA-LOAD-17", "MPA-LOAD-18"]),
    ("Load::json_stream", "Load as JSON a batch at a time, as a stream", ["MPA-LOAD-17"]),
    ("Load::batch_size", "The number of values a stream loads at a time", ["MPA-LOAD-15"]),
    ("mabat::save", "Save an aggregate", ["MPA-WRITE-3"]),
    ("mabat::save_changes", "Save what changed", ["MPA-WRITE-8"]),
    ("mabat::save_graph", "Save every entity of a graph", ["MPA-WRITE-14", "MPA-WRITE-15"]),
    ("mabat::save_all", "Save many values, batched by table and level", ["MPA-WRITE-19"]),
    ("mabat::save_graph_changes", "Save the changed entities of a graph", ["MPA-WRITE-20"]),
    ("Graph::is_changed", "Whether save_graph_changes writes an entity", ["MPA-WRITE-20"]),
    ("mabat::schema::snapshot", "A snapshot of the database's schema", ["MPA-SCH-1"]),
    ("Manifest::check_snapshot", "Check the views against a snapshot of the schema, without a database", ["MPA-SCH-4", "MPA-SCH-5", "MPA-SCH-6"]),
    ("mabat_check::build", "Check the views against a snapshot from a build script", ["MPA-SCH-7"]),
    ("Graph::insert", "Add an entity to a graph, for saving", ["MPA-LOAD-14", "MPA-WRITE-14"]),
    ("mabat::delete", "Delete an aggregate", ["MPA-WRITE-7"]),
    ("mabat::plan", "The plan of a view", ["MPA-PLAN-5"]),
    ("mabat::scaffold", "An override file with the generated SQL", ["MPA-OVR-8"]),
    ("Mabat::builder / Builder::register / overrides_dir / overrides / overrides_sql / on_invalid", "Configure a registry", ["MPA-OVR-1", "MPA-OVR-5"]),
    ("Builder::check / build", "Check and build a registry", ["MPA-OVR-5"]),
    ("Builder::manifest", "The manifest of the views", ["MPA-OVR-8"]),
    ("Mabat::load / explain / report / reload / shadow_stats", "Use a registry", ["MPA-OVR-1", "MPA-PLAN-5", "MPA-OVR-6", "MPA-OVR-7"]),
    ("Pooled::snapshot / read_committed", "Concurrent loads on a pool", ["MPA-LOAD-11"]),
    ("Selection::new / field / nested / parse", "Build a selection", ["MPA-JSON-3"]),
    ("Graph::root / roots / get / get_mut / all / count", "Read a graph", ["MPA-LOAD-14"]),
    ("mabat_graphql::schema / SchemaBuilder::list / by_key / registry / connections / finish / into_dynamic", "A GraphQL schema of views", ["MPA-GQL-1"]),
]

errors = [
    ("Plan", ["MPA-PLAN-4", "MPA-JSON-4"]), ("Query", []), ("Decode", ["MPA-VIEW-5"]),
    ("MissingReference", ["MPA-VIEW-8"]), ("NullTag", ["MPA-SUM-4"]), ("UnknownTag", ["MPA-SUM-4"]),
    ("OtherVariantColumn", ["MPA-SUM-4"]), ("MissingVariant", ["MPA-SUM-4"]), ("ListIndex", ["MPA-VIEW-9"]),
    ("DuplicateMapKey", ["MPA-VIEW-9"]), ("Cycle", ["MPA-LOAD-12"]), ("UnloadedReference", ["MPA-LOAD-14"]),
    ("GraphRequired", ["MPA-LOAD-14", "MPA-JSON-5"]), ("MixedKeys", ["MPA-LOAD-3"]), ("NotFound", ["MPA-LOAD-2"]),
    ("TooManyRows", ["MPA-LOAD-2"]), ("NestedArguments", ["MPA-LOAD-9"]), ("SelectionWithoutJson", ["MPA-JSON-6"]),
    ("Json", ["MPA-JSON-2"]), ("ColumnNotSelected", ["MPA-LOAD-7", "MPA-LOAD-10"]), ("KeysRequired", ["MPA-OVR-4"]), ("Params", ["MPA-LOAD-19", "MPA-VIEW-13"]),
    ("Invalid", ["MPA-OVR-5"]), ("Check", ["MPA-OVR-5"]), ("NotRegistered", ["MPA-OVR-1"]),
    ("WrongBackend", ["MPA-DB-3"]), ("ManifestBackend", ["MPA-OVR-8"]), ("Connection", ["MPA-LOAD-11"]),
    ("Write", ["MPA-WRITE-2", "MPA-WRITE-11", "MPA-WRITE-13", "MPA-WRITE-14", "MPA-WRITE-15", "MPA-WRITE-16"]), ("Conflict", ["MPA-WRITE-9"]),
]

diagnostics = [
    ("M0100", "error", "An override file cannot be read or parsed"),
    ("M0101", "error", "An override names no registered view, or no query of its view"),
    ("M0102", "error", "A query's columns do not match the view"),
    ("M0103", "error", "A query does not prepare on the database"),
    ("M0104", "error", "A query has the wrong parameters"),
    ("M0105", "warning", "A query does not select every optional path"),
    ("M0201", "error", "A query reads a table that is not in the schema snapshot"),
    ("M0202", "error", "A query reads a column that is not in the schema snapshot"),
    ("M0203", "error", "A column has a type the field cannot be decoded from"),
    ("M0204", "warning", "A column is nullable under a field that is not an Option"),
    ("M0205", "error", "The columns linking a query to its parent hold different kinds of key"),
    ("M0206", "warning", "A view's key column is not the primary key of its table"),
    ("M0207", "error", "A generated key is on a column the database does not generate"),
    ("M0301", "error", "A view cannot be planned"),
]

def check(refs):
    unknown = [r for r in refs if r not in ids]
    assert not unknown, f"unknown rules {unknown}"
    return refs

index = {
    "spec": "MPA",
    "name": "Mabat Persistence Architecture",
    "version": "0.1",
    "crates": {"mabat": "0.1", "mabat-graphql": "0.1", "mabat-cli": "0.1"},
    "document": "docs/mpa.md",
    "databases": ["postgres", "mysql", "sqlite"],
    "capabilities": [{"id": c, "summary": s, "api": a, "rules": check(r)} for c, s, a, r in capabilities],
    "attributes": [{"name": n, "on": o, "syntax": x, "summary": s, "rules": check(r)} for n, o, x, s, r in attributes],
    "functions": [{"api": a, "summary": s, "rules": check(r)} for a, s, r in functions],
    "errors": [{"variant": f"mabat::Error::{v}", "rules": check(r)} for v, r in errors],
    "diagnostics": [{"code": c, "severity": sev, "summary": s} for c, sev, s in diagnostics],
    # What is not supported, without the rules removed since (MPA-DOC-2)
    "unsupported": [r for r in rules if r["id"].startswith("MPA-NOT-") and not r["text"].startswith("Removed")],
    "rules": rules,
}
(ROOT / "docs" / "mpa.json").write_text(json.dumps(index, indent=2, ensure_ascii=False) + "\n")
print(f"{len(rules)} rules, {len(capabilities)} capabilities, {len(attributes)} attributes, {len(errors)} errors")
