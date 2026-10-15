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

use std::collections::{BTreeMap, HashMap};
use std::io::{self, IsTerminal, Write};
use std::process::exit;

use colored::Colorize;

use crate::config::{mutate_config, Config};
use crate::db::AlpmManager;
use crate::repo_menu;
use crate::resolver::ResolverEngine;
use crate::ui::{print_banner, prompt_multiselect};

pub fn parse_pin_args(
    pin_args: &[String],
    known_repos: &[String],
) -> Result<(String, Vec<String>), String> {
    let mut file_patterns = Vec::new();
    let mut positional = Vec::new();
    let mut i = 0;
    while i < pin_args.len() {
        let arg = &pin_args[i];
        if arg == "-f" || arg == "--file" {
            if i + 1 >= pin_args.len() {
                return Err("'-f/--file' requires a file path.".to_string());
            }
            let file_path = &pin_args[i + 1];
            match std::fs::read_to_string(file_path) {
                Ok(content) => {
                    for line in content.lines() {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() && !trimmed.starts_with('#') {
                            file_patterns.push(trimmed.to_string());
                        }
                    }
                }
                Err(e) => {
                    return Err(format!(
                        "Error reading package list file '{}': {}",
                        file_path, e
                    ));
                }
            }
            i += 2;
        } else {
            positional.push(arg.clone());
            i += 1;
        }
    }

    if positional.is_empty() {
        Err("'pacpin pin' requires a repository name.".to_string())
    } else if positional.len() == 1 {
        if file_patterns.is_empty() {
            return Err(
                "'pacpin pin' requires both a repository and at least one package.".to_string(),
            );
        }
        Ok((positional[0].clone(), file_patterns))
    } else if positional.len() == 2 && file_patterns.is_empty() {
        if known_repos.iter().any(|r| r == &positional[1])
            && !known_repos.iter().any(|r| r == &positional[0])
        {
            Ok((positional[1].clone(), vec![positional[0].clone()]))
        } else {
            Ok((positional[0].clone(), vec![positional[1].clone()]))
        }
    } else {
        let (r, mut p) = if known_repos.iter().any(|r| r == &positional[0]) {
            (positional[0].clone(), positional[1..].to_vec())
        } else if known_repos.iter().any(|r| r == positional.last().unwrap()) {
            let last_idx = positional.len() - 1;
            (
                positional[last_idx].clone(),
                positional[..last_idx].to_vec(),
            )
        } else {
            (positional[0].clone(), positional[1..].to_vec())
        };
        p.extend(file_patterns);
        Ok((r, p))
    }
}

