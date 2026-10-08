//! Loads whose independent queries run concurrently on pooled connections.
//!
//! A load runs its root query, then the child queries of each level. The child queries of
//! one level only need the keys of the level above, so with [`Pooled`] they run at the same
//! time, each on a connection of its own. A connection is held for one query at a time.

use std::ops::{Deref, DerefMut};

use sqlx::Transaction;
use sqlx::pool::{Pool, PoolConnection};
use tokio::sync::{Mutex, MutexGuard, Semaphore, SemaphorePermit};

use crate::backend::{Backend, Conn};
use crate::error::Error;

/// Runs the queries of a load on up to `connections` connections of a pool, at the same
/// time where they are independent: the child queries of one level, such as the
/// collections and references of the root rows. Pass it where a load takes a connection:
///
/// ```ignore
/// let mut pooled = mabat::Pooled::snapshot(&pool, 4);
/// let tasks = mabat::load::<TaskView>().all(&mut pooled).await?;
/// ```
///
/// Queries on different connections see the same data only in a shared snapshot, which
/// [`Pooled::snapshot`] provides on PostgreSQL. [`Pooled::read_committed`] lets each query
/// see what is committed when it runs: a row may then reference a row that a concurrent
/// transaction deleted in between, which fails the load as a missing reference, and
/// collections may include rows committed after their parents were read.
///
/// Graph loads ([`crate::Load::graph`]) run their queries one at a time on one connection
/// of the pool, so that each entity is fetched by the same query on every run.
#[derive(Debug, Clone)]
pub struct Pooled<B: Backend> {
    pool: Pool<B>,
    connections: usize,
    snapshot: bool,
}

impl<B: Backend> Pooled<B> {
    /// Each query sees the data committed when it runs, on any database. At least one
    /// connection is used.
    pub fn read_committed(pool: &Pool<B>, connections: usize) -> Self {
        Pooled { pool: pool.clone(), connections: connections.max(1), snapshot: false }
    }

    /// The most connections a load uses at once.
    pub fn connections(&self) -> usize {
        self.connections
    }
}

#[cfg(feature = "postgres")]
impl Pooled<sqlx::Postgres> {
    /// Every query of a load sees the same snapshot of the database, as one query would:
    /// the first connection begins a `REPEATABLE READ READ ONLY` transaction and exports
    /// its snapshot, which the other connections import. At least one connection is used.
    pub fn snapshot(pool: &Pool<sqlx::Postgres>, connections: usize) -> Self {
        Pooled { pool: pool.clone(), connections: connections.max(1), snapshot: true }
    }
}

impl<B: Backend> Conn for Pooled<B> {
    type Backend = B;

    fn source(&mut self) -> Source<'_, B> {
        Source::Pool(self)
    }
}

/// Where the queries of a load run: a connection, or a pool. See [`Conn`].
#[doc(hidden)]
pub enum Source<'c, B: Backend> {
    Connection(&'c mut B::Connection),
    Pool(&'c Pooled<B>),
}

impl<'c, B: Backend> Source<'c, B> {
    /// One connection, for what runs on one: checks and counting.
    pub(crate) async fn single(self) -> Result<Single<'c, B>, Error> {
        Ok(match self {
            Source::Connection(conn) => Single::Borrowed(conn),
            Source::Pool(pooled) => Single::Pooled(pooled.pool.acquire().await.map_err(Error::Connection)?),
        })
    }
}

/// A connection of a [`Source`].
pub(crate) enum Single<'c, B: Backend> {
    Borrowed(&'c mut B::Connection),
    Pooled(PoolConnection<B>),
}

impl<B: Backend> Deref for Single<'_, B> {
    type Target = B::Connection;

    fn deref(&self) -> &B::Connection {
        match self {
            Single::Borrowed(conn) => conn,
            Single::Pooled(conn) => conn,
        }
    }
}

impl<B: Backend> DerefMut for Single<'_, B> {
    fn deref_mut(&mut self) -> &mut B::Connection {
        match self {
            Single::Borrowed(conn) => conn,
            Single::Pooled(conn) => conn,
        }
    }
}

/// Gives the queries of a load their connections.
pub(crate) enum Runner<'c, B: Backend> {
    /// One connection, for one query at a time.
    One(Mutex<&'c mut B::Connection>),
    /// Connections of a pool, opened as they are needed.
    Pool(Workers<B>),
}

