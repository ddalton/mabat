//! The registry of views and their checked overrides.

use std::collections::HashMap;
use std::fmt::Write;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::ArcSwap;

use mabat_core::sql::{self, RootOptions};
use mabat_core::{ChildQuery, QueryPlan, ViewShape};

use crate::backend::{Backend, Conn};
use crate::check::{self, Checked, ViewEntry};
use crate::manifest::{self, Manifest, ScaffoldFormat};
use crate::overrides::{Origin, Source, parse_sources, read_sources};
use crate::report::{Diagnostic, Report, Severity};
use crate::{Error, Load, View, ViewDecoder};

/// A checked override, used instead of the generated SQL of its query.
#[derive(Debug, Clone)]
pub(crate) struct ActiveOverride {
    pub(crate) sql: Arc<str>,
    /// `true` if the override takes the keys as `$1`. Always `true` except for a root
    /// override without parameters, which is filtered by the keys as a subquery.
    pub(crate) keys_param: bool,
    pub(crate) shadow: bool,
    pub(crate) origin: Origin,
    pub(crate) stats: Arc<ShadowStats>,
}

impl ActiveOverride {
    /// The root SQL of a load ([`crate::Load::sql`]), which is not checked beforehand.
    pub(crate) fn of_load(sql: String) -> ActiveOverride {
        ActiveOverride {
            keys_param: mabat_core::sql::takes_keys(&sql),
            sql: sql.into(),
            shadow: false,
            origin: Origin { file: "Load::sql".to_string(), line: 1 },
            stats: Arc::new(ShadowStats::default()),
        }
    }
}

/// The checked overrides of a view, by query name.
pub(crate) type Overrides = HashMap<String, ActiveOverride>;

/// Counters of a shadowed override.
#[derive(Debug, Default)]
pub(crate) struct ShadowStats {
    runs: AtomicU64,
    mismatches: AtomicU64,
    override_nanos: AtomicU64,
    generated_nanos: AtomicU64,
}

impl ShadowStats {
    pub(crate) fn record(&self, override_time: Duration, generated_time: Duration) {
        self.runs.fetch_add(1, Ordering::Relaxed);
        self.override_nanos.fetch_add(nanos(override_time), Ordering::Relaxed);
        self.generated_nanos.fetch_add(nanos(generated_time), Ordering::Relaxed);
    }

    pub(crate) fn mismatch(&self) {
        self.mismatches.fetch_add(1, Ordering::Relaxed);
    }
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// What a shadowed override did so far: how often it ran, how often its rows differed
/// from the generated query's, and the total time each took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowSummary {
    pub view: &'static str,
    pub query: String,
    pub origin: Origin,
    pub runs: u64,
    pub mismatches: u64,
    pub override_time: Duration,
    pub generated_time: Duration,
}

/// What to do when an override does not pass the checks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OnInvalid {
    /// Fail [`Builder::build`] with the report. The default.
    #[default]
    Fail,
    /// Build anyway and run the generated query in place of each invalid override. The
    /// problems are in [`Mabat::report`].
    UseGenerated,
}

/// Builds a [`Mabat`] registry for database `B`. Created by [`Mabat::builder`].
#[must_use = "a builder does nothing until it is built"]
pub struct Builder<B: Backend> {
    views: Vec<ViewEntry<B>>,
    dirs: Vec<PathBuf>,
    inline: Vec<Source>,
    on_invalid: OnInvalid,
}

impl<B: Backend> Default for Builder<B> {
    fn default() -> Self {
        Builder { views: Vec::new(), dirs: Vec::new(), inline: Vec::new(), on_invalid: OnInvalid::default() }
    }
}

impl<B: Backend> Clone for Builder<B> {
    fn clone(&self) -> Self {
        Builder {
            views: self.views.clone(),
            dirs: self.dirs.clone(),
            inline: self.inline.clone(),
            on_invalid: self.on_invalid,
        }
    }
}

impl<B: Backend> Builder<B> {
    /// Register a view, so that it can be loaded with [`Mabat::load`] and overridden.
    /// The views it contains do not need to be registered.
    pub fn register<T: ViewDecoder<B>>(mut self) -> Self {
        let shape = T::shape();
        if !self.views.iter().any(|v| std::ptr::eq(v.shape, shape)) {
            self.views.push(ViewEntry { shape, describe: T::describe });
        }
        self
    }