pub fn cmd_list(config: &Config) {
    print_banner();
    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    println!("\n{}", "Active Engine Features:".bold());
    println!(
        "  • Repository Pinning:      {}",
        if config.features.pinning {
            "Enabled".green()
        } else {
            "Disabled".dimmed()
        }
    );
    println!(
        "  • Vendor Stickiness:       {}",
        if config.features.vendor_stickiness {
            "Enabled (packages bound to originating repository)".blue()
        } else {
            "Disabled".dimmed()
        }
    );
    println!(
        "  • Smart Orphan Lifecycle:  {}",
        if config.features.smart_orphans {
            "Enabled".green()
        } else {
            "Disabled".dimmed()
        }
    );
    println!(
        "  • Stability Delay Buffers: {}",
        if config.features.stability_delays {
            "Enabled".green()
        } else {
            "Disabled".dimmed()
        }
    );
    let ext_status = if config.features.integrations {
        let mut enabled_exts = Vec::new();
        if config.integrations.flatpak {
            enabled_exts.push("Flatpak");
        }
        if config.integrations.nix {
            enabled_exts.push("Nix");
        }
        if config.integrations.pipx {
            enabled_exts.push("Pipx");
        }
        if enabled_exts.is_empty() {
            "Enabled (none active)".yellow().to_string()
        } else {
            format!("Enabled ({})", enabled_exts.join(", "))
                .green()
                .to_string()
        }
    } else {
        "Disabled".dimmed().to_string()
    };
    println!("  • External Integrations:   {}", ext_status);

    println!("\n{}", "Configured Repository Rules:".bold());

    if !config.repo_order.is_empty() {
        println!("  {}", "Repository Search Priority Order:".bold());
        for (i, r) in config.repo_order.iter().enumerate() {
            let prio = if i == 0 {
                "(Priority 1 - Highest)".green().bold().to_string()
            } else if i == config.repo_order.len() - 1 {
                format!("(Priority {} - Lowest)", i + 1)
                    .dimmed()
                    .to_string()
            } else {
                format!("(Priority {})", i + 1).cyan().to_string()
            };
            println!("    {:>2}. [{}] {}", i + 1, r.cyan(), prio);
        }
        println!();
    }

    if config.pins.is_empty() {
        println!("  {}", "No custom package pins configured.".yellow());
    } else {
        println!("  {}", "Custom Pins (Locked Targets):".bold());
        for (pat, repo) in &config.pins {
            println!("    • {:<26} ➔  [{}]", pat.cyan(), repo.green());
        }
    }

    if !config.exclude.is_empty() {
        println!("\n  {}", "Repository Exclusions:".bold());
        for (repo, pats) in &config.exclude {
            println!(
                "    • [{}] excludes ➔  {}",
                repo.yellow(),
                pats.join(", ").red()
            );
        }
    }

    if !config.delay.is_empty() {
        println!("\n  {}", "Stability Delay Rules:".bold());
        for (pkg, days) in &config.delay {
            println!("    • {:<26} ➔  {} days hold buffer", pkg.yellow(), days);
        }
    }

    let alpm = manager.handle();
    let local_pkgs = alpm.localdb().pkgs();
    let glob_pins = ResolverEngine::compile_glob_pins(&config.pins);
    let glob_delays = ResolverEngine::compile_glob_delays(&config.delay);
    let syncdb_map: HashMap<&str, &alpm::Db> =
        alpm.syncdbs().into_iter().map(|d| (d.name(), d)).collect();
    let installed_dbs = ResolverEngine::load_installed_dbs();

    let mut custom_matches = Vec::new();
    for p in local_pkgs {
        if let Some((_, target_repo, _)) =
            ResolverEngine::find_matching_pin_rule(p.name(), &config.pins, &glob_pins)
        {
            let inst_db = installed_dbs.get(p.name()).cloned().unwrap_or_default();
            let mut cand_info = None;
            if let Some(&db) = syncdb_map.get(target_repo.as_str()) {
                if let Ok(cand) = db.pkg(p.name()) {
                    cand_info = Some((target_repo.clone(), cand.version().to_string()));
                }
            }
            let delay_info = if config.features.stability_delays {
                ResolverEngine::match_delay_days(p.name(), &config.delay, &glob_delays)
            } else {
                None
            };
            custom_matches.push((
                p.name().to_string(),
                p.version().to_string(),
                inst_db,
                target_repo,
                cand_info,
                delay_info,
            ));
        }
    }
    custom_matches.sort_by(|a, b| a.0.cmp(&b.0));

    if !custom_matches.is_empty() {
        println!(
            "\n{}",
            format!(
                "Installed Packages with Custom Pins ({}):",
                custom_matches.len()
            )
            .bold()
        );
        for (name, installed_ver, inst_db, pinned_repo, cand_info, delay_info) in custom_matches {
            let mut status_str = if let Some((repo, version)) = cand_info {
                format!("➔ [{}] {}", repo, version)
            } else if config.sources.contains_key(&pinned_repo) {
                format!("➔ [{}] upstream release source", pinned_repo)
            } else {
                format!("➔ {}", format!("[{}] NOT FOUND", pinned_repo).red())
            };
            if let Some(days) = delay_info {
                status_str.push_str(&format!(
                    " {}",
                    format!("[DELAYED - {}d buffer]", days).yellow()
                ));
            }
            let inst_db_str = if !inst_db.is_empty() {
                format!(" (installed from [{}])", inst_db)
            } else {
                String::new()
            };
            println!(
                "  ✔ {:<28} : {}{} {}",
                name.bold(),
                installed_ver,
                inst_db_str,
                status_str
            );
        }
    }
    println!();
}

