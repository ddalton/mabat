//! Chinook loaded on pools of every database, compared with the same loads on one
//! connection.

use mabat::{Mabat, Pooled};
use mabat_e2e::Dataset;

/// The loads of a database, on one connection and on a pool. A macro because the views
/// differ between databases.
macro_rules! compare {
    ($views:ident, $conn:expr, $pooled:expr) => {{
        use mabat_e2e::$views::*;
        let conn = $conn;
        let pooled = $pooled;

        let one = mabat::load::<InvoiceView>().order_by("invoice_id").all(&mut *conn).await.unwrap();
        let many = mabat::load::<InvoiceView>().order_by("invoice_id").all(&mut *pooled).await.unwrap();
        assert_eq!(many, one);
        assert_eq!(many.len(), 412);

        let one = mabat::load::<ArtistAlbums>().order_by("artist_id").all(&mut *conn).await.unwrap();
        let many = mabat::load::<ArtistAlbums>().order_by("artist_id").all(&mut *pooled).await.unwrap();
        assert_eq!(many, one);

        let one = mabat::load::<PlaylistView>().order_by("playlist_id").all(&mut *conn).await.unwrap();
        let many = mabat::load::<PlaylistView>().order_by("playlist_id").all(&mut *pooled).await.unwrap();
        assert_eq!(many, one);

        for root in [1_i32, 2] {
            let one = mabat::load::<EmployeeTree>().by_key(root).one(&mut *conn).await.unwrap();
            let many = mabat::load::<EmployeeTree>().by_key(root).one(&mut *pooled).await.unwrap();
            assert_eq!(many, one);
            let one = mabat::load::<EmployeeLevels>().by_key(root).one(&mut *conn).await.unwrap();
            let many = mabat::load::<EmployeeLevels>().by_key(root).one(&mut *pooled).await.unwrap();
            assert_eq!(many, one);
        }

        let graph = mabat::load::<Employee>().by_key(3_i32).graph(&mut *pooled).await.unwrap();
        assert_eq!(graph.count::<Employee>(), 8);
        assert_eq!(graph.count::<Invoice>(), 412);

        let n = mabat::load::<InvoiceView>().count(&mut *pooled).await.unwrap();
        assert_eq!(n, 412);

        let mabat = Mabat::builder().register::<InvoiceView>().build(&mut *pooled).await.unwrap();
        let tuned = mabat.load::<InvoiceView>().order_by("invoice_id").all(&mut *pooled).await.unwrap();
        assert_eq!(tuned.len(), 412);
    }};
}

#[tokio::test]
async fn postgres_snapshot_and_read_committed() {
    let Some(mut conn) = Dataset::Chinook.connect().await else { return };
    let pool = Dataset::Chinook.pg_pool(4).await.unwrap();
    compare!(chinook, &mut conn, &mut Pooled::snapshot(&pool, 4));
    compare!(chinook, &mut conn, &mut Pooled::read_committed(&pool, 4));
    assert!(pool.size() > 1, "{} connections", pool.size());
    pool.close().await;
}

#[tokio::test]
async fn mysql_read_committed() {
    let Some(mut conn) = Dataset::Chinook.connect_mysql().await else { return };
    let pool = Dataset::Chinook.mysql_pool(4).await.unwrap();
    compare!(chinook, &mut conn, &mut Pooled::read_committed(&pool, 4));
    assert!(pool.size() > 1, "{} connections", pool.size());
    pool.close().await;
}

#[tokio::test]
async fn sqlite_read_committed() {
    let mut conn = Dataset::Chinook.connect_sqlite().await;
    let (pool, _file) = Dataset::Chinook.sqlite_pool(4).await;
    compare!(chinook_sqlite, &mut conn, &mut Pooled::read_committed(&pool, 4));
    assert!(pool.size() > 1, "{} connections", pool.size());
    pool.close().await;
}
