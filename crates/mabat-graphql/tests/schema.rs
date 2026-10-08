//! The generated schema and its queries, on an in-memory SQLite database.
// The views are only read through GraphQL
#![allow(dead_code)]

use async_graphql::dynamic::Schema;
use mabat::View;
use serde_json::json;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Executor, SqlitePool};

const SCHEMA: &str = r#"
CREATE TABLE person (id INTEGER PRIMARY KEY, name TEXT NOT NULL, born DATE);
CREATE TABLE project (
    id INTEGER PRIMARY KEY, name TEXT NOT NULL, lead_id INTEGER REFERENCES person (id),
    priority TEXT NOT NULL, state TEXT NOT NULL, blocked_reason TEXT, done_at TEXT,
    budget_amount REAL, budget_currency TEXT, parent_id INTEGER REFERENCES project (id)
);
CREATE TABLE setting (id INTEGER PRIMARY KEY, project_id INTEGER NOT NULL, name TEXT NOT NULL, value TEXT NOT NULL);

INSERT INTO person VALUES (1, 'Ada', '1815-12-10'), (2, 'Grace', NULL);
INSERT INTO project VALUES
    (1, 'Compiler', 2,    'high', 'active',  NULL,       NULL,                  1000.5, 'EUR', NULL),
    (2, 'Parser',   1,    'low',  'blocked', 'waiting',  NULL,                  NULL,   NULL,  1),
    (3, 'Linker',   NULL, 'high', 'done',    NULL,       '2026-01-02T03:04:05Z', 20.0,   'USD', 1),
    (4, 'Lexer',    1,    'low',  'active',  NULL,       NULL,                  NULL,   NULL,  2);
INSERT INTO setting VALUES (1, 1, 'lang', 'rust'), (2, 1, 'edition', '2024'), (3, 2, 'lang', 'c');
"#;

#[derive(View, Debug)]
#[view(table = "project", databases = "sqlite")]
struct Project {
    id: i64,
    name: String,
    #[view(to_one(fk = "lead_id"))]
    lead: Option<Person>,
    #[view(embed)]
    priority: Priority,
    #[view(embed)]
    state: State,
    #[view(embed(prefix = "budget_"))]
    budget: Budget,
    #[view(child(fk = "parent_id", order_by = "id", depth = 3))]
    subprojects: Vec<Project>,
    #[view(child(fk = "project_id", key = "name"))]
    settings: std::collections::BTreeMap<String, Setting>,
}

#[derive(View, Debug)]
#[view(table = "person", databases = "sqlite")]
struct Person {
    name: String,
    born: Option<chrono::NaiveDate>,
}

/// An enum without data: a GraphQL enum.
#[derive(View, Debug)]
#[view(tag = "priority", databases = "sqlite")]
enum Priority {
    #[view(tag_value = "low")]
    Low,
    #[view(tag_value = "high")]
    High,
}

/// An enum with data: a union.
#[derive(View, Debug)]
#[view(tag = "state", databases = "sqlite")]
enum State {
    #[view(tag_value = "active")]
    Active,
    #[view(tag_value = "blocked")]
    Blocked {
        #[view(column = "blocked_reason")]
        reason: String,
    },
    #[view(tag_value = "done")]
    Done(#[view(column = "done_at")] chrono::DateTime<chrono::Utc>),
}

#[derive(View, Debug)]
#[view(embedded, databases = "sqlite")]
struct Budget {
    amount: Option<f64>,
    currency: Option<String>,
}

#[derive(View, Debug)]
#[view(table = "setting", databases = "sqlite")]
struct Setting {
    value: String,
}

async fn setup() -> (SqlitePool, Schema) {
    // One connection that stays open, so the in-memory database lives as long as the pool
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .min_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    pool.execute(SCHEMA).await.unwrap();
    let schema = mabat_graphql::schema(&pool)
        .list::<Project>("projects")
        .by_key::<Project>("project")
        .list::<Person>("people")
        .finish()
        .unwrap();
    (pool, schema)
}

async fn query(schema: &Schema, query: &str) -> serde_json::Value {
    let response = schema.execute(query).await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    response.data.into_json().unwrap()
}

#[tokio::test]
async fn objects_enums_unions_maps_and_recursion() {
    let (_pool, schema) = setup().await;
    let data = query(
        &schema,
        r#"{
            project(key: 1) {
                name
                lead { name born }
                priority
                state { __typename ... on StateBlocked { reason } }
                budget { amount currency }
                settings { key value { value } }
                subprojects {
                    name
                    state {
                        __typename
                        ... on StateBlocked { _variant reason }
                        ... on StateDone { _0 }
                    }
                    subprojects { name lead { name } }
                }
            }
        }"#,
    )
    .await;
    assert_eq!(
        data,
        json!({
            "project": {
                "name": "Compiler",
                "lead": { "name": "Grace", "born": null },
                "priority": "High",
                "state": { "__typename": "StateActive" },
                "budget": { "amount": 1000.5, "currency": "EUR" },
                "settings": [{ "key": "edition", "value": { "value": "2024" } }, { "key": "lang", "value": { "value": "rust" } }],
                "subprojects": [
                    {
                        "name": "Parser",
                        "state": { "__typename": "StateBlocked", "_variant": "Blocked", "reason": "waiting" },
                        "subprojects": [{ "name": "Lexer", "lead": { "name": "Ada" } }],
                    },
                    {
                        "name": "Linker",
                        "state": { "__typename": "StateDone", "_0": "2026-01-02T03:04:05Z" },
                        "subprojects": [],
                    },
                ],
            }
        })
    );