    /// Read override files from a directory: one file per view, named after the view,
    /// either TOML (`TaskView.toml`) or SQL with a marker line before each query
    /// (`TaskView.sql`). Other files are ignored. The directory is read again by
    /// [`Mabat::reload`].
    pub fn overrides_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dirs.push(dir.into());
        self
    }

    /// The overrides of a view as TOML, in the format of an override file. For tests and
    /// for overrides that are embedded in the binary.
    pub fn overrides(mut self, view: impl Into<String>, toml: impl Into<String>) -> Self {
        let view = view.into();
        let file = format!("{view}.toml (inline)");
        self.inline.push(Source::toml(view, file, toml));
        self
    }

    /// The overrides of a view as SQL, in the format of a `.sql` override file.
    pub fn overrides_sql(mut self, view: impl Into<String>, sql: impl Into<String>) -> Self {
        let view = view.into();
        let file = format!("{view}.sql (inline)");
        self.inline.push(Source::sql(view, file, sql));
        self
    }

    /// What to do when an override does not pass the checks at build time,
    /// [`OnInvalid::Fail`] by default.
    pub fn on_invalid(mut self, on_invalid: OnInvalid) -> Self {
        self.on_invalid = on_invalid;
        self
    }

    /// Check the registered views and the overrides against the database, without
    /// building the registry. Use this in CI:
    ///
    /// ```ignore
    /// let report = builder().check(&mut conn).await?;
    /// assert!(report.is_ok(), "{report}");
    /// ```
    ///
    /// Every query, generated or overridden, is prepared on the database without running
    /// it. When the connection is in a transaction, each one is prepared in a savepoint
    /// that is rolled back, so a failing statement does not abort the transaction.
    pub async fn check<C: Conn<Backend = B>>(&self, conn: &mut C) -> Result<Report, Error> {
        let mut report = Report::default();
        let sources = self.read_sources(&mut report);
        let mut conn = conn.source().single().await?;
        Ok(self.run_checks(&mut conn, &sources, report).await?.1)
    }

    /// Check the views and the overrides against the database, as [`Builder::check`] does,
    /// and build the registry.
    ///
    /// Fails with [`Error::Invalid`] if there are errors, unless invalid overrides are
    /// replaced by the generated queries with [`OnInvalid::UseGenerated`]. Errors that no
    /// override is involved in, such as a generated query that does not match the
    /// database, always fail.
    pub async fn build<C: Conn<Backend = B>>(self, conn: &mut C) -> Result<Mabat<B>, Error> {
        let mut report = Report::default();
        let sources = self.read_sources(&mut report);
        let fingerprint = fingerprint(&sources, &report);
        let mut conn = conn.source().single().await?;
        let (checked, report) = self.run_checks(&mut conn, &sources, report).await?;
        let fails = match self.on_invalid {
            OnInvalid::Fail => !report.is_ok(),
            OnInvalid::UseGenerated => report.errors().any(|d| d.origin.is_none() || d.code == "M0301"),
        };
        if fails {
            return Err(Error::Invalid(report));
        }
        Ok(Mabat {
            builder: self,
            state: ArcSwap::from_pointee(State::new(checked, report)),
            files: Mutex::new(Fingerprints { active: fingerprint, rejected: None }),
        })
    }

    async fn run_checks(
        &self,
        conn: &mut B::Connection,
        sources: &[Source],
        mut report: Report,
    ) -> Result<(Vec<Checked>, Report), Error> {
        let mut names: HashMap<&str, usize> = HashMap::new();
        for view in &self.views {
            *names.entry(view.shape.name).or_default() += 1;
        }
        for (name, count) in names {
            if count > 1 {
                report.push(Diagnostic {
                    severity: Severity::Error,
                    code: "M0101",
                    view: name.to_string(),
                    query: String::new(),
                    origin: None,
                    summary: format!("{count} registered views are named {name}"),
                    notes: vec!["override files are named after the view, so view names need to be unique".into()],
                });
            }
        }

        let (manifest, plans) = self.build_manifest(&mut report);
        let files = parse_sources(sources, &mut report);
        let mut overrides = check::check::<B>(conn, &manifest, files, &mut report).await.map_err(Error::Check)?;
        let checked = plans
            .into_iter()
            .map(|(shape, plan)| Checked {
                shape,
                plan: Arc::new(plan),
                overrides: overrides.remove(shape.name).unwrap_or_default(),
                backend: B::NAME,
            })
            .collect();
        Ok((checked, report))
    }

    /// The manifest of the registered views, and their plans. A view that cannot be planned
    /// is left out and reported.
    fn build_manifest(&self, report: &mut Report) -> (Manifest, Vec<(&'static ViewShape, QueryPlan)>) {
        let mut views = Vec::new();
        let mut plans = Vec::new();
        for view in &self.views {
            match manifest::build::<B>(view) {
                Ok((manifest, plan)) => {
                    views.push(manifest);
                    plans.push((view.shape, plan));
                }
                Err(e) => report.push(Diagnostic {
                    severity: Severity::Error,
                    code: "M0301",
                    view: view.shape.name.to_string(),
                    query: String::new(),
                    origin: None,
                    summary: format!("{} cannot be planned", view.shape.name),
                    notes: vec![e.to_string()],
                }),
            }
        }
        (Manifest { format: manifest::FORMAT, backend: B::NAME.to_string(), views }, plans)
    }

    /// The manifest of the registered views, for checking overrides without the
    /// application with `mabat check`, see [`Manifest`].
    pub fn manifest(&self) -> Result<Manifest, Error> {
        let mut views = Vec::new();
        for view in &self.views {
            views.push(manifest::build::<B>(view)?.0);
        }
        Ok(Manifest { format: manifest::FORMAT, backend: B::NAME.to_string(), views })
    }

    /// Read the override files of the directories, and the inline overrides.
    fn read_sources(&self, report: &mut Report) -> Vec<Source> {
        read_sources(&self.dirs, &self.inline, report)
    }
}

