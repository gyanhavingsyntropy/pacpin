use super::*;
use crate::integrations::{ExternalUpdate, IntegrationProvider, UpgradeAction};
use std::io;
use std::sync::Mutex;

pub struct SourceProvider {
    config: Config,
    approved: Mutex<Vec<Plan>>,
}
impl SourceProvider {
    pub fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
            approved: Mutex::new(Vec::new()),
        }
    }
}

impl IntegrationProvider for SourceProvider {
    fn check_options(&self, _policy: crate::scratch::Policy, _no_cache: bool) -> Result<(), crate::runtime::OptionError> {
        // Sources own temporary workspaces under either policy. Their actual
        // recipe/archive capacity remains checked before download/build/install.
        for plan in self.approved.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|p| p.recipe.build.is_some()) {
            super::build::check_prerequisites(&self.config, plan, None).map_err(crate::runtime::OptionError::Invalid)?;
            let build = plan.recipe.build.as_ref().unwrap();
            crate::scratch::choose(_policy, build.initial_bytes(), build.scratch_inodes.saturating_add(32)).map_err(crate::runtime::OptionError::Invalid)?;
        }
        Ok(())
    }
    fn name(&self) -> &str {
        "Sources"
    }
    fn is_available(&self) -> bool {
        self.config
            .pins
            .values()
            .any(|alias| self.config.sources.contains_key(alias))
    }
    fn refresh_metadata(&self) -> Result<(), io::Error> {
        Ok(())
    }
    fn check_updates(&self) -> crate::discovery::Report<ExternalUpdate> {
        self.approved.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let result = self.discover_updates();
        match result {
            Ok(report) => report,
            Err(error) => crate::discovery::Report::updates(self.name(), Err(error)),
        }
    }
    fn upgrade_actions(&self, updates: &[ExternalUpdate]) -> io::Result<Vec<UpgradeAction>> {
        let plans = self.approved.lock().unwrap_or_else(|e| e.into_inner());
        updates.iter().map(|update| {
            let plan = plans.iter().find(|plan| update.id == format!("{}/{}", plan.alias, plan.name) && update.version == plan.version)
                .ok_or_else(|| io::Error::other("Source update changed after approval"))?;
            let preparation = if plan.git.is_some() { "fetch approved Git revision and build" }
                else if plan.recipe.build.is_some() { "download, verify and build source archive" }
                else { "download and verify release; package if needed" };
            Ok(UpgradeAction::Internal(format!("{} -> {}: {preparation}; install local package via {} pacman -U; remove temporary workspace", update.id, update.version, self.config.options.privilege_tool)))
        }).collect()
    }
    fn execute_upgrade(&self, updates: &[ExternalUpdate]) -> Result<bool, io::Error> {
        let plans = self
            .approved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut config = self.config.clone();
        for update in updates {
            let plan = plans
                .iter()
                .find(|p| {
                    update.id == format!("{}/{}", p.alias, p.name) && update.version == p.version
                })
                .ok_or_else(|| io::Error::other("Source update changed after approval"))?;
            // Execute the exact catalog selection shown before confirmation,
            // not a new "latest" lookup after the user approves.
            install::execute(&mut config, plan, true).map_err(io::Error::other)?;
        }
        Ok(true)
    }
    fn clean_command_str(&self) -> String {
        String::new()
    }
    fn execute_clean(&self) -> Result<bool, io::Error> {
        Ok(true)
    }
    fn supports_clean(&self) -> bool {
        false
    }
    fn preparation_policy(&self) -> crate::integrations::preparation::Policy {
        if self.approved.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|plan| plan.git.is_none()) { crate::integrations::preparation::Policy::Background }
        else { crate::integrations::preparation::Policy::Unsupported("Git source builds fetch during their foreground build stage") }
    }
    fn prefetch(&self, updates: &[ExternalUpdate]) -> io::Result<()> {
        crate::progress::background(|| {
            let plans = self
                .approved
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            for update in updates {
                if let Some(plan) = plans.iter().find(|p| {
                    update.id == format!("{}/{}", p.alias, p.name) && update.version == p.version
                }) {
                    install::prefetch(plan).map_err(io::Error::other)?;
                }
            }
            Ok(())
        })
    }
    fn supports_aur_failure_continuation(&self) -> bool { true }
    fn execute_after_aur_failure(&self, update: &ExternalUpdate, permit: &mut crate::pm::AurBuildFailurePermit) -> Result<bool, io::Error> {
        let plans = self.approved.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let plan = plans.iter().find(|p| update.id == format!("{}/{}", p.alias, p.name) && update.version == p.version)
            .ok_or_else(|| io::Error::other("Source update changed after approval"))?;
        let mut config = self.config.clone();
        install::execute_independent(&mut config, plan, permit).map_err(io::Error::other)?;
        permit.source_completed().map_err(|e| io::Error::other(format!("{} installed, but further continuation is blocked: {e}", plan.name)))?;
        Ok(true)
    }
    fn preview_upgrade(&self, updates: &[ExternalUpdate]) {
        for plan in self.approved.lock().unwrap_or_else(|e| e.into_inner()).iter()
            .filter(|plan| updates.iter().any(|u| u.id == format!("{}/{}", plan.alias, plan.name))) {
            super::build::preview(plan);
        }
    }
}

