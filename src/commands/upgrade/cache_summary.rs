//! Collect adapter-owned cache estimates from the eligible upgrade plan only.
use crate::integrations::{ExternalUpdate, IntegrationProvider};
use crate::{pm, ui};
use std::sync::Arc;

pub(super) fn collect(
    no_cache: bool,
    native: bool,
    aur: bool,
    providers: &[Arc<dyn IntegrationProvider>],
    updates: &[ExternalUpdate],
) -> Option<ui::CacheSummary> {
    if !no_cache { return None; }
    let mut summary = None;
    if native { include(&mut summary, "Pacman", Some(pm::native_cache_usage())); }
    // Helper clone/build destinations can be changed by its configuration and
    // forwarded arguments. Do not guess ~/.cache/paru or count it as empty.
    if aur { include(&mut summary, "AUR", Some(pm::CacheUsage::default())); }
    for provider in providers {
        if updates.iter().any(|update| update.runner == provider.name()) {
            include(&mut summary, provider.name(), provider.cache_usage());
        }
    }
    summary
}

fn include(summary: &mut Option<ui::CacheSummary>, name: &str, usage: Option<pm::CacheUsage>) {
    let Some(usage) = usage else { return; };
    let summary = summary.get_or_insert_default();
    let complete = if let Some(bytes) = summary.bytes.checked_add(usage.bytes) {
        summary.bytes = bytes;
        usage.complete
    } else { false };
    if !complete && !summary.unknown_managers.iter().any(|manager| manager == name) {
        summary.unknown_managers.push(name.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Provider { name: &'static str }
    impl IntegrationProvider for Provider {
        fn name(&self) -> &str { self.name }
        fn cache_usage(&self) -> Option<pm::CacheUsage> {
            assert_eq!(self.name, "planned", "must not inspect unplanned/skipped caches");
            Some(pm::CacheUsage { bytes: 12, complete: true })
        }
        fn is_available(&self) -> bool { panic!("cache summary must not discover managers") }
        fn refresh_metadata(&self) -> std::io::Result<()> { unreachable!() }
        fn check_updates(&self) -> crate::discovery::Report<ExternalUpdate> { unreachable!() }
        fn upgrade_actions(&self, _: &[ExternalUpdate]) -> std::io::Result<Vec<crate::integrations::UpgradeAction>> { unreachable!() }
        fn execute_upgrade(&self, _: &[ExternalUpdate]) -> std::io::Result<bool> { unreachable!() }
        fn check_options(&self, _: crate::scratch::Policy, _: bool) -> Result<(), crate::runtime::OptionError> { unreachable!() }
        fn clean_command_str(&self) -> String { unreachable!() }
        fn execute_clean(&self) -> std::io::Result<bool> { unreachable!() }
        fn preparation_policy(&self) -> crate::integrations::preparation::Policy { unreachable!() }
    }

    #[test]
    fn only_eligible_planned_caches_are_inspected_once_without_manager_commands() {
        let providers: Vec<Arc<dyn IntegrationProvider>> = ["skipped", "planned"].into_iter()
            .map(|name| Arc::new(Provider { name }) as Arc<dyn IntegrationProvider>).collect();
        let updates = ["a", "b"].map(|id| ExternalUpdate { runner: "planned".into(), id: id.into(), ..Default::default() });
        let summary = collect(true, false, false, &providers, &updates).unwrap();
        assert_eq!(summary.bytes, 12);
        assert!(summary.unknown_managers.is_empty());
        assert!(collect(false, false, false, &providers, &updates).is_none());
        assert!(collect(true, false, false, &providers, &[]).is_none());
    }

    #[test]
    fn cache_summary_retains_known_subtotal_and_unknown_manager_identities() {
        let mut summary = None;
        include(&mut summary, "Pacman", Some(pm::CacheUsage { bytes: 1024, complete: true }));
        for name in ["AUR", "Flatpak", "Nix", "Pipx", "custom", "AUR"] {
            include(&mut summary, name, Some(pm::CacheUsage::default()));
        }
        include(&mut summary, "Sources", None);
        let summary = summary.unwrap();
        assert_eq!(summary.bytes, 1024);
        assert_eq!(summary.unknown_managers, ["AUR", "Flatpak", "Nix", "Pipx", "custom"]);
    }

    #[test]
    fn temporary_artifacts_and_absent_work_do_not_create_a_cache_row() {
        let mut summary = None;
        include(&mut summary, "Sources", None);
        assert!(summary.is_none());
        assert!(collect(false, true, true, &[], &[]).is_none());
        assert!(collect(true, false, false, &[], &[]).is_none());
    }

    #[test]
    fn incomplete_native_scan_keeps_partial_bytes_unknown() {
        let mut summary = None;
        include(&mut summary, "Pacman", Some(pm::CacheUsage { bytes: 7, complete: false }));
        let summary = summary.unwrap();
        assert_eq!(summary.bytes, 7);
        assert_eq!(summary.unknown_managers, ["Pacman"]);
    }
}
