//! End-to-end tests against Chinook on SQLite, in a new in-memory database per test: every
//! load is compared with an independent answer computed in SQL.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use mabat::Mabat;
use mabat::filter::col;
use mabat::manifest::ScaffoldFormat;
use mabat_e2e::Dataset;
use mabat_e2e::chinook_sqlite::*;
use sqlx::{AssertSqlSafe, Row, SqliteConnection};

async fn chinook() -> SqliteConnection {
    Dataset::Chinook.connect_sqlite().await
}

async fn count(conn: &mut SqliteConnection, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(AssertSqlSafe(sql.to_string())).fetch_one(conn).await.unwrap()
}

/// An amount in cents, to compare sums of `REAL` money.
fn cents(amount: f64) -> i64 {
    (amount * 100.0).round() as i64
}

#[tokio::test]
async fn invoices_match_independent_sql() {
    let mut conn = chinook().await;

    let invoices = mabat::load::<InvoiceView>().order_by("invoice_id").all(&mut conn).await.unwrap();
    let expected = sqlx::query(
        "SELECT i.invoice_id, i.total, i.billing_country, c.last_name,
                count(l.invoice_line_id) AS lines, sum(l.unit_price * l.quantity) AS line_total,
                group_concat(t.name, '|' ORDER BY l.invoice_line_id) AS tracks,
                group_concat(coalesce(ar.name, '-'), '|' ORDER BY l.invoice_line_id) AS artists
         FROM invoice i JOIN customer c ON c.customer_id = i.customer_id
         JOIN invoice_line l ON l.invoice_id = i.invoice_id JOIN track t ON t.track_id = l.track_id
         LEFT JOIN album al ON al.album_id = t.album_id LEFT JOIN artist ar ON ar.artist_id = al.artist_id
         GROUP BY i.invoice_id, c.last_name ORDER BY i.invoice_id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    assert_eq!(invoices.len(), expected.len());
    assert_eq!(invoices.len(), 412);
    for (invoice, row) in invoices.iter().zip(&expected) {
        let id = invoice.invoice_id;
        assert_eq!(cents(invoice.total), cents(row.get::<f64, _>("total")), "invoice {id}");
        assert_eq!(invoice.billing.country.as_deref(), row.get::<Option<&str>, _>("billing_country"));
        assert_eq!(invoice.customer.last_name, row.get::<&str, _>("last_name"));
        assert_eq!(invoice.lines.len() as i64, row.get::<i64, _>("lines"), "invoice {id}");

        // The total is the sum of the lines
        let lines: f64 = invoice.lines.iter().map(|l| l.unit_price * f64::from(l.quantity)).sum();
        assert_eq!(cents(lines), cents(row.get::<f64, _>("line_total")), "invoice {id}");
        assert_eq!(cents(lines), cents(invoice.total), "invoice {id}");

        let tracks: Vec<&str> = invoice.lines.iter().map(|l| l.track.name.as_str()).collect();
        assert_eq!(tracks.join("|"), row.get::<&str, _>("tracks"), "invoice {id}");
        let artists: Vec<&str> = invoice
            .lines
            .iter()
            .map(|l| l.track.album.as_ref().and_then(|a| a.artist.name.as_deref()).unwrap_or("-"))
            .collect();
        assert_eq!(artists.join("|"), row.get::<&str, _>("artists"), "invoice {id}");
    }
    // Dates are decoded from SQLite text
    assert_eq!(invoices[0].invoice_date.to_string(), "2021-01-01 00:00:00");

    // Tracks and media types are shared across lines
    let mut tracks: HashMap<i32, &Arc<TrackView>> = HashMap::new();
    for line in invoices.iter().flat_map(|i| &i.lines) {
        assert!(Arc::ptr_eq(tracks.entry(line.track.track_id).or_insert(&line.track), &line.track));
    }
    assert_eq!(tracks.len() as i64, count(&mut conn, "SELECT count(DISTINCT track_id) FROM invoice_line").await);
    let media: BTreeSet<*const MediaTypeView> = tracks.values().map(|t| Arc::as_ptr(&t.media_type)).collect();
    assert_eq!(
        media.len() as i64,
        count(
            &mut conn,
            "SELECT count(DISTINCT t.media_type_id) FROM invoice_line l JOIN track t ON t.track_id = l.track_id"
        )
        .await
    );
}

#[tokio::test]
async fn artists_albums_and_playlists() {
    let mut conn = chinook().await;

    // Albums in a map keyed by their title
    let artists = mabat::load::<ArtistAlbums>().order_by("artist_id").all(&mut conn).await.unwrap();
    let expected = sqlx::query(
        "SELECT ar.artist_id, coalesce(group_concat(al.title, '|' ORDER BY al.title), '') AS albums,
                count(t.track_id) AS tracks
         FROM artist ar LEFT JOIN album al ON al.artist_id = ar.artist_id LEFT JOIN track t ON t.album_id = al.album_id
         GROUP BY ar.artist_id ORDER BY ar.artist_id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    assert_eq!(artists.len(), expected.len());
    for (artist, row) in artists.iter().zip(&expected) {
        let titles: Vec<&str> = artist.albums.keys().map(String::as_str).collect();
        let expected_titles: BTreeSet<&str> =
            row.get::<&str, _>("albums").split('|').filter(|t| !t.is_empty()).collect();
        assert_eq!(titles, expected_titles.into_iter().collect::<Vec<_>>(), "artist {}", artist.artist_id);
        let tracks: usize = artist.albums.values().map(|a| a.tracks.len()).sum();
        assert_eq!(tracks as i64, row.get::<i64, _>("tracks"), "artist {}", artist.artist_id);
    }

    // Playlists of tracks through the link table
    let playlists = mabat::load::<PlaylistView>().order_by("playlist_id").all(&mut conn).await.unwrap();
    let expected: BTreeMap<i32, i64> = sqlx::query(
        "SELECT p.playlist_id, count(pt.track_id) AS n
         FROM playlist p LEFT JOIN playlist_track pt ON pt.playlist_id = p.playlist_id GROUP BY p.playlist_id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get("playlist_id"), r.get("n")))
    .collect();
    let loaded: BTreeMap<i32, i64> = playlists.iter().map(|p| (p.playlist_id, p.tracks.len() as i64)).collect();
    assert_eq!(loaded, expected);
    // The playlist "Music" has 3290 tracks: more keys than one statement binds
    assert!(loaded.values().any(|n| *n > 1000));
    for playlist in &playlists {
        assert!(playlist.tracks.windows(2).all(|w| w[0].track_id < w[1].track_id), "ordered by track");
    }
}

#[tokio::test]
async fn filters_and_paging_on_tracks() {
    let mut conn = chinook().await;

    let cases = [
        (col("genre_id").is_in([1_i32, 3, 4]), "genre_id IN (1, 3, 4)"),
        (col("milliseconds").gt(600_000_i32), "milliseconds > 600000"),
        (col("composer").is_null(), "composer IS NULL"),
        (
            col("name").ilike("%LOVE%") & !col("composer").is_null(),
            "lower(name) LIKE '%love%' AND composer IS NOT NULL",
        ),
        (col("unit_price").ge(1.0_f64) | col("media_type_id").eq(3_i32), "unit_price >= 1.0 OR media_type_id = 3"),
    ];
    for (condition, sql) in cases {
        let n = mabat::load::<TrackView>().filter(condition).count(&mut conn).await.unwrap();
        assert_eq!(n, count(&mut conn, &format!("SELECT count(*) FROM track WHERE {sql}")).await, "{sql}");
    }

    // A list longer than the padding of its placeholders
    let ids: Vec<i32> = (1..=37).collect();
    let n = mabat::load::<TrackName>().filter(col("track_id").is_in(ids)).count(&mut conn).await.unwrap();
    assert_eq!(n, 37);

    let page: Vec<i32> = mabat::load::<TrackName>()
        .order_by_desc("milliseconds")
        .order_by("track_id")
        .limit(10)
        .offset(5)
        .all(&mut conn)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.track_id)
        .collect();
    let expected: Vec<i32> =
        sqlx::query_scalar("SELECT track_id FROM track ORDER BY milliseconds DESC, track_id LIMIT 10 OFFSET 5")
            .fetch_all(&mut conn)
            .await
            .unwrap();
    assert_eq!(page, expected);

    // An offset without a limit
    let rest = mabat::load::<TrackName>().order_by("track_id").offset(3500).all(&mut conn).await.unwrap();
    assert_eq!(rest.len() as i64, count(&mut conn, "SELECT count(*) FROM track").await - 3500);
}

