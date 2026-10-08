//! Arguments of nested collections on every database, compared with the whole collections
//! trimmed in Rust.

use mabat::Nested;
use mabat::filter::col;
use mabat_e2e::Dataset;

macro_rules! check {
    ($views:ident, $conn:expr) => {{
        use mabat_e2e::$views::*;
        let conn = $conn;

        // Many-to-many through the link table: the three highest tracks of each playlist
        let all = mabat::load::<PlaylistView>().order_by("playlist_id").all(&mut *conn).await.unwrap();
        let top = mabat::load::<PlaylistView>()
            .order_by("playlist_id")
            .nested("tracks", Nested::new().order_by_desc("track_id").limit(3))
            .all(&mut *conn)
            .await
            .unwrap();
        for (all, top) in all.iter().zip(&top) {
            let mut expected: Vec<i32> = all.tracks.iter().map(|t| t.track_id).collect();
            expected.sort_by(|a, b| b.cmp(a));
            expected.truncate(3);
            assert_eq!(
                top.tracks.iter().map(|t| t.track_id).collect::<Vec<_>>(),
                expected,
                "playlist {}",
                all.playlist_id
            );
        }

        // A filter and an offset: the lines of each invoice with an id over 100, after the first
        let all = mabat::load::<InvoiceView>().order_by("invoice_id").limit(50).all(&mut *conn).await.unwrap();
        let some = mabat::load::<InvoiceView>()
            .order_by("invoice_id")
            .limit(50)
            .nested("lines", Nested::new().filter(col("invoice_line_id").gt(0_i32)).offset(1))
            .nested("lines.track", Nested::new())
            .all(&mut *conn)
            .await;
        // A to-one reference takes no arguments
        assert!(some.unwrap_err().to_string().contains("only to-many collections take arguments"));
        let some = mabat::load::<InvoiceView>()
            .order_by("invoice_id")
            .limit(50)
            .nested("lines", Nested::new().filter(col("invoice_line_id").gt(100_i32)).offset(1))
            .all(&mut *conn)
            .await
            .unwrap();
        for (all, some) in all.iter().zip(&some) {
            let expected: Vec<i32> =
                all.lines.iter().map(|l| l.invoice_line_id).filter(|id| *id > 100).skip(1).collect();
            assert_eq!(some.lines.iter().map(|l| l.invoice_line_id).collect::<Vec<_>>(), expected);
        }

        // A map: the albums of each artist whose title has a word
        let all = mabat::load::<ArtistAlbums>().order_by("artist_id").all(&mut *conn).await.unwrap();
        let live = mabat::load::<ArtistAlbums>()
            .order_by("artist_id")
            .nested("albums", Nested::new().filter(col("title").like("%Live%")))
            .all(&mut *conn)
            .await
            .unwrap();
        let mut found = 0;
        for (all, live) in all.iter().zip(&live) {
            let expected: Vec<&String> = all.albums.keys().filter(|t| t.contains("Live")).collect();
            found += expected.len();
            assert_eq!(live.albums.keys().collect::<Vec<_>>(), expected);
        }
        assert!(found > 5);

        // A recursive collection: the arguments apply to each level
        fn first_reports(tree: &EmployeeLevels) -> Vec<i32> {
            let mut ids = vec![tree.employee_id];
            if let Some(first) = tree.reports.first() {
                ids.extend(first_reports(first));
            }
            ids
        }
        let all = mabat::load::<EmployeeLevels>().by_key(1_i32).one(&mut *conn).await.unwrap();
        let first = mabat::load::<EmployeeLevels>()
            .by_key(1_i32)
            .nested("reports", Nested::new().limit(1))
            .one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(first_reports(&first), first_reports(&all));
        assert!(first.reports.iter().all(|r| r.reports.len() <= 1));
        assert_eq!(first.reports.len(), 1);
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
