//! End-to-end tests against Chinook: every load is compared with an independent answer
//! computed in SQL.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Instant;

use mabat::Mabat;
use mabat::filter::col;
use mabat::manifest::ScaffoldFormat;
use mabat_e2e::Dataset;
use mabat_e2e::chinook::*;
use rust_decimal::Decimal;
use sqlx::{AssertSqlSafe, PgConnection, Row};

async fn chinook() -> Option<PgConnection> {
    Dataset::Chinook.connect().await
}

async fn count(conn: &mut PgConnection, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(AssertSqlSafe(sql.to_string())).fetch_one(conn).await.unwrap()
}

#[tokio::test]
async fn invoices_match_independent_sql() {
    let Some(mut conn) = chinook().await else { return };

    let start = Instant::now();
    let invoices = mabat::load::<InvoiceView>().order_by("invoice_id").all(&mut conn).await.unwrap();
    eprintln!("loaded {} invoices with lines, tracks, albums and artists in {:?}", invoices.len(), start.elapsed());

    let expected = sqlx::query(
        "SELECT i.invoice_id, i.total, i.billing_country, c.last_name,
                count(l.invoice_line_id) AS lines, sum(l.unit_price * l.quantity) AS line_total,
                string_agg(t.name, '|' ORDER BY l.invoice_line_id) AS tracks,
                string_agg(coalesce(ar.name, '-'), '|' ORDER BY l.invoice_line_id) AS artists
         FROM invoice i JOIN customer c ON c.customer_id = i.customer_id
         JOIN invoice_line l ON l.invoice_id = i.invoice_id JOIN track t ON t.track_id = l.track_id
         LEFT JOIN album al ON al.album_id = t.album_id LEFT JOIN artist ar ON ar.artist_id = al.artist_id
         GROUP BY i.invoice_id, c.last_name ORDER BY i.invoice_id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap();
    assert_eq!(invoices.len(), expected.len());
    for (invoice, row) in invoices.iter().zip(&expected) {
        let id = invoice.invoice_id;
        assert_eq!(invoice.total, row.get::<Decimal, _>("total"), "invoice {id}");
        assert_eq!(invoice.billing.country.as_deref(), row.get::<Option<&str>, _>("billing_country"));
        assert_eq!(invoice.customer.last_name, row.get::<&str, _>("last_name"));
        assert_eq!(invoice.lines.len() as i64, row.get::<i64, _>("lines"), "invoice {id}");

        // The total is the sum of the lines, exactly
        let lines: Decimal = invoice.lines.iter().map(|l| l.unit_price * Decimal::from(l.quantity)).sum();
        assert_eq!(lines, row.get::<Decimal, _>("line_total"), "invoice {id}");
        assert_eq!(lines, invoice.total, "invoice {id}");

        let tracks: Vec<&str> = invoice.lines.iter().map(|l| l.track.name.as_str()).collect();
        assert_eq!(tracks.join("|"), row.get::<&str, _>("tracks"), "invoice {id}");
        let artists: Vec<&str> = invoice
            .lines
            .iter()
            .map(|l| l.track.album.as_ref().and_then(|a| a.artist.name.as_deref()).unwrap_or("-"))
            .collect();
        assert_eq!(artists.join("|"), row.get::<&str, _>("artists"), "invoice {id}");
    }

    // Tracks, albums, genres and media types are shared across lines
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
    let Some(mut conn) = chinook().await else { return };

    // Albums in a map keyed by their title
    let artists = mabat::load::<ArtistAlbums>().order_by("artist_id").all(&mut conn).await.unwrap();
    let expected = sqlx::query(
        "SELECT ar.artist_id, coalesce(string_agg(al.title, '|' ORDER BY al.title COLLATE \"C\"), '') AS albums,
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
        // A title appears once per artist even if it repeats across albums of the same name
        let expected_titles: BTreeSet<&str> =
            row.get::<&str, _>("albums").split('|').filter(|t| !t.is_empty()).collect();
        assert_eq!(titles, expected_titles.into_iter().collect::<Vec<_>>(), "artist {}", artist.artist_id);
        let tracks: usize = artist.albums.values().map(|a| a.tracks.len()).sum();
        assert_eq!(tracks as i64, row.get::<i64, _>("tracks"), "artist {}", artist.artist_id);
    }

    // Playlists of tracks through the link table
    let playlists = mabat::load::<PlaylistView>().order_by("playlist_id").all(&mut conn).await.unwrap();
    let expected: BTreeMap<i32, i64> =
        sqlx::query("SELECT p.playlist_id, count(pt.track_id) AS n FROM playlist p LEFT JOIN playlist_track pt ON pt.playlist_id = p.playlist_id GROUP BY p.playlist_id")
            .fetch_all(&mut conn)
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get("playlist_id"), r.get("n")))
            .collect();
    let loaded: BTreeMap<i32, i64> = playlists.iter().map(|p| (p.playlist_id, p.tracks.len() as i64)).collect();
    assert_eq!(loaded, expected);
    for playlist in &playlists {
        assert!(playlist.tracks.windows(2).all(|w| w[0].track_id < w[1].track_id), "ordered by track");
    }
}