fn pairs_tree(tree: &EmployeeTree, manager: Option<i32>, out: &mut Vec<(i32, Option<i32>)>) {
    out.push((tree.employee_id, manager));
    tree.reports.iter().for_each(|r| pairs_tree(r, Some(tree.employee_id), out));
}

fn pairs_levels(tree: &EmployeeLevels, manager: Option<i32>, out: &mut Vec<(i32, Option<i32>)>) {
    out.push((tree.employee_id, manager));
    tree.reports.iter().for_each(|r| pairs_levels(r, Some(tree.employee_id), out));
}

#[tokio::test]
async fn reporting_hierarchy_as_recursive_trees() {
    let mut conn = chinook().await;

    let root = mabat::load::<EmployeeTree>().filter(col("reports_to").is_null()).one(&mut conn).await.unwrap();
    let mut cte = Vec::new();
    pairs_tree(&root, None, &mut cte);
    let levels = mabat::load::<EmployeeLevels>().filter(col("reports_to").is_null()).one(&mut conn).await.unwrap();
    let mut by_level = Vec::new();
    pairs_levels(&levels, None, &mut by_level);
    assert_eq!(by_level, cte);

    let expected: Vec<(i32, Option<i32>)> = sqlx::query(
        "WITH RECURSIVE tree AS (
             SELECT employee_id, reports_to, printf('%010d', employee_id) AS path FROM employee WHERE reports_to IS NULL
             UNION ALL
             SELECT e.employee_id, e.reports_to, t.path || '/' || printf('%010d', e.employee_id)
             FROM employee e JOIN tree t ON e.reports_to = t.employee_id)
         SELECT employee_id, reports_to FROM tree ORDER BY path",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get("employee_id"), r.get("reports_to")))
    .collect();
    assert_eq!(cte, expected);
    assert_eq!(root.title.as_deref(), Some("General Manager"));

    // A subtree from any employee
    let sales = mabat::load::<EmployeeTree>().by_key(2_i32).one(&mut conn).await.unwrap();
    let mut subtree = Vec::new();
    pairs_tree(&sales, Some(1), &mut subtree);
    assert_eq!(subtree, expected.iter().filter(|(id, m)| *id == 2 || *m == Some(2)).copied().collect::<Vec<_>>());
}

