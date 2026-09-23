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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalUpdate {
    pub runner: String,  // e.g. "Flatpak", "Nix"
    pub id: String,      // e.g. "io.mrarm.mcpelauncher"
    pub name: String,    // e.g. "Minecraft Bedrock Launcher"
    pub repo: String,    // e.g. "flathub", "nixpkgs"
    pub version: String, // e.g. "v1.8.5"
}

pub trait IntegrationProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn is_available(&self) -> bool;
    fn refresh_metadata(&self) -> Result<(), std::io::Error>;
    fn check_updates(&self) -> Vec<ExternalUpdate>;
    fn upgrade_command_str(&self, updates: &[ExternalUpdate]) -> String;
    fn execute_upgrade(&self, updates: &[ExternalUpdate]) -> Result<bool, std::io::Error>;
    fn clean_command_str(&self) -> String;
    fn execute_clean(&self) -> Result<bool, std::io::Error>;
    fn supports_clean(&self) -> bool {
        true
    }
}

pub struct FlatpakProvider;

impl IntegrationProvider for FlatpakProvider {
    fn name(&self) -> &'static str {
        "Flatpak"
    }

    fn is_available(&self) -> bool {
        Command::new("flatpak")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn refresh_metadata(&self) -> Result<(), std::io::Error> {
        let _ = Command::new("flatpak")
            .args(&["update", "--appstream"])
            .output()?;
        Ok(())
    }

    fn check_updates(&self) -> Vec<ExternalUpdate> {
        let output = Command::new("flatpak")
            .args(&[
                "remote-ls",
                "--updates",
                "--columns=name,application,version,origin",
            ])
            .output();

        let mut results = Vec::new();
        if let Ok(out) = output {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                for line in stdout.lines() {
                    let parts: Vec<&str> = line.split('\t').collect();
                    if parts.len() >= 4 {
                        let name = parts[0].trim().to_string();
                        let app_id = parts[1].trim().to_string();
                        let ver = parts[2].trim().to_string();
                        let origin = parts[3].trim().to_string();

                        if !app_id.is_empty() {
                            results.push(ExternalUpdate {
                                runner: "Flatpak".to_string(),
                                id: app_id,
                                name: if name.is_empty() {
                                    parts[1].to_string()
                                } else {
                                    name
                                },
                                repo: if origin.is_empty() {
                                    "flathub".to_string()
                                } else {
                                    origin
                                },
                                version: if ver.is_empty() {
                                    "update".to_string()
                                } else {
                                    ver
                                },
                            });
                        }
                    }
                }
            }
        }
        results
    }

    fn upgrade_command_str(&self, updates: &[ExternalUpdate]) -> String {
        if updates.is_empty() {
            "flatpak update -y".to_string()
        } else {
            let ids: Vec<&str> = updates.iter().map(|u| u.id.as_str()).collect();
            format!("flatpak update -y {}", ids.join(" "))
        }
    }

    fn execute_upgrade(&self, updates: &[ExternalUpdate]) -> Result<bool, std::io::Error> {
        let mut cmd = Command::new("flatpak");
        cmd.arg("update").arg("-y");
        for u in updates {
            cmd.arg(&u.id);
        }
        let status = cmd.status()?;
        Ok(status.success())
    }

    fn clean_command_str(&self) -> String {
        "flatpak uninstall --unused -y".to_string()
    }

    fn execute_clean(&self) -> Result<bool, std::io::Error> {
        let status = Command::new("flatpak")
            .args(&["uninstall", "--unused", "-y"])
            .status()?;
        Ok(status.success())
    }
}

pub struct NixProvider;

