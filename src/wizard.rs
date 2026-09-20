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

use crate::config::{get_config_path, save_config, Config, Features, Options};
use crate::ui::print_banner;
use colored::Colorize;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::process::Command;

fn prompt_yn(prompt_text: &str, default_yes: bool) -> bool {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    print!("  {} {} ", prompt_text.bold(), hint.cyan());
    io::stdout().flush().unwrap();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return default_yes;
    }
    let trimmed = input.trim().to_lowercase();
    if trimmed.is_empty() {
        default_yes
    } else if trimmed == "y" || trimmed == "yes" {
        true
    } else if trimmed == "n" || trimmed == "no" {
        false
    } else {
        default_yes
    }
}

fn prompt_input(prompt_text: &str, default_val: &str) -> String {
    print!("  {} (default: {}) : ", prompt_text.bold(), default_val.cyan());
    io::stdout().flush().unwrap();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return default_val.to_string();
    }
    let trimmed = input.trim();
    if trimmed.is_empty() {
        default_val.to_string()
    } else {
        trimmed.to_string()
    }
}

fn detect_aur_helper() -> Option<String> {
    for helper in &["paru", "yay", "pikaur", "trizen"] {
        if Command::new("which").arg(helper).output().map(|o| o.status.success()).unwrap_or(false) {
            return Some(helper.to_string());
        }
    }
    None
}

fn has_cachyos_repos() -> bool {
    if let Ok(content) = std::fs::read_to_string("/etc/pacman.conf") {
        content.contains("[cachyos]") || content.contains("[cachyos-v3]") || content.contains("[cachyos-core-v3]")
    } else {
        false
    }
}