#[tokio::test]
async fn employees_customers_and_invoices_as_a_graph() {
    let mut conn = chinook().await;

    let graph = mabat::load::<Employee>().by_key(3_i32).graph(&mut conn).await.unwrap();
    assert_eq!(graph.count::<Employee>() as i64, count(&mut conn, "SELECT count(*) FROM employee").await);
    assert_eq!(
        graph.count::<Customer>() as i64,
        count(&mut conn, "SELECT count(*) FROM customer WHERE support_rep_id IS NOT NULL").await
    );
    assert_eq!(graph.count::<Invoice>() as i64, count(&mut conn, "SELECT count(*) FROM invoice").await);

    let agent = graph.root().unwrap();
    assert_eq!(agent.manager(&graph).unwrap().manager(&graph).unwrap().title.as_deref(), Some("General Manager"));

    let mut revenue: BTreeMap<i32, i64> = BTreeMap::new();
    for (employee_ref, employee) in graph.all::<Employee>() {
        for report in employee.reports(&graph) {
            assert_eq!(report.manager, Some(employee_ref));
        }
        for (customer_ref, customer) in employee.customers.iter().map(|r| (*r, graph.get(*r))) {
            assert_eq!(customer.support_rep, Some(employee_ref));
            for invoice in customer.invoices(&graph) {
                assert_eq!(invoice.customer, customer_ref);
                *revenue.entry(employee.employee_id).or_default() += cents(invoice.total);
            }
        }
    }
    let expected: BTreeMap<i32, i64> = sqlx::query(
        "SELECT c.support_rep_id, sum(i.total) AS revenue FROM invoice i JOIN customer c ON c.customer_id = i.customer_id
         GROUP BY c.support_rep_id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get("support_rep_id"), cents(r.get("revenue"))))
    .collect();
    assert_eq!(revenue, expected);
}

