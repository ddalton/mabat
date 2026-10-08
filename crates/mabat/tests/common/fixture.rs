//! The schema, views and data shared by the database tests.

use super::TestDb;
use chrono::{DateTime, Utc};
use mabat::View;
use uuid::Uuid;

pub const SCHEMA: &str = r#"
CREATE TABLE person (
    id        BIGINT PRIMARY KEY,
    full_name TEXT NOT NULL,
    email     TEXT
);
CREATE TABLE tag (
    code  TEXT PRIMARY KEY,
    label TEXT NOT NULL
);
CREATE TABLE task (
    id           UUID PRIMARY KEY,
    name         TEXT NOT NULL,
    description  TEXT,
    addr_street  TEXT NOT NULL DEFAULT '',
    addr_city    TEXT NOT NULL DEFAULT '',
    addr_geo_lat DOUBLE PRECISION,
    addr_geo_lon DOUBLE PRECISION,
    parent_id    UUID REFERENCES task (id),
    assignee_id  BIGINT REFERENCES person (id),
    position     INTEGER NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE task_note (
    id       SERIAL PRIMARY KEY,
    task_id  UUID NOT NULL REFERENCES task (id),
    body     TEXT NOT NULL,
    tag_code TEXT REFERENCES tag (code)
);
"#;

#[derive(View, Debug, PartialEq)]
#[view(table = "task")]
pub struct TaskView {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    #[view(embed(prefix = "addr_"))]
    pub address: Address,
    #[view(to_one(fk = "assignee_id"))]
    pub assignee: Option<PersonView>,
    #[view(child(fk = "parent_id", order_by = "position, name"))]
    pub children: Vec<SubtaskView>,
}

#[derive(View, Debug, PartialEq)]
#[view(embedded)]
pub struct Address {
    pub street: String,
    pub city: String,
    #[view(embed(prefix = "geo_"))]
    pub geo: Geo,
}

#[derive(View, Debug, PartialEq)]
#[view(embedded)]
pub struct Geo {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "person")]
pub struct PersonView {
    pub id: i64,
    #[view(column = "full_name")]
    pub name: String,
    pub email: Option<String>,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "task")]
pub struct SubtaskView {
    pub name: String,
    pub position: i32,
    #[view(child(fk = "task_id", order_by = "id"))]
    pub notes: Vec<NoteView>,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "task_note")]
pub struct NoteView {
    pub body: String,
    #[view(to_one(fk = "tag_code"))]
    pub tag: Option<TagView>,
}

#[derive(View, Debug, PartialEq)]
#[view(table = "tag", key = "code")]
pub struct TagView {
    pub code: String,
    pub label: String,
}

/// A view whose `description` is not optional, to test decoding errors.
#[derive(View, Debug)]
#[view(table = "task")]
pub struct StrictTaskView {
    pub description: String,
}

/// A view whose assignee is required, to test missing references.
#[derive(View, Debug)]
#[view(table = "task")]
pub struct AssignedTaskView {
    #[view(to_one(fk = "assignee_id"))]
    pub assignee: PersonView,
}

/// The task with the UUID 00000000-0000-0000-0000-0000000000NN, written as `id(0xNN)`
pub fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

pub async fn setup(name: &str) -> Option<TestDb> {
    let mut db = TestDb::new(name, SCHEMA).await?;
    db.execute(
        r#"
        INSERT INTO person (id, full_name, email) VALUES (1, 'Ada Lovelace', 'ada@example.com'), (2, 'Alan Turing', NULL);
        INSERT INTO tag (code, label) VALUES ('bug', 'Bug'), ('doc', 'Documentation');
        INSERT INTO task (id, name, description, addr_street, addr_city, addr_geo_lat, addr_geo_lon, assignee_id, created_at)
            VALUES ('00000000-0000-0000-0000-000000000001', 'Release', 'Ship 1.0', '1 Main St', 'Springfield', 1.5, -2.5, 1,
                    '2026-01-02T03:04:05Z');
        INSERT INTO task (id, name, parent_id, position) VALUES
            ('00000000-0000-0000-0000-000000000011', 'Write docs', '00000000-0000-0000-0000-000000000001', 2),
            ('00000000-0000-0000-0000-000000000012', 'Fix bugs',   '00000000-0000-0000-0000-000000000001', 0),
            ('00000000-0000-0000-0000-000000000013', 'Tag build',  '00000000-0000-0000-0000-000000000001', 1);
        INSERT INTO task_note (task_id, body, tag_code) VALUES
            ('00000000-0000-0000-0000-000000000012', 'Crash on start', 'bug'),
            ('00000000-0000-0000-0000-000000000012', 'Typo in error', 'bug'),
            ('00000000-0000-0000-0000-000000000011', 'Explain views', 'doc'),
            ('00000000-0000-0000-0000-000000000011', 'Untagged', NULL);
        -- a second root with its own children
        INSERT INTO task (id, name, assignee_id) VALUES ('00000000-0000-0000-0000-000000000002', 'Hiring', 2);
        INSERT INTO task (id, name, parent_id, position) VALUES
            ('00000000-0000-0000-0000-000000000021', 'Interview', '00000000-0000-0000-0000-000000000002', 0);
        "#,
    )
    .await;
    Some(db)
}