/// A hash of the override files and the problems reading them, to tell whether they changed.
fn fingerprint(sources: &[Source], read_problems: &Report) -> u64 {
    let mut hasher = DefaultHasher::new();
    sources.hash(&mut hasher);
    for diagnostic in read_problems.diagnostics() {
        diagnostic.to_string().hash(&mut hasher);
    }
    hasher.finish()
}

fn shape_id(shape: &'static ViewShape) -> usize {
    std::ptr::from_ref(shape) as usize
}

/// The registered views, their plans and their checked overrides.
///
/// Build it once at startup and share it. Loads see a consistent set of overrides: one
/// that started before a [`Mabat::reload`] finishes with the overrides it started with.
///
/// ```ignore
/// let mabat = Mabat::builder()
///     .register::<TaskView>()
///     .overrides_dir("mabat/overrides")
///     .build(&mut conn)
///     .await?;
///
/// let task = mabat.load::<TaskView>().by_key(id).one(&mut *tx).await?;
/// ```
pub struct Mabat<B: Backend> {
    builder: Builder<B>,
    state: ArcSwap<State>,
    files: Mutex<Fingerprints>,
}

/// The checked views, replaced as a whole by a reload.
struct State {
    views: HashMap<usize, Arc<Checked>>,
    report: Arc<Report>,
}

impl State {
    fn new(checked: Vec<Checked>, report: Report) -> State {
        let views = checked.into_iter().map(|c| (shape_id(c.shape), Arc::new(c))).collect();
        State { views, report: Arc::new(report) }
    }
}

/// The override files in use, and the last ones a reload rejected.
struct Fingerprints {
    active: u64,
    rejected: Option<u64>,
}

/// The result of [`Mabat::reload`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reloaded {
    /// The override files are the same as at the last build or reload attempt; nothing
    /// was checked.
    Unchanged,
    /// The override files changed, passed the checks and are in use. The report has the
    /// warnings.
    Updated(Report),
}

impl<B: Backend> Mabat<B> {
    /// A builder for a registry of database `B`, which the connection given to
    /// [`Builder::build`] or [`Builder::check`] determines.
    pub fn builder() -> Builder<B> {
        Builder::default()
    }

    /// Start loading values of a registered view, with its overrides.
    ///
    /// Loading a view that is not registered fails with [`Error::NotRegistered`].
    pub fn load<T: View>(&self) -> Load<T> {
        Load::registered(self.checked::<T>())
    }

    fn checked<T: View>(&self) -> Option<Arc<Checked>> {
        self.state.load().views.get(&shape_id(T::shape())).cloned()
    }

    /// The plan of a registered view.
    pub fn plan<T: View>(&self) -> Option<Arc<QueryPlan>> {
        self.checked::<T>().map(|c| c.plan.clone())
    }

    /// The warnings of checking the views, and with [`OnInvalid::UseGenerated`] the errors
    /// of the overrides that are not used. After a reload, the report of the reload.
    pub fn report(&self) -> Arc<Report> {
        self.state.load().report.clone()
    }