#[tokio::test]
async fn filters_and_paging_on_tracks() {
    let Some(mut conn) = chinook().await else { return };

    let cases = [
        (col("genre_id").is_in([1_i32, 3, 4]), "genre_id IN (1, 3, 4)"),
        (col("milliseconds").gt(600_000_i32), "milliseconds > 600000"),
        (col("composer").is_null(), "composer IS NULL"),
        (col("name").ilike("%love%") & !col("composer").is_null(), "name ILIKE '%love%' AND composer IS NOT NULL"),
        (col("unit_price").ge(1.0_f64) | col("media_type_id").eq(3_i32), "unit_price >= 1.0 OR media_type_id = 3"),
    ];
    for (condition, sql) in cases {
        let n = mabat::load::<TrackView>().filter(condition).count(&mut conn).await.unwrap();
        assert_eq!(n, count(&mut conn, &format!("SELECT count(*) FROM track WHERE {sql}")).await, "{sql}");
    }

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
    let Some(mut conn) = chinook().await else { return };

    let root = mabat::load::<EmployeeTree>().filter(col("reports_to").is_null()).one(&mut conn).await.unwrap();
    let mut cte = Vec::new();
    pairs_tree(&root, None, &mut cte);
    let levels = mabat::load::<EmployeeLevels>().filter(col("reports_to").is_null()).one(&mut conn).await.unwrap();
    let mut by_level = Vec::new();
    pairs_levels(&levels, None, &mut by_level);
    assert_eq!(by_level, cte);

    let expected: Vec<(i32, Option<i32>)> = sqlx::query(
        "WITH RECURSIVE tree AS (
             SELECT employee_id, reports_to, ARRAY[employee_id] AS path FROM employee WHERE reports_to IS NULL
             UNION ALL
             SELECT e.employee_id, e.reports_to, t.path || e.employee_id FROM employee e JOIN tree t ON e.reports_to = t.employee_id)
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
    let Some(mut conn) = chinook().await else { return };

    // From one sales agent, through the manager, every employee, customer and invoice is reachable
    let graph = mabat::load::<Employee>().by_key(3_i32).graph(&mut conn).await.unwrap();
    assert_eq!(graph.count::<Employee>() as i64, count(&mut conn, "SELECT count(*) FROM employee").await);
    assert_eq!(
        graph.count::<Customer>() as i64,
        count(&mut conn, "SELECT count(*) FROM customer WHERE support_rep_id IS NOT NULL").await
    );
    assert_eq!(graph.count::<Invoice>() as i64, count(&mut conn, "SELECT count(*) FROM invoice").await);

    let agent = graph.root().unwrap();
    assert_eq!(agent.manager(&graph).unwrap().manager(&graph).unwrap().title.as_deref(), Some("General Manager"));

    let mut revenue: BTreeMap<i32, Decimal> = BTreeMap::new();
    for (employee_ref, employee) in graph.all::<Employee>() {
        for report in employee.reports(&graph) {
            assert_eq!(report.manager, Some(employee_ref));
        }
        for (customer_ref, customer) in employee.customers.iter().map(|r| (*r, graph.get(*r))) {
            assert_eq!(customer.support_rep, Some(employee_ref));
            for invoice in customer.invoices(&graph) {
                assert_eq!(invoice.customer, customer_ref);
                *revenue.entry(employee.employee_id).or_default() += invoice.total;
            }
        }
    }
    let expected: BTreeMap<i32, Decimal> = sqlx::query(
        "SELECT c.support_rep_id, sum(i.total) AS revenue FROM invoice i JOIN customer c ON c.customer_id = i.customer_id
         GROUP BY c.support_rep_id",
    )
    .fetch_all(&mut conn)
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get("support_rep_id"), r.get("revenue")))
    .collect();
    assert_eq!(revenue, expected);
}

const TUNED_INVOICES: &str = r#"
-- mabat: query lines, shadow
SELECT l.invoice_id AS "$parent", l.invoice_line_id AS "invoice_line_id", l.unit_price AS "unit_price",
       l.quantity AS "quantity", l.track_id AS "$ref.track"
FROM invoice_line l
WHERE l.invoice_id = ANY($1)
ORDER BY l.invoice_line_id

-- mabat: query lines.track
SELECT t.track_id AS "track_id", t.name AS "name", t.composer AS "composer", t.milliseconds AS "milliseconds",
       t.unit_price AS "unit_price", t.album_id AS "$ref.album", t.genre_id AS "$ref.genre",
       t.media_type_id AS "$ref.media_type"
FROM track t
WHERE t.track_id = ANY($1)
"#;

#[tokio::test]
async fn tuned_overrides_return_the_same_invoices() {
    let Some(mut conn) = chinook().await else { return };

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
    let Some(mut conn) = chinook().await else { return };

    fn views() -> mabat::Builder {
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