impl SourceProvider {
    fn discover_updates(&self) -> io::Result<crate::discovery::Report<ExternalUpdate>> {
        let manager = crate::db::AlpmManager::with_repo_order(&self.config.repo_order).map_err(io::Error::other)?;
        let native_config = crate::pacman_config::snapshot().map_err(io::Error::other)?;
        let ignore_flags: Vec<String> = {
                native_config.option("IgnorePkg").iter().flat_map(|s| s.split_whitespace())
                    .map(|pattern| format!("--ignore={pattern}"))
                    .collect()
            };
        let targets: Vec<_> = manager
            .handle()
            .localdb()
            .pkgs()
            .into_iter()
            .filter_map(|p| {
                if ignored(p.name(), &ignore_flags) {
                    return None;
                }
                owner(&self.config, p.name())
                    .map(|alias| (format!("{alias}/{}", p.name()), p.version().to_string()))
            })
            .collect();
        drop(manager);
        let checked = super::discovery::check_targets(&targets, |target, installed| {
            let plan = plan(&self.config, target)?;
            Ok(if alpm::vercmp(plan.version.as_str(), installed).is_gt() { Some(plan) } else { None })
        });
        let (plans, failures) = checked.into_parts();
        let updates = plans
            .iter()
            .map(|p| ExternalUpdate {
                runner: self.name().into(),
                id: format!("{}/{}", p.alias, p.name),
                name: p.name.clone(),
                repo: p.alias.clone(),
                version: p.version.clone(),
                ..ExternalUpdate::default()
            })
            .collect();
        *self.approved.lock().unwrap_or_else(|e| e.into_inner()) = plans;
        let mut report = crate::discovery::Report::updates(self.name(), Ok(updates));
        for failure in failures {
            report.record(failure.manager, failure.phase, Err(failure.detail));
        }
        Ok(report)
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;
    #[test]
    fn sources_describe_approved_internal_work_instead_of_a_recursive_command() {
        let mut config = Config::default();
        config.options.privilege_tool = "pkexec".into();
        let provider = SourceProvider::new(&config);
        let recipe: Recipe = toml::from_str("asset='app.tar'\nformat='archive'\n[binaries]\napp='bin/app'\n").unwrap();
        provider.approved.lock().unwrap().push(Plan {
            alias: "upstream".into(), name: "app".into(),
            source: Source { kind: Kind::Local, url: "/tmp/fixture-catalog.json".into(), token_env: None, packages: BTreeMap::new() },
            recipe, tag: "v2".into(), version: "2-1".into(),
            asset: Asset { name: "app.tar".into(), url: "app.tar".into(), sha256: Some("a".repeat(64)) },
            git: None, persist_source: false,
        });
        let mut update = ExternalUpdate { runner: "Sources".into(), id: "upstream/app".into(), version: "2-1".into(), ..Default::default() };
        let actions = provider.upgrade_actions(std::slice::from_ref(&update)).unwrap();
        assert!(matches!(&actions[0], UpgradeAction::Internal(action) if action.contains("upstream/app -> 2-1") && action.contains("pkexec pacman -U") && !action.contains("pacpin -S")));
        assert!(provider.upgrade_description(std::slice::from_ref(&update)).unwrap().starts_with("Internal:"));
        update.version = "3-1".into();
        assert!(provider.upgrade_actions(&[update]).is_err());
    }
}
