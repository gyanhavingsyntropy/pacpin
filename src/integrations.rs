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
use std::thread;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalUpdate {
    pub runner: String,     // e.g. "Flatpak", "Nix"
    pub id: String,         // e.g. "io.mrarm.mcpelauncher"
    pub name: String,       // e.g. "Minecraft Bedrock Launcher"
    pub repo: String,       // e.g. "flathub", "nixpkgs"
    pub version: String,    // e.g. "v1.8.5"
}

pub trait IntegrationProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn is_available(&self) -> bool;
    fn check_updates(&self) -> Vec<ExternalUpdate>;
    fn upgrade_command_str(&self) -> String;
    fn execute_upgrade(&self) -> Result<bool, std::io::Error>;
    fn clean_command_str(&self) -> String;
    fn execute_clean(&self) -> Result<bool, std::io::Error>;
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
                                name: if name.is_empty() { parts[1].to_string() } else { name },
                                repo: if origin.is_empty() { "flathub".to_string() } else { origin },
                                version: if ver.is_empty() { "update".to_string() } else { ver },
                            });
                        }
                    }
                }
            }
        }
        results
    }

    fn upgrade_command_str(&self) -> String {
        "flatpak update -y".to_string()
    }

    fn execute_upgrade(&self) -> Result<bool, std::io::Error> {
        let status = Command::new("flatpak")
            .args(&["update", "-y"])
            .status()?;
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

    fn check_updates(&self) -> Vec<ExternalUpdate> {
        // Check outdated packages via nix-env or nix profile
        let output = Command::new("nix-env")
            .args(&["-q", "--outdated"])
            .output();

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

    fn upgrade_command_str(&self) -> String {
        "nix profile upgrade '.*'".to_string()
    }

    fn execute_upgrade(&self) -> Result<bool, std::io::Error> {
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
        let status = Command::new("nix-collect-garbage")
            .arg("-d")
            .status()?;
        Ok(status.success())
    }
}

pub struct IntegrationsManager;

impl IntegrationsManager {
    pub fn get_active_providers(config: &Config) -> Vec<Box<dyn IntegrationProvider>> {
        if !config.features.integrations {
            return Vec::new();
        }

        let mut providers: Vec<Box<dyn IntegrationProvider>> = Vec::new();

        if config.integrations.flatpak {
            let fp = FlatpakProvider;
            if fp.is_available() {
                providers.push(Box::new(fp));
            }
        }

        if config.integrations.nix {
            let nix = NixProvider;
            if nix.is_available() {
                providers.push(Box::new(nix));
            }
        }

        providers
    }

    pub fn check_updates_parallel(providers: &[Box<dyn IntegrationProvider>]) -> Vec<ExternalUpdate> {
        if providers.is_empty() {
            return Vec::new();
        }

        let mut handles = Vec::new();

        for p in providers {
            let name = p.name();
            if name == "Flatpak" {
                handles.push(thread::spawn(|| FlatpakProvider.check_updates()));
            } else if name == "Nix" {
                handles.push(thread::spawn(|| NixProvider.check_updates()));
            }
        }

        let mut all_updates = Vec::new();
        for h in handles {
            if let Ok(mut updates) = h.join() {
                all_updates.append(&mut updates);
            }
        }

        all_updates
    }

    pub fn execute_upgrades(providers: &[Box<dyn IntegrationProvider>]) {
        for p in providers {
            println!(
                "\n{} Upgrading {} packages ({})...",
                "::".cyan(),
                p.name().bold(),
                p.upgrade_command_str().dimmed()
            );
            match p.execute_upgrade() {
                Ok(true) => {
                    println!("✔ {} upgrade completed successfully.", p.name().green());
                }
                Ok(false) => {
                    eprintln!("{}", format!("Warning: {} upgrade exited with errors.", p.name()).yellow());
                }
                Err(e) => {
                    eprintln!("{}", format!("Failed to run {} upgrade: {}", p.name(), e).red());
                }
            }
        }
    }

    pub fn execute_cleanups(providers: &[Box<dyn IntegrationProvider>]) {
        for p in providers {
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
                    eprintln!("{}", format!("Warning: {} cleanup exited with errors.", p.name()).yellow());
                }
                Err(e) => {
                    eprintln!("{}", format!("Failed to run {} cleanup: {}", p.name(), e).red());
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
}
