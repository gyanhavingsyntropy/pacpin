//! One eligibility decision feeds approval, execution, prefetch and cleanup.
//! Failed independent discovery and unsupported options share one skip policy.
//! Native transaction/configuration/capacity failures remain fatal.
use std::sync::Arc;

use crate::config::Config;
use crate::integrations::{ExternalUpdate, IntegrationProvider};
use crate::resolver::ResolveResult;
use crate::runtime::OptionError;
use crate::scratch::Policy;
use crate::discovery::{Failure, Manager as Stage, Phase};

use super::planning::RequestedTargets;

#[derive(Clone, Copy)]
pub(super) struct Options {
    pub policy: Policy,
    pub no_cache: bool,
    pub keep_going: bool,
}

enum Cause { Unsupported, Discovery(Phase) }

struct Skipped {
    manager: Stage,
    reason: String,
    packages: Vec<String>,
    cause: Cause,
}

pub(super) struct Plan {
    skipped: Vec<Skipped>,
}

impl Plan {
    pub fn incomplete(&self) -> bool {
        !self.skipped.is_empty()
    }

    pub fn aur_available(&self) -> bool {
        self.includes(&Stage::Aur)
    }

    fn includes(&self, manager: &Stage) -> bool {
        !self.skipped.iter().any(|skip| &skip.manager == manager)
    }

    fn includes_external(&self, name: &str) -> bool {
        !self.skipped.iter().any(|skip| matches!(&skip.manager, Stage::External(manager) if manager == name))
    }

    fn includes_update(&self, update: &ExternalUpdate) -> bool {
        self.includes_external(&update.runner)
            && (update.runner != "Sources" || self.includes(&Stage::Source(update.id.clone())))
    }

    /// Project the discovered inventory once, before counts, hooks or approval.
    pub fn apply(&self, resolved: &mut ResolveResult, requested: &mut RequestedTargets, external: &mut Vec<ExternalUpdate>) {
        external.retain(|update| self.includes_update(update));
        if !self.includes(&Stage::Aur) {
            resolved.updates.retain(|update| !update.candidate.as_ref().is_some_and(|candidate| candidate.is_aur));
            requested.aur.clear();
            requested.items.retain(|item| item.manager != "aur");
        }
    }

    pub fn preview(&self) {
        let source_failures = self.skipped.iter().filter(|skip| matches!(skip.manager, Stage::Source(_))).count();
        crate::progress::source_checks_failed(source_failures);
        if self.skipped.iter().any(|skip| matches!(skip.cause, Cause::Discovery(_))) {
            println!(":: Totals cover successful checks; failed checks have unknown update counts.");
            println!(":: Orphan cleanup is deferred; hooks use verified eligible work.");
        }
        for skip in &self.skipped {
            match skip.cause {
                Cause::Unsupported => println!("\n:: {} skipped (--keep-going, {} package(s)):", skip.manager.label(), skip.packages.len()),
                Cause::Discovery(phase) => println!("\n:: {} skipped (--keep-going, {} failed; package count unknown):", skip.manager.label(), phase.label()),
            }
            println!("   {}", crate::utils::sanitize_display_text(&skip.reason));
            for package in &skip.packages {
                println!("   - {}", crate::utils::sanitize_display_text(package));
            }
            let manager = match &skip.manager {
                Stage::Aur => crate::progress::Manager::Aur,
                Stage::External(name) => crate::progress::integration_manager(name),
                Stage::Source(_) => continue, // Keep healthy Sources active; annotate its row instead.
            };
            let reason = match skip.cause {
                Cause::Unsupported => crate::progress::SkipReason::UnsupportedOptions,
                Cause::Discovery(Phase::Updates) => crate::progress::SkipReason::UpdateCheck,
                Cause::Discovery(Phase::Refresh) => crate::progress::SkipReason::MetadataRefresh,
            };
            crate::progress::skipped(manager, reason);
        }
    }

