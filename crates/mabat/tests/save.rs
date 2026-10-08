//! Saving views with PostgreSQL types: UUID keys, timestamps, nested embedded structs and
//! references, and the errors of what cannot be saved.

mod common;

use common::fixture::{Address, Geo, PersonView, SubtaskView, TaskView, id, setup};
use mabat::{Error, View};

#[tokio::test]
async fn tasks_round_trip() {
    let Some(mut db) = setup("save_tasks").await else { return };

    let task = TaskView {
        id: id(0x99),
        name: "Benchmark".into(),
        description: Some("Measure the parser".into()),
        created_at: "2026-02-03T04:05:06Z".parse().unwrap(),
        address: Address {
            street: "2 Side St".into(),
            city: "Shelbyville".into(),
            geo: Geo { lat: Some(10.5), lon: None },
        },
        assignee: Some(PersonView { id: 2, name: "Alan Turing".into(), email: None }),
        children: Vec::new(),
    };
    mabat::save(&task, &mut db.conn).await.unwrap();
    let loaded = mabat::load::<TaskView>().by_key(id(0x99)).one(&mut db.conn).await.unwrap();
    assert_eq!(loaded, task);

    // The referenced person is a separate aggregate: its row is not written
    let mut renamed = task;
    renamed.assignee = Some(PersonView { id: 2, name: "Someone else".into(), email: None });
    mabat::save(&renamed, &mut db.conn).await.unwrap();
    let person = mabat::load::<PersonView>().by_key(2_i64).one(&mut db.conn).await.unwrap();
    assert_eq!(person.name, "Alan Turing");

    // A collection of a view without a key field cannot be saved
    renamed.children = vec![SubtaskView { name: "Profile".into(), position: 0, notes: Vec::new() }];
    let error = mabat::save(&renamed, &mut db.conn).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "SubtaskView cannot be written: it has no key field, so its collection cannot be saved"
    );

    db.drop().await;
}

/// A column type that can be decoded but not encoded.
#[derive(Debug, PartialEq)]
struct Code(String);

impl sqlx::Type<sqlx::Postgres> for Code {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for Code {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        Ok(Code(<String as sqlx::Decode<sqlx::Postgres>>::decode(value)?))
    }
}

#[derive(View, Debug)]
#[view(table = "tag", key = "code", databases = "postgres")]
struct TagCode {
    code: Code,
    label: String,
}

#[tokio::test]
async fn columns_without_encode_fail_when_saved() {
    let Some(mut db) = setup("save_encode").await else { return };
    let tag = mabat::load::<TagCode>().by_key("bug").one(&mut db.conn).await.unwrap();
    assert_eq!(tag.label, "Bug");
    let error = mabat::save(&tag, &mut db.conn).await.unwrap_err();
    assert!(matches!(error, Error::Write { view: "TagCode", .. }), "{error}");
    assert!(error.to_string().contains("save::Code does not implement sqlx::Encode"), "{error}");
    db.drop().await;
}
