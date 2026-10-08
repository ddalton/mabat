//! The registry of views and their checked overrides.

use std::collections::HashMap;
use std::fmt::Write;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::ArcSwap;

use refract_core::sql::{self, Layout, RootOptions};
use refract_core::{Link, QueryPlan, ViewShape};
use sqlx::PgConnection;

use crate::check::{self, Checked, ViewEntry};
use crate::overrides::{self, Origin, OverrideFile};
use crate::report::{Diagnostic, Report, Severity};
use crate::{Error, Load, View};

/// A checked override, used instead of the generated SQL of its query.
#[derive(Debug)]
pub(crate) struct ActiveOverride {
    pub(crate) sql: Arc<str>,
    /// `true` if the override takes the keys as `$1`. Always `true` except for a root
    /// override without parameters, which is filtered by the keys as a subquery.
    pub(crate) keys_param: bool,
    pub(crate) shadow: bool,
    pub(crate) origin: Origin,
    pub(crate) stats: Arc<ShadowStats>,
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
    /// problems are in [`Refract::report`].
    UseGenerated,
}

/// Builds a [`Refract`] registry. Created by [`Refract::builder`].
#[derive(Clone, Default)]
#[must_use = "a builder does nothing until it is built"]
pub struct Builder {
    views: Vec<ViewEntry>,
    dirs: Vec<PathBuf>,
    inline: Vec<Source>,
    on_invalid: OnInvalid,
}

/// The content of an override file, before it is parsed.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Source {
    view: String,
    file: String,
    format: Format,
    content: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Format {
    Toml,
    Sql,
}

impl Builder {
    /// Register a view, so that it can be loaded with [`Refract::load`] and overridden.
    /// The views it contains do not need to be registered.
    pub fn register<T: View>(mut self) -> Self {
        let shape = T::shape();
        if !self.views.iter().any(|v| std::ptr::eq(v.shape, shape)) {
            self.views.push(ViewEntry { shape, describe: T::describe });
        }
        self
    }

    /// Read override files from a directory: one file per view, named after the view,
    /// either TOML (`TaskView.toml`) or SQL with a marker line before each query
    /// (`TaskView.sql`). Other files are ignored. The directory is read again by
    /// [`Refract::reload`].
    pub fn overrides_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dirs.push(dir.into());
        self
    }

    /// The overrides of a view as TOML, in the format of an override file. For tests and
    /// for overrides that are embedded in the binary.
    pub fn overrides(mut self, view: impl Into<String>, toml: impl Into<String>) -> Self {
        let view = view.into();
        let file = format!("{view}.toml (inline)");
        self.inline.push(Source { view, file, format: Format::Toml, content: toml.into() });
        self
    }

    /// The overrides of a view as SQL, in the format of a `.sql` override file.
    pub fn overrides_sql(mut self, view: impl Into<String>, sql: impl Into<String>) -> Self {
        let view = view.into();
        let file = format!("{view}.sql (inline)");
        self.inline.push(Source { view, file, format: Format::Sql, content: sql.into() });
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
    pub async fn check(&self, conn: &mut PgConnection) -> Result<Report, Error> {
        let mut report = Report::default();
        let sources = self.read_sources(&mut report);
        Ok(self.run_checks(conn, &sources, report).await?.1)
    }

    /// Check the views and the overrides against the database, as [`Builder::check`] does,
    /// and build the registry.
    ///
    /// Fails with [`Error::Invalid`] if there are errors, unless invalid overrides are
    /// replaced by the generated queries with [`OnInvalid::UseGenerated`]. Errors that no
    /// override is involved in, such as a generated query that does not match the
    /// database, always fail.
    pub async fn build(self, conn: &mut PgConnection) -> Result<Refract, Error> {
        let mut report = Report::default();
        let sources = self.read_sources(&mut report);
        let fingerprint = fingerprint(&sources, &report);
        let (checked, report) = self.run_checks(conn, &sources, report).await?;
        let fails = match self.on_invalid {
            OnInvalid::Fail => !report.is_ok(),
            OnInvalid::UseGenerated => report.errors().any(|d| d.origin.is_none() || d.code == "R0301"),
        };
        if fails {
            return Err(Error::Invalid(report));
        }
        Ok(Refract {
            builder: self,
            state: ArcSwap::from_pointee(State::new(checked, report)),
            files: Mutex::new(Fingerprints { active: fingerprint, rejected: None }),
        })
    }

    async fn run_checks(
        &self,
        conn: &mut PgConnection,
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
                    code: "R0101",
                    view: name.to_string(),
                    query: String::new(),
                    origin: None,
                    summary: format!("{count} registered views are named {name}"),
                    notes: vec!["override files are named after the view, so view names need to be unique".into()],
                });
            }
        }

        let files = parse_sources(sources, &mut report);
        let checked = check::check(conn, &self.views, files, &mut report).await.map_err(Error::Check)?;
        Ok((checked, report))
    }

    /// Read the override files of the directories, and the inline overrides.
    fn read_sources(&self, report: &mut Report) -> Vec<Source> {
        let mut sources = Vec::new();
        for dir in &self.dirs {
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(e) => {
                    report.push(file_error(&dir.display().to_string(), 1, format!("cannot read the directory: {e}")));
                    continue;
                }
            };
            let mut paths: Vec<(PathBuf, Format)> = entries
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter_map(|path| {
                    let format = match path.extension()?.to_str()? {
                        "toml" => Format::Toml,
                        "sql" => Format::Sql,
                        _ => return None,
                    };
                    Some((path, format))
                })
                .collect();
            paths.sort_by(|a, b| a.0.cmp(&b.0));
            for (path, format) in paths {
                let file = path.display().to_string();
                let view = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                match std::fs::read_to_string(&path) {
                    Ok(content) => sources.push(Source { view, file, format, content }),
                    Err(e) => report.push(file_error(&file, 1, format!("cannot read the file: {e}"))),
                }
            }
        }
        sources.extend(self.inline.iter().cloned());
        sources
    }
}