impl IntegrationProvider for NixProvider {
    fn name(&self) -> &'static str {
        "Nix"
    }

    fn is_available(&self) -> bool {
        Command::new("nix")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn refresh_metadata(&self) -> Result<(), std::io::Error> {
        if Command::new("nix-channel")
            .arg("--version")
            .output()
            .is_ok()
        {
            let _ = Command::new("nix-channel").arg("--update").output()?;
        }
        Ok(())
    }

    fn check_updates(&self) -> Vec<ExternalUpdate> {
        // Check outdated packages via nix-env or nix profile
        let output = Command::new("nix-env").args(&["-q", "--outdated"]).output();

        let mut results = Vec::new();
        if let Ok(out) = output {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                for line in stdout.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        results.push(ExternalUpdate {
                            runner: "Nix".to_string(),
                            id: trimmed.to_string(),
                            name: trimmed.to_string(),
                            repo: "nixpkgs".to_string(),
                            version: "latest".to_string(),
                        });
                    }
                }
            }
        }
        results
    }

    fn upgrade_command_str(&self, _updates: &[ExternalUpdate]) -> String {
        "nix profile upgrade '.*'".to_string()
    }

    fn execute_upgrade(&self, _updates: &[ExternalUpdate]) -> Result<bool, std::io::Error> {
        let status = Command::new("nix")
            .args(&["profile", "upgrade", ".*"])
            .status();

        match status {
            Ok(s) if s.success() => Ok(true),
            _ => {
                // Fallback to nix-env if profile is not used
                let env_status = Command::new("nix-env").arg("-u").status()?;
                Ok(env_status.success())
            }
        }
    }

    fn clean_command_str(&self) -> String {
        "nix-collect-garbage -d".to_string()
    }

    fn execute_clean(&self) -> Result<bool, std::io::Error> {
        let status = Command::new("nix-collect-garbage").arg("-d").status()?;
        Ok(status.success())
    }
}

pub struct PipxProvider;

