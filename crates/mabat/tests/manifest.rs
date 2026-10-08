//! The manifest of views, for checking overrides without the application.

mod common;

use common::fixture::*;
use mabat::Mabat;
use mabat::manifest::{LinkManifest, Manifest, Role};

#[test]
fn describes_every_query_and_column() {
    let manifest = Mabat::<sqlx::Postgres>::builder().register::<TaskView>().manifest().unwrap();
    let view = manifest.view("TaskView").unwrap();
    let names: Vec<&str> = view.queries.iter().map(|q| q.name.as_str()).collect();
    assert_eq!(names, ["$root", "assignee", "children", "children.notes", "children.notes.tag"]);

    let root = &view.queries[0];
    assert_eq!((root.parent, &root.link, root.key_alias.as_str()), (None, &LinkManifest::Root, "id"));
    let description = root.columns.iter().find(|c| c.alias == "description").unwrap();
    assert_eq!(description.role, Role::Field);
    assert!(description.optional);
    let ty = description.r#type.as_ref().unwrap();
    assert_eq!((ty.rust.as_str(), ty.sql.as_str()), ("Option<String>", "TEXT"));
    assert!(ty.accepts.contains(&"VARCHAR".to_string()));
    let assignee = root.columns.iter().find(|c| c.alias == "$ref.assignee").unwrap();
    assert_eq!((assignee.role, assignee.r#type.is_none()), (Role::Reference, true));

    let tag = &view.queries[4];
    assert_eq!(tag.parent, Some(3));
    assert_eq!(tag.link, LinkManifest::ToOne { ref_alias: "$ref.tag".into() });
    assert!(tag.sql.starts_with("SELECT t0.\"code\" AS \"code\",\n"), "{}", tag.sql);
}

#[test]
fn round_trips_through_json_and_files() {
    let manifest =
        Mabat::<sqlx::Postgres>::builder().register::<TaskView>().register::<PersonView>().manifest().unwrap();
    let json = manifest.to_json();
    assert!(json.contains("\"role\": \"reference\""), "{json}");
    assert_eq!(Manifest::from_json(&json).unwrap(), manifest);

    let path = std::env::temp_dir().join(format!("mabat-manifest-{}/views.json", uuid::Uuid::new_v4().simple()));
    assert!(manifest.write(&path).unwrap(), "a new file is written");
    assert!(!manifest.write(&path).unwrap(), "an up-to-date file is left as is");
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
