//! The result of checking the queries of registered views against the database.

use std::fmt;

/// How serious a [`Diagnostic`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A problem found by checking a query.
///
/// | Code | Problem |
/// | --- | --- |
/// | `M0100` | An override file could not be read or parsed |
/// | `M0101` | An override file or query does not address a registered view or query |
/// | `M0102` | The columns of a query do not match the view |
/// | `M0103` | A query does not prepare on the database |
/// | `M0104` | The parameters of a query do not match its link to the parent query |
/// | `M0105` | An optional path is not selected by an override, and is always `None` (warning) |
/// | `M0201` | A query reads a table that is not in the schema snapshot |
/// | `M0202` | A query reads a column that is not in the schema snapshot |
/// | `M0203` | A column has a type the field cannot be decoded from |
/// | `M0204` | A column is nullable under a field that is not an `Option` (warning) |
/// | `M0205` | The columns linking a query to its parent hold different kinds of key |
/// | `M0206` | A view's key column is not the primary key of its table (warning) |
/// | `M0207` | A generated key is on a column the database does not generate |
///
/// `M0201` to `M0207` come from checking against a schema snapshot, [`Manifest::check_snapshot`](crate::manifest::Manifest::check_snapshot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: &'static str,
    /// The view the query belongs to.
    pub view: String,
    /// The name of the query: `$root` or the path of the field it fills. Empty for a
    /// problem with a whole file.
    pub query: String,
    /// The override file and line, or `None` for a generated query.
    pub origin: Option<String>,
    pub summary: String,
    pub notes: Vec<String>,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        writeln!(f, "{severity}[{}]: {}", self.code, self.summary)?;
        match &self.origin {
            Some(origin) => writeln!(f, "  --> {origin}")?,
            None => writeln!(f, "  --> generated query")?,
        }
        for note in &self.notes {
            writeln!(f, "   | {note}")?;
        }
        Ok(())
    }
}

/// The diagnostics of checking the views of a registry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    diagnostics: Vec<Diagnostic>,
}

impl Report {
    /// `true` if there are no errors. Warnings are allowed.
    pub fn is_ok(&self) -> bool {
        self.errors().next().is_none()
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter().filter(|d| d.severity == Severity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter().filter(|d| d.severity == Severity::Warning)
    }

    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }

    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diagnostics
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for diagnostic in &self.diagnostics {
            writeln!(f, "{diagnostic}")?;
        }
        let errors = self.errors().count();
        let warnings = self.warnings().count();
        write!(f, "{errors} error(s), {warnings} warning(s)")
    }
}

impl std::error::Error for Report {}

/// The closest of `candidates` to `name`, if it is close enough to be a likely typo.
pub fn suggest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let limit = (name.chars().count() / 3).max(2);
    candidates
        .into_iter()
        .map(|candidate| (edit_distance(name, candidate), candidate))
        .filter(|(distance, _)| *distance <= limit)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            current.push((previous[j] + cost).min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions() {
        let candidates = ["name", "description", "address.city", "address.street"];
        assert_eq!(suggest("nmae", candidates), Some("name"));
        assert_eq!(suggest("descripton", candidates), Some("description"));
        assert_eq!(suggest("address.cty", candidates), Some("address.city"));
        assert_eq!(suggest("owner", candidates), None);
    }
}
