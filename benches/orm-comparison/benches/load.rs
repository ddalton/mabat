//! Load 1, 100 and 10,000 tasks with their 10 subtasks each, with each library, on one
//! connection each to the database of `DATABASE_URL`:
//!
//! ```sh
//! ../../scripts/with-postgres.sh sh -c 'DATABASE_URL=$MABAT_TEST_DATABASE_URL cargo bench'
//! ```

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use diesel_async::{AsyncConnection, AsyncPgConnection};
use mabat_orm_comparison::{SETUP, SUBTASKS, with_diesel, with_mabat, with_sea_orm, with_sqlx};
use sqlx::{Connection, Executor, PgConnection};
use tokio::sync::Mutex;

fn load(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let url = std::env::var("DATABASE_URL").expect("set DATABASE_URL to a PostgreSQL database the benchmark may fill");
    let (sqlx_conn, mabat_conn, sea_orm_db, diesel_conn) = runtime.block_on(async {
        let mut setup = PgConnection::connect(&url).await.unwrap();
        setup.execute(SETUP).await.unwrap();
        let mut options = sea_orm::ConnectOptions::new(url.clone());
        options.max_connections(1).min_connections(1).sqlx_logging(false);
        (
            Mutex::new(PgConnection::connect(&url).await.unwrap()),
            Mutex::new(PgConnection::connect(&url).await.unwrap()),
            sea_orm::Database::connect(options).await.unwrap(),
            Mutex::new(AsyncPgConnection::establish(&url).await.unwrap()),
        )
    });

    for roots in [1_i64, 100, 10_000] {
        let keys: Vec<i64> = (1..=roots).collect();
        // Every library loads the same values
        runtime.block_on(async {
            let expected = (roots as usize, (roots * SUBTASKS) as usize);
            let mabat = with_mabat::load(&mut *mabat_conn.lock().await, &keys).await;
            assert_eq!((mabat.len(), mabat.iter().map(|t| t.subtasks.len()).sum()), expected);
            let sqlx = with_sqlx::load(&mut *sqlx_conn.lock().await, &keys).await;
            assert_eq!((sqlx.len(), sqlx.iter().map(|t| t.subtasks.len()).sum()), expected);
            let sea_orm = with_sea_orm::load(&sea_orm_db, &keys).await;
            assert_eq!((sea_orm.len(), sea_orm.iter().map(|(_, s)| s.len()).sum()), expected);
            let diesel = with_diesel::load(&mut *diesel_conn.lock().await, &keys).await;
            assert_eq!((diesel.len(), diesel.iter().map(|(_, s)| s.len()).sum()), expected);
        });

        let mut group = c.benchmark_group(format!("load {roots} tasks"));
        if roots >= 10_000 {
            group.sample_size(20);
        }
        group.bench_function(BenchmarkId::from_parameter("sqlx (hand-written)"), |b| {
            b.to_async(&runtime).iter(|| async { with_sqlx::load(&mut *sqlx_conn.lock().await, &keys).await })
        });
        group.bench_function(BenchmarkId::from_parameter("mabat"), |b| {
            b.to_async(&runtime).iter(|| async { with_mabat::load(&mut *mabat_conn.lock().await, &keys).await })
        });
        group.bench_function(BenchmarkId::from_parameter("sea-orm"), |b| {
            b.to_async(&runtime).iter(|| async { with_sea_orm::load(&sea_orm_db, &keys).await })
        });
        group.bench_function(BenchmarkId::from_parameter("diesel-async"), |b| {
            b.to_async(&runtime).iter(|| async { with_diesel::load(&mut *diesel_conn.lock().await, &keys).await })
        });
        group.finish();
    }
}

criterion_group!(benches, load);
criterion_main!(benches);