/// The same tuning as on PostgreSQL, with `:keys` for the keys of the parents.
const TUNED_INVOICES: &str = r#"
-- mabat: query lines, shadow
SELECT l.invoice_id AS "$parent", l.invoice_line_id AS "invoice_line_id", l.unit_price AS "unit_price",
       l.quantity AS "quantity", l.track_id AS "$ref.track"
FROM invoice_line l
WHERE l.invoice_id IN (:keys)
ORDER BY l.invoice_line_id

-- mabat: query lines.track
SELECT t.track_id AS "track_id", t.name AS "name", t.composer AS "composer", t.milliseconds AS "milliseconds",
       t.unit_price AS "unit_price", t.album_id AS "$ref.album", t.genre_id AS "$ref.genre",
       t.media_type_id AS "$ref.media_type"
FROM track t
WHERE t.track_id IN (:keys)
"#;

#[tokio::test]
async fn tuned_overrides_return_the_same_invoices() {
    let mut conn = chinook().await;

    let mabat =
        Mabat::builder().register::<InvoiceView>().overrides_sql("InvoiceView", TUNED_INVOICES).build(&mut conn);
    let mabat = mabat.await.unwrap();
    assert!(mabat.report().diagnostics().is_empty(), "{}", mabat.report());

    let generated = mabat::load::<InvoiceView>().order_by("invoice_id").all(&mut conn).await.unwrap();
    let tuned = mabat.load::<InvoiceView>().order_by("invoice_id").all(&mut conn).await.unwrap();
    assert_eq!(tuned, generated);
    let stats = mabat.shadow_stats();
    assert_eq!((stats[0].query.as_str(), stats[0].runs, stats[0].mismatches), ("lines", 1, 0));
}

#[tokio::test]
async fn every_view_and_scaffold_passes_the_checks() {
    let mut conn = chinook().await;

    fn views() -> mabat::Builder<sqlx::Sqlite> {
        Mabat::builder()
            .register::<InvoiceView>()
            .register::<ArtistAlbums>()
            .register::<PlaylistView>()
            .register::<EmployeeTree>()
            .register::<EmployeeLevels>()
            .register::<Employee>()
    }
    let report = views().check(&mut conn).await.unwrap();
    assert!(report.diagnostics().is_empty(), "{report}");

    let manifest = views().manifest().unwrap();
    assert_eq!(manifest.backend, "SQLite");
    for view in &manifest.views {
        for format in [ScaffoldFormat::Toml, ScaffoldFormat::Sql] {
            let scaffold = manifest.scaffold(&view.name, format).unwrap();
            let builder = match format {
                ScaffoldFormat::Toml => views().overrides(&view.name, scaffold),
                ScaffoldFormat::Sql => views().overrides_sql(&view.name, scaffold),
            };
            let report = builder.check(&mut conn).await.unwrap();
            assert!(report.diagnostics().is_empty(), "{}: {report}", view.name);
        }
    }
}

#[tokio::test]
async fn a_postgres_manifest_is_refused() {
    let mut conn = chinook().await;
    let error = Mabat::<sqlx::Sqlite>::builder().register::<InvoiceView>().manifest().unwrap();
    assert_eq!(error.backend, "SQLite");
    let mut manifest = error;
    manifest.backend = "PostgreSQL".to_string();
    let error = manifest.check(&mut conn, &[]).await.unwrap_err();
    assert!(error.to_string().contains("PostgreSQL"), "{error}");
}
