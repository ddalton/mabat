//! Every route of the service, called in-process on a new copy of the store.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The service on a new store in a file of its own, removed when dropped.
struct Service {
    router: Router,
    path: std::path::PathBuf,
}

impl Drop for Service {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

async fn service(name: &str) -> Service {
    let path = std::env::temp_dir().join(format!("mabat-chinook-{name}-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let app = mabat_example_chinook::open(&path).await.unwrap();
    Service { router: mabat_example_chinook::router(Arc::new(app)), path }
}

impl Service {
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, String) {
        let request = Request::builder().method(method).uri(uri).header("content-type", "application/json");
        let body = body.map_or_else(Body::empty, |body| Body::from(body.to_string()));
        let response = self.router.clone().oneshot(request.body(body).unwrap()).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    async fn json(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let (status, text) = self.call(method, uri, body).await;
        (status, serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}")))
    }

    async fn get(&self, uri: &str) -> Value {
        let (status, json) = self.json("GET", uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {json}");
        json
    }
}

#[tokio::test]
async fn reads() {
    let service = service("reads").await;

    // AC/DC's albums, newest first as the override orders them, with their tracks and genres
    let artist = service.get("/artists/1").await;
    assert_eq!(artist["name"], "AC/DC");
    let titles: Vec<&str> = artist["albums"].as_array().unwrap().iter().map(|a| a["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Let There Be Rock", "For Those About To Rock We Salute You"]);
    assert_eq!(artist["albums"][1]["tracks"][0]["name"], "For Those About To Rock (We Salute You)");
    assert_eq!(artist["albums"][1]["tracks"][0]["genre"]["name"], "Rock");
    let (status, _) = service.json("GET", "/artists/99999", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A filtered page
    let page = service.get("/tracks?genre_id=1&composer=angus&limit=3&offset=1").await;
    let page = page.as_array().unwrap();
    assert_eq!(page.len(), 3);
    assert!(page.iter().all(|t| t["genre"]["genre_id"] == 1 && t["composer"].as_str().unwrap().contains("Angus")));

    // Every track, streamed
    let (status, ndjson) = service.call("GET", "/tracks/export", None).await;
    assert_eq!(status, StatusCode::OK);
    let lines: Vec<Value> = ndjson.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 3503);
    assert_eq!(lines[0]["track_id"], 1);

    // A customer's account, whole or selected
    let customer = service.get("/customers/5").await;
    assert_eq!(customer["support_rep"]["last_name"], "Park");
    assert_eq!(customer["invoices"].as_array().unwrap().len(), 7);
    let selected = service.get("/customers/5?select=first_name%20invoices%20%7B%20total%20%7D").await;
    assert_eq!(selected.as_object().unwrap().len(), 2, "{selected}");
    assert!(selected["invoices"][0].get("lines").is_none());
    let (status, _) = service.json("GET", "/customers/5?select=nope", None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "an unknown field is a plan error");

    // Up the chain of managers, and down the chart
    let jane = service.get("/employees/3/managers").await;
    assert_eq!(jane["last_name"], "Peacock");
    assert_eq!(jane["manager"]["last_name"], "Edwards");
    assert_eq!(jane["manager"]["manager"]["last_name"], "Adams");
    assert!(jane["manager"]["manager"]["manager"].is_null());
    let chart = service.get("/employees/chart").await;
    assert_eq!(chart["last_name"], "Adams");
    assert_eq!(chart["reports"].as_array().unwrap().len(), 2);

    // A report: what the DBA's override computes, with each customer's invoices of the period
    let top = service.get("/reports/top-customers?from=2022-01-01&to=2023-01-01&limit=3").await;
    let top = top.as_array().unwrap();
    assert_eq!(top.len(), 3);
    for customer in top {
        let period = customer["period"].as_array().unwrap();
        assert_eq!(customer["invoices"].as_u64().unwrap() as usize, period.len());
        let spent: f64 = period.iter().map(|i| i["total"].as_f64().unwrap()).sum();
        assert!((customer["spent"].as_f64().unwrap() - spent).abs() < 0.001, "{customer}");
    }
    assert!(top[0]["spent"].as_f64() >= top[1]["spent"].as_f64());

    // The SQL of a view, with the override
    let (status, explained) = service.call("GET", "/explain/Discography", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(explained.contains("ORDER BY a.album_id DESC"), "{explained}");
}

#[tokio::test]
async fn playlists() {
    let service = service("playlists").await;

    // A new playlist gets a key from the database
    let body = json!({ "name": "Road trip", "track_ids": [3, 1, 2] });
    let (status, created) = service.json("POST", "/playlists", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["playlist_id"].as_i64().unwrap();
    assert!(id > 18, "after Chinook's playlists: {id}");
    let read = service.get(&format!("/playlists/{id}")).await;
    let ids: Vec<i64> = read["tracks"].as_array().unwrap().iter().map(|t| t["track_id"].as_i64().unwrap()).collect();
    assert_eq!(ids, [1, 2, 3]);

    // Renamed and with other tracks; unknown tracks are refused
    let body = json!({ "name": "Long road trip", "track_ids": [5, 6] });
    let (status, updated) = service.json("PUT", &format!("/playlists/{id}"), Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let read = service.get(&format!("/playlists/{id}")).await;
    assert_eq!(read["name"], "Long road trip");
    assert_eq!(read["tracks"].as_array().unwrap().len(), 2);
    let (status, error) = service.json("POST", "/playlists", Some(json!({ "name": "x", "track_ids": [999999] }))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error["error"].as_str().unwrap().contains("999999"));

    // Deleted with its links
    let (status, _) = service.call("DELETE", &format!("/playlists/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = service.call("DELETE", &format!("/playlists/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn graphql() {
    let service = service("graphql").await;
    let query = r#"{
        artist(key: 1) { name albums { title } }
        employee(key: 3) { last_name manager { last_name manager { last_name } } }
        tracks(where: { milliseconds: { gt: 1000000 } }, limit: 2) { name genre { name } }
    }"#;
    let (status, response) = service.json("POST", "/graphql", Some(json!({ "query": query }))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(response.get("errors").is_none(), "{response}");
    let data = &response["data"];
    assert_eq!(data["artist"]["albums"][0]["title"], "Let There Be Rock");
    assert_eq!(data["employee"]["manager"]["manager"]["last_name"], "Adams");
    assert_eq!(data["tracks"].as_array().unwrap().len(), 2);
    let (status, page) = service.call("GET", "/graphql", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("graphiql"));
}
