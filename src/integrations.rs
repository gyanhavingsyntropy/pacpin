// pacpin - Declarative Package Resolver & Upgrade Engine for Arch Linux / CachyOS
// Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use crate::config::Config;
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::Arc;
use std::thread;

const MAX_METADATA_BYTES: u64 = 10 * 1024 * 1024;

fn pypi_version(reader: impl std::io::Read, limit: u64) -> std::io::Result<String> {
    use std::io::Read;
    #[derive(Deserialize)]
    struct Metadata { info: Info }
    #[derive(Deserialize)]
    struct Info { version: String }
    let mut body = Vec::new();
    reader.take(limit.saturating_add(1)).read_to_end(&mut body)?;
    if body.len() as u64 > limit {
        return Err(std::io::Error::other("PyPI metadata exceeds size limit"));
    }
    // Skip the potentially large release history instead of constructing a
    // serde_json::Value tree for data the update check never uses.
    let metadata: Metadata = serde_json::from_slice(&body).map_err(std::io::Error::other)?;
    let version = metadata.info.version;
    if version.is_empty() || version.len() > 256 || !version.chars().all(|c|
        c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | '!')) {
        return Err(std::io::Error::other("Invalid PyPI version"));
    }
    Ok(version)
}

fn flatpak_cache_file() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    flatpak_cache_file_in(std::path::Path::new(&home))
}

fn flatpak_cache_file_in(home: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = home.join(".cache").join("pacpin");
    std::fs::create_dir_all(&dir).ok()?;
    if std::fs::symlink_metadata(&dir).ok()?.file_type().is_symlink() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if std::fs::metadata(&dir).ok()?.uid() != unsafe { libc::geteuid() } {
            return None;
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    }
    Some(dir.join("flatpak_updates.json"))
}

fn read_flatpak_cache(path: &std::path::Path) -> Option<Vec<ExternalUpdate>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.modified().ok()?.elapsed().ok()?.as_secs() >= 120 {
        return None;
    }
    let mut content = Vec::new();
    file.take(MAX_METADATA_BYTES + 1).read_to_end(&mut content).ok()?;
    if content.len() as u64 > MAX_METADATA_BYTES { return None; }
    let mut cached: Vec<ExternalUpdate> = serde_json::from_slice(&content).ok()?;
    for update in &mut cached {
        if update.runner != "Flatpak" { return None; }
        update.id = crate::utils::sanitize_display_text(&update.id);
        update.name = crate::utils::sanitize_display_text(&update.name);
        update.repo = crate::utils::sanitize_display_text(&update.repo);
        update.version = crate::utils::sanitize_display_text(&update.version);
    }
    Some(cached)
}

fn write_flatpak_cache(path: &std::path::Path, content: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().ok_or_else(|| std::io::Error::other("Cache has no parent"))?;
    let temporary = crate::download::create_secure_temp_file(parent, ".flatpak-updates", "tmp")
        .map_err(std::io::Error::other)?;
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(&temporary)?;
        file.write_all(content)?;
        drop(file);
        // Rename replaces a destination symlink itself; it never truncates the
        // symlink target. Readers see either the previous or complete new cache.
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temporary); }
    result
}

#[path = "integrations/builtin.rs"]
mod builtin;
#[path = "integrations/configured.rs"]
mod configured;
pub(crate) mod preparation;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExternalUpdate {
    pub runner: String,
    pub id: String,
    pub name: String,
    pub repo: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// Providers must distinguish an actual command from pacpin-owned work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeAction {
    Command(String),
    Internal(String),
}
impl UpgradeAction {
    pub fn description(&self) -> String {
        match self {
            Self::Command(command) => command.clone(),
            Self::Internal(action) => format!("Internal: {action}"),
        }
    }
}

