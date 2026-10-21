//! Phase tabs derive state from the same actual manager results as detail rows.
use super::Manager;

pub(super) struct Snapshot<'a> {
    pub current: Manager,
    pub phase: &'a str,
    pub pending: &'a [(Manager, String)],
    pub completed: &'a [Manager],
    pub failed: &'a [Manager],
    pub skipped: &'a [Manager],
    pub source_checks_failed: bool,
    pub background_updates: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status { Pending, Active, Complete, Failed, Skipped }
impl Status {
    fn marker(self) -> &'static str {
        match self {
            Self::Pending => "○", Self::Active => "●", Self::Complete => "✔",
            Self::Failed => "✗", Self::Skipped => "⚠",
        }
    }
}

fn package_manager(manager: Manager) -> bool {
    matches!(manager, Manager::Native | Manager::Aur | Manager::Sources | Manager::Flatpak | Manager::Nix | Manager::Pipx | Manager::Custom(_))
}

pub(super) fn history_detail(manager: Manager, current: Manager) -> bool {
    !matches!(manager, Manager::Orphans | Manager::Cache)
        && (!matches!(current, Manager::Orphans | Manager::Cache) || !package_manager(manager))
}

impl Snapshot<'_> {
    fn updates(&self) -> Option<Status> {
        let has_work = package_manager(self.current) || self.background_updates
            || self.pending.iter().any(|(manager, _)| package_manager(*manager))
            || self.completed.iter().chain(self.failed).copied().any(package_manager);
        if !has_work { return None; }
        if package_manager(self.current) || self.background_updates { return Some(Status::Active); }
        if self.failed.iter().copied().any(package_manager) { return Some(Status::Failed); }
        if self.source_checks_failed || self.skipped.iter().copied().any(package_manager) { return Some(Status::Skipped); }
        if self.pending.iter().any(|(manager, state)| package_manager(*manager) && state != "Pending") { return Some(Status::Skipped); }
        if self.pending.iter().any(|(manager, _)| package_manager(*manager)) { return Some(Status::Pending); }
        self.completed.iter().copied().any(package_manager).then_some(Status::Complete)
    }

    fn cleanup(&self, manager: Manager) -> Option<Status> {
        if self.current == manager || manager == Manager::Orphans && self.current == Manager::Checks && self.phase.starts_with("Rechecking orphan") {
            return Some(Status::Active);
        }
        if self.failed.contains(&manager) { return Some(Status::Failed); }
        if let Some((_, state)) = self.pending.iter().find(|(m, _)| *m == manager) {
            return Some(if state == "Pending" { Status::Pending } else { Status::Skipped });
        }
        self.completed.contains(&manager).then_some(Status::Complete)
    }
}

pub(super) fn line(snapshot: Snapshot<'_>, width: usize) -> String {
    let mut tabs = Vec::new();
    if let Some(status) = snapshot.updates() { tabs.push(("Updating", status)); }
    for (manager, label) in [(Manager::Orphans, "Orphans"), (Manager::Cache, "Cache")] {
        if let Some(status) = snapshot.cleanup(manager) { tabs.push((label, status)); }
    }
    let current = match snapshot.current {
        Manager::Refresh | Manager::Metadata => Some("Checking"),
        Manager::Checks if snapshot.phase.starts_with("Rechecking orphan") => None,
        Manager::Checks => Some("Checking"),
        Manager::Review => Some("Review"),
        Manager::Hooks => Some("Hooks"),
        Manager::Runtime => Some("Preparing"),
        Manager::Record | Manager::Restart if tabs.is_empty() => Some("Finishing"),
        _ => None,
    };
    if let Some(current) = current { tabs.insert(0, (current, Status::Active)); }
    let format = |tabs: &[(&str, Status)]| tabs.iter().map(|(label, status)| format!("{} {label}", status.marker())).collect::<Vec<_>>().join(" │ ");
    let mut text = format(&tabs);
    if crate::ui::str_width(&text) > width {
        // Keep active work visible in narrow terminals; account for omitted
        // tabs instead of silently losing all mention of future cleanup.
        tabs.sort_by_key(|(_, status)| *status != Status::Active);
        for keep in (1..tabs.len()).rev() {
            let candidate = format!("{} · +{}", format(&tabs[..keep]), tabs.len() - keep);
            if crate::ui::str_width(&candidate) <= width { text = candidate; break; }
        }
    }
    text = crate::ui::truncate_str(&text, width);
    let padding = width.saturating_sub(crate::ui::str_width(&text) + 1);
    if padding > 0 { text.push(' '); text.push_str(&"─".repeat(padding)); }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(current: Manager) -> Snapshot<'static> {
        Snapshot { current, phase: "", pending: &[], completed: &[], failed: &[], skipped: &[], source_checks_failed: false, background_updates: false }
    }

    #[test]
    fn successful_updates_remain_checked_as_cleanup_becomes_active() {
        let pending = [(Manager::Orphans, "Pending".into()), (Manager::Cache, "Pending".into())];
        let text = line(Snapshot { pending: &pending, ..snapshot(Manager::Native) }, 79);
        assert!(text.starts_with("● Updating │ ○ Orphans │ ○ Cache"));
        let completed = [Manager::Native, Manager::Sources, Manager::Orphans];
        let text = line(Snapshot { completed: &completed, ..snapshot(Manager::Cache) }, 79);
        assert!(text.starts_with("✔ Updating │ ✔ Orphans │ ● Cache"));
        assert_eq!(crate::ui::str_width(&text), 79);
        let pending = [(Manager::Aur, "Pending".into())];
        let text = line(Snapshot { pending: &pending, completed: &[Manager::Native], ..snapshot(Manager::Record) }, 79);
        assert!(!text.contains("✔ Updating"));
        assert!(history_detail(Manager::Native, Manager::Sources));
        assert!(!history_detail(Manager::Native, Manager::Cache));
        assert!(!history_detail(Manager::Sources, Manager::Orphans));
        assert!(!history_detail(Manager::Orphans, Manager::Cache));
    }

    #[test]
    fn failure_or_incomplete_discovery_cannot_earn_an_update_tick() {
        for partial in [false, true] {
            let text = line(Snapshot { completed: &[Manager::Native], failed: if partial { &[] } else { &[Manager::Aur] }, source_checks_failed: partial, ..snapshot(Manager::Cache) }, 79);
            assert!(text.contains(if partial { "⚠ Updating" } else { "✗ Updating" }));
            assert!(!text.contains("✔ Updating"));
            assert!(text.contains("● Cache"));
        }
    }

    #[test]
    fn narrow_tabs_preserve_active_work_and_summarize_hidden_cleanup() {
        let pending = [(Manager::Orphans, "Pending".into()), (Manager::Cache, "Pending".into())];
        for width in [19, 25, 31, 43] {
            let text = line(Snapshot { pending: &pending, ..snapshot(Manager::Native) }, width);
            assert!(text.contains("● Updating"));
            assert!(crate::ui::str_width(&text) <= width);
            if width == 19 { assert!(text.contains("+2")); }
            let text = line(Snapshot { completed: &[Manager::Native, Manager::Orphans], ..snapshot(Manager::Cache) }, width);
            assert!(text.contains("● Cache"));
            assert!(crate::ui::str_width(&text) <= width);
        }
        assert!(line(snapshot(Manager::Checks), 79).starts_with("● Checking ─"));
        assert!(line(snapshot(Manager::Cache), 79).starts_with("● Cache ─"));
    }
}