impl IntegrationProvider for PipxProvider {
    fn name(&self) -> &'static str {
        "Pipx"
    }

    fn is_available(&self) -> bool {
        Command::new("pipx")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn refresh_metadata(&self) -> Result<(), std::io::Error> {
        Ok(())
    }

    fn check_updates(&self) -> Vec<ExternalUpdate> {
        let output = Command::new("pipx").args(&["list", "--json"]).output();

        let mut results = Vec::new();
        if let Ok(out) = output {
            if out.status.success() {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    if let Some(venvs) = v.get("venvs").and_then(|v| v.as_object()) {
                        let mut targets = Vec::new();
                        for (app_name, info) in venvs {
                            let installed_ver = info
                                .get("metadata")
                                .and_then(|m| m.get("main_package"))
                                .and_then(|p| p.get("package_version"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            if !installed_ver.is_empty() {
                                targets.push((app_name.clone(), installed_ver.to_string()));
                            }
                        }

                        let agent = ureq::AgentBuilder::new()
                            .timeout(std::time::Duration::from_secs(3))
                            .user_agent(
                                "pacpin/3.1 (GPLv3; +https://github.com/gyanhavingsyntropy/pacpin)",
                            )
                            .build();

                        let arc_targets = std::sync::Arc::new(targets);
                        let num_workers = 4.min(arc_targets.len().max(1));
                        let mut handles = Vec::new();

                        for worker_idx in 0..num_workers {
                            let targets_clone = std::sync::Arc::clone(&arc_targets);
                            let agent_clone = agent.clone();
                            handles.push(std::thread::spawn(move || {
                                let mut worker_results = Vec::new();
                                for (i, (app_name, installed_ver)) in
                                    targets_clone.iter().enumerate()
                                {
                                    if i % num_workers == worker_idx {
                                        let pypi_url =
                                            format!("https://pypi.org/pypi/{}/json", app_name);
                                        if let Ok(resp) = agent_clone.get(&pypi_url).call() {
                                            if resp.status() == 200 {
                                                if let Ok(json) =
                                                    resp.into_json::<serde_json::Value>()
                                                {
                                                    if let Some(latest_ver) = json
                                                        .get("info")
                                                        .and_then(|info| info.get("version"))
                                                        .and_then(|v| v.as_str())
                                                    {
                                                        if alpm::vercmp(latest_ver, installed_ver)
                                                            == std::cmp::Ordering::Greater
                                                        {
                                                            worker_results.push(ExternalUpdate {
                                                                runner: "Pipx".to_string(),
                                                                id: app_name.clone(),
                                                                name: app_name.clone(),
                                                                repo: "pypi".to_string(),
                                                                version: latest_ver.to_string(),
                                                            });
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                worker_results
                            }));
                        }

                        for h in handles {
                            if let Ok(updates) = h.join() {
                                results.extend(updates);
                            }
                        }
                    }
                }
            }
        }
        results
    }

    fn upgrade_command_str(&self, _updates: &[ExternalUpdate]) -> String {
        "pipx upgrade-all".to_string()
    }

    fn execute_upgrade(&self, _updates: &[ExternalUpdate]) -> Result<bool, std::io::Error> {
        let status = Command::new("pipx").arg("upgrade-all").status()?;
        Ok(status.success())
    }

    fn clean_command_str(&self) -> String {
        "pipx list".to_string()
    }

    fn execute_clean(&self) -> Result<bool, std::io::Error> {
        Ok(true)
    }

    fn supports_clean(&self) -> bool {
        false
    }
}

pub struct IntegrationsManager;

impl IntegrationsManager {
    pub fn get_active_providers(config: &Config) -> Vec<Arc<dyn IntegrationProvider>> {
        if !config.features.integrations {
            return Vec::new();
        }

        let mut providers: Vec<Arc<dyn IntegrationProvider>> = Vec::new();

        if config.integrations.flatpak {
            let fp = FlatpakProvider;
            if fp.is_available() {
                providers.push(Arc::new(fp));
            }
        }

        if config.integrations.nix {
            let nix = NixProvider;
            if nix.is_available() {
                providers.push(Arc::new(nix));
            }
        }

        if config.integrations.pipx {
            let pipx = PipxProvider;
            if pipx.is_available() {
                providers.push(Arc::new(pipx));
            }
        }

        providers
    }

    pub fn refresh_all_parallel(providers: &[Arc<dyn IntegrationProvider>]) {
        if providers.is_empty() {
            return;
        }

        println!(
            "{}",
            ":: Refreshing external package manager metadata...".cyan()
        );
        let mut handles = Vec::new();

        for p in providers {
            let provider = Arc::clone(p);
            handles.push(thread::spawn(move || provider.refresh_metadata()));
        }

        for h in handles {
            let _ = h.join();
        }
    }

    pub fn check_updates_parallel(
        providers: &[Arc<dyn IntegrationProvider>],
    ) -> Vec<ExternalUpdate> {
        if providers.is_empty() {
            return Vec::new();
        }

        let mut handles = Vec::new();

        for p in providers {
            let provider = Arc::clone(p);
            handles.push(thread::spawn(move || provider.check_updates()));
        }

        let mut all_updates = Vec::new();
        for h in handles {
            if let Ok(updates) = h.join() {
                all_updates.extend(updates);
            }
        }

        all_updates
    }

    pub fn execute_upgrades(
        providers: &[Arc<dyn IntegrationProvider>],
        external_updates: &[ExternalUpdate],
    ) -> Vec<(String, String, bool)> {
        let mut outcomes = Vec::new();
        for p in providers {
            let p_updates: Vec<ExternalUpdate> = external_updates
                .iter()
                .filter(|u| u.runner == p.name())
                .cloned()
                .collect();

            if p_updates.is_empty() {
                continue;
            }

            let cmd_str = p.upgrade_command_str(&p_updates);
            println!(
                "\n{} Upgrading {} ({} packages: {})...",
                "::".cyan(),
                p.name().bold(),
                p_updates.len(),
                cmd_str.dimmed()
            );
            match p.execute_upgrade(&p_updates) {
                Ok(true) => {
                    println!("✔ {} upgrade completed successfully.", p.name().green());
                    outcomes.push((p.name().to_string(), cmd_str, true));
                }
                Ok(false) => {
                    eprintln!(
                        "{}",
                        format!("Warning: {} upgrade exited with errors.", p.name()).yellow()
                    );
                    outcomes.push((p.name().to_string(), cmd_str, false));
                }
                Err(e) => {
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

    pub fn execute_cleanups(providers: &[Arc<dyn IntegrationProvider>]) {
        for p in providers {
            if !p.supports_clean() {
                continue;
            }
            println!(
                "\n{} Cleaning {} unused packages ({})...",
                "::".cyan(),
                p.name().bold(),
                p.clean_command_str().dimmed()
            );
            match p.execute_clean() {
                Ok(true) => {
                    println!("✔ {} cleanup completed successfully.", p.name().green());
                }
                Ok(false) => {
                    eprintln!(
                        "{}",
                        format!("Warning: {} cleanup exited with errors.", p.name()).yellow()
                    );
                }
                Err(e) => {
                    eprintln!(
                        "{}",
                        format!("Failed to run {} cleanup: {}", p.name(), e).red()
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_external_update_struct() {
        let update = ExternalUpdate {
            runner: "Flatpak".to_string(),
            id: "org.mozilla.firefox".to_string(),
            name: "Firefox".to_string(),
            repo: "flathub".to_string(),
            version: "130.0".to_string(),
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
        let flatpak = FlatpakProvider;
        let nix = NixProvider;
        let pipx = PipxProvider;
        assert!(flatpak.supports_clean());
        assert!(nix.supports_clean());
        assert!(!pipx.supports_clean());
    }
}
