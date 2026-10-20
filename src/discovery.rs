//! Lossless, per-manager metadata outcomes. Collection never decides whether
//! partial discovery authorizes an upgrade; eligibility owns that policy.
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Manager {
    Aur,
    External(String),
    /// One owned source package, identified by its exact alias/package key.
    Source(String),
}

impl Manager {
    pub fn label(&self) -> &str {
        match self {
            Self::Aur => "AUR",
            Self::External(name) => name,
            Self::Source(target) => target,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Refresh,
    Updates,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Refresh => "metadata refresh",
            Self::Updates => "update check",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    pub manager: Manager,
    pub phase: Phase,
    pub detail: String,
}

impl Failure {
    pub fn new(manager: Manager, phase: Phase, detail: impl Into<String>) -> Self {
        Self { manager, phase, detail: detail.into() }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} failed: {}", self.manager.label(), self.phase.label(),
            crate::utils::sanitize_display_text(&self.detail))
    }
}

pub(crate) fn describe(failures: &[Failure]) -> String {
    failures.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")
}

/// Successful managers, including empty checks, survive another manager's failure.
/// Callers must explicitly require completeness or pass failures to eligibility.
#[must_use = "discovery failures must be handled before approval or execution"]
pub(crate) struct Report<T> {
    successes: Vec<(Manager, Vec<T>)>,
    failures: Vec<Failure>,
}

impl<T> Default for Report<T> {
    fn default() -> Self {
        Self { successes: Vec::new(), failures: Vec::new() }
    }
}

impl<T> Report<T> {
    pub fn updates(name: &str, result: std::io::Result<Vec<T>>) -> Self {
        let mut report = Self::default();
        report.record(Manager::External(name.into()), Phase::Updates, result.map_err(|error| error.to_string()));
        report
    }

    pub fn append(&mut self, other: Self) {
        self.successes.extend(other.successes);
        self.failures.extend(other.failures);
    }

    pub fn record(&mut self, manager: Manager, phase: Phase, result: Result<Vec<T>, String>) {
        match result {
            Ok(values) => self.successes.push((manager, values)),
            Err(detail) => self.failures.push(Failure::new(manager, phase, detail)),
        }
    }

    pub fn into_parts(self) -> (Vec<T>, Vec<Failure>) {
        (self.successes.into_iter().flat_map(|(_, values)| values).collect(), self.failures)
    }

    pub fn require_complete(self) -> Result<Vec<T>, String> {
        let (values, failures) = self.into_parts();
        if failures.is_empty() { Ok(values) }
        else { Err(format!("Update check incomplete: {}", describe(&failures))) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_and_empty_checks_survive_other_failures() {
        let mut report = Report::default();
        report.record(Manager::External("Flatpak".into()), Phase::Updates, Ok(vec![42]));
        report.record(Manager::External("Nix".into()), Phase::Updates, Ok(Vec::new()));
        report.record(Manager::External("Sources".into()), Phase::Updates, Err("missing asset".into()));
        assert_eq!(report.successes.len(), 2);
        let (values, failures) = report.into_parts();
        assert_eq!(values, [42]);
        assert_eq!(failures[0].manager, Manager::External("Sources".into()));
        assert_eq!(failures[0].phase, Phase::Updates);
    }

    #[test]
    fn strict_consumers_cannot_report_partial_checks_as_complete() {
        let mut report: Report<()> = Report::default();
        report.record(Manager::Aur, Phase::Updates, Err("RPC failed".into()));
        assert!(report.require_complete().unwrap_err().contains("AUR update check failed"));
        assert_ne!(Manager::Aur, Manager::External("AUR".into()));
    }
}