fn parse_sources(sources: &[Source], report: &mut Report) -> Vec<OverrideFile> {
    let mut files: Vec<OverrideFile> = Vec::new();
    for source in sources {
        if let Some(other) = files.iter().find(|f| f.view == source.view) {
            report.push(file_error(
                &source.file,
                1,
                format!("{} already has an override file, {}", source.view, other.file),
            ));
            continue;
        }
        let parsed = match source.format {
            Format::Toml => overrides::parse(&source.view, &source.file, &source.content),
            Format::Sql => overrides::parse_sql(&source.view, &source.file, &source.content),
        };
        match parsed {
            Ok(parsed) => files.push(parsed),
            Err((origin, message)) => report.push(file_error(&origin.file, origin.line, message)),
        }
    }
    files
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

fn file_error(file: &str, line: usize, message: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        code: "R0100",
        view: String::new(),
        query: String::new(),
        origin: Some(format!("{file}:{line}")),
        summary: "override file cannot be used".to_string(),
        notes: vec![message],
    }
}

fn shape_id(shape: &'static ViewShape) -> usize {
    std::ptr::from_ref(shape) as usize
}

/// The registered views, their plans and their checked overrides.
///
/// Build it once at startup and share it. Loads see a consistent set of overrides: one
/// that started before a [`Refract::reload`] finishes with the overrides it started with.
///
/// ```ignore
/// let refract = Refract::builder()
///     .register::<TaskView>()
///     .overrides_dir("refract/overrides")
///     .build(&mut conn)
///     .await?;
///
/// let task = refract.load::<TaskView>().by_key(id).one(&mut *tx).await?;
/// ```
pub struct Refract {
    builder: Builder,
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

/// The result of [`Refract::reload`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reloaded {
    /// The override files are the same as at the last build or reload attempt; nothing
    /// was checked.
    Unchanged,
    /// The override files changed, passed the checks and are in use. The report has the
    /// warnings.
    Updated(Report),
}

impl Refract {
    pub fn builder() -> Builder {
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
    ///     match refract.reload(&mut *pool.acquire().await?).await {
    ///         Ok(Reloaded::Updated(_)) => tracing::info!("overrides reloaded"),
    ///         Ok(Reloaded::Unchanged) => {}
    ///         Err(e) => tracing::error!("overrides not reloaded: {e}"),
    ///     }
    /// }
    /// ```
    ///
    /// Shadow statistics are kept for overrides whose SQL did not change.
    pub async fn reload(&self, conn: &mut PgConnection) -> Result<Reloaded, Error> {
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
        let mut queries = Vec::new();
        checked.plan.walk(&mut |plan| queries.push(plan));
        for plan in queries {
            let depth = plan.path.matches('.').count() + usize::from(!plan.path.is_empty());
            let indent = "  ".repeat(depth);
            let link = match &plan.link {
                Link::Root => String::new(),
                Link::Child { fk } => format!(" (to-many by {fk})"),
                Link::ToOne { ref_alias } => format!(" (to-one by {ref_alias})"),
            };
            let _ = writeln!(out, "{indent}{}: {}{link}", plan.query_name(), plan.shape.name);
            match checked.overrides.get(plan.query_name()) {
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
        }
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

/// An override file for a view with the generated SQL of every query, as a starting point
/// for tuning. Delete the queries you do not change.
pub fn scaffold<T: View>() -> Result<String, Error> {
    let plan = QueryPlan::build(T::shape())?;
    let view = T::shape().name;
    let mut out = format!(
        "# Overrides for {view}.\n\
         #\n\
         # Each query is addressed by its name and is replaced by its `sql`. The rows are decoded\n\
         # by column alias, so keep the aliases; joins, ordering, hints and the tables themselves\n\
         # can change. Every query is checked against {view} at startup.\n\
         #\n\
         # $root takes no parameter, and is then filtered, ordered and paged as a subquery, or\n\
         # the array of root keys as $1. The other queries take the array of the keys they are\n\
         # selected by as $1. Set `shadow = true` to also run the generated query and compare.\n"
    );
    let mut queries = Vec::new();
    plan.walk(&mut |plan| queries.push(plan));
    for plan in queries {
        let sql = sql::select_with(plan, &RootOptions::default(), Layout::Multiline);
        let _ = write!(out, "\n[query.{}]\nsql = {}\n", toml_key(plan.query_name()), toml_multiline(&sql));
    }
    Ok(out)
}

fn toml_key(key: &str) -> String {
    if key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        key.to_string()
    } else {
        toml_string(key)
    }
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

fn toml_multiline(sql: &str) -> String {
    if sql.contains("'''") { toml_string(sql) } else { format!("'''\n{sql}\n'''") }
}