pub trait IntegrationProvider: Send + Sync {
    /// Optional recipe detail from the exact approved snapshot, before approval.
    fn preview_upgrade(&self, _updates: &[ExternalUpdate]) {}
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn refresh_metadata(&self) -> Result<(), std::io::Error>;
    fn supports_refresh(&self) -> bool { false }
    fn check_updates(&self) -> crate::discovery::Report<ExternalUpdate>;
    fn upgrade_actions(&self, updates: &[ExternalUpdate]) -> std::io::Result<Vec<UpgradeAction>>;
    fn upgrade_description(&self, updates: &[ExternalUpdate]) -> std::io::Result<String> {
        Ok(self.upgrade_actions(updates)?.iter().map(UpgradeAction::description).collect::<Vec<_>>().join("; "))
    }
    fn execute_upgrade(&self, updates: &[ExternalUpdate]) -> Result<bool, std::io::Error>;
    fn check_options(&self, policy: crate::scratch::Policy, no_cache: bool) -> Result<(), crate::runtime::OptionError>;
    fn validate_options(&self, no_cache: bool) -> Result<(), String> {
        self.check_options(crate::scratch::policy(), no_cache).map_err(|error| error.to_string())
    }
    fn configure_no_cache(&self, _no_cache: bool) {}
    fn cache_command_str(&self) -> Option<String> { None }
    fn execute_cache_cleanup(&self) -> std::io::Result<bool> { Ok(true) }
    fn clean_command_str(&self) -> String;
    fn execute_clean(&self) -> Result<bool, std::io::Error>;
    fn supports_clean(&self) -> bool { false }
    fn preparation_policy(&self) -> preparation::Policy;
    fn prefetch(&self, _updates: &[ExternalUpdate]) -> std::io::Result<()> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "No download-only preparation adapter"))
    }
    fn supports_aur_failure_continuation(&self) -> bool { false }
    fn execute_after_aur_failure(&self, _update: &ExternalUpdate, _permit: &mut crate::pm::AurBuildFailurePermit) -> Result<bool, std::io::Error> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "This integration does not support continuation after AUR failure"))
    }
}

pub struct IntegrationsManager;
impl IntegrationsManager {
    pub fn configured_actions(config: &Config, name: &str, flags: &[String], execute: bool) -> Result<Vec<String>, String> {
        let definition = config.manager_file.managers.get(name).ok_or_else(|| format!("Unknown manager '{name}'; configure managers.toml"))?;
        if flags.is_empty() { return Err("Choose at least one configured manager flag".into()); }
        let mut commands = Vec::new();
        for flag in flags {
            let key = flag.strip_prefix("--").ok_or("Manager actions must be flags")?;
            let key = if let Some((owner, key)) = key.split_once('.') {
                if owner != name { return Err(format!("{flag} belongs to a different manager")); } key
            } else { key };
            let action = definition.flags.get(key).ok_or_else(|| format!("Unknown custom flag --{name}.{key}"))?;
            if action.command.is_empty() { return Err(format!("--{name}.{key} only adds upgrade arguments; use it with pin upgrade")); }
            if !commands.contains(&action.command) { commands.push(action.command.clone()); }
        }
        if execute {
            crate::progress::configure_managers(config.manager_file.managers.keys().cloned().collect());
            if unsafe { libc::geteuid() } == 0 { return Err("Configured actions must run as a regular user".into()); }
            for arguments in &commands {
                let mut command = Command::new(&arguments[0]); command.args(&arguments[1..]).envs(&definition.env);
                let status = crate::progress::run(&mut command, crate::progress::integration_manager(match name { "flatpak" => "Flatpak", "nix" => "Nix", "pipx" => "Pipx", _ => name }), "Running configured action", || {}).map_err(|e| e.to_string())?;
                if !status.success() { return Err(format!("{name} action failed with {status}")); }
            }
        }
        Ok(commands.iter().map(|c| builtin::display(c)).collect())
    }
    pub fn get_active_providers(config: &Config) -> Vec<Arc<dyn IntegrationProvider>> {
        let mut providers: Vec<Arc<dyn IntegrationProvider>> = Vec::new();
        if !config.sources.is_empty() {
            providers.push(Arc::new(crate::sources::SourceProvider::new(config)));
        }
        crate::progress::configure_managers(config.manager_file.managers.keys().cloned().collect());
        for (key, name, legacy) in [("flatpak", "Flatpak", config.integrations.flatpak), ("nix", "Nix", config.integrations.nix), ("pipx", "Pipx", config.integrations.pipx)] {
            let definition = config.manager_file.managers.get(key).cloned().unwrap_or_else(|| crate::manager_config::Definition { adapter: key.into(), ..Default::default() });
            if definition.enabled.unwrap_or(config.features.integrations && legacy) {
                providers.push(Arc::new(configured::Provider::new(name.into(), definition)));
            }
        }
        for (name, definition) in &config.manager_file.managers {
            if definition.adapter == "command" && definition.enabled == Some(true) {
                providers.push(Arc::new(configured::Provider::new(name.clone(), definition.clone())));
            }
        }
        // Explicitly enabled unavailable managers fail their checks. Sources
        // with no owned installed packages have no work to discover.
        providers.retain(|p| p.name() != "Sources" || p.is_available());
        providers
    }

