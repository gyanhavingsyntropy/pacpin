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
    fn name(&self) -> &'static str {
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
    fn check_updates(&self) -> Vec<ExternalUpdate> {
        let manager = match crate::db::AlpmManager::with_repo_order(&self.config.repo_order) {
            Ok(manager) => manager,
            Err(e) => {
                eprintln!("Warning: Cannot check source packages: {e}");
                return vec![];
            }
        };
        let ignore_flags: Vec<String> = std::process::Command::new("pacman-conf")
            .arg("IgnorePkg")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .split_whitespace()
                    .map(|pattern| format!("--ignore={pattern}"))
                    .collect()
            })
            .unwrap_or_default();
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
        // Bound HTTP pressure independently of core count. All source checks run
        // concurrently with native ALPM/AUR/external-manager checks.
        for batch in targets.chunks(4) {
            std::thread::scope(|scope| {
                let handles: Vec<_> = batch.iter().map(|(target, installed)| scope.spawn(move || {
                    let plan = match plan(&self.config, target) { Ok(plan) => plan, Err(e) => { eprintln!("Warning: Source check for '{target}' failed: {e}; package held at installed version"); return None; } };
                    if alpm::vercmp(plan.version.as_str(), installed.as_str()).is_gt() { Some(plan) } else { None }
                })).collect();
                for handle in handles {
                    if let Ok(Some(plan)) = handle.join() {
                        plans.push(plan);
                    }
                }
            });
        }
        let updates = plans
            .iter()
            .map(|p| ExternalUpdate {
                runner: self.name().into(),
                id: format!("{}/{}", p.alias, p.name),
                name: p.name.clone(),
                repo: p.alias.clone(),
                version: p.version.clone(),
            })
            .collect();
        *self.approved.lock().unwrap_or_else(|e| e.into_inner()) = plans;
        updates
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
}