impl<'c, B: Backend> Runner<'c, B> {
    /// A runner for a load. A graph load runs on one connection, even of a pool.
    pub(crate) async fn new(source: Source<'c, B>, graph: bool) -> Result<Runner<'c, B>, Error> {
        Ok(match source {
            Source::Connection(conn) => Runner::One(Mutex::new(conn)),
            Source::Pool(pooled) => {
                let connections = if graph { 1 } else { pooled.connections };
                Runner::Pool(Workers::new(pooled, connections).await?)
            }
        })
    }

    /// Whether queries can run at the same time.
    pub(crate) fn concurrent(&self) -> bool {
        matches!(self, Runner::Pool(workers) if workers.connections > 1)
    }

    /// A connection for one query, waiting until one is free.
    pub(crate) async fn lease(&self) -> Result<Lease<'_, 'c, B>, Error> {
        match self {
            Runner::One(conn) => Ok(Lease::One(conn.lock().await)),
            Runner::Pool(workers) => {
                let permit = workers.permits.acquire().await.expect("the semaphore is never closed");
                let idle = workers.idle.lock().unwrap_or_else(|e| e.into_inner()).pop();
                let worker = match idle {
                    Some(worker) => worker,
                    None => workers.open().await?,
                };
                Ok(Lease::Pool { worker: Some(worker), workers, _permit: permit })
            }
        }
    }

    /// End the transactions of the pooled connections, once every query has run.
    pub(crate) async fn finish(self) -> Result<(), Error> {
        if let Runner::Pool(workers) = self {
            let idle = std::mem::take(&mut *workers.idle.lock().unwrap_or_else(|e| e.into_inner()));
            for worker in idle {
                if let Worker::Snapshot(tx) = worker {
                    tx.rollback().await.map_err(Error::Connection)?;
                }
            }
        }
        Ok(())
    }
}

/// The pooled connections of a load.
pub(crate) struct Workers<B: Backend> {
    pool: Pool<B>,
    connections: usize,
    /// The snapshot the connections import, for [`Pooled::snapshot`].
    snapshot: Option<String>,
    idle: std::sync::Mutex<Vec<Worker<B>>>,
    permits: Semaphore,
}

impl<B: Backend> Workers<B> {
    async fn new(pooled: &Pooled<B>, connections: usize) -> Result<Workers<B>, Error> {
        let mut idle = Vec::new();
        let mut snapshot = None;
        if pooled.snapshot {
            // The first connection exports the snapshot and keeps it alive until the end
            let (tx, id) = B::begin_snapshot(&pooled.pool, None).await.map_err(Error::Connection)?;
            idle.push(Worker::Snapshot(tx));
            snapshot = Some(id);
        }
        Ok(Workers {
            pool: pooled.pool.clone(),
            connections,
            snapshot,
            idle: std::sync::Mutex::new(idle),
            permits: Semaphore::new(connections),
        })
    }

    async fn open(&self) -> Result<Worker<B>, Error> {
        Ok(match &self.snapshot {
            Some(id) => {
                let (tx, _) = B::begin_snapshot(&self.pool, Some(id.clone())).await.map_err(Error::Connection)?;
                Worker::Snapshot(tx)
            }
            None => Worker::Connection(self.pool.acquire().await.map_err(Error::Connection)?),
        })
    }
}

/// A pooled connection of a load: in the load's snapshot, or not in a transaction.
pub(crate) enum Worker<B: Backend> {
    Connection(PoolConnection<B>),
    /// Rolled back when dropped, if the load does not finish.
    Snapshot(Transaction<'static, B>),
}

/// A connection held for one query.
pub(crate) enum Lease<'r, 'c, B: Backend> {
    One(MutexGuard<'r, &'c mut B::Connection>),
    Pool { worker: Option<Worker<B>>, workers: &'r Workers<B>, _permit: SemaphorePermit<'r> },
}

impl<B: Backend> Deref for Lease<'_, '_, B> {
    type Target = B::Connection;

    fn deref(&self) -> &B::Connection {
        match self {
            Lease::One(conn) => conn,
            Lease::Pool { worker: Some(Worker::Connection(conn)), .. } => conn,
            Lease::Pool { worker: Some(Worker::Snapshot(tx)), .. } => tx,
            Lease::Pool { worker: None, .. } => unreachable!("a lease holds its worker until dropped"),
        }
    }
}

impl<B: Backend> DerefMut for Lease<'_, '_, B> {
    fn deref_mut(&mut self) -> &mut B::Connection {
        match self {
            Lease::One(conn) => conn,
            Lease::Pool { worker: Some(Worker::Connection(conn)), .. } => conn,
            Lease::Pool { worker: Some(Worker::Snapshot(tx)), .. } => tx,
            Lease::Pool { worker: None, .. } => unreachable!("a lease holds its worker until dropped"),
        }
    }
}

impl<B: Backend> Drop for Lease<'_, '_, B> {
    fn drop(&mut self) {
        // The connection goes back before the permit is released
        if let Lease::Pool { worker, workers, .. } = self
            && let Some(worker) = worker.take()
        {
            workers.idle.lock().unwrap_or_else(|e| e.into_inner()).push(worker);
        }
    }
}
