//! Compact phase labels; detailed manager metrics remain in their own rows.
use super::Manager;

pub(super) fn line(current: Manager, phase: &str, pending: &[(Manager, String)], completed: &[Manager], failed: &[Manager], width: usize) -> String {
    let current_label = match current {
        Manager::Refresh | Manager::Metadata => "Checking",
        Manager::Checks if phase.starts_with("Rechecking orphan") => "Orphans",
        Manager::Checks => "Checking",
        Manager::Review => "Review",
        Manager::Orphans => "Orphans",
        Manager::Cache => "Cache",
        Manager::Hooks => "Hooks",
        Manager::Record | Manager::Restart => "Finishing",
        Manager::Runtime => "Preparing",
        _ => "Updating",
    };
    let mut labels = vec![format!("● {current_label}")];
    for (manager, label) in [(Manager::Orphans, "Orphans"), (Manager::Cache, "Cache")] {
        if label == current_label { continue; }
        let state = if failed.contains(&manager) { Some("✗") }
            else if let Some((_, state)) = pending.iter().find(|(m, _)| *m == manager) {
                Some(if state == "Pending" { "○" } else { "–" })
            }
            else if completed.contains(&manager) { Some("✔") }
            else { None };
        if let Some(state) = state { labels.push(format!("{state} {label}")); }
    }
    let mut text = String::new();
    for label in labels {
        let next = if text.is_empty() { label } else { format!("{text} │ {label}") };
        if crate::ui::str_width(&next) > width { break; }
        text = next;
    }
    if text.is_empty() { return crate::ui::truncate_str(current_label, width); }
    let padding = width.saturating_sub(crate::ui::str_width(&text) + 1);
    if padding > 0 { text.push(' '); text.push_str(&"─".repeat(padding)); }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overview_shows_only_real_cleanup_and_preserves_active_phase_in_small_windows() {
        let pending = [(Manager::Orphans, "Pending".into()), (Manager::Cache, "Pending".into())];
        let text = line(Manager::Native, "Installing", &pending, &[], &[], 79);
        assert!(text.starts_with("● Updating │ ○ Orphans │ ○ Cache"));
        assert_eq!(crate::ui::str_width(&text), 79);
        for width in [19, 31, 43] {
            let text = line(Manager::Cache, "Cleaning", &[], &[Manager::Orphans], &[], width);
            assert!(text.starts_with("● Cache"));
            assert!(crate::ui::str_width(&text) <= width);
            assert!(!text.contains("Updating"));
        }
        assert!(line(Manager::Checks, "Checking", &[], &[], &[], 79).starts_with("● Checking ─"));
        let text = line(Manager::Cache, "Cleaning", &[], &[], &[Manager::Orphans], 79);
        assert!(text.contains("✗ Orphans"));
        assert!(!text.contains("✔ Orphans"));
    }
}
