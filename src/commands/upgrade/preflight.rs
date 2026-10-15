//! One eligibility decision feeds approval, execution, prefetch and cleanup.
//! Unsupported options are skippable; invalid/uncertain plans remain fatal.
use std::sync::Arc;

use crate::config::Config;
use crate::integrations::{ExternalUpdate, IntegrationProvider};
use crate::resolver::ResolveResult;
use crate::runtime::OptionError;
use crate::scratch::Policy;

use super::planning::RequestedTargets;

#[derive(Clone, Copy)]
pub(super) struct Options {
    pub policy: Policy,
    pub no_cache: bool,
    pub keep_going: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Stage {
    Aur,
    External(String),
}
impl Stage {
    fn label(&self) -> &str {
        match self {
            Self::Aur => "AUR",
            Self::External(name) => name,
        }
    }
}

struct Skipped {
    manager: Stage,
    reason: String,
    packages: Vec<String>,
}

pub(super) struct Plan {
    skipped: Vec<Skipped>,
}

impl Plan {
    pub fn incomplete(&self) -> bool {
        !self.skipped.is_empty()
    }

    fn includes(&self, manager: &Stage) -> bool {
        !self.skipped.iter().any(|skip| &skip.manager == manager)
    }

    fn includes_external(&self, name: &str) -> bool {
        !self.skipped.iter().any(|skip| matches!(&skip.manager, Stage::External(manager) if manager == name))
    }

    /// Project the discovered inventory once, before counts, hooks or approval.
    pub fn apply(&self, resolved: &mut ResolveResult, requested: &mut RequestedTargets, external: &mut Vec<ExternalUpdate>) {
        external.retain(|update| self.includes_external(&update.runner));
        if !self.includes(&Stage::Aur) {
            resolved.updates.retain(|update| !update.candidate.as_ref().is_some_and(|candidate| candidate.is_aur));
            requested.aur.clear();
            requested.items.retain(|item| item.manager != "aur");
        }
    }

    pub fn preview(&self) {
        for skip in &self.skipped {
            println!("\n:: {} skipped (--keep-going, {} package(s)):", skip.manager.label(), skip.packages.len());
            println!("   {}", crate::utils::sanitize_display_text(&skip.reason));
            for package in &skip.packages {
                println!("   - {}", crate::utils::sanitize_display_text(package));
            }
            let manager = match &skip.manager {
                Stage::Aur => crate::progress::Manager::Aur,
                Stage::External(name) => crate::progress::integration_manager(name),
            };
            crate::progress::skipped(manager);
        }
    }