pub fn cmd_repos(config: Config) {
    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };
    let current_repos = manager.repos();
    if let Some(new_order) = repo_menu::run_repo_menu(current_repos) {
        if let Err(e) = mutate_config(|current| {
            current.repo_order = new_order.clone();
            Ok(())
        }) {
            eprintln!("{}", format!("Failed to save config: {}", e).red());
            exit(1);
        }
        print_banner();
        println!(
            "{}",
            format!(
                "✔ Successfully saved repository search priority order ({} repositories):",
                new_order.len()
            )
            .green()
            .bold()
        );
        for (i, r) in new_order.iter().enumerate() {
            let prio = if i == 0 {
                "(Priority 1 - Highest)".green().bold().to_string()
            } else if i == new_order.len() - 1 {
                format!("(Priority {} - Lowest)", i + 1)
                    .dimmed()
                    .to_string()
            } else {
                format!("(Priority {})", i + 1).cyan().to_string()
            };
            println!("  {:>2}. [{}] {}", i + 1, r.cyan(), prio);
        }
        println!();
    } else {
        println!("{}", "Repository order unchanged.".dimmed());
    }
}

pub fn cmd_pin(config: Config, repo: String, patterns: Vec<String>) {
    print_banner();

    let is_valid_repo_name = !repo.is_empty()
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !is_valid_repo_name {
        eprintln!(
            "{}",
            format!("Error: Invalid repository name '[{}]'.", repo)
                .red()
                .bold()
        );
        eprintln!("Repository names may only contain letters, numbers, hyphens, and underscores.");
        exit(1);
    }

    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    let mut known_repos = manager.repos().to_vec();
    known_repos.push("aur".to_string());
    known_repos.extend(config.sources.keys().cloned());
    if !known_repos.iter().any(|r| r == &repo) {
        eprintln!(
            "{}",
            format!("Error: Repository '[{}]' is not recognized.", repo)
                .red()
                .bold()
        );
        eprintln!(
            "Available repositories on your system: {}",
            known_repos.join(", ")
        );
        exit(1);
    }

    if patterns.is_empty() {
        eprintln!(
            "{}",
            "Error: No packages or patterns specified to pin."
                .red()
                .bold()
        );
        exit(1);
    }

    for pattern in &patterns {
        if let Some(source) = config.sources.get(&repo) {
            if !source.packages.contains_key(pattern) {
                eprintln!("Error: Source pins require an exact configured package recipe: {repo}/{pattern}");
                exit(1);
            }
        }
        if !crate::utils::is_safe_pattern(pattern) || glob::Pattern::new(pattern).is_err() {
            eprintln!(
                "{}",
                format!("Error: Invalid package pattern '{}'. Patterns must be valid glob expressions without shell metacharacters or whitespace.", pattern).red().bold()
            );
            exit(1);
        }
    }

    let mut to_pin = patterns.clone();
    let mut all_companions = Vec::new();
    let existing_set: std::collections::HashSet<String> = to_pin.iter().cloned().collect();
    for pat in &patterns {
        if config.sources.contains_key(&repo) { continue; }
        if !pat.contains('*') && !pat.contains('?') && !pat.contains('[') {
            let companions = manager.find_companions(pat, &repo, &config.pins);
            for c in companions {
                if !existing_set.contains(&c) && !all_companions.contains(&c) {
                    all_companions.push(c);
                }
            }
        }
    }
    if !all_companions.is_empty() {
        let title = if patterns.len() == 1 {
            format!(
                "'{}' has companion packages in [{}] to avoid version mismatches",
                patterns[0], repo
            )
        } else {
            format!(
                "Target packages have companion packages in [{}] to avoid version mismatches",
                repo
            )
        };
        let selected = prompt_multiselect(&title, &repo, &all_companions);
        to_pin.extend(selected);
    }

    if let Err(e) = mutate_config(|current| {
        for p in &to_pin {
            current.pins.insert(p.clone(), repo.clone());
        }
        Ok(())
    }) {
        eprintln!("{}", format!("Failed to save config: {}", e).red());
        exit(1);
    }

    if to_pin.len() == 1 {
        println!(
            "{}",
            format!(
                "✔ Successfully pinned '{}' to repository [{}].",
                to_pin[0], repo
            )
            .green()
        );
    } else {
        println!(
            "{}",
            format!(
                "✔ Successfully pinned {} package(s) to [{}]:",
                to_pin.len(),
                repo
            )
            .green()
        );
        for c in &to_pin {
            println!("    • {:<26} ➔  [{}]", c.cyan(), repo.green());
        }
    }
}