    pub fn report_incomplete(&self) {
        let sources = self.skipped.iter().filter(|skip| matches!(skip.manager, Stage::Source(_))).count();
        let managers = self.skipped.len() - sources;
        let mut parts = Vec::new();
        if managers != 0 { parts.push(format!("{managers} manager(s) skipped")); }
        if sources != 0 { parts.push(format!("{sources} source package check(s) failed")); }
        println!(":: Upgrade incomplete: {}.", parts.join("; "));
    }
}

fn include(manager: Stage, packages: Vec<String>, result: Result<(), OptionError>, options: Options, skipped: &mut Vec<Skipped>) -> Result<(), String> {
    match result {
        Ok(()) => Ok(()),
        Err(OptionError::Unsupported(reason)) if options.keep_going => {
            skipped.push(Skipped { manager, reason, packages, cause: Cause::Unsupported });
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn prepare(config: &Config, providers: &[Arc<dyn IntegrationProvider>], external: &[ExternalUpdate], aur_targets: Vec<String>, flags: &[String], options: Options, discovery_failures: Vec<Failure>) -> Result<Plan, String> {
    if !options.keep_going && !discovery_failures.is_empty() {
        return Err(format!("Update check incomplete: {}", crate::discovery::describe(&discovery_failures)));
    }
    let mut skipped: Vec<_> = discovery_failures.into_iter().map(|failure| Skipped {
        manager: failure.manager, cause: Cause::Discovery(failure.phase), reason: failure.detail, packages: Vec::new(),
    }).collect();
    if !aur_targets.is_empty() && !skipped.iter().any(|skip| skip.manager == Stage::Aur) {
        include(Stage::Aur, aur_targets, crate::pm::check_aur_options(config, options.policy, flags), options, &mut skipped)?;
    }
    for provider in providers {
        let packages: Vec<_> = external.iter().filter(|update| update.runner == provider.name()
            && !skipped.iter().any(|skip| update.runner == "Sources" && skip.manager == Stage::Source(update.id.clone())))
            .map(|update| update.id.clone()).collect();
        if !packages.is_empty() && !skipped.iter().any(|skip| skip.manager == Stage::External(provider.name().into())) {
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

/// Avoid retrying explicit/bare AUR requests through the separate request
/// resolver after AUR discovery failed. Native/source-owned requests retain
/// their normal validation; malformed requests must never be silently skipped.
pub(super) fn eligible_requests(config: &Config, manager: &crate::db::AlpmManager, requested: &[String], failures: &[Failure]) -> Vec<String> {
    if !failures.iter().any(|failure| failure.manager == Stage::Aur) { return requested.to_vec(); }
    let glob_pins = crate::resolver::ResolverEngine::compile_glob_pins(&config.pins);
    requested.iter().filter(|target| {
        if !crate::utils::is_safe_install_target(target) { return true; }
        if let Some((repo, _)) = target.split_once('/') { return !repo.eq_ignore_ascii_case("aur"); }
        if crate::sources::owner(config, target).is_some() { return true; }
        let pin = config.features.pinning.then(|| crate::resolver::ResolverEngine::match_pinned_package(target, &config.pins, &glob_pins)).flatten();
        if let Some(repo) = pin { return repo != "aur"; }
        // Requested upgrade classification requires an exact native name.
        manager.handle().syncdbs().into_iter().any(|db| db.pkg(target.as_str()).is_ok())
    }).cloned().collect()
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
        fn preparation_policy(&self) -> crate::integrations::preparation::Policy { crate::integrations::preparation::Policy::Unsupported("preflight fixture") }
        fn name(&self) -> &str { self.name }
        fn is_available(&self) -> bool { true }
        fn refresh_metadata(&self) -> std::io::Result<()> { panic!("preflight must not refresh") }
        fn check_updates(&self) -> crate::discovery::Report<ExternalUpdate> { panic!("preflight consumes the discovered inventory") }
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
    fn discovery_failures_require_keep_going_and_preserve_successful_managers() {
        for name in ["Sources", "Flatpak", "Nix", "Pipx", "custom", "AUR"] {
            let failed = provider(name, None);
            let healthy = provider("healthy", None);
            let providers: Vec<Arc<dyn IntegrationProvider>> = vec![failed.clone(), healthy.clone()];
            let failure = Failure::new(Stage::External(name.into()), Phase::Updates, "bad metadata");
            assert!(prepare(&Config::default(), &providers, &[update(name), update("healthy")], Vec::new(), &[], options(false), vec![failure.clone()]).is_err());
            assert_eq!(healthy.checked.load(Ordering::SeqCst), 0);
            let plan = prepare(&Config::default(), &providers, &[update(name), update("healthy")], Vec::new(), &[], options(true), vec![failure]).unwrap();
            assert!(plan.incomplete());
            assert!(!plan.includes_external(name));
            assert!(plan.includes(&Stage::Aur));
            assert_eq!(failed.checked.load(Ordering::SeqCst), 0);
            assert!(!failed.configured.load(Ordering::SeqCst));
            assert!(healthy.configured.load(Ordering::SeqCst));
            let mut external = vec![update(name), update("healthy")];
            let mut resolved = ResolveResult { check_errors: Vec::new(), installed_count: 0, repos: Vec::new(), packages: Default::default(), updates: Vec::new(), held_packages: Vec::new(), unresolved_pins: Vec::new() };
            let mut requested = RequestedTargets { items: Vec::new(), native: Vec::new(), aur: Vec::new() };
            plan.apply(&mut resolved, &mut requested, &mut external);
            assert_eq!(external, [update("healthy")]);
        }
    }

    #[test]
    fn a_failed_source_package_preserves_healthy_siblings_and_stays_incomplete() {
        let source = provider("Sources", None);
        let providers: Vec<Arc<dyn IntegrationProvider>> = vec![source.clone()];
        let good = ExternalUpdate { runner: "Sources".into(), id: "repo/good".into(), ..Default::default() };
        let bad = ExternalUpdate { runner: "Sources".into(), id: "repo/bad".into(), ..Default::default() };
        let failure = Failure::new(Stage::Source(bad.id.clone()), Phase::Updates, "missing asset");
        assert!(prepare(&Config::default(), &providers, std::slice::from_ref(&good), Vec::new(), &[], options(false), vec![failure.clone()]).is_err());
        let plan = prepare(&Config::default(), &providers, std::slice::from_ref(&good), Vec::new(), &[], options(true), vec![failure]).unwrap();
        assert!(plan.incomplete());
        assert!(plan.includes_external("Sources"));
        assert!(!plan.includes_update(&bad));
        assert!(plan.includes_update(&good));
        // An external adapter with a coincidentally identical package ID stays eligible.
        assert!(plan.includes_update(&ExternalUpdate { runner: "Nix".into(), ..bad }));
        assert_eq!(source.checked.load(Ordering::SeqCst), 1);
        assert!(source.configured.load(Ordering::SeqCst));
    }

    #[test]
    fn refresh_failures_and_empty_plans_stay_incomplete() {
        let failure = Failure::new(Stage::External("Flatpak".into()), Phase::Refresh, "refresh failed");
        let plan = prepare(&Config::default(), &[], &[], Vec::new(), &[], options(true), vec![failure]).unwrap();
        assert!(plan.incomplete());
        assert!(plan.aur_available());
        assert!(!plan.includes_external("Flatpak"));
    }

    #[test]
    fn failed_core_aur_discovery_defers_requests_without_skipping_native_or_sources() {
        use crate::db::test_support::{AlpmFixture, FixturePackage};
        let sync = [FixturePackage::new("native", "2-1")];
        let fixture = AlpmFixture::new(&[], &[("extra", &sync)]);
        let mut config = Config::default();
        config.pins.insert("pinned".into(), "aur".into());
        let requests = ["aur/explicit", "pinned", "missing", "native", "extra/native", "bad;target"]
            .map(str::to_string);
        let failures = [Failure::new(Stage::Aur, Phase::Updates, "RPC failed")];
        assert_eq!(eligible_requests(&config, &fixture.manager, &requests, &failures), ["native", "extra/native", "bad;target"]);
        let plan = prepare(&config, &[], &[], vec!["pinned".into()], &[], options(true), failures.to_vec()).unwrap();
        assert!(!plan.includes(&Stage::Aur));
        assert!(plan.includes_external("AUR"));
    }

    #[test]
    fn unsupported_options_require_keep_going_before_any_configuration() {
        let provider = provider("Flatpak", Some(OptionError::Unsupported("RAM unsupported".into())));
        let providers: Vec<Arc<dyn IntegrationProvider>> = vec![provider.clone()];
        assert!(prepare(&Config::default(), &providers, &[update("Flatpak")], Vec::new(), &[], options(false), Vec::new()).is_err());
        assert!(!provider.configured.load(Ordering::SeqCst));
    }

    #[test]
    fn capability_skips_share_one_projection_and_do_not_configure_skipped_managers() {
        let flatpak = provider("Flatpak", Some(OptionError::Unsupported("RAM unsupported".into())));
        let sources = provider("Sources", None);
        let providers: Vec<Arc<dyn IntegrationProvider>> = vec![flatpak.clone(), sources.clone()];
        let mut external = vec![update("Flatpak"), update("Sources")];
        let plan = prepare(&Config::default(), &providers, &external, Vec::new(), &[], options(true), Vec::new()).unwrap();
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
        assert!(prepare(&Config::default(), &providers, &[update("Sources"), update("custom")], Vec::new(), &[], options(true), Vec::new()).is_err());
        assert!(!supported.configured.load(Ordering::SeqCst));
    }

    #[test]
    fn managers_without_updates_are_never_validated_or_configured() {
        let provider = provider("Flatpak", Some(OptionError::Unsupported("RAM unsupported".into())));
        let providers: Vec<Arc<dyn IntegrationProvider>> = vec![provider.clone()];
        let plan = prepare(&Config::default(), &providers, &[], Vec::new(), &[], options(false), Vec::new()).unwrap();
        assert!(!plan.incomplete());
        assert_eq!(provider.checked.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn multiple_manager_capability_gaps_are_recorded_individually() {
        let providers: Vec<Arc<dyn IntegrationProvider>> = vec![
            provider("Flatpak", Some(OptionError::Unsupported("RAM".into()))),
            provider("Nix", Some(OptionError::Unsupported("cache".into()))),
        ];
        let plan = prepare(&Config::default(), &providers, &[update("Flatpak"), update("Nix")], Vec::new(), &[], options(true), Vec::new()).unwrap();
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
        let plan = prepare(&config, &[], &[], vec!["fixture".into()], &[], options, Vec::new()).unwrap();
        assert!(!plan.includes(&Stage::Aur));
        assert_eq!(config.options.helper, "yay");
        options.keep_going = false;
        assert!(prepare(&config, &[], &[], vec!["fixture".into()], &[], options, Vec::new()).is_err());
    }

    #[test]
    fn external_display_name_cannot_skip_the_core_aur_stage() {
        let custom = provider("AUR", Some(OptionError::Unsupported("cache unsupported".into())));
        let providers: Vec<Arc<dyn IntegrationProvider>> = vec![custom.clone()];
        let mut external = vec![update("AUR")];
        let plan = prepare(&Config::default(), &providers, &external, vec!["hello".into()], &[], options(true), Vec::new()).unwrap();
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
        let plan = prepare(&config, &providers, &external, vec!["hello".into()], &[], selected, Vec::new()).unwrap();
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
