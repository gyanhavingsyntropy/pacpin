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
    fn preparation_policy_for(&self, policy: crate::scratch::Policy, no_cache: bool) -> crate::integrations::preparation::Policy {
        use crate::integrations::preparation::Policy;
        if policy != crate::scratch::Policy::Auto {
            return Policy::Unsupported("preparation omitted under forced scratch; upgrade uses its declared scratch variant");
        }
        if no_cache && self.definition.cache_cleanup.is_empty() {
            return Policy::Unsupported("cache-avoidance upgrades omit preparation cache writes");
        }
        if !self.definition.prepare.is_empty() { return Policy::BeforeStage; }
        if self.definition.adapter == "flatpak" && self.definition.upgrade == builtin::defaults("flatpak")
            && self.definition.selected_flags.is_empty() {
            Policy::BeforeStage
        } else { Policy::Unsupported("no download-only preparation configured") }
    }
    fn preparation_arguments(&self, update: &ExternalUpdate) -> io::Result<Vec<String>> {
        if !self.definition.prepare.is_empty() { return builtin::expand(&self.definition.prepare, update); }
        let mut arguments = builtin::expand(&builtin::defaults("flatpak"), update)?;
        // Exact approved ref/commit only: never deploy or implicitly expand
        // into related refs/dependency installations during preparation.
        arguments.splice(2..2, ["--no-deploy", "--no-deps", "--no-related", "--noninteractive"].map(str::to_string));
        Ok(arguments)
    }
    pub(super) fn new(name: String, mut definition: Definition) -> Self {
        if definition.upgrade.is_empty() { definition.upgrade = builtin::defaults(&definition.adapter); }
        if definition.adapter == "pipx" && definition.no_cache_args.is_empty() { definition.no_cache_args = vec!["--pip-args=--no-cache-dir".into()]; }
        Self { name, definition, approved: Mutex::new(Vec::new()), no_cache: AtomicBool::new(false) }
    }
    fn template(&self) -> &[String] {
        self.template_for(crate::scratch::policy())
    }
    fn template_for(&self, policy: crate::scratch::Policy) -> &[String] {
        match policy {
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
    fn option_previews(&self) -> Vec<String> {
        self.definition.selected_flags.iter().map(|name| {
            let flag = &self.definition.flags[name];
            let mut effects = Vec::new();
            if !flag.args.is_empty() { effects.push(format!("add {}", builtin::display(&flag.args))); }
            if !flag.command.is_empty() { effects.push(format!("after success run {}", builtin::display(&flag.command))); }
            format!("{} option {name}: {}", self.name, effects.join("; "))
        }).collect()
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
        self.run_as(arguments, crate::progress::integration_manager(&self.name), phase)
    }
    fn run_as(&self, arguments: &[String], manager: crate::progress::Manager, phase: &str) -> io::Result<bool> {
        if unsafe { libc::geteuid() } == 0 { return Err(io::Error::other("Start pacpin as a regular user; manager commands are unprivileged")); }
        let result = crate::progress::run(&mut self.command(arguments)?, manager, phase, || {});
        crate::pacman_config::invalidate();
        Ok(result?.success())
    }
}
impl IntegrationProvider for Provider {
    fn cache_usage(&self) -> Option<crate::pm::CacheUsage> {
        // Arbitrary commands/environments can change backend cache locations.
        // Neither installed Flatpak runtimes nor the Nix store are cache sizes.
        Some(crate::pm::CacheUsage::default())
    }
    fn preview_upgrade(&self, _updates: &[ExternalUpdate]) {
        // Custom flag effects still need explicit review; routine download
        // preparation belongs to the detailed dry-run, not approval chatter.
        for text in self.option_previews() {
            for line in crate::ui::wrap_field(&text, ":: ", "    ", crate::ui::get_terminal_width()) { println!("{line}"); }
        }
    }
    fn preparation_policy(&self) -> crate::integrations::preparation::Policy {
        self.preparation_policy_for(crate::scratch::policy(), self.no_cache.load(Ordering::SeqCst))
    }
    fn prefetch(&self, updates: &[ExternalUpdate]) -> io::Result<()> {
        use crate::integrations::preparation::Policy;
        if self.preparation_policy() != Policy::BeforeStage { return Err(io::Error::other("Preparation policy changed")); }
        let approved = self.approved.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for update in updates {
            if !approved.contains(update) { return Err(io::Error::other("Preparation target changed after approval")); }
            let arguments = self.preparation_arguments(update)?;
            crate::progress::capture_preparation(&mut self.command(&arguments)?, crate::progress::integration_manager(&self.name))?;
        }
        Ok(())
    }
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
    fn check_updates(&self) -> crate::discovery::Report<ExternalUpdate> {
        self.approved.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let result = (|| {
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
        })();
        crate::discovery::Report::updates(self.name(), result)
    }
    fn check_options(&self, policy: crate::scratch::Policy, no_cache: bool) -> Result<(), crate::runtime::OptionError> {
        use crate::runtime::OptionError;
        if self.template_for(policy).is_empty() { return Err(OptionError::Unsupported(format!("{} does not support {policy:?} scratch; declare a matching upgrade variant in managers.toml", self.name))); }
        if no_cache && self.definition.no_cache_args.is_empty() && self.definition.cache_cleanup.is_empty() {
            return Err(OptionError::Unsupported(format!("{} does not support --no-cache; declare no_cache_args or cache_cleanup in managers.toml. Unused installed packages/generations are not caches.", self.name)));
        }
        if policy != crate::scratch::Policy::Auto {
            let footprint = self.definition.scratch_mib.ok_or_else(|| OptionError::Invalid("Missing scratch_mib".into()))? * 1_048_576;
            crate::scratch::choose(policy, footprint, 128).map_err(OptionError::Invalid)?;
        }
        let prepare = if self.preparation_policy_for(policy, no_cache) == super::preparation::Policy::BeforeStage { &self.definition.prepare[..] } else { &[] };
        for command in [&self.definition.upgrade[..], self.template_for(policy), &self.definition.verify, &self.definition.cache_cleanup, prepare] {
            if !command.is_empty() && !crate::is_command_available(&command[0]) { return Err(OptionError::Invalid(format!("{} executable '{}' is unavailable", self.name, command[0]))); }
        }
        Ok(())
    }
    fn configure_no_cache(&self, no_cache: bool) { self.no_cache.store(no_cache, Ordering::SeqCst); }
    fn upgrade_actions(&self, updates: &[ExternalUpdate]) -> io::Result<Vec<UpgradeAction>> {
        let mut commands: Vec<_> = updates.iter().map(|u| self.arguments(u).map(|args| UpgradeAction::Command(builtin::display(&args)))).collect::<io::Result<_>>()?;
        for flag in &self.definition.selected_flags {
            let command = &self.definition.flags[flag].command;
            if !command.is_empty() { commands.push(UpgradeAction::Command(builtin::display(command))); }
        }
        Ok(commands)
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
        else { None }
    }
    fn execute_cache_cleanup(&self) -> io::Result<bool> {
        if self.definition.cache_cleanup.is_empty() { return Ok(true); }
        let result = self.run_as(&self.definition.cache_cleanup, crate::progress::Manager::Cache, &format!("Cleaning {} cache", self.name));
        if result.as_ref().is_ok_and(|success| *success) { crate::progress::completed(crate::progress::Manager::Cache); }
        else { crate::progress::failed(crate::progress::Manager::Cache); }
        result
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

#[cfg(test)]
mod preparation_tests {
    use super::*;
    use crate::integrations::preparation::Policy;
    fn provider(adapter: &str) -> Provider {
        Provider::new(adapter.into(), Definition { adapter: adapter.into(), ..Default::default() })
    }
    #[test]
    fn concise_review_still_exposes_custom_flag_arguments_and_commands() {
        let mut provider = provider("flatpak");
        assert!(provider.option_previews().is_empty());
        provider.definition.selected_flags.push("review".into());
        provider.definition.flags.insert("review".into(), crate::manager_config::UserFlag {
            args: vec!["--verbose".into()], command: vec!["tool".into(), "action".into()],
        });
        let text = provider.option_previews().join("\n");
        assert!(text.contains("option review: add '--verbose'"));
        assert!(text.contains("after success run 'tool' 'action'"));
    }
    #[test]
    fn disabling_cache_writes_does_not_invent_a_later_cleanup_step() {
        let mut provider = provider("pipx");
        assert!(!provider.definition.no_cache_args.is_empty());
        assert!(provider.cache_command_str().is_none());
        provider.definition.cache_cleanup = vec!["tool".into(), "cache".into()];
        assert_eq!(provider.cache_command_str().as_deref(), Some("'tool' 'cache'"));
    }
    #[test]
    fn flatpak_prepares_only_the_approved_installation_ref_and_commit() {
        let provider = provider("flatpak");
        assert_eq!(provider.preparation_policy_for(crate::scratch::Policy::Auto, false), Policy::BeforeStage);
        let update = ExternalUpdate { runner: "Flatpak".into(), id: "app/org.example.App/x86_64/stable".into(), name: "App".into(),
            repo: "flathub".into(), version: "2".into(), installed_version: Some("1".into()), revision: Some("0123456789abcdef".into()), scope: Some("--user".into()) };
        assert_eq!(provider.preparation_arguments(&update).unwrap(), ["flatpak", "update", "--no-deploy", "--no-deps", "--no-related", "--noninteractive", "-y", "--user", "--commit=0123456789abcdef", "--", "app/org.example.App/x86_64/stable"]);
    }
    #[test]
    fn forced_scratch_cache_avoidance_and_custom_variants_do_not_inherit_preparation() {
        let mut provider = provider("flatpak");
        for policy in [crate::scratch::Policy::Ram, crate::scratch::Policy::Storage] {
            assert!(matches!(provider.preparation_policy_for(policy, false), Policy::Unsupported(_)));
        }
        assert!(matches!(provider.preparation_policy_for(crate::scratch::Policy::Auto, true), Policy::Unsupported(_)));
        provider.definition.cache_cleanup = vec!["tool".into(), "cache".into()];
        assert_eq!(provider.preparation_policy_for(crate::scratch::Policy::Auto, true), Policy::BeforeStage);
        provider.definition.selected_flags.push("custom".into());
        assert!(matches!(provider.preparation_policy_for(crate::scratch::Policy::Auto, false), Policy::Unsupported(_)));
        provider.definition.prepare = vec!["tool".into(), "download".into(), "{id}".into()];
        assert_eq!(provider.preparation_policy_for(crate::scratch::Policy::Auto, false), Policy::BeforeStage);
        for adapter in ["nix", "pipx", "command"] {
            assert!(matches!(self::provider(adapter).preparation_policy_for(crate::scratch::Policy::Auto, false), Policy::Unsupported(_)));
        }
    }

    #[test]
    fn upgrade_actions_share_actual_argument_expansion_and_fail_closed() {
        let flatpak = provider("flatpak");
        let mut update = ExternalUpdate { id: "app/org.example.App/x86_64/stable".into(), version: "2".into(), scope: Some("--user".into()), ..Default::default() };
        assert!(flatpak.upgrade_actions(std::slice::from_ref(&update)).is_err());
        update.revision = Some("approved-commit".into());
        let actions = flatpak.upgrade_actions(std::slice::from_ref(&update)).unwrap();
        assert_eq!(actions, vec![UpgradeAction::Command(builtin::display(&flatpak.arguments(&update).unwrap()))]);
        let pipx = provider("pipx");
        pipx.configure_no_cache(true);
        let actions = pipx.upgrade_actions(&[ExternalUpdate { id: "environment".into(), ..Default::default() }]).unwrap();
        assert!(matches!(&actions[0], UpgradeAction::Command(command) if command.contains("--pip-args=--no-cache-dir")));
    }
}
