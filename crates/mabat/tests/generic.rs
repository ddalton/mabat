//! Generic embedded structs on SQLite: one `Range<T>` instantiated with dates and integers, a
//! generic struct nested in another, loaded, saved, saved by changes and written as JSON; and the
//! value types they find at run time, the same as `#[derive(View)]` finds for concrete types.
#![cfg(feature = "sqlite")]

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use mabat::{Embedded, Mabat, View};
use sqlx::{Connection, Executor, SqliteConnection};
use uuid::Uuid;

#[derive(View, Debug, Clone, PartialEq)]
#[view(embedded)]
pub struct Range<T> {
    pub start: T,
    pub end: T,
}

/// A generic struct holding another.
#[derive(View, Debug, Clone, PartialEq)]
#[view(embedded)]
pub struct Labeled<T> {
    pub label: String,
    #[view(embed(prefix = "r_"))]
    pub range: Range<T>,
}

#[derive(View, Debug, Clone, PartialEq)]
#[view(table = "booking")]
pub struct Booking {
    pub id: i64,
    pub guest: String,
    #[view(embed(prefix = "stay_"))]
    pub stay: Range<NaiveDate>,
    #[view(embed(prefix = "guests_"))]
    pub guests: Range<Option<i32>>,
    #[view(embed(prefix = "price_"))]
    pub price: Labeled<f64>,
}

const SCHEMA: &str = "CREATE TABLE booking (
    id INTEGER PRIMARY KEY, guest TEXT NOT NULL,
    stay_start TEXT NOT NULL, stay_end TEXT NOT NULL,
    guests_start INTEGER, guests_end INTEGER,
    price_label TEXT NOT NULL, price_r_start REAL NOT NULL, price_r_end REAL NOT NULL
)";

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, d).unwrap()
}

fn booking() -> Booking {
    Booking {
        id: 1,
        guest: "Ada".into(),
        stay: Range { start: day(9), end: day(12) },
        guests: Range { start: Some(1), end: None },
        price: Labeled { label: "nightly".into(), range: Range { start: 90.0, end: 120.0 } },
    }
}

#[tokio::test]
async fn loads_saves_and_writes_json() {
    let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    conn.execute(SCHEMA).await.unwrap();

    let mut saved = booking();
    mabat::save(&mut saved, &mut conn).await.unwrap();
    let loaded = mabat::load::<Booking>().by_key(1_i64).one(&mut conn).await.unwrap();
    assert_eq!(loaded, saved);

    // Only what changed is written
    let mut after = loaded.clone();
    after.stay.end = day(14);
    after.price.range.end = 150.0;
    mabat::save_changes(&loaded, &mut after, &mut conn).await.unwrap();
    assert_eq!(mabat::load::<Booking>().by_key(1_i64).one(&mut conn).await.unwrap(), after);

    let json = mabat::load::<Booking>().json(&mut conn).await.unwrap();
    assert_eq!(json[0]["stay"]["end"], "2026-10-14");
    assert_eq!(json[0]["guests"]["end"], serde_json::Value::Null);
    assert_eq!(json[0]["price"]["range"]["end"], 150.0);

    // The generated queries check against the database
    let report = Mabat::<sqlx::Sqlite>::builder().register::<Booking>().check(&mut conn).await.unwrap();
    assert!(report.diagnostics().is_empty(), "{report}");
}

#[test]
fn each_instantiation_has_a_shape() {
    let dates = <Range<NaiveDate> as Embedded>::shape();
    let numbers = <Range<Option<i32>> as Embedded>::shape();
    assert_eq!(dates.name, "Range<NaiveDate>");
    assert_eq!(numbers.name, "Range<Option<i32>>");
    assert!(std::ptr::eq(dates, <Range<NaiveDate> as Embedded>::shape()), "built once");
    let ty = |shape: &mabat_core::EmbeddedShape| match &shape.kind {
        mabat_core::EmbeddedKind::Product { fields } => match fields[0].kind {
            mabat_core::FieldKind::Column { ty, .. } => ty,
            _ => unreachable!(),
        },
        _ => unreachable!(),
    };
    assert_eq!(ty(dates), mabat_core::ValueType::of::<NaiveDate>());
    assert!(ty(numbers).nullable);
}

/// Column types, as the derive writes their value type from the type as written.
#[derive(View)]
#[view(table = "everything", databases = "sqlite")]
pub struct Everything {
    pub id: i64,
    pub a: bool,
    pub b: i16,
    pub c: Option<i32>,
    pub d: u32,
    pub e: f32,
    pub f: String,
    pub g: Uuid,
    pub h: NaiveDate,
    pub i: NaiveTime,
    pub j: DateTime<Utc>,
    pub k: Option<NaiveDateTime>,
    pub l: Vec<u8>,
    pub n: serde_json::Value,
}

#[test]
fn run_time_value_types_match_the_derive() {
    use mabat_core::ValueType;
    let expected = [
        ValueType::of::<i64>(),
        ValueType::of::<bool>(),
        ValueType::of::<i16>(),
        ValueType::of::<Option<i32>>(),
        ValueType::of::<u32>(),
        ValueType::of::<f32>(),
        ValueType::of::<String>(),
        ValueType::of::<Uuid>(),
        ValueType::of::<NaiveDate>(),
        ValueType::of::<NaiveTime>(),
        ValueType::of::<DateTime<Utc>>(),
        ValueType::of::<Option<NaiveDateTime>>(),
        ValueType::of::<Vec<u8>>(),
        ValueType::of::<serde_json::Value>(),
    ];
    let derived: Vec<ValueType> = <Everything as View>::shape()
        .fields
        .iter()
        .map(|f| match f.kind {
            mabat_core::FieldKind::Column { ty, .. } => ty,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(derived, expected);
    // Lists, which SQLite does not store: a `Vec` of anything but bytes
    let list = ValueType::of::<Vec<String>>();
    assert!(list.list && list.scalar == mabat_core::Scalar::String);
}
