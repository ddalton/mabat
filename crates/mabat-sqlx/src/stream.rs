//! Loading many values a batch at a time, as a stream: [`Load::stream`] and
//! [`Load::json_stream`].
//!
//! The keys of the matching roots are read with one query that applies the filter, the order
//! and the page. Each batch of keys is then loaded as [`Load::by_keys`] loads keys: the root
//! query and every child query, for that batch. The values of a batch are yielded in the order
//! of the keys, and at most one batch is in memory. On one PostgreSQL connection the keys are
//! read a batch at a time from a cursor; otherwise they are all read first.

use std::collections::{HashMap, VecDeque};

use futures_util::stream::{self, Stream};
use mabat_core::KEY_ALIAS;
use mabat_core::sql::RootOptions;

use crate::backend::{Backend, Conn};
use crate::graph::Identity;
use crate::key::{Key, KeyList};
use crate::node::{KeysQuery, Node, read_keys};
use crate::pooled::{Runner, Source};
use crate::{Error, Load, Prepared, View, ViewDecoder};

/// The number of roots of a batch, unless [`Load::batch_size`] says otherwise.
pub(crate) const BATCH_SIZE: usize = 1000;

/// Decodes a root row of a batch.
type Decode<B, I> = fn(&<B as sqlx::Database>::Row, &Node<B>) -> Result<I, Error>;

impl<T: View> Load<T> {
    /// The number of values [`Load::stream`] loads at a time: 1,000 by default, at least 1.
    pub fn batch_size(mut self, size: usize) -> Self {
        self.batch_size = size.max(1);
        self
    }

    /// Load the matching values a batch at a time, as a stream, so that loading many values
    /// does not hold them all in memory.
    ///
    /// The keys of the matching values are read with the filter, order, limit and offset, then
    /// each batch of [`Load::batch_size`] values is loaded with its collections and references,
    /// and yielded in order. On a PostgreSQL connection the keys are fetched a batch at a time
    /// from a cursor, `WITH HOLD`, which a dropped stream leaves open until the next stream on
    /// the connection; elsewhere they are all read first. Outside a snapshot, a later batch sees what was
    /// committed after the keys were read: a value deleted in between is skipped. In a
    /// `REPEATABLE READ` transaction, or with [`Pooled::snapshot`](crate::Pooled), every batch
    /// sees the same data.
    ///
    /// The stream holds the connection until it ends or is dropped; dropping it stops the
    /// load. An error is yielded once, and ends the stream. A view with references into a
    /// graph (`Ref<T>` fields) cannot be streamed: [`Error::GraphRequired`].
    ///
    /// ```ignore
    /// use futures_util::TryStreamExt;
    /// let mut tasks = mabat::load::<TaskView>().order_by("id").batch_size(500).stream(&mut conn);
    /// while let Some(task) = tasks.try_next().await? {
    ///     export(task)?;
    /// }
    /// ```
    pub fn stream<'c, C: Conn>(self, conn: &'c mut C) -> impl Stream<Item = Result<T, Error>> + 'c
    where
        T: ViewDecoder<C::Backend> + 'c,
    {
        let start = self.typed().and_then(|()| self.start::<C::Backend>(true));
        batches(start, conn.source(), T::decode)
    }

    /// Load the matching values as JSON objects a batch at a time, as a stream: the fields of
    /// [`Load::select`], or all fields, as [`Load::json`] writes them, loaded as
    /// [`Load::stream`] loads values. A view with references into a graph needs a selection.
    pub fn json_stream<'c, C: Conn>(self, conn: &'c mut C) -> impl Stream<Item = Result<serde_json::Value, Error>> + 'c
    where
        T: ViewDecoder<C::Backend>,
    {
        let unselected = self.selection.is_none();
        let start = self.start::<C::Backend>(unselected);
        batches(start, conn.source(), T::decode_json)
    }

    /// The load to stream, `None` if no key can match; `graph_refused` refuses views with
    /// references into a graph.
    fn start<B: Backend>(self, graph_refused: bool) -> Result<Option<(Prepared, usize)>, Error> {
        let batch_size = self.batch_size;
        let Some(load) = self.prepare::<B>()? else { return Ok(None) };
        if graph_refused && load.plan.has_graph_edges() {
            return Err(Error::GraphRequired { view: T::shape().name });
        }
        Ok(Some((load, batch_size)))
    }
}

/// Where a stream is.
struct State<'c, B: Backend, I> {
    source: Option<Source<'c, B>>,
    runner: Option<Runner<'c, B>>,
    load: Option<Prepared>,
    batch_size: usize,
    /// Where the keys of the batches still to load are.
    keys: Keys,
    /// The values of the batch being yielded.
    ready: VecDeque<I>,
    /// An error to yield, once.
    error: Option<Error>,
    decode: Decode<B, I>,
    done: bool,
}