    #[cfg(test)]
    fn filter_available_parallel(
        providers: Vec<Arc<dyn IntegrationProvider>>,
    ) -> Vec<Arc<dyn IntegrationProvider>> {
        if providers.len() < 2 {
            return providers.into_iter().filter(|provider| provider.is_available()).collect();
        }
        let handles: Vec<_> = providers
            .into_iter()
            .map(|provider| thread::spawn(move || {
                provider.is_available().then_some(provider)
            }))
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
    }

    pub(crate) fn check_updates_parallel(
        providers: &[Arc<dyn IntegrationProvider>],
    ) -> crate::discovery::Report<ExternalUpdate> {
        Self::discover_parallel(providers, false)
    }

    /// Each manager waits only for its own refresh. Sources need no refresh and
    /// can check immediately, even while Flatpak or native metadata is loading.
    pub(crate) fn discover_parallel(
        providers: &[Arc<dyn IntegrationProvider>], refresh: bool,
    ) -> crate::discovery::Report<ExternalUpdate> {
        if providers.is_empty() {
            return crate::discovery::Report::default();
        }

        let mut handles = Vec::new();

        for p in providers {
            let provider = Arc::clone(p);
            handles.push((p.name().to_string(), thread::spawn(move || {
                let manager = crate::progress::integration_manager(provider.name());
                if refresh && provider.supports_refresh() {
                    let _activity = crate::progress::activity(manager, "Refreshing metadata");
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| provider.refresh_metadata()));
                    let error = match result {
                        Ok(Ok(())) => None,
                        Ok(Err(error)) => Some(error.to_string()),
                        Err(_) => Some("metadata worker failed".into()),
                    };
                    if let Some(error) = error {
                        let mut report = crate::discovery::Report::default();
                        report.record(crate::discovery::Manager::External(provider.name().into()), crate::discovery::Phase::Refresh, Err(error));
                        return report;
                    }
                }
                let _activity = crate::progress::activity(manager, "Checking updates");
                provider.check_updates()
            })));
        }

        let mut report = crate::discovery::Report::default();
        for (name, handle) in handles {
            match handle.join() {
                Ok(result) => report.append(result),
                Err(_) => report.record(crate::discovery::Manager::External(name), crate::discovery::Phase::Updates, Err("update worker failed".into())),
            }
        }
        report
    }

    pub fn execute_after_aur_failure(
        providers: &[Arc<dyn IntegrationProvider>], updates: &[ExternalUpdate], permit: &mut crate::pm::AurBuildFailurePermit,
        preparation: Option<&preparation::Session>,
    ) -> Vec<(String, bool)> {
        let mut outcomes = Vec::new();
        let mut stopped = false;
        for update in updates {
            let provider = providers.iter().find(|p| p.name() == update.runner && p.supports_aur_failure_continuation());
            let Some(provider) = provider else {
                println!(":: {}: skipped {} after AUR failure (continuation unsupported).", update.runner, update.name); continue;
            };
            if stopped { println!(":: Sources: skipped {} because safe continuation could not be re-established.", update.name); continue; }
            if let Err(reason) = permit.validate() {
                eprintln!(":: Sources: skipped {}: {reason}", update.name); stopped = true; continue;
            }
            println!(":: Checking independent Sources update: {}", update.name);
            let description = match provider.upgrade_description(std::slice::from_ref(update)) {
                Ok(description) => description,
                Err(error) => { eprintln!(":: Sources plan is invalid: {error}"); outcomes.push((String::new(), false)); stopped = true; continue; }
            };
            if let Some(preparation) = preparation { preparation.before(provider.name()); }
            let success = match provider.execute_after_aur_failure(update, permit) {
                Ok(true) => { println!("✔ Sources: {} updated successfully.", update.name); true },
                Ok(false) => { eprintln!(":: Sources: {} failed.", update.name); false },
                Err(error) => { eprintln!(":: Sources: {} was not completed: {error}", update.name); false },
            };
            outcomes.push((description, success));
            if !success && permit.validate().is_err() { stopped = true; }
        }
        let source_count = updates.iter().filter(|u| u.runner == "Sources").count();
        let completed = outcomes.iter().filter(|(_, success)| *success).count();
        if source_count != 0 && completed == source_count { crate::progress::completed(crate::progress::Manager::Sources); }
        else if source_count != 0 { crate::progress::failed(crate::progress::Manager::Sources); }
        if source_count != 0 { println!(":: Sources after AUR failure: {completed} completed, {} not completed.", source_count.saturating_sub(completed)); }
        outcomes
    }

    pub fn execute_upgrades(
        providers: &[Arc<dyn IntegrationProvider>],
        external_updates: &[ExternalUpdate],
        preparation: Option<&preparation::Session>,
    ) -> Vec<(String, String, bool)> {
        let mut outcomes = Vec::new();
        let mut stopped = false;
        for p in providers {
            let p_updates: Vec<ExternalUpdate> = external_updates
                .iter()
                .filter(|u| u.runner == p.name())
                .cloned()
                .collect();

            if p_updates.is_empty() {
                continue;
            }
            if stopped { println!(":: {}: skipped because an earlier external upgrade failed.", p.name()); outcomes.push((p.name().to_string(), String::new(), false)); continue; }
            let cmd_str = match p.upgrade_description(&p_updates) {
                Ok(description) => description,
                Err(error) => {
                    eprintln!(":: {} plan is invalid: {error}", p.name());
                    crate::progress::failed(crate::progress::integration_manager(p.name()));
                    outcomes.push((p.name().to_string(), String::new(), false));
                    stopped = true;
                    continue;
                }
            };
            if let Some(preparation) = preparation { preparation.before(p.name()); }
            println!(
                "\n{} Upgrading {} ({} approved {})...",
                "::".cyan(),
                p.name().bold(),
                p_updates.len(),
                if p_updates.len() == 1 { "target" } else { "targets" }
            );
            let manager = Some(crate::progress::integration_manager(p.name()));
            match p.execute_upgrade(&p_updates) {
                Ok(true) => {
                    if let Some(manager) = manager { crate::progress::completed(manager); }
                    println!("✔ {} upgrade completed successfully.", p.name().green());
                    outcomes.push((p.name().to_string(), cmd_str, true));
                }
                Ok(false) => {
                    stopped = true;
                    if let Some(manager) = manager { crate::progress::failed(manager); }
                    eprintln!(
                        "{}",
                        format!("Warning: {} upgrade exited with errors.", p.name()).yellow()
                    );
                    outcomes.push((p.name().to_string(), cmd_str, false));
                }
                Err(e) => {
                    stopped = true;
                    if let Some(manager) = manager { crate::progress::failed(manager); }
                    eprintln!(
                        "{}",
                        format!("Failed to run {} upgrade: {}", p.name(), e).red()
                    );
                    outcomes.push((p.name().to_string(), cmd_str, false));
                }
            }
        }
        outcomes
    }


    pub fn transaction_cache_cleanup(providers: &[Arc<dyn IntegrationProvider>], outcomes: &[(String, bool)], no_cache: bool) -> bool {
        if !no_cache { return true; }
        let mut success = true;
        for (name, completed) in outcomes {
            if !completed { println!(":: {name}: cache cleanup skipped because its upgrade did not complete."); continue; }
            let Some(provider) = providers.iter().find(|p| p.name() == name) else { continue; };
            let Some(preview) = provider.cache_command_str() else { continue; };
            println!(":: {name}: --no-cache action: {preview}");
            match provider.execute_cache_cleanup() {
                Ok(true) => println!("✔ {name}: cache action completed."),
                Ok(false) => { eprintln!(":: {name}: cache cleanup failed."); success = false; },
                Err(error) => { eprintln!(":: {name}: cache cleanup failed: {error}"); success = false; },
            }
        }
        success
    }

    pub fn execute_cleanups(providers: &[Arc<dyn IntegrationProvider>]) -> bool {
        // One foreground owner at a time: print the heading before execution,
        // keep manager output intact, and propagate failures to the CLI.
        let mut success = true;
        for provider in providers.iter().filter(|p| p.supports_clean()) {
                let name = provider.name();
                let cmd_str = provider.clean_command_str();
                println!(
                    "\n{} Cleaning {} unused packages ({})...",
                    "::".cyan(),
                    name.bold(),
                    cmd_str.dimmed()
                );
                match provider.execute_clean() {
                    Ok(true) => {
                        println!("✔ {} cleanup completed successfully.", name.green());
                    }
                    Ok(false) => {
                        success = false;
                        eprintln!(
                            "{}",
                            format!("Warning: {} cleanup exited with errors.", name).yellow()
                        );
                    }
                    Err(e) => {
                        success = false;
                        eprintln!(
                            "{}",
                            format!("Failed to run {} cleanup: {}", name, e).red()
                        );
                    }
                }
        }
        success
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn flatpak_cache_never_follows_a_file_symlink_and_writes_atomically() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let fixture = crate::db::test_support::AlpmFixture::new(&[], &[]);
        let root = std::path::PathBuf::from(fixture.manager.handle().dbpath());
        let cache = root.join("flatpak.json");
        let sentinel = root.join("keep");
        std::fs::write(&sentinel, b"preserve").unwrap();
        symlink(&sentinel, &cache).unwrap();
        assert!(read_flatpak_cache(&cache).is_none());
        write_flatpak_cache(&cache, b"[]").unwrap();
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"preserve");
        assert!(!std::fs::symlink_metadata(&cache).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(read_flatpak_cache(&cache).unwrap().is_empty());
        // Never block on a FIFO that occupies the cache filename.
        std::fs::remove_file(&cache).unwrap();
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(cache.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(read_flatpak_cache(&cache).is_none());
        std::fs::remove_file(&cache).unwrap();
        std::fs::create_dir(&cache).unwrap();
        assert!(write_flatpak_cache(&cache, b"[]").is_err());
        assert!(!std::fs::read_dir(&root).unwrap().flatten().any(|entry| entry.file_name().to_string_lossy().starts_with(".flatpak-updates-")));
    }

    #[test]
    fn pypi_metadata_is_bounded_and_only_retains_the_version() {
        let body = br#"{"info":{"version":"1!2.0rc1+local"},"releases":{"old":[{"unused":"data"}]}}"#;
        assert_eq!(pypi_version(body.as_slice(), body.len() as u64).unwrap(), "1!2.0rc1+local");
        assert!(pypi_version(body.as_slice(), body.len() as u64 - 1).is_err());
        assert!(pypi_version(b"{}".as_slice(), 100).is_err());
        assert!(pypi_version(b"invalid".as_slice(), 100).is_err());
        for version in ["", "1.0\u{1b}[2J", "1.0\nspoofed", "1.0/escape"] {
            let body = serde_json::json!({"info":{"version":version}}).to_string();
            assert!(pypi_version(body.as_bytes(), 1024).is_err());
        }
        // A valid JSON document followed by an oversized whitespace body must
        // also be rejected, rather than treating a truncated prefix as complete.
        let body = format!("{{\"info\":{{\"version\":\"1.0\"}}}}{}", " ".repeat(200));
        assert!(pypi_version(body.as_bytes(), 100).is_err());
    }

    #[test]
    fn test_external_update_struct() {
        let update = ExternalUpdate {
            runner: "Flatpak".to_string(),
            id: "org.mozilla.firefox".to_string(),
            name: "Firefox".to_string(),
            repo: "flathub".to_string(),
            version: "130.0".to_string(),
            ..ExternalUpdate::default()
        };
        assert_eq!(update.runner, "Flatpak");
        assert_eq!(update.repo, "flathub");
    }

    #[test]
    fn test_flatpak_parsing() {
        let sample = "Minecraft Bedrock Launcher\tio.mrarm.mcpelauncher\tv1.8.4\tflathub\n";
        let parts: Vec<&str> = sample.trim().split('\t').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "Minecraft Bedrock Launcher");
        assert_eq!(parts[1], "io.mrarm.mcpelauncher");
        assert_eq!(parts[2], "v1.8.4");
        assert_eq!(parts[3], "flathub");
    }

    #[test]
    fn test_integrations_manager_disabled_by_default() {
        let config = Config::default();
        let providers = IntegrationsManager::get_active_providers(&config);
        assert!(providers.is_empty());
    }

    #[test]
    fn test_provider_supports_clean() {
        let flatpak = test_provider("flatpak");
        let nix = test_provider("nix");
        let pipx = test_provider("pipx");
        assert!(flatpak.supports_clean());
        assert!(nix.supports_clean());
        assert!(!pipx.supports_clean());
        assert!(!flatpak.supports_aur_failure_continuation());
        assert!(!nix.supports_aur_failure_continuation());
        assert!(!pipx.supports_aur_failure_continuation());
        assert!(crate::sources::SourceProvider::new(&Config::default()).supports_aur_failure_continuation());
    }

    #[cfg(unix)]
    #[test]
    fn test_flatpak_cache_is_private_and_rejects_symlink() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let home = std::env::temp_dir().join(format!("pacpin-flatpak-cache-test-{}", std::process::id()));
        std::fs::create_dir(&home).unwrap();
        let cache = flatpak_cache_file_in(&home).unwrap();
        assert_eq!(std::fs::metadata(cache.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        std::fs::remove_dir(cache.parent().unwrap()).unwrap();
        symlink(&home, cache.parent().unwrap()).unwrap();
        assert!(flatpak_cache_file_in(&home).is_none());
        std::fs::remove_dir_all(home).unwrap();
    }

    struct MockProvider {
        label: &'static str,
        available: bool,
        active: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    impl IntegrationProvider for MockProvider {
        fn preparation_policy(&self) -> preparation::Policy { preparation::Policy::Unsupported("fixture") }
        fn check_options(&self, _: crate::scratch::Policy, _: bool) -> Result<(), crate::runtime::OptionError> { Ok(()) }
        fn name(&self) -> &'static str { self.label }
        fn is_available(&self) -> bool {
            let count = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(count, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(25));
            self.active.fetch_sub(1, Ordering::SeqCst);
            self.available
        }
        fn supports_refresh(&self) -> bool { self.label == "fail-refresh" }
        fn refresh_metadata(&self) -> Result<(), std::io::Error> {
            if self.label == "fail-refresh" { Err(std::io::Error::other("fixture refresh failed")) } else { Ok(()) }
        }
        fn check_updates(&self) -> crate::discovery::Report<ExternalUpdate> {
            let mut report = crate::discovery::Report::updates(self.name(), self.check_result());
            if self.label == "partial-source" {
                report.record(crate::discovery::Manager::Source("repo/broken".into()), crate::discovery::Phase::Updates, Err("missing asset".into()));
            }
            report
        }
        fn upgrade_actions(&self, _: &[ExternalUpdate]) -> std::io::Result<Vec<UpgradeAction>> { Ok(Vec::new()) }
        fn execute_upgrade(&self, _: &[ExternalUpdate]) -> Result<bool, std::io::Error> {
            self.active.fetch_add(1, Ordering::SeqCst); Ok(self.label != "fail-upgrade")
        }
        fn clean_command_str(&self) -> String { String::new() }
        fn execute_clean(&self) -> Result<bool, std::io::Error> { Ok(true) }
    }

    impl MockProvider {
        fn check_result(&self) -> std::io::Result<Vec<ExternalUpdate>> {
            if self.label == "panic-check" { panic!("fixture worker panic"); }
            if matches!(self.label, "has-update" | "partial-source") {
                return Ok(vec![ExternalUpdate { runner: self.label.into(), id: "app".into(), ..Default::default() }]);
            }
            if self.label.starts_with("parallel-check-") {
                let count = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(count, Ordering::SeqCst);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while self.peak.load(Ordering::SeqCst) < 3 && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                self.active.fetch_sub(1, Ordering::SeqCst);
                if self.peak.load(Ordering::SeqCst) < 3 {
                    return Err(std::io::Error::other("check workers did not overlap"));
                }
            }
            if self.label == "fail-check" { Err(std::io::Error::other("fixture check failed")) } else { Ok(Vec::new()) }
        }
    }

    #[test]
    fn test_provider_detection_overlaps_and_preserves_order() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let providers: Vec<Arc<dyn IntegrationProvider>> = [
            ("one", true), ("two", false), ("three", true),
        ].into_iter().map(|(label, available)| Arc::new(MockProvider {
            label, available, active: Arc::clone(&active), peak: Arc::clone(&peak),
        }) as Arc<dyn IntegrationProvider>).collect();
        let selected = IntegrationsManager::filter_available_parallel(providers);
        assert_eq!(selected.iter().map(|p| p.name()).collect::<Vec<_>>(), vec!["one", "three"]);
        assert!(peak.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn discovery_preserves_successes_and_reports_each_failed_or_panicked_manager() {
        let provider = |label| Arc::new(MockProvider { label, available: true,
            active: Arc::new(AtomicUsize::new(0)), peak: Arc::new(AtomicUsize::new(0)) }) as Arc<dyn IntegrationProvider>;
        let (updates, failures) = IntegrationsManager::check_updates_parallel(&[
            provider("fail-check"), provider("has-update"), provider("panic-check"),
        ]).into_parts();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].runner, "has-update");
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[1].manager, crate::discovery::Manager::External("panic-check".into()));
        let (_, failures) = IntegrationsManager::discover_parallel(&[provider("fail-refresh")], true).into_parts();
        assert_eq!(failures[0].phase, crate::discovery::Phase::Refresh);
        let (updates, failures) = IntegrationsManager::check_updates_parallel(&[provider("partial-source")]).into_parts();
        assert_eq!(updates.len(), 1);
        assert_eq!(failures[0].manager, crate::discovery::Manager::Source("repo/broken".into()));
    }

    #[test]
    fn independent_checks_do_not_wait_for_another_managers_refresh() {
        use std::sync::atomic::AtomicBool;
        struct Pipeline {
            refresh: bool,
            independent_checked: Arc<AtomicBool>,
        }
        impl IntegrationProvider for Pipeline {
            fn preparation_policy(&self) -> preparation::Policy { preparation::Policy::Unsupported("fixture") }
            fn name(&self) -> &str { if self.refresh { "Flatpak" } else { "Sources" } }
            fn is_available(&self) -> bool { true }
            fn supports_refresh(&self) -> bool { self.refresh }
            fn refresh_metadata(&self) -> std::io::Result<()> {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                while !self.independent_checked.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                if self.independent_checked.load(Ordering::SeqCst) { Ok(()) }
                else { Err(std::io::Error::other("independent check waited for refresh")) }
            }
            fn check_updates(&self) -> crate::discovery::Report<ExternalUpdate> {
                if !self.refresh { self.independent_checked.store(true, Ordering::SeqCst); }
                crate::discovery::Report::updates(self.name(), Ok(Vec::new()))
            }
            fn check_options(&self, _: crate::scratch::Policy, _: bool) -> Result<(), crate::runtime::OptionError> { Ok(()) }
            fn upgrade_actions(&self, _: &[ExternalUpdate]) -> std::io::Result<Vec<UpgradeAction>> { unreachable!() }
            fn execute_upgrade(&self, _: &[ExternalUpdate]) -> std::io::Result<bool> { unreachable!() }
            fn clean_command_str(&self) -> String { unreachable!() }
            fn execute_clean(&self) -> std::io::Result<bool> { unreachable!() }
        }
        let ready = Arc::new(AtomicBool::new(false));
        let providers: Vec<Arc<dyn IntegrationProvider>> = [true, false].into_iter().map(|refresh|
            Arc::new(Pipeline { refresh, independent_checked: ready.clone() }) as Arc<dyn IntegrationProvider>).collect();
        assert!(IntegrationsManager::discover_parallel(&providers, true).require_complete().unwrap().is_empty());
        assert!(ready.load(Ordering::SeqCst));
    }

    #[test]
    fn update_checks_start_all_manager_workers_before_waiting() {
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let providers: Vec<Arc<dyn IntegrationProvider>> = [
            "parallel-check-sources", "parallel-check-flatpak", "parallel-check-nix",
        ].into_iter().map(|label| Arc::new(MockProvider {
            label, available: true, active: Arc::clone(&active), peak: Arc::clone(&peak),
        }) as Arc<dyn IntegrationProvider>).collect();
        assert!(IntegrationsManager::check_updates_parallel(&providers).require_complete().unwrap().is_empty());
        assert_eq!(peak.load(Ordering::SeqCst), 3);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn metadata_refresh_requires_an_explicit_capability() {
        assert!(test_provider("flatpak").supports_refresh());
        assert!(!test_provider("nix").supports_refresh());
        assert!(!test_provider("pipx").supports_refresh());
        assert!(!crate::sources::SourceProvider::new(&Config::default()).supports_refresh());
        let provider = MockProvider { label: "future", available: true,
            active: Arc::new(AtomicUsize::new(0)), peak: Arc::new(AtomicUsize::new(0)) };
        assert!(!provider.supports_refresh());
        IntegrationsManager::discover_parallel(&[Arc::new(provider)], true).require_complete().unwrap();
    }

    #[test]
    fn cleanup_requires_an_explicit_provider_capability() {
        let provider = MockProvider { label: "future", available: true,
            active: Arc::new(AtomicUsize::new(0)), peak: Arc::new(AtomicUsize::new(0)) };
        assert!(!provider.supports_clean());
        assert!(test_provider("flatpak").supports_clean());
        assert!(test_provider("nix").supports_clean());
        assert!(!test_provider("pipx").supports_clean());
    }

    #[test]
    fn prefetch_is_opt_in_and_matches_the_selected_manager() {
        assert_eq!(test_provider("flatpak").preparation_policy(), preparation::Policy::BeforeStage);
        for adapter in ["nix", "pipx"] {
            assert!(matches!(test_provider(adapter).preparation_policy(), preparation::Policy::Unsupported(_)));
        }
        assert!(matches!(crate::sources::SourceProvider::new(&Config::default()).preparation_policy(), preparation::Policy::Unsupported(_)));
    }

    fn test_provider(adapter: &str) -> configured::Provider {
        configured::Provider::new(adapter.into(), crate::manager_config::Definition {
            adapter: adapter.into(), ..Default::default()
        })
    }

    #[test]
    fn failed_checks_are_errors_and_failed_upgrades_stop_later_managers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = |label| Arc::new(MockProvider { label, available: true, active: Arc::clone(&calls), peak: Arc::new(AtomicUsize::new(0)) }) as Arc<dyn IntegrationProvider>;
        let error = IntegrationsManager::check_updates_parallel(&[provider("fail-check")]).require_complete().unwrap_err();
        assert!(error.to_string().contains("fail-check"));
        let outcomes = IntegrationsManager::execute_upgrades(&[provider("fail-upgrade"), provider("later")], &[
            ExternalUpdate { runner: "fail-upgrade".into(), id: "first".into(), ..ExternalUpdate::default() },
            ExternalUpdate { runner: "later".into(), id: "second".into(), ..ExternalUpdate::default() },
        ], None);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(outcomes.iter().all(|(_, _, success)| !success));
    }
}