pub fn cmd_unpin(pattern: &str) {
    print_banner();
    let removed = match mutate_config(|current| Ok(current.pins.remove(pattern).is_some())) {
        Ok(removed) => removed,
        Err(e) => {
            eprintln!("{}", format!("Failed to save config: {}", e).red());
            exit(1);
        }
    };
    if removed {
        println!("{}", format!("✔ Unpinned '{}'.", pattern).green());
    } else {
        println!(
            "{}",
            format!("Pattern '{}' was not pinned.", pattern).yellow()
        );
    }
}

pub fn find_matching_pin(
    pkg: &str,
    pins: &BTreeMap<String, String>,
) -> Option<(String, String, bool)> {
    let glob_pins = ResolverEngine::compile_glob_pins(pins);
    ResolverEngine::find_matching_pin_rule(pkg, pins, &glob_pins)
}

pub(crate) fn check_and_prompt_smart_unpin(mut config: Config, removed_pkgs: &[String], noconfirm: bool) {
    let mut removed_pins = BTreeMap::new();
    let mut removed_delays = BTreeMap::new();
    let interactive = io::stdin().is_terminal();

    for pkg in removed_pkgs {
        if let Some((pattern, repo, is_exact)) = find_matching_pin(pkg, &config.pins) {
            let should_remove = if noconfirm {
                // A non-interactive removal may clean up a direct pin, but a
                // wildcard rule can apply to many packages and must survive.
                is_exact
            } else if interactive {
                if is_exact {
                    print!(
                        "\n:: Package '{}' is pinned to repository [{}]. Remove this pin rule from config? [Y/n] ",
                        pkg.cyan().bold(),
                        repo.green()
                    );
                } else {
                    print!(
                        "\n:: Package '{}' matched wildcard pin '{}' ➔ [{}]. Remove this pin rule from config? [Y/n] ",
                        pkg.cyan().bold(),
                        pattern.yellow(),
                        repo.green()
                    );
                }
                io::stdout().flush().unwrap();
                let mut input = String::new();
                if io::stdin().read_line(&mut input).is_ok() {
                    let trimmed = input.trim().to_lowercase();
                    trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
                } else {
                    false
                }
            } else {
                false
            };

            if should_remove {
                config.pins.remove(&pattern);
                println!(
                    "{}",
                    format!("✔ Removed pin rule '{}' ➔ [{}].", pattern, repo).green()
                );
                removed_pins.insert(pattern, repo);
            }
        }

        if let Some(days) = config.delay.get(pkg).copied() {
            let should_remove = if noconfirm {
                true
            } else if interactive {
                print!(
                    "\n:: Package '{}' has an active {}-day stability delay. Remove this delay rule? [Y/n] ",
                    pkg.cyan().bold(),
                    days
                );
                io::stdout().flush().unwrap();
                let mut input = String::new();
                if io::stdin().read_line(&mut input).is_ok() {
                    let trimmed = input.trim().to_lowercase();
                    trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
                } else {
                    false
                }
            } else {
                false
            };

            if should_remove {
                config.delay.remove(pkg);
                println!(
                    "{}",
                    format!("✔ Removed {}-day stability delay on '{}'.", days, pkg).green()
                );
                removed_delays.insert(pkg.clone(), days);
            }
        }
    }

    if !removed_pins.is_empty() || !removed_delays.is_empty() {
        if let Err(e) = mutate_config(|current| {
            // A concurrently changed rule was not the one the user approved removing.
            for (pattern, repo) in &removed_pins {
                if current.pins.get(pattern) == Some(repo) {
                    current.pins.remove(pattern);
                }
            }
            for (pkg, days) in &removed_delays {
                if current.delay.get(pkg) == Some(days) {
                    current.delay.remove(pkg);
                }
            }
            Ok(())
        }) {
            eprintln!(
                "{}",
                format!("Warning: Failed to save updated config: {}", e).yellow()
            );
        } else {
            println!("{}", ":: Configuration updated successfully.".dimmed());
        }
    }
}