fn batches<'c, B: Backend, I: 'c>(
    start: Result<Option<(Prepared, usize)>, Error>,
    source: Source<'c, B>,
    decode: Decode<B, I>,
) -> impl Stream<Item = Result<I, Error>> + 'c {
    let (load, batch_size, error) = match start {
        Ok(Some((load, batch_size))) => (Some(load), batch_size, None),
        Ok(None) => (None, BATCH_SIZE, None),
        Err(error) => (None, BATCH_SIZE, Some(error)),
    };
    let done = load.is_none();
    let state = State {
        source: Some(source),
        runner: None,
        load,
        batch_size,
        keys: Keys::Unread,
        ready: VecDeque::new(),
        error,
        decode,
        done,
    };
    stream::unfold(state, |mut state| async move {
        loop {
            if let Some(error) = state.error.take() {
                state.done = true;
                state.ready.clear();
                // The pooled connections of a snapshot are rolled back when dropped
                state.runner = None;
                return Some((Err(error), state));
            }
            if let Some(value) = state.ready.pop_front() {
                return Some((Ok(value), state));
            }
            if state.done {
                return None;
            }
            if let Err(error) = state.advance().await {
                state.error = Some(error);
            }
        }
    })
}

/// Where the keys of a stream's batches are.
enum Keys {
    /// Not read yet.
    Unread,
    /// All read, in batches.
    Read(VecDeque<Vec<Key>>),
    /// In the connection's cursor, read a batch at a time: PostgreSQL, on one connection.
    Cursor(KeysQuery),
}

impl<'c, B: Backend, I> State<'c, B, I> {
    /// Read the keys, load the next batch, or finish.
    async fn advance(&mut self) -> Result<(), Error> {
        let load = self.load.as_ref().expect("a stream that is not done has a load");
        let plan = &load.plan;
        let Some(runner) = &self.runner else {
            let source = self.source.take().expect("the keys are read once");
            let runner = self.runner.insert(Runner::new(source, false).await?);
            let overrides = load.overrides.as_ref().map(|c| &c.overrides);
            let query = KeysQuery::new::<B>(plan, &load.options, load.keys.clone(), overrides, load.values.clone())?;
            let mut conn = runner.lease().await?;
            let (sql, keys, values) = (query.sql.clone(), query.keys.clone(), query.values.clone());
            // A cursor would need a connection of the pool for the whole stream
            let cursor = matches!(runner, Runner::One(_))
                && B::open_cursor(&mut conn, sql.clone(), keys.clone(), values.clone())
                    .await
                    .map_err(|e| query.error(plan, e))?;
            self.keys = if cursor {
                Keys::Cursor(query)
            } else {
                let rows = B::fetch(&mut conn, sql, keys, values).await.map_err(|e| query.error(plan, e))?;
                let keys = read_keys::<B>(plan, &rows)?;
                Keys::Read(keys.chunks(self.batch_size).map(<[Key]>::to_vec).collect())
            };
            return Ok(());
        };
        let keys = match &mut self.keys {
            Keys::Unread => unreachable!("the keys are read with the runner"),
            Keys::Read(batches) => batches.pop_front(),
            Keys::Cursor(query) => {
                let mut conn = runner.lease().await?;
                let rows = B::fetch_cursor(&mut conn, self.batch_size).await.map_err(|e| query.error(plan, e))?;
                if rows.len() < self.batch_size {
                    B::close_cursor(&mut conn).await.map_err(|e| query.error(plan, e))?;
                    self.keys = Keys::Read(VecDeque::new());
                }
                // No row is the end; rows whose keys are all NULL are an empty batch
                (!rows.is_empty()).then(|| read_keys::<B>(plan, &rows)).transpose()?
            }
        };
        let Some(keys) = keys else {
            self.done = true;
            if let Some(runner) = self.runner.take() {
                runner.finish().await?;
            }
            return Ok(());
        };
        if keys.is_empty() {
            return Ok(());
        }

        // The batch's values, in the order of its keys; a root deleted since its key was read
        // is skipped
        let view = load.plan.shape.name;
        let positions: HashMap<&Key, usize> = keys.iter().enumerate().map(|(i, key)| (key, i)).collect();
        let list = KeyList::new(keys.clone()).map_err(|_| Error::MixedKeys { view })?;
        let options = RootOptions { by_keys: true, filter: load.options.filter.clone(), ..RootOptions::default() };
        let node = load.load_on(runner, Identity::new(false), Some(list), options).await?;
        let mut values = Vec::with_capacity(keys.len());
        for row in node.rows() {
            let position = node.key(row, KEY_ALIAS)?.and_then(|key| positions.get(&key).copied());
            values.push((position.unwrap_or(usize::MAX), (self.decode)(row, &node)?));
        }
        values.sort_by_key(|(position, _)| *position);
        self.ready = values.into_iter().map(|(_, value)| value).collect();
        Ok(())
    }
}
