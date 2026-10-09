//! Checking the views from a build script, so that `cargo build` fails when they no longer match
//! the schema.
//!
//! ```no_run
//! // In the `main` of build.rs, with `mabat-check` in [build-dependencies]
//! mabat_check::build("mabat/views.json", "mabat/schema.json").overrides("mabat/overrides").run();
//! ```
//!
//! The build script checks the committed manifest, which the application's manifest test keeps up
//! to date (`Builder::manifest`), against the committed snapshot, which `mabat schema --check`
//! keeps up to date in CI. Errors fail the build, warnings are shown by Cargo, and the build
//! script runs again when either file or an override file changes.

use std::path::{Path, PathBuf};

use crate::manifest::Manifest;
use crate::report::{Diagnostic, Report, Severity};
use crate::schema::Snapshot;

/// Set to skip the check, e.g. while the schema and the views change together.
pub const SKIP_VARIABLE: &str = "MABAT_SKIP_CHECK";

/// Check the views of the manifest against the snapshot from a build script: see [`Build`].
/// Relative paths are relative to the package, as Cargo runs build scripts there.
pub fn build(manifest: impl Into<PathBuf>, snapshot: impl Into<PathBuf>) -> Build {
    Build { manifest: manifest.into(), snapshot: snapshot.into(), overrides: Vec::new() }
}

/// A check of the views against a snapshot of the schema, from a build script.
#[derive(Debug, Clone)]
#[must_use = "a build check does nothing until it is run"]
pub struct Build {
    manifest: PathBuf,
    snapshot: PathBuf,
    overrides: Vec<PathBuf>,
}

/// What a build check found.
#[derive(Debug)]
pub enum Outcome {
    /// The views were checked.
    Checked(Report),
    /// A file is missing, e.g. before the manifest test first wrote the manifest: nothing was
    /// checked.
    Missing(PathBuf),
    /// [`SKIP_VARIABLE`] is set.
    Skipped,
}

impl Build {
    /// A directory of override files, whose names are checked; their SQL needs a database.
    pub fn overrides(mut self, dir: impl Into<PathBuf>) -> Self {
        self.overrides.push(dir.into());
        self
    }

    /// Check the views, and tell Cargo: errors fail the build, warnings are shown, and the
    /// build script runs again when the files change.
    ///
    /// # Panics
    ///
    /// If a file cannot be read or parsed, or the manifest and the snapshot are of different
    /// databases, which fails the build with the message.
    pub fn run(self) {
        for path in self.watched() {
            println!("cargo::rerun-if-changed={}", path.display());
        }
        println!("cargo::rerun-if-env-changed={SKIP_VARIABLE}");
        let outcome = self.check().unwrap_or_else(|message| panic!("mabat-check: {message}"));
        for line in directives(&outcome) {
            println!("{line}");
        }
    }

    /// Check the views, without telling Cargo anything.
    pub fn check(&self) -> Result<Outcome, String> {
        if std::env::var_os(SKIP_VARIABLE).is_some_and(|v| !v.is_empty() && v != "0") {
            return Ok(Outcome::Skipped);
        }
        let manifest = resolve(&self.manifest);
        let snapshot_path = resolve(&self.snapshot);
        for path in [&manifest, &snapshot_path] {
            if !path.exists() {
                return Ok(Outcome::Missing(path.clone()));
            }
        }
        let read =
            |path: &Path| std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()));
        let views = Manifest::from_json(&read(&manifest)?)
            .map_err(|e| format!("{} is not a manifest: {e}", manifest.display()))?;
        let snapshot = Snapshot::from_json(&read(&snapshot_path)?)
            .map_err(|e| format!("{} is not a snapshot: {e}", snapshot_path.display()))?;
        let overrides: Vec<PathBuf> =
            self.overrides.iter().map(|dir| resolve(dir)).filter(|dir| dir.exists()).collect();
        let report = views.check_snapshot(&snapshot, &self.snapshot.display().to_string(), &overrides)?;
        Ok(Outcome::Checked(report))
    }

    fn watched(&self) -> impl Iterator<Item = PathBuf> + '_ {
        [&self.manifest, &self.snapshot].into_iter().chain(&self.overrides).map(|path| resolve(path))
    }
}

/// A path relative to the package being built.
fn resolve(path: &Path) -> PathBuf {
    match std::env::var_os("CARGO_MANIFEST_DIR") {
        Some(dir) if path.is_relative() => Path::new(&dir).join(path),
        _ => path.to_path_buf(),
    }
}

/// The lines that tell Cargo what a check found: `cargo::error=` for each error, which fails
/// the build, and `cargo::warning=` for each warning and for a check that did not run.
pub fn directives(outcome: &Outcome) -> Vec<String> {
    match outcome {
        Outcome::Skipped => vec![format!("cargo::warning=mabat: the views were not checked, {SKIP_VARIABLE} is set")],
        Outcome::Missing(path) => {
            vec![format!("cargo::warning=mabat: the views were not checked, {} does not exist", path.display())]
        }
        Outcome::Checked(report) => report.diagnostics().iter().map(directive).collect(),
    }
}

/// One line for a diagnostic, as Cargo shows a line per directive.
fn directive(diagnostic: &Diagnostic) -> String {
    let kind = match diagnostic.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    let mut line = format!("cargo::{kind}=mabat {kind}[{}]: {}", diagnostic.code, diagnostic.summary);
    for note in &diagnostic.notes {
        line.push_str("; ");
        line.push_str(note);
    }
    if let Some(origin) = &diagnostic.origin {
        line.push_str(&format!(" ({origin})"));
    }
    line.replace(['\n', '\r'], " ")
}