    pub fn report_incomplete(&self) {
        println!(":: Upgrade incomplete: {} manager(s) skipped because their options are unsupported.", self.skipped.len());
    }
}

fn include(manager: Stage, packages: Vec<String>, result: Result<(), OptionError>, options: Options, skipped: &mut Vec<Skipped>) -> Result<(), String> {
    match result {
        Ok(()) => Ok(()),
        Err(OptionError::Unsupported(reason)) if options.keep_going => {
            skipped.push(Skipped { manager, reason, packages });
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn prepare(config: &Config, providers: &[Arc<dyn IntegrationProvider>], external: &[ExternalUpdate], aur_targets: Vec<String>, flags: &[String], options: Options) -> Result<Plan, String> {
    let mut skipped = Vec::new();
    if !aur_targets.is_empty() {
        include(Stage::Aur, aur_targets, crate::pm::check_aur_options(config, options.policy, flags), options, &mut skipped)?;
    }
    for provider in providers {
        let packages: Vec<_> = external.iter().filter(|update| update.runner == provider.name()).map(|update| update.id.clone()).collect();
        if !packages.is_empty() {
            include(Stage::External(provider.name().into()), packages, provider.check_options(options.policy, options.no_cache), options, &mut skipped)?;
        }
    }
    let plan = Plan { skipped };
    // Configure supported adapters only after every fatal check has passed.
    for provider in providers {
        if plan.includes_external(provider.name()) && external.iter().any(|update| update.runner == provider.name()) {
            provider.configure_no_cache(options.no_cache);
        }
    }
    Ok(plan)
}

pub(super) fn aur_targets(resolved: &ResolveResult, requested: &RequestedTargets) -> Vec<String> {
    let mut names: Vec<_> = resolved.updates.iter()
        .filter(|update| update.candidate.as_ref().is_some_and(|candidate| candidate.is_aur))
        .map(|update| update.name.clone()).collect();
    names.extend(requested.aur.iter().cloned());
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct Provider {
        name: &'static str,
        error: Option<OptionError>,
        checked: AtomicUsize,
        configured: AtomicBool,
    }
    impl IntegrationProvider for Provider {
        fn name(&self) -> &str { self.name }
        fn is_available(&self) -> bool { true }
        fn refresh_metadata(&self) -> std::io::Result<()> { panic!("preflight must not refresh") }
        fn check_updates(&self) -> std::io::Result<Vec<ExternalUpdate>> { panic!("preflight consumes the discovered inventory") }
        fn check_options(&self, _: Policy, _: bool) -> Result<(), OptionError> {
            self.checked.fetch_add(1, Ordering::SeqCst);
            self.error.clone().map_or(Ok(()), Err)
        }
        fn configure_no_cache(&self, value: bool) { self.configured.store(value, Ordering::SeqCst); }
        fn upgrade_command_str(&self, _: &[ExternalUpdate]) -> String { panic!("preflight must not execute or render commands") }
        fn execute_upgrade(&self, _: &[ExternalUpdate]) -> std::io::Result<bool> { panic!("preflight must not execute") }
        fn clean_command_str(&self) -> String { panic!("preflight must not clean") }
        fn execute_clean(&self) -> std::io::Result<bool> { panic!("preflight must not clean") }
    }
    fn provider(name: &'static str, error: Option<OptionError>) -> Arc<Provider> {
        Arc::new(Provider { name, error, checked: AtomicUsize::new(0), configured: AtomicBool::new(false) })
    }
    fn update(name: &str) -> ExternalUpdate {
        ExternalUpdate { runner: name.into(), id: format!("{name}-package"), ..Default::default() }
    }
    fn options(keep_going: bool) -> Options {
        Options { policy: Policy::Auto, no_cache: true, keep_going }
    }

    #[test]
    fn unsupported_options_require_keep_going_before_any_configuration() {
        let provider = provider("Flatpak", Some(OptionError::Unsupported("RAM unsupported".into())));
        let providers: Vec<Arc<dyn IntegrationProvider>> = vec![provider.clone()];
        assert!(prepare(&Config::default(), &providers, &[update("Flatpak")], Vec::new(), &[], options(false)).is_err());
        assert!(!provider.configured.load(Ordering::SeqCst));
    }

    #[test]
    fn capability_skips_share_one_projection_and_do_not_configure_skipped_managers() {
        let flatpak = provider("Flatpak", Some(OptionError::Unsupported("RAM unsupported".into())));
        let sources = provider("Sources", None);
        let providers: Vec<Arc<dyn IntegrationProvider>> = vec![flatpak.clone(), sources.clone()];
        let mut external = vec![update("Flatpak"), update("Sources")];
        let plan = prepare(&Config::default(), &providers, &external, Vec::new(), &[], options(true)).unwrap();
        let mut resolved = ResolveResult { check_errors: Vec::new(), installed_count: 0, repos: Vec::new(), packages: Default::default(), updates: Vec::new(), held_packages: Vec::new(), unresolved_pins: Vec::new() };
        let mut requested = RequestedTargets { items: Vec::new(), native: Vec::new(), aur: Vec::new() };
        plan.apply(&mut resolved, &mut requested, &mut external);
        assert!(plan.incomplete());
        assert_eq!(external, [update("Sources")]);
        assert!(!flatpak.configured.load(Ordering::SeqCst));
        assert!(sources.configured.load(Ordering::SeqCst));
    }

    #[test]
    fn invalid_errors_remain_fatal_even_when_their_text_mentions_unsupported() {
        let supported = provider("Sources", None);
        let invalid = provider("custom", Some(OptionError::Invalid("unsupported metadata or capacity".into())));
        let providers: Vec<Arc<dyn IntegrationProvider>> = vec![supported.clone(), invalid];
        assert!(prepare(&Config::default(), &providers, &[update("Sources"), update("custom")], Vec::new(), &[], options(true)).is_err());
        assert!(!supported.configured.load(Ordering::SeqCst));
    }

    #[test]
    fn managers_without_updates_are_never_validated_or_configured() {
        let provider = provider("Flatpak", Some(OptionError::Unsupported("RAM unsupported".into())));
        let providers: Vec<Arc<dyn IntegrationProvider>> = vec![provider.clone()];
        let plan = prepare(&Config::default(), &providers, &[], Vec::new(), &[], options(false)).unwrap();
        assert!(!plan.incomplete());
        assert_eq!(provider.checked.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn multiple_manager_capability_gaps_are_recorded_individually() {
        let providers: Vec<Arc<dyn IntegrationProvider>> = vec![
            provider("Flatpak", Some(OptionError::Unsupported("RAM".into()))),
            provider("Nix", Some(OptionError::Unsupported("cache".into()))),
        ];
        let plan = prepare(&Config::default(), &providers, &[update("Flatpak"), update("Nix")], Vec::new(), &[], options(true)).unwrap();
        assert_eq!(plan.skipped.len(), 2);
        assert_eq!(plan.skipped[0].packages, ["Flatpak-package"]);
        assert_eq!(plan.skipped[1].packages, ["Nix-package"]);
    }

    #[test]
    fn unsupported_aur_helper_is_skippable_without_changing_privilege_or_scratch_policy() {
        let mut config = Config::default();
        config.options.helper = "yay".into();
        let mut options = options(true);
        options.policy = Policy::Ram;
        let plan = prepare(&config, &[], &[], vec!["fixture".into()], &[], options).unwrap();
        assert!(!plan.includes(&Stage::Aur));
        assert_eq!(config.options.helper, "yay");
        options.keep_going = false;
        assert!(prepare(&config, &[], &[], vec!["fixture".into()], &[], options).is_err());
    }

    #[test]
    fn external_display_name_cannot_skip_the_core_aur_stage() {
        let custom = provider("AUR", Some(OptionError::Unsupported("cache unsupported".into())));
        let providers: Vec<Arc<dyn IntegrationProvider>> = vec![custom.clone()];
        let mut external = vec![update("AUR")];
        let plan = prepare(&Config::default(), &providers, &external, vec!["hello".into()], &[], options(true)).unwrap();
        let mut resolved = ResolveResult { check_errors: Vec::new(), installed_count: 0, repos: Vec::new(), packages: Default::default(), updates: Vec::new(), held_packages: Vec::new(), unresolved_pins: Vec::new() };
        let mut requested = RequestedTargets { items: Vec::new(), native: Vec::new(), aur: vec!["hello".into()] };
        plan.apply(&mut resolved, &mut requested, &mut external);
        assert!(plan.includes(&Stage::Aur));
        assert!(!plan.includes_external("AUR"));
        assert_eq!(requested.aur, ["hello"]);
        assert!(external.is_empty());
        assert!(!custom.configured.load(Ordering::SeqCst));
    }

    #[test]
    fn core_aur_skip_cannot_exclude_a_supported_external_namesake() {
        let custom = provider("AUR", None);
        let providers: Vec<Arc<dyn IntegrationProvider>> = vec![custom.clone()];
        let mut config = Config::default();
        config.options.helper = "yay".into();
        let mut external = vec![update("AUR")];
        let mut selected = options(true);
        selected.policy = Policy::Storage;
        let plan = prepare(&config, &providers, &external, vec!["hello".into()], &[], selected).unwrap();
        let mut resolved = ResolveResult { check_errors: Vec::new(), installed_count: 0, repos: Vec::new(), packages: Default::default(), updates: Vec::new(), held_packages: Vec::new(), unresolved_pins: Vec::new() };
        let mut requested = RequestedTargets { items: Vec::new(), native: Vec::new(), aur: vec!["hello".into()] };
        plan.apply(&mut resolved, &mut requested, &mut external);
        assert!(!plan.includes(&Stage::Aur));
        assert!(plan.includes_external("AUR"));
        assert!(requested.aur.is_empty());
        assert_eq!(external, [update("AUR")]);
        assert!(custom.configured.load(Ordering::SeqCst));
    }
}
