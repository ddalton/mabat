//! Chinook through a generated GraphQL schema on every database, compared with typed loads.

use std::sync::Arc;

use mabat::Mabat;
use mabat::filter::col;
use mabat_e2e::Dataset;
use serde_json::{Value, json};

const INVOICES: &str = r#"{
    invoices(where: { total: { gt: 10 }, invoice_id: { in: [88, 89, 201, 207, 299, 404, 409, 411] } }, orderBy: [{ total: DESC }, { invoice_id: ASC }], limit: 5, offset: 1) {
        invoice_id
        total
        customer { last_name }
        lines { quantity track { name genre { name } } }
    }
}"#;

/// The answers of a schema, checked against typed loads. A macro because the views differ
/// between databases.
macro_rules! check {
    ($views:ident, $schema:expr, $conn:expr) => {{
        use mabat_e2e::$views::*;
        let schema = $schema;
        let conn = $conn;

        let response = schema.execute(INVOICES).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let data = response.data.into_json().unwrap();
        let typed = mabat::load::<InvoiceView>()
            .filter(col("total").gt(10.0_f64) & col("invoice_id").is_in([88_i32, 89, 201, 207, 299, 404, 409, 411]))
            .order_by_desc("total")
            .order_by("invoice_id")
            .limit(5)
            .offset(1)
            .all(&mut *conn)
            .await
            .unwrap();
        let expected: Vec<Value> = typed
            .iter()
            .map(|i| {
                json!({
                    "invoice_id": i.invoice_id,
                    "total": serde_json::to_value(&i.total).unwrap(),
                    "customer": { "last_name": i.customer.last_name },
                    "lines": i.lines.iter().map(|l| json!({
                        "quantity": l.quantity,
                        "track": { "name": l.track.name, "genre": l.track.genre.as_ref().map(|g| json!({ "name": g.name })) },
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        assert_eq!(expected.len(), 5);
        assert_eq!(data, json!({ "invoices": expected }));

        // The reporting tree, as deep as the query asks
        let response = schema.execute("{ employee(key: 1) { title reports { title reports { employee_id } } } }").await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let typed = mabat::load::<EmployeeTree>().by_key(1_i32).one(&mut *conn).await.unwrap();
        let expected = json!({
            "title": typed.title,
            "reports": typed.reports.iter().map(|r| json!({
                "title": r.title,
                "reports": r.reports.iter().map(|r| json!({ "employee_id": r.employee_id })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        });
        assert_eq!(response.data.into_json().unwrap(), json!({ "employee": expected }));

        // Albums as map entries
        let response = schema.execute(r#"{ artist(key: 90) { albums { key value { tracks { name } } } } }"#).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let data = response.data.into_json().unwrap();
        let typed = mabat::load::<ArtistAlbums>().by_key(90_i32).one(&mut *conn).await.unwrap();
        let entries = data["artist"]["albums"].as_array().unwrap();
        assert_eq!(entries.len(), typed.albums.len());
        for (entry, (title, album)) in entries.iter().zip(&typed.albums) {
            assert_eq!(entry["key"], json!(title));
            assert_eq!(entry["value"]["tracks"].as_array().unwrap().len(), album.tracks.len());
        }
    }};
}

macro_rules! schema {
    ($views:ident, $pool:expr) => {{
        use mabat_e2e::$views::*;
        mabat_graphql::schema($pool)
            .list::<InvoiceView>("invoices")
            .by_key::<EmployeeTree>("employee")
            .by_key::<ArtistAlbums>("artist")
    }};
}

#[tokio::test]
async fn postgres() {
    let Some(mut conn) = Dataset::Chinook.connect().await else { return };
    let pool = Dataset::Chinook.pg_pool(4).await.unwrap();
    check!(chinook, &schema!(chinook, &pool).finish().unwrap(), &mut conn);

    // With a registry and concurrent queries
    use mabat_e2e::chinook::*;
    let mabat = Mabat::builder()
        .register::<InvoiceView>()
        .register::<EmployeeTree>()
        .register::<ArtistAlbums>()
        .build(&mut conn)
        .await
        .unwrap();
    let schema = schema!(chinook, &pool).registry(Arc::new(mabat)).connections(4).finish().unwrap();
    check!(chinook, &schema, &mut conn);
    pool.close().await;
}

#[tokio::test]
async fn mysql() {
    let Some(mut conn) = Dataset::Chinook.connect_mysql().await else { return };
    let pool = Dataset::Chinook.mysql_pool(4).await.unwrap();
    check!(chinook, &schema!(chinook, &pool).finish().unwrap(), &mut conn);
    pool.close().await;
}

#[tokio::test]
async fn sqlite() {
    let mut conn = Dataset::Chinook.connect_sqlite().await;
    let (pool, _file) = Dataset::Chinook.sqlite_pool(4).await;
    check!(chinook_sqlite, &schema!(chinook_sqlite, &pool).finish().unwrap(), &mut conn);
    pool.close().await;
}