pub fn run_first_launch_wizard(force: bool, reset: bool) -> Option<Config> {
    print_banner();

    let config_path = get_config_path();
    let existing_cfg = if config_path.exists() && !reset {
        Some(crate::config::load_config())
    } else {
        None
    };

    if reset && config_path.exists() {
        println!("{}", ":: Resetting configuration and launching setup from scratch...\n".yellow().bold());
    } else if existing_cfg.is_some() && !force {
        println!("{}", ":: Existing configuration found at ~/.config/pacpin/config.toml.".yellow());
        if !prompt_yn("Run setup wizard and reconfigure?", false) {
            println!("Configuration unchanged.");
            return None;
        }
        println!();
    }

    println!("{}", ":: Welcome to pacpin! Let's tailor your package management preferences.\n".cyan().bold());

    let (mut pins, mut exclude, mut delays, mut repo_order) = if let Some(ref cfg) = existing_cfg {
        if !cfg.pins.is_empty() || !cfg.delay.is_empty() || !cfg.repo_order.is_empty() {
            println!(
                "  {}",
                format!(
                    "ℹ Preserving {} existing pin(s), {} exclusion rule(s), {} delay rule(s), and custom repo order.",
                    cfg.pins.len(),
                    cfg.exclude.len(),
                    cfg.delay.len()
                )
                .cyan()
            );
        }
        (cfg.pins.clone(), cfg.exclude.clone(), cfg.delay.clone(), cfg.repo_order.clone())
    } else {
        (BTreeMap::new(), BTreeMap::new(), BTreeMap::new(), Vec::new())
    };

    // 1. Repository Pinning & Shielding
    println!("{}", "[1/5] Repository Pinning & Shielding:".bold());
    println!("      Lock packages or wildcards to specific repos (e.g. core, cachyos, extra)");
    println!("      to prevent unwanted upstream overrides and prioritize curated repositories.");
    let enable_pinning = prompt_yn("Enable Repository Pinning?", true);
    println!();

    if enable_pinning {
        let discovered_repos = crate::db::AlpmManager::resolve_repo_order(&repo_order);
        println!(
            "  {}",
            format!(
                "ℹ Detected {} repositories on your system:",
                discovered_repos.len()
            )
            .cyan()
        );
        for (i, r) in discovered_repos.iter().enumerate() {
            print!("    {}. [{}]", i + 1, r);
            if (i + 1) % 4 == 0 || i == discovered_repos.len() - 1 {
                println!();
            } else {
                print!("  ");
            }
        }
        println!();

        if has_cachyos_repos() {
            println!("  {}", "ℹ Detected CachyOS repositories on your system.".cyan());
            if prompt_yn("Add baseline protection pins (amd-ucode, intel-ucode, linux-firmware* to core)?", true) {
                pins.insert("amd-ucode".to_string(), "core".to_string());
                pins.insert("intel-ucode".to_string(), "core".to_string());
                pins.insert("linux-firmware*".to_string(), "core".to_string());
                exclude.insert("cachyos".to_string(), vec!["linux-firmware*".to_string()]);
                println!("  ✔ Added core microcode and firmware protection pins.\n");
            } else {
                println!();
            }
        }

        if prompt_yn("Customize repository search priority order or add repos?", false) {
            if let Some(new_order) = crate::repo_menu::run_repo_menu(&discovered_repos) {
                repo_order = new_order;
                println!("  ✔ Saved custom repository search order.\n");
            }
        }
    }

    // 2. Stability Delay Buffer
    println!("{}", "[2/5] Stability Delay Buffer:".bold());
    println!("      Hold back bleeding-edge updates (e.g., Linux kernel, mesa, nvidia)");
    println!("      for a designated buffer period (in days) to avoid day-0 upstream regressions.");
    let enable_delays = prompt_yn("Enable Stability Delay Buffer?", false);
    if enable_delays {
        let default_days_str = prompt_input("Default buffer period for kernel/drivers in days", "3");
        let days = default_days_str.parse::<u32>().unwrap_or(3);
        if prompt_yn("Apply default delay buffer to 'linux' kernel packages?", true) {
            delays.insert("linux".to_string(), days);
            delays.insert("linux-cachyos".to_string(), days);
            println!("  ✔ Set {}-day stability buffer on kernel packages.\n", days);
        } else {
            println!();
        }
    } else {
        println!();
    }

    // 3. Smart Orphan Lifecycle Management
    println!("{}", "[3/5] Smart Orphan Lifecycle Management:".bold());
    println!("      Distinguish pure useless dependencies from active optional plugins");
    println!("      (like LADSPA for ffmpeg), and only prompt when dependencies actually change.");
    let enable_orphans = prompt_yn("Enable Smart Orphan Management?", true);
    println!();

    // 4. AUR Integration
    println!("{}", "[4/5] AUR Helper Integration:".bold());
    let detected_helper = detect_aur_helper();
    let helper = if let Some(ref h) = detected_helper {
        println!("      Detected installed AUR helper: '{}'", h.green().bold());
        if prompt_yn(&format!("Use '{}' for AUR packages?", h), true) {
            h.clone()
        } else {
            prompt_input("Enter your preferred AUR helper command", "paru")
        }
    } else {
        println!("      No standard AUR helper detected (paru/yay).");
        prompt_input("Enter your preferred AUR helper command", "paru")
    };
    println!();

    // 5. External Package Manager Integrations
    println!("{}", "[5/5] External Package Manager Integrations:".bold());
    println!("      Automatically check and unify updates for additional package managers.");
    let has_flatpak = Command::new("flatpak").arg("--version").output().is_ok();
    let has_nix = Command::new("nix").arg("--version").output().is_ok();

    let mut enable_flatpak = false;
    let mut enable_nix = false;

    if has_flatpak || has_nix {
        if has_flatpak {
            enable_flatpak = prompt_yn("  • Enable Flatpak integration (flathub updates & cleanup)?", false);
        }
        if has_nix {
            enable_nix = prompt_yn("  • Enable Nix integration (profile updates & garbage collection)?", false);
        }
    } else {
        println!("      No external package managers detected (Flatpak/Nix).");
    }
    println!();

    let enable_integrations = enable_flatpak || enable_nix;

    let config = Config {
        features: Features {
            pinning: enable_pinning,
            stability_delays: enable_delays,
            smart_orphans: enable_orphans,
            integrations: enable_integrations,
        },
        options: Options { helper },
        repo_order,
        pins,
        exclude,
        delay: delays,
        integrations: crate::config::IntegrationsConfig {
            flatpak: enable_flatpak,
            nix: enable_nix,
        },
    };

    if let Err(e) = save_config(&config) {
        eprintln!("{}", format!("Error saving configuration: {}", e).red());
        return None;
    }

    crate::alias::AliasManager::setup_aliases();
    crate::alias::AliasManager::mark_notified();

    println!("{}", "✔ Configuration saved successfully!".green().bold());
    println!("  Path: {}", config_path.display().to_string().cyan());
    println!("\nYou're all set! Try running:");
    println!("  • {} to verify your repository setup", "pin check".bold().cyan());
    println!("  • {} to perform a full system upgrade", "pin -Syu".bold().cyan());
    println!("  • {} to manage or inspect orphaned packages", "pin orphans".bold().cyan());
    println!("  {}", "(Aliases for 'pin' and 'pacpin' have been configured in your shell)\n".dimmed());

    Some(config)
}
