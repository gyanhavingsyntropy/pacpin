//! Literal command adapters with explicit capabilities and an approved snapshot.
use super::*;
use crate::manager_config::Definition;
use std::io;
use std::sync::{Mutex, atomic::{AtomicBool, Ordering}};

pub(super) struct Provider {
    name: String,
    definition: Definition,
    approved: Mutex<Vec<ExternalUpdate>>,
    no_cache: AtomicBool,
}
impl Provider {
    pub(super) fn new(name: String, mut definition: Definition) -> Self {
        if definition.upgrade.is_empty() { definition.upgrade = builtin::defaults(&definition.adapter); }
        if definition.adapter == "pipx" && definition.no_cache_args.is_empty() { definition.no_cache_args = vec!["--pip-args=--no-cache-dir".into()]; }
        Self { name, definition, approved: Mutex::new(Vec::new()), no_cache: AtomicBool::new(false) }
    }
    fn template(&self) -> &[String] {
        match crate::scratch::policy() {
            crate::scratch::Policy::Ram => &self.definition.upgrade_ram,
            crate::scratch::Policy::Storage => &self.definition.upgrade_storage,
            crate::scratch::Policy::Auto => &self.definition.upgrade,
        }
    }
    fn arguments(&self, update: &ExternalUpdate) -> io::Result<Vec<String>> {
        let mut command = builtin::expand(self.template(), update)?;
        let mut extra = if self.no_cache.load(Ordering::SeqCst) { self.definition.no_cache_args.clone() } else { Vec::new() };
        for flag in &self.definition.selected_flags { extra.extend(self.definition.flags[flag].args.clone()); }
        let index = command.iter().position(|arg| arg == "--").unwrap_or(command.len());
        command.splice(index..index, extra);
        Ok(command)
    }
    fn command(&self, arguments: &[String]) -> io::Result<Command> {
        if unsafe { libc::geteuid() } == 0 { return Err(io::Error::other("Start pacpin as a regular user; manager commands are unprivileged")); }
        let executable = arguments.first().ok_or_else(|| io::Error::other("Unsupported operation: no configured command"))?;
        let mut command = Command::new(executable);
        command.args(&arguments[1..]).envs(&self.definition.env);
        if self.definition.adapter == "flatpak" {
            command.env("FLATPAK_FANCY_OUTPUT", "0");
        }
        Ok(command)
    }
    fn run(&self, arguments: &[String], phase: &str) -> io::Result<bool> {
        if unsafe { libc::geteuid() } == 0 { return Err(io::Error::other("Start pacpin as a regular user; manager commands are unprivileged")); }
        let result = crate::progress::run(&mut self.command(arguments)?, crate::progress::integration_manager(&self.name), phase, || {});
        crate::pacman_config::invalidate();
        Ok(result?.success())
    }
}
impl IntegrationProvider for Provider {
    fn name(&self) -> &str { &self.name }
    fn is_available(&self) -> bool {
        let executable = self.definition.check.first().map(String::as_str).unwrap_or(&self.definition.adapter);
        crate::is_command_available(executable)
    }
    fn refresh_metadata(&self) -> io::Result<()> {
        if unsafe { libc::geteuid() } == 0 { return Err(io::Error::other("Configured managers must be refreshed as the regular user")); }
        if self.definition.refresh.is_empty() { builtin::refresh(&self.definition.adapter, &self.definition.env) }
        else { builtin::capture(&mut self.command(&self.definition.refresh)?)?; Ok(()) }
    }
    fn supports_refresh(&self) -> bool { !self.definition.refresh.is_empty() || self.definition.adapter == "flatpak" }
    fn check_updates(&self) -> io::Result<Vec<ExternalUpdate>> {
        if unsafe { libc::geteuid() } == 0 { return Err(io::Error::other("Configured managers must be checked as the regular user")); }
        if !self.is_available() { return Err(io::Error::new(io::ErrorKind::NotFound, "Configured manager executable is unavailable")); }
        let mut updates = if self.definition.check.is_empty() { builtin::check(&self.definition.adapter, &self.definition.env)? } else {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Candidate {
                id: String, version: String,
                #[serde(default)] name: String,
                #[serde(default)] repo: String,
                #[serde(default)] installed_version: Option<String>,
                #[serde(default)] revision: Option<String>,
                #[serde(default)] scope: Option<String>,
            }
            let output = builtin::capture(&mut self.command(&self.definition.check)?)?;
            let candidates: Vec<Candidate> = serde_json::from_slice(&output.stdout).map_err(io::Error::other)?;
            candidates.into_iter().map(|c| ExternalUpdate { runner: self.name.clone(), name: if c.name.is_empty() { c.id.clone() } else { c.name },
                id: c.id, repo: c.repo, version: c.version, installed_version: c.installed_version, revision: c.revision, scope: c.scope }).collect()
        };
        let mut seen = std::collections::HashSet::new();
        for update in &mut updates {
            update.runner = self.name.clone();
            if update.id.is_empty() || update.id.starts_with('-') || update.version.is_empty()
                || [&update.id, &update.version].into_iter().chain(update.scope.iter()).chain(update.revision.iter()).chain(update.installed_version.iter())
                    .any(|s| s.len() > 4096 || s.chars().any(char::is_control))
                || !seen.insert((update.id.clone(), update.scope.clone())) {
                return Err(io::Error::other("Invalid or duplicate manager update metadata"));
            }
            update.name = crate::utils::sanitize_display_text(&update.name);
            update.repo = crate::utils::sanitize_display_text(&update.repo);
        }
        *self.approved.lock().unwrap_or_else(|e| e.into_inner()) = updates.clone();
        Ok(updates)
    }
    fn validate_options(&self, no_cache: bool) -> Result<(), String> {
        if self.template().is_empty() { return Err(format!("{} does not support {:?} scratch; declare a matching upgrade variant in managers.toml", self.name, crate::scratch::policy())); }
        if no_cache && self.definition.no_cache_args.is_empty() && self.definition.cache_cleanup.is_empty() {
            return Err(format!("{} does not support --no-cache; declare no_cache_args or cache_cleanup in managers.toml. Unused installed packages/generations are not caches.", self.name));
        }
        if crate::scratch::policy() != crate::scratch::Policy::Auto {
            crate::scratch::choose(crate::scratch::policy(), self.definition.scratch_mib.ok_or("Missing scratch_mib")? * 1_048_576, 128)?;
        }
        for command in [&self.definition.upgrade, self.template(), &self.definition.verify, &self.definition.cache_cleanup] {
            if !command.is_empty() && !crate::is_command_available(&command[0]) { return Err(format!("{} executable '{}' is unavailable", self.name, command[0])); }
        }
        Ok(())
    }
    fn configure_no_cache(&self, no_cache: bool) { self.no_cache.store(no_cache, Ordering::SeqCst); }
    fn upgrade_command_str(&self, updates: &[ExternalUpdate]) -> String {
        let mut commands: Vec<_> = updates.iter().map(|u| self.arguments(u).map(|args| builtin::display(&args)).unwrap_or_else(|e| format!("Invalid plan: {e}"))).collect();
        for flag in &self.definition.selected_flags {
            let command = &self.definition.flags[flag].command;
            if !command.is_empty() { commands.push(builtin::display(command)); }
        }
        commands.join(" && ")
    }
    fn execute_upgrade(&self, updates: &[ExternalUpdate]) -> io::Result<bool> {
        self.validate_options(self.no_cache.load(Ordering::SeqCst)).map_err(io::Error::other)?;
        let approved = self.approved.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let workspace = if crate::scratch::policy() == crate::scratch::Policy::Auto && self.definition.adapter != "pipx" { None } else {
            Some(crate::scratch::Workspace::create(crate::scratch::policy(), self.definition.scratch_mib.unwrap_or(0) * 1_048_576).map_err(io::Error::other)?)
        };
        for update in updates {
            if !approved.contains(update) { return Err(io::Error::other("Manager update changed after approval")); }
            let arguments = self.arguments(update)?;
            let mut command = self.command(&arguments)?;
            if let Some(workspace) = &workspace { command.env("TMPDIR", &workspace.path); }
            if self.definition.adapter == "pipx" {
                let workspace = workspace.as_ref().ok_or_else(|| io::Error::other("Missing Pipx constraint workspace"))?;
                let constraint = workspace.path.join("approved-version.txt");
                if !update.name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                    || !update.version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | '!')) {
                    return Err(io::Error::other("Invalid Pipx version constraint"));
                }
                std::fs::write(&constraint, format!("{}=={}\n", update.name, update.version))?;
                command.env("PIP_CONSTRAINT", &constraint).env("UV_CONSTRAINT", &constraint);
            }
            if unsafe { libc::geteuid() } == 0 { return Err(io::Error::other("Manager commands must run as the regular user")); }
            let status = crate::progress::run(&mut command, crate::progress::integration_manager(&self.name), "Upgrading packages", || {});
            crate::pacman_config::invalidate();
            if !status?.success() { return Ok(false); }
            if self.definition.verify.is_empty() { builtin::verify(&self.definition.adapter, update, &self.definition.env)?; }
            else { builtin::capture(&mut self.command(&builtin::expand(&self.definition.verify, update)?)?)?; }
            let record = serde_json::json!({
                "name": update.name, "id": update.id, "manager": self.name,
                "scope": update.scope, "revision": update.revision,
                "installed_version_before": update.installed_version,
                "to_version": update.version, "to_repo": update.repo,
                "state": "verified-external",
            });
            if let Err(error) = crate::progress::operation(crate::progress::Manager::Record, "Recording external upgrade", ||
                crate::journal::TransactionJournal::record_transaction("external-upgrade", vec![record], &builtin::display(&arguments))) {
                eprintln!(":: Warning: {} was installed and verified, but its history could not be saved: {error}", update.id);
            }
        }
        for flag in &self.definition.selected_flags {
            let command = &self.definition.flags[flag].command;
            if !command.is_empty() && !self.run(command, "Running configured action")? { return Ok(false); }
        }
        Ok(true)
    }
    fn cache_command_str(&self) -> Option<String> {
        if !self.definition.cache_cleanup.is_empty() { Some(builtin::display(&self.definition.cache_cleanup)) }
        else if !self.definition.no_cache_args.is_empty() { Some("Cache writes disabled by configured upgrade arguments".into()) }
        else { None }
    }
    fn execute_cache_cleanup(&self) -> io::Result<bool> {
        if self.definition.cache_cleanup.is_empty() { Ok(true) } else { self.run(&self.definition.cache_cleanup, "Cleaning package cache") }
    }
    fn clean_command_str(&self) -> String {
        if self.definition.clean.is_empty() { builtin::clean_preview(&self.definition.adapter) } else { builtin::display(&self.definition.clean) }
    }
    fn execute_clean(&self) -> io::Result<bool> {
        if unsafe { libc::geteuid() } == 0 { return Err(io::Error::other("Configured managers must be cleaned as the regular user")); }
        if self.definition.clean.is_empty() { builtin::clean(&self.definition.adapter, &self.definition.env) } else { self.run(&self.definition.clean, "Cleaning unused packages") }
    }
    fn supports_clean(&self) -> bool { !self.definition.clean.is_empty() || matches!(self.definition.adapter.as_str(), "flatpak" | "nix") }
}