    /// Read the override files again and, if they changed, check them and put them in use.
    ///
    /// The change is all or nothing: if the new files have any error, the overrides in use
    /// stay as they are and the reload fails with [`Error::Invalid`], whatever
    /// [`OnInvalid`] is. The same files are not checked again until they change, so this
    /// can be called on a timer, on `SIGHUP` or from an admin endpoint:
    ///
    /// ```ignore
    /// let mut interval = tokio::time::interval(Duration::from_secs(10));
    /// loop {
    ///     interval.tick().await;
    ///     match mabat.reload(&mut *pool.acquire().await?).await {
    ///         Ok(Reloaded::Updated(_)) => tracing::info!("overrides reloaded"),
    ///         Ok(Reloaded::Unchanged) => {}
    ///         Err(e) => tracing::error!("overrides not reloaded: {e}"),
    ///     }
    /// }
    /// ```
    ///
    /// Shadow statistics are kept for overrides whose SQL did not change.
    pub async fn reload<C: Conn<Backend = B>>(&self, conn: &mut C) -> Result<Reloaded, Error> {
        let mut conn = conn.source().single().await?;
        let conn = &mut *conn;
        let mut report = Report::default();
        let sources = self.builder.read_sources(&mut report);
        let fingerprint = fingerprint(&sources, &report);
        {
            let files = self.files.lock().unwrap_or_else(|e| e.into_inner());
            if files.active == fingerprint || files.rejected == Some(fingerprint) {
                return Ok(Reloaded::Unchanged);
            }
        }

        let (mut checked, report) = self.builder.run_checks(conn, &sources, report).await?;
        if !report.is_ok() {
            self.files.lock().unwrap_or_else(|e| e.into_inner()).rejected = Some(fingerprint);
            return Err(Error::Invalid(report));
        }

        let previous = self.state.load();
        for view in &mut checked {
            let Some(old) = previous.views.get(&shape_id(view.shape)) else { continue };
            for (query, active) in &mut view.overrides {
                if let Some(old) = old.overrides.get(query)
                    && old.sql == active.sql
                    && old.shadow == active.shadow
                {
                    active.stats = old.stats.clone();
                }
            }
        }

        self.state.store(Arc::new(State::new(checked, report.clone())));
        *self.files.lock().unwrap_or_else(|e| e.into_inner()) = Fingerprints { active: fingerprint, rejected: None };
        Ok(Reloaded::Updated(report))
    }

    /// A readable description of the queries of a registered view, showing the SQL that
    /// runs for each: the override, or the generated SQL.
    pub fn explain<T: View>(&self) -> Option<String> {
        let checked = self.checked::<T>()?;
        let mut out = String::new();
        explain_query(&mut out, &checked.plan, &checked.overrides, 0);
        Some(out)
    }

    /// The statistics of every shadowed override.
    pub fn shadow_stats(&self) -> Vec<ShadowSummary> {
        let state = self.state.load();
        let mut summaries = Vec::new();
        for checked in state.views.values() {
            for (query, active) in &checked.overrides {
                if !active.shadow {
                    continue;
                }
                let stats = &active.stats;
                summaries.push(ShadowSummary {
                    view: checked.shape.name,
                    query: query.clone(),
                    origin: active.origin.clone(),
                    runs: stats.runs.load(Ordering::Relaxed),
                    mismatches: stats.mismatches.load(Ordering::Relaxed),
                    override_time: Duration::from_nanos(stats.override_nanos.load(Ordering::Relaxed)),
                    generated_time: Duration::from_nanos(stats.generated_nanos.load(Ordering::Relaxed)),
                });
            }
        }
        summaries.sort_by(|a, b| (a.view, &a.query).cmp(&(b.view, &b.query)));
        summaries
    }
}

fn explain_query(out: &mut String, plan: &QueryPlan, overrides: &Overrides, depth: usize) {
    let indent = "  ".repeat(depth);
    let _ = writeln!(out, "{indent}{}: {}{}", plan.query_name(), plan.shape.name, plan.link.describe());
    match overrides.get(plan.query_name()) {
        Some(active) => {
            let shadow = if active.shadow { ", shadowed" } else { "" };
            let _ = writeln!(out, "{indent}  override ({}{shadow}):", active.origin);
            for line in active.sql.trim().lines() {
                let _ = writeln!(out, "{indent}    {}", line.trim_end());
            }
        }
        None => {
            let _ = writeln!(out, "{indent}  {}", sql::select(plan, &RootOptions::default()));
        }
    }
    for child in &plan.children {
        let field = plan.shape.fields[child.field_index].name;
        match &child.query {
            ChildQuery::Query(child) => explain_query(out, child, overrides, depth + 1),
            ChildQuery::Repeat { up, depth: levels } => {
                let _ = writeln!(out, "{indent}  {field}: repeats the query {up} level(s) up, at most {levels} levels");
            }
            ChildQuery::Same => {
                let _ = writeln!(out, "{indent}  {field}: all levels in this query");
            }
        }
    }
}

/// An override file for a view with the generated SQL of every query, as a starting point
/// for tuning. Delete the queries you do not change.
pub fn scaffold<T: ViewDecoder<B>, B: Backend>() -> Result<String, Error> {
    let manifest = Mabat::<B>::builder().register::<T>().manifest()?;
    Ok(manifest.scaffold(T::shape().name, ScaffoldFormat::Toml).expect("the view is in its manifest"))
}
