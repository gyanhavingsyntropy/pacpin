use super::*;
use crate::integrations::{ExternalUpdate, IntegrationProvider};
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
    fn check_updates(&self) -> io::Result<Vec<ExternalUpdate>> {
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
        let mut plans = Vec::new();
        let mut errors = Vec::new();
        // Bound HTTP pressure independently of core count. All source checks run
        // concurrently with native ALPM/AUR/external-manager checks.
        for batch in targets.chunks(4) {
            std::thread::scope(|scope| {
                let handles: Vec<_> = batch.iter().map(|(target, installed)| scope.spawn(move || {
                    let plan = plan(&self.config, target).map_err(|e| format!("Source '{target}': {e}"))?;
                    Ok::<_, String>(if alpm::vercmp(plan.version.as_str(), installed.as_str()).is_gt() { Some(plan) } else { None })
                })).collect();
                for handle in handles {
                    match handle.join() {
                        Ok(Ok(Some(plan))) => plans.push(plan), Ok(Ok(None)) => {},
                        Ok(Err(error)) => errors.push(error), Err(_) => errors.push("Source check worker failed".into()),
                    }
                }
            });
        }
        if !errors.is_empty() { self.approved.lock().unwrap_or_else(|e| e.into_inner()).clear(); return Err(io::Error::other(errors.join("; "))); }
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
        Ok(updates)
    }
    fn upgrade_command_str(&self, updates: &[ExternalUpdate]) -> String {
        format!(
            "pacpin -S -- {}",
            updates
                .iter()
                .map(|u| crate::utils::shell_quote(&u.id))
                .collect::<Vec<_>>()
                .join(" ")
        )
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
    fn prefetch(&self, updates: &[ExternalUpdate]) {
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
                    if let Err(e) = install::prefetch(plan) {
                        crate::progress::source_warning(format!(
                            ":: Warning: Background prefetch for {}/{} failed: {e}; will retry during install.",
                            plan.alias, plan.name
                        ));
                    }
                }
            }
        });
    }
    fn supports_prefetch(&self) -> bool { true }
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
}