    // A missing key is null, aliases name the answers
    let data = query(&schema, r#"{ missing: project(key: 99) { name } first: project(key: 2) { title: name } }"#).await;
    assert_eq!(data, json!({ "missing": null, "first": { "title": "Parser" } }));
}

#[tokio::test]
async fn where_order_by_limit_and_offset() {
    let (_pool, schema) = setup().await;
    let names = |data: serde_json::Value| -> Vec<String> {
        data["projects"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap().to_string()).collect()
    };

    let all = query(&schema, "{ projects(orderBy: [{ name: ASC }]) { name } }").await;
    assert_eq!(names(all), ["Compiler", "Lexer", "Linker", "Parser"]);

    let cases = [
        (r#"where: { name: { eq: "Parser" } }"#, vec!["Parser"]),
        (r#"where: { id: { in: [1, 3] } }"#, vec!["Compiler", "Linker"]),
        (r#"where: { name: { ilike: "%ER" } }"#, vec!["Compiler", "Lexer", "Linker", "Parser"]),
        (r#"where: { name: { like: "L%" }, id: { gt: 3 } }"#, vec!["Lexer"]),
        (r#"where: { or: [{ id: { le: 1 } }, { name: { eq: "Lexer" } }] }"#, vec!["Compiler", "Lexer"]),
        (r#"where: { not: { name: { notIn: ["Parser", "Linker"] } } }"#, vec!["Linker", "Parser"]),
        (r#"where: { and: [{ id: { ge: 2 } }, { id: { lt: 4 } }] }"#, vec!["Linker", "Parser"]),
    ];
    for (arguments, expected) in cases {
        let data = query(&schema, &format!("{{ projects({arguments}, orderBy: [{{ name: ASC }}]) {{ name }} }}")).await;
        assert_eq!(names(data), expected, "{arguments}");
    }

    let page = query(&schema, "{ projects(orderBy: [{ name: DESC }], limit: 2, offset: 1) { name } }").await;
    assert_eq!(names(page), ["Linker", "Lexer"]);

    let dated = query(&schema, r#"{ people(where: { born: { lt: "1900-01-01" } }) { name } }"#).await;
    assert_eq!(dated, json!({ "people": [{ "name": "Ada" }] }));

    let response = schema.execute("{ projects(limit: -1) { name } }").await;
    assert_eq!(response.errors[0].message, "limit cannot be negative");
}

#[tokio::test]
async fn the_schema_describes_the_views() {
    let (_pool, schema) = setup().await;
    let sdl = schema.sdl();
    for expected in [
        "type Project {\n\tid: BigInt!\n\tname: String!\n\tlead: Person\n\tpriority: Priority!\n\tstate: State!\n\tbudget: Budget!\n\tsubprojects: [Project!]!\n\tsettings: [SettingEntry!]!\n}",
        "enum Priority {\n\tLow\n\tHigh\n}",
        "union State = StateActive | StateBlocked | StateDone",
        "type StateBlocked {\n\t_variant: String!\n\treason: String!\n}",
        "type SettingEntry {\n\tkey: String!\n\tvalue: Setting!\n}",
        "projects(where: ProjectWhere, orderBy: [ProjectOrderBy!], limit: Int, offset: Int): [Project!]!",
        "project(key: BigInt!): Project",
        "input StringFilter {\n\teq: String\n\tne: String\n\tisNull: Boolean\n\tlt: String\n\tle: String\n\tgt: String\n\tge: String\n\tin: [String!]\n\tnotIn: [String!]\n\tlike: String\n\tilike: String\n}",
        "scalar Date",
    ] {
        assert!(sdl.contains(expected), "missing:\n{expected}\n\nin:\n{sdl}");
    }
}

#[tokio::test]
#[ignore = "prints the schema"]
async fn print_sdl() {
    let (_pool, schema) = setup().await;
    println!("{}", schema.sdl());
}
