//! Chinook loaded as JSON on every database, compared with the typed loads.

use mabat::{Error, Selection};
use mabat_e2e::Dataset;
use serde_json::{Value, json};

/// The JSON loads of a database. A macro because the views differ between databases.
macro_rules! check {
    ($views:ident, $conn:expr) => {{
        use mabat_e2e::$views::*;
        let conn = $conn;

        // A selection across a reference, a nested collection and an embedded struct
        let selection = Selection::parse("invoice_id billing { country } customer { last_name } lines { quantity track { name album { title } } }").unwrap();
        let invoices = mabat::load::<InvoiceView>().order_by("invoice_id").select(selection).json(&mut *conn).await.unwrap();
        let typed = mabat::load::<InvoiceView>().order_by("invoice_id").all(&mut *conn).await.unwrap();
        assert_eq!(invoices.len(), typed.len());
        for (json, typed) in invoices.iter().zip(&typed) {
            assert_eq!(json["invoice_id"], json!(typed.invoice_id));
            assert_eq!(json["billing"]["country"], json!(typed.billing.country));
            assert_eq!(json["billing"]["city"], json!(typed.billing.city));
            assert_eq!(json["customer"], json!({ "last_name": typed.customer.last_name }));
            assert!(json.get("total").is_none() && json.get("invoice_date").is_none());
            let lines: Vec<Value> = typed
                .lines
                .iter()
                .map(|l| {
                    let album = l.track.album.as_ref().map(|a| json!({ "title": a.title }));
                    json!({ "quantity": l.quantity, "track": { "name": l.track.name, "album": album } })
                })
                .collect();
            assert_eq!(json["lines"], Value::Array(lines));
        }

        // A map keyed by title, and a many-to-many collection
        let artists = mabat::load::<ArtistAlbums>().by_key(90_i32).select(Selection::parse("name albums { tracks { name } }").unwrap()).json(&mut *conn).await.unwrap();
        let typed = mabat::load::<ArtistAlbums>().by_key(90_i32).one(&mut *conn).await.unwrap();
        let albums = artists[0]["albums"].as_object().unwrap();
        assert_eq!(albums.keys().collect::<Vec<_>>(), typed.albums.keys().collect::<Vec<_>>());
        for (title, album) in &typed.albums {
            let names: Vec<Value> = album.tracks.iter().map(|t| json!({ "name": t.name })).collect();
            assert_eq!(albums[title]["tracks"], Value::Array(names));
        }
        let playlist = mabat::load::<PlaylistView>().by_key(18_i32).json(&mut *conn).await.unwrap();
        let typed = mabat::load::<PlaylistView>().by_key(18_i32).one(&mut *conn).await.unwrap();
        assert_eq!(playlist[0]["tracks"].as_array().unwrap().len(), typed.tracks.len());

        // A recursive tree, as deep as the selection asks
        let tree = mabat::load::<EmployeeTree>().by_key(1_i32).select(Selection::parse("title reports { title reports { employee_id } }").unwrap()).json(&mut *conn).await.unwrap();
        let typed = mabat::load::<EmployeeTree>().by_key(1_i32).one(&mut *conn).await.unwrap();
        let expected = json!({
            "title": typed.title,
            "reports": typed.reports.iter().map(|r| json!({
                "title": r.title,
                "reports": r.reports.iter().map(|r| json!({ "employee_id": r.employee_id })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        });
        assert_eq!(tree, [expected]);

        // A graph view is loaded as a tree, as deep as the selection asks
        let error = mabat::load::<Employee>().by_key(3_i32).json(&mut *conn).await.unwrap_err();
        assert!(matches!(error, Error::GraphRequired { .. }), "{error}");
        let agent = mabat::load::<Employee>().by_key(3_i32).select(Selection::parse("first_name manager { title manager { title } } customers { last_name invoices { invoice_id } }").unwrap()).json(&mut *conn).await.unwrap();
        assert_eq!(agent[0]["first_name"], "Jane");
        assert_eq!(agent[0]["manager"]["manager"], json!({ "title": "General Manager" }));
        let invoices: usize = agent[0]["customers"].as_array().unwrap().iter().map(|c| c["invoices"].as_array().unwrap().len()).sum();
        assert_eq!(invoices, 146);
    }};
}

#[tokio::test]
async fn postgres() {
    let Some(mut conn) = Dataset::Chinook.connect().await else { return };
    check!(chinook, &mut conn);
}

#[tokio::test]
async fn mysql() {
    let Some(mut conn) = Dataset::Chinook.connect_mysql().await else { return };
    check!(chinook, &mut conn);
}

#[tokio::test]
async fn sqlite() {
    let mut conn = Dataset::Chinook.connect_sqlite().await;
    check!(chinook_sqlite, &mut conn);
}
