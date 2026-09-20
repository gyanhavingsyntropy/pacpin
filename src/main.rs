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

mod alias;
mod aur;
mod config;
mod db;
mod integrations;
mod journal;
mod orphans;
mod resolver;
mod ui;
mod wizard;

use colored::Colorize;
use config::{load_config, save_config, Config};
use db::AlpmManager;
use integrations::IntegrationsManager;
use journal::TransactionJournal;
use orphans::OrphanManager;
use resolver::ResolverEngine;
use glob::Pattern;
use std::collections::BTreeMap;
use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::{exit, Command};
use ui::{print_banner, prompt_multiselect, render_transaction_view};

fn check_pacman_lock() {
    if Path::new("/var/lib/pacman/db.lck").exists() {
        eprintln!(
            "{}",
            "Error: Pacman database is locked (/var/lib/pacman/db.lck).".red()
        );
        eprintln!("Another package management process is currently running. Exiting.");
        exit(1);
    }
}

fn cmd_check(config: &Config) {
    print_banner();
    let manager = match AlpmManager::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };
    let resolver = ResolverEngine::new(&manager);

    let alpm = manager.handle();
    let local_pkgs = alpm.localdb().pkgs();

    let mut custom_pkgs = Vec::new();
    for p in local_pkgs {
        if let Some(target_repo) = ResolverEngine::is_pinned(p.name(), &config.pins) {
            let mut cand_ver = "pinned".to_string();
            if let Some(db) = alpm.syncdbs().into_iter().find(|d| d.name() == target_repo) {
                if let Ok(cand) = db.pkg(p.name()) {
                    cand_ver = cand.version().to_string();
                }
            }
            custom_pkgs.push((p.name().to_string(), target_repo, cand_ver));
        }
    }
    custom_pkgs.sort_by(|a, b| a.0.cmp(&b.0));

    if !custom_pkgs.is_empty() {
        println!(
            "\n{}",
            format!("Active Custom Pins ({} packages):", custom_pkgs.len()).bold()
        );
        for (name, repo, cand_ver) in custom_pkgs {
            println!(
                "  • {:<28} ➔  [{}] {:<18} {}",
                name.cyan(),
                repo.green(),
                cand_ver.green(),
                "(Shielded)".dimmed()
            );
        }
    }

    let res = resolver.resolve_all(config);
    let ext_providers = IntegrationsManager::get_active_providers(config);
    let external_updates = IntegrationsManager::check_updates_parallel(&ext_providers);
    render_transaction_view(&res, &config.options.helper, &external_updates);

    if config.features.smart_orphans {
        let orphans = manager.get_orphans(&[]);
        if !orphans.is_empty() {
            let tot_size: i64 = orphans.iter().map(|o| o.isize).sum();
            println!(
                "\n{} {}",
                format!(
                    "🧹 {} orphaned package(s) detected ({})",
                    orphans.len(),
                    ui::format_size(tot_size)
                )
                .yellow()
                .bold(),
                "— run 'pacpin orphans -c' to remove unneeded dependencies".dimmed()
            );
        }
    }
}


fn cmd_upgrade(config: &Config, dry_run: bool, refresh: bool, noconfirm: bool, autoremove: bool) {
    check_pacman_lock();
    print_banner();

    if refresh && !dry_run {
        println!(
            "{}",
            ":: Refreshing package databases (sudo pacman -Sy)...".cyan()
        );
        let status = Command::new("sudo")
            .args(["pacman", "-Sy"])
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!(
                    "{}",
                    format!("Error: Database refresh failed with exit code {}.", s.code().unwrap_or(1)).red()
                );
                exit(s.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("{}", format!("Failed to run sudo pacman: {}", e).red());
                exit(1);
            }
        }
    }

    let manager = match AlpmManager::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };
    let resolver = ResolverEngine::new(&manager);
    let res = resolver.resolve_all(config);
    let ext_providers = IntegrationsManager::get_active_providers(config);
    let external_updates = IntegrationsManager::check_updates_parallel(&ext_providers);
    let updates = render_transaction_view(&res, &config.options.helper, &external_updates);
    let orphans = if config.features.smart_orphans {
        manager.get_orphans(&updates)
    } else {
        Vec::new()
    };
    let orphan_list_changed = if config.features.smart_orphans {
        OrphanManager::has_changed(&orphans)
    } else {
        false
    };

    // Only show up when something changed (or if user explicitly requested autoremove)
    if autoremove || orphan_list_changed {
        ui::render_orphans_summary(&orphans);
    }

    if updates.is_empty() && external_updates.is_empty() {
        if (autoremove || orphan_list_changed) && !dry_run && !orphans.is_empty() {
            let selected_orphans = if !noconfirm {
                ui::prompt_orphan_selection(&orphans, false)
            } else {
                Vec::new()
            };

            if !selected_orphans.is_empty() {
                OrphanManager::execute_removal(&selected_orphans);
            }
            let remaining: Vec<String> = orphans
                .iter()
                .map(|o| o.name.clone())
                .filter(|name| !selected_orphans.contains(name))
                .collect();
            OrphanManager::save_known(&remaining);
        } else if !dry_run && OrphanManager::load_known().is_none() {
            let all: Vec<String> = orphans.iter().map(|o| o.name.clone()).collect();
            OrphanManager::save_known(&all);
        }
        return;
    }

    let mut pacman_targets = Vec::new();
    let mut aur_targets = Vec::new();

    for p in &updates {
        let cand = match p.candidate.as_ref() {
            Some(c) => c,
            None => continue,
        };

        if cand.is_aur {
            aur_targets.push(p.name.clone());
        } else if p.state == "custom" || p.diverted_from_top {
            pacman_targets.push(format!("{}/{}", cand.repo, p.name));
        } else {
            pacman_targets.push(p.name.clone());
        }
    }

    let mut command_strs = Vec::new();

    if !pacman_targets.is_empty() {
        command_strs.push(format!(
            "sudo pacman -S --needed --noconfirm {}",
            pacman_targets.join(" ")
        ));
    }

    if !aur_targets.is_empty() {
        let helper = &config.options.helper;
        command_strs.push(format!(
            "{} -S --needed --aur --noconfirm {}",
            helper,
            aur_targets.join(" ")
        ));
    }

    for p in &ext_providers {
        command_strs.push(p.upgrade_command_str());
    }

    let full_cmd = command_strs.join(" && ");

    if dry_run {
        println!("{}", "Dry-Run: Synthesized Upgrade Commands:".bold());
        println!("  ➔ {}\n", full_cmd.cyan());
        let empty_orphans: Vec<crate::db::OrphanPackage> = Vec::new();
        let targets_to_show = if autoremove || orphan_list_changed {
            &orphans
        } else {
            &empty_orphans
        };
        if !targets_to_show.is_empty() {
            let names: Vec<String> = targets_to_show.iter().map(|o| o.name.clone()).collect();
            println!("{}", "Dry-Run: Projected Orphan Removal Command:".bold());
            println!("  ➔ {}\n", format!("sudo pacman -Rns --noconfirm {}", names.join(" ")).cyan());
        }
        return;
    }

    println!("{}", ":: Synthesized Upgrade Command:".cyan());
    println!("  {}\n", full_cmd.bold());

    let mut selected_orphans = Vec::new();

    if !noconfirm {
        print!("{}", "Proceed with installation? [Y/n] ".bold());
        io::stdout().flush().unwrap();
        let mut resp = String::new();
        if io::stdin().read_line(&mut resp).is_err() {
            println!("\nAborted.");
            return;
        }
        let r = resp.trim().to_lowercase();
        if !r.is_empty() && r != "y" && r != "yes" {
            println!("Aborted.");
            return;
        }

        if (autoremove || orphan_list_changed) && !orphans.is_empty() {
            selected_orphans = ui::prompt_orphan_selection(&orphans, true);
        }
    }

    let mut exit_status = Ok(std::process::ExitStatus::default());
    let mut success = true;

    if !pacman_targets.is_empty() {
        let mut args = vec!["pacman", "-S", "--needed", "--noconfirm"];
        args.extend(pacman_targets.iter().map(|s| s.as_str()));
        let status = Command::new("sudo").args(&args).status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                exit_status = Ok(s);
                success = false;
            }
            Err(e) => {
                exit_status = Err(e);
                success = false;
            }
        }
    }

    if success && !aur_targets.is_empty() {
        let helper = &config.options.helper;
        let mut args = vec!["-S", "--needed", "--aur", "--noconfirm"];
        args.extend(aur_targets.iter().map(|s| s.as_str()));
        let status = Command::new(helper).args(&args).status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                exit_status = Ok(s);
                success = false;
            }
            Err(e) => {
                exit_status = Err(e);
                success = false;
            }
        }
    }

    if success && !ext_providers.is_empty() {
        IntegrationsManager::execute_upgrades(&ext_providers);
    }

    if success {
        let tx_packages: Vec<serde_json::Value> = updates
            .iter()
            .map(|u| {
                let cand = u.candidate.as_ref();
                serde_json::json!({
                    "name": u.name,
                    "from_repo": if u.installed_db.is_empty() { "unknown" } else { &u.installed_db },
                    "from_version": u.installed_ver,
                    "to_repo": cand.map(|c| c.repo.as_str()).unwrap_or("unknown"),
                    "to_version": cand.map(|c| c.version.as_str()).unwrap_or("unknown"),
                    "state": u.state
                })
            })
            .collect();
        let _ = TransactionJournal::record_transaction("upgrade", tx_packages, &full_cmd);

        // Execute orphan removal ONLY AFTER successful upgrade
        if !selected_orphans.is_empty() {
            OrphanManager::execute_removal(&selected_orphans);
        }

        // Update known orphans state after transaction & removals
        let remaining_orphans: Vec<String> = orphans
            .iter()
            .map(|o| o.name.clone())
            .filter(|name| !selected_orphans.contains(name))
            .collect();
        OrphanManager::save_known(&remaining_orphans);
    } else {
        match exit_status {
            Ok(s) => exit(s.code().unwrap_or(1)),
            Err(e) => {
                eprintln!("{}", format!("Execution failed: {}", e).red());
                exit(1);
            }
        }
    }
}


fn cmd_install(mut config: Config, targets: &[String], flags: &[String]) {
    if targets.is_empty() {
        eprintln!("{}", "Error: No package targets specified.".red());
        eprintln!("Usage: pacpin -S [flags] [repo/]package ...");
        exit(1);
    }

    check_pacman_lock();
    print_banner();

    let dry_run = flags.iter().any(|f| f == "-n" || f == "--dry-run");
    let refresh = flags.iter().any(|f| f == "-y" || f == "--refresh");
    let noconfirm = flags.iter().any(|f| f == "--noconfirm");
    let needed = true;

    if refresh && !dry_run {
        println!(
            "{}",
            ":: Refreshing package databases (sudo pacman -Sy)...".cyan()
        );
        let status = Command::new("sudo")
            .args(["pacman", "-Sy"])
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!(
                    "{}",
                    format!("Error: Database refresh failed with exit code {}.", s.code().unwrap_or(1)).red()
                );
                exit(s.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("{}", format!("Failed to run sudo pacman: {}", e).red());
                exit(1);
            }
        }
    }

    let manager = match AlpmManager::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    let mut pacman_targets = Vec::new();
    let mut aur_targets = Vec::new();
    let mut new_pins_added = false;

    let mut known_repos = manager.repos().to_vec();
    known_repos.push("aur".to_string());

    for target in targets {
        if target.contains('/') {
            let parts: Vec<&str> = target.splitn(2, '/').collect();
            let repo = parts[0];
            let pkg = parts[1];

            if !known_repos.iter().any(|r| r == repo) {
                eprintln!(
                    "{}",
                    format!("Error: Repository '[{}]' is not recognized.", repo).red()
                );
                eprintln!("Configured repositories: {}", known_repos.join(", "));
                exit(1);
            }

            if repo.eq_ignore_ascii_case("aur") {
                aur_targets.push(pkg.to_string());
                config.pins.insert(pkg.to_string(), "aur".to_string());
                new_pins_added = true;
            } else {
                let companions = manager.find_companions(pkg, repo, &config.pins);
                let mut to_install_repo = vec![pkg.to_string()];
                if !companions.is_empty() {
                    let title = format!(
                        "'{}' has companion packages in [{}] to avoid version mismatches",
                        pkg, repo
                    );
                    let selected = prompt_multiselect(&title, repo, &companions);
                    to_install_repo.extend(selected);
                }

                for p in to_install_repo {
                    config.pins.insert(p.clone(), repo.to_string());
                    pacman_targets.push(format!("{}/{}", repo, p));
                }
                new_pins_added = true;
            }
        } else {
            pacman_targets.push(target.clone());
        }
    }

    if new_pins_added && !dry_run {
        let _ = save_config(&config);
    }

    let mut command_strs = Vec::new();
    if refresh {
        command_strs.push("sudo pacman -Sy".to_string());
    }

    if !pacman_targets.is_empty() {
        let mut cmd = vec!["sudo", "pacman", "-S"];
        if needed {
            cmd.push("--needed");
        }
        if noconfirm {
            cmd.push("--noconfirm");
        }
        let mut s = cmd.join(" ");
        s.push(' ');
        s.push_str(&pacman_targets.join(" "));
        command_strs.push(s);
    }

    if !aur_targets.is_empty() {
        let helper = &config.options.helper;
        let mut cmd = vec![helper.as_str(), "-S", "--aur"];
        if needed {
            cmd.push("--needed");
        }
        if noconfirm {
            cmd.push("--noconfirm");
        }
        let mut s = cmd.join(" ");
        s.push(' ');
        s.push_str(&aur_targets.join(" "));
        command_strs.push(s);
    }

    let full_cmd = command_strs.join(" && ");

    if dry_run {
        println!("\n{}", "Dry-Run: Synthesized Installation Commands:".bold());
        println!("  ➔ {}", full_cmd.cyan());
        return;
    }

    println!("\n{} {}", ":: Executing:".cyan(), full_cmd.bold());
    let mut exit_status = Ok(std::process::ExitStatus::default());
    let mut success = true;

    if !pacman_targets.is_empty() {
        let mut args = vec!["pacman", "-S"];
        if needed {
            args.push("--needed");
        }
        if noconfirm {
            args.push("--noconfirm");
        }
        args.extend(pacman_targets.iter().map(|s| s.as_str()));
        let status = Command::new("sudo").args(&args).status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                exit_status = Ok(s);
                success = false;
            }
            Err(e) => {
                exit_status = Err(e);
                success = false;
            }
        }
    }

    if success && !aur_targets.is_empty() {
        let helper = &config.options.helper;
        let mut args = vec!["-S", "--aur"];
        if needed {
            args.push("--needed");
        }
        if noconfirm {
            args.push("--noconfirm");
        }
        args.extend(aur_targets.iter().map(|s| s.as_str()));
        let status = Command::new(helper).args(&args).status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                exit_status = Ok(s);
                success = false;
            }
            Err(e) => {
                exit_status = Err(e);
                success = false;
            }
        }
    }

    if success {
        let tx_packages: Vec<serde_json::Value> = targets
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t,
                    "target": t
                })
            })
            .collect();
        let _ = TransactionJournal::record_transaction("install", tx_packages, &full_cmd);
    } else {
        match exit_status {
            Ok(s) => exit(s.code().unwrap_or(1)),
            Err(e) => {
                eprintln!("{}", format!("Execution failed: {}", e).red());
                exit(1);
            }
        }
    }
}

fn cmd_list(config: &Config) {
    print_banner();
    let manager = match AlpmManager::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };
    let resolver = ResolverEngine::new(&manager);

    println!("\n{}", "Configured Repository Rules:".bold());
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

    let res = resolver.resolve_all(config);
    let mut custom_matches: Vec<_> = res
        .packages
        .values()
        .filter(|p| p.state == "custom")
        .collect();
    custom_matches.sort_by(|a, b| a.name.cmp(&b.name));

    if !custom_matches.is_empty() {
        println!(
            "\n{}",
            format!(
                "Installed Packages with Custom Pins ({}):",
                custom_matches.len()
            )
            .bold()
        );
        for m in custom_matches {
            let status_str = if let Some(ref cand) = m.candidate {
                format!("➔ [{}] {}", cand.repo, cand.version)
            } else {
                format!(
                    "➔ {}",
                    format!("[{}] NOT FOUND", m.pinned_repo.as_deref().unwrap_or("?")).red()
                )
            };
            let inst_db = if !m.installed_db.is_empty() {
                format!(" (installed from [{}])", m.installed_db)
            } else {
                String::new()
            };
            println!(
                "  ✔ {:<28} : {}{} {}",
                m.name.bold(),
                m.installed_ver,
                inst_db,
                status_str
            );
        }
    }
    println!();
}

fn cmd_pin(mut config: Config, pattern: String, repo: String) {
    print_banner();

    let is_valid_pattern = !pattern.is_empty()
        && !pattern.contains('/')
        && !pattern.contains('\\')
        && !pattern.contains(';')
        && !pattern.contains('&')
        && !pattern.contains('|')
        && !pattern.contains('`')
        && !pattern.contains('$');
    if !is_valid_pattern {
        eprintln!(
            "{}",
            format!("Error: Invalid package pattern '{}'.", pattern).red().bold()
        );
        exit(1);
    }

    let is_valid_repo_name = !repo.is_empty()
        && repo.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !is_valid_repo_name {
        eprintln!(
            "{}",
            format!("Error: Invalid repository name '[{}]'.", repo).red().bold()
        );
        eprintln!("Repository names may only contain letters, numbers, hyphens, and underscores.");
        exit(1);
    }

    let manager = match AlpmManager::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    let mut known_repos = manager.repos().to_vec();
    known_repos.push("aur".to_string());
    if !known_repos.iter().any(|r| r == &repo) {
        eprintln!(
            "{}",
            format!("Error: Repository '[{}]' is not recognized.", repo).red().bold()
        );
        eprintln!("Available repositories on your system: {}", known_repos.join(", "));
        exit(1);
    }

    let mut to_pin = vec![pattern.clone()];
    if repo != "aur" && !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('[') {
        let companions = manager.find_companions(&pattern, &repo, &config.pins);
        if !companions.is_empty() {
            let title = format!(
                "'{}' has companion packages in [{}] to avoid version mismatches",
                pattern, repo
            );
            let selected = prompt_multiselect(&title, &repo, &companions);
            to_pin.extend(selected);
        }
    }

    for p in &to_pin {
        config.pins.insert(p.clone(), repo.clone());
    }

    if let Err(e) = save_config(&config) {
        eprintln!("{}", format!("Failed to save config: {}", e).red());
        exit(1);
    }

    if to_pin.len() == 1 {
        println!(
            "{}",
            format!("✔ Successfully pinned '{}' to repository [{}].", pattern, repo).green()
        );
    } else {
        let companions_pinned = &to_pin[1..];
        println!(
            "{}",
            format!(
                "✔ Successfully pinned '{}' and {} companion package(s) to [{}]:",
                pattern,
                companions_pinned.len(),
                repo
            )
            .green()
        );
        for c in companions_pinned {
            println!("    • {:<26} ➔  [{}]", c.cyan(), repo.green());
        }
    }
}

fn cmd_unpin(mut config: Config, pattern: &str) {
    print_banner();
    if config.pins.remove(pattern).is_some() {
        if let Err(e) = save_config(&config) {
            eprintln!("{}", format!("Failed to save config: {}", e).red());
            exit(1);
        }
        println!("{}", format!("✔ Unpinned '{}'.", pattern).green());
    } else {
        println!("{}", format!("Pattern '{}' was not pinned.", pattern).yellow());
    }
}

fn cmd_delay(mut config: Config, pkg: &str, days: u32) {
    print_banner();

    if !crate::journal::TransactionJournal::is_safe_pkg_name(pkg) {
        eprintln!("{}", format!("Error: Invalid package name '{}'.", pkg).red().bold());
        exit(1);
    }

    let manager = match AlpmManager::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    config.delay.insert(pkg.to_string(), days);
    if let Err(e) = save_config(&config) {
        eprintln!("{}", format!("Failed to save config: {}", e).red());
        exit(1);
    }
    println!(
        "{}",
        format!("✔ Set {}-day stability delay on '{}'.", days, pkg).green()
    );

    let dependents = manager.get_dependents(pkg);
    if !dependents.is_empty() {
        println!(
            "{}",
            ":: Notice: The following installed package(s) will also be held during this window to prevent ABI breakage:".cyan()
        );
        for d in dependents {
            println!("    • {}", d.yellow());
        }
    }
}

fn cmd_undelay(mut config: Config, pkg: &str) {
    print_banner();
    if config.delay.remove(pkg).is_some() {

        if let Err(e) = save_config(&config) {
            eprintln!("{}", format!("Failed to save config: {}", e).red());
            exit(1);
        }
        println!("{}", format!("✔ Removed stability delay on '{}'.", pkg).green());
    } else {
        println!("{}", format!("Package '{}' did not have a delay rule.", pkg).yellow());
    }
}

fn cmd_history(tx_id: Option<usize>) {
    print_banner();
    if let Some(id) = tx_id {
        let tx = match TransactionJournal::get_transaction(id) {
            Some(t) => t,
            None => {
                eprintln!("{}", format!("Error: Transaction #{} not found in history.", id).red());
                return;
            }
        };

        println!(
            "\n{} ({} — Action: {})",
            format!("Transaction #{}", tx.id).bold(),
            tx.timestamp,
            tx.action.green()
        );
        println!("Command: {}\n", tx.command.dimmed());

        println!("{}", format!("Packages Involved ({}):", tx.packages.len()).bold());
        for p in &tx.packages {
            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
            let from_repo = p.get("from_repo").and_then(|v| v.as_str()).unwrap_or("?");
            let from_ver = p.get("from_version").and_then(|v| v.as_str()).unwrap_or("?");
            let to_repo = p.get("to_repo").and_then(|v| v.as_str()).unwrap_or("?");
            let to_ver = p.get("to_version").and_then(|v| v.as_str()).unwrap_or("?");
            let state = p.get("state").and_then(|v| v.as_str()).unwrap_or("default");

            if from_ver != "?" && to_ver != "?" {
                println!(
                    "  • {:<28} : [{}] {} ➔ [{}] {} ({})",
                    name.bold(),
                    from_repo,
                    from_ver,
                    to_repo,
                    to_ver,
                    state
                );
            } else if let Some(restored) = p.get("restored_version").and_then(|v| v.as_str()) {
                println!(
                    "  • {:<28} : restored to {}",
                    name.bold(),
                    restored.green()
                );
            } else {
                let target = p.get("target").and_then(|v| v.as_str()).unwrap_or(name);
                println!("  • {:<28} : {}", name.bold(), target);
            }
        }
        println!();
        return;
    }

    let txs = TransactionJournal::list_transactions(15);
    if txs.is_empty() {
        println!(
            "\n{}",
            "No transactions recorded yet in history.jsonl.".yellow()
        );
        return;
    }

    println!("\n{}", format!("Transaction Journal (Last {}):", txs.len()).bold());
    println!(
        "  {:<5} {:<20} {:<10} {:<10} {}",
        "ID", "TIMESTAMP", "ACTION", "PACKAGES", "SUMMARY"
    );
    println!(
        "  {:<5} {:<20} {:<10} {:<10} {}",
        "─────", "────────────────────", "──────────", "──────────", "─────────────────────────"
    );

    for tx in &txs {
        let mut repos = Vec::new();
        for p in &tx.packages {
            if let Some(r) = p.get("to_repo").and_then(|v| v.as_str()) {
                if !r.is_empty() && r != "unknown" && !repos.contains(&r) {
                    repos.push(r);
                }
            }
        }
        repos.sort();
        let repo_str = if !repos.is_empty() {
            format!("[{}]", repos.join(", "))
        } else {
            String::new()
        };
        println!(
            "  {:<5} {:<20} {:<10} {:<10} {}",
            tx.id, tx.timestamp, tx.action, tx.total_packages, repo_str
        );
    }

    println!(
        "\n{}",
        "Use 'pacpin history <id>' to inspect full package diff.".dimmed()
    );
    println!(
        "{}",
        "Use 'pacpin rollback [id]' to restore previous package versions.".dimmed()
    );
    println!();
}

fn cmd_orphans(clean: bool) {

    print_banner();
    let manager = match AlpmManager::new() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    let orphans = manager.get_orphans(&[]);
    if orphans.is_empty() {
        println!("\n{}", "✔ No orphaned packages found on your system.".green());
        return;
    }

    ui::render_orphans_summary(&orphans);

    if clean {
        let selected = ui::prompt_orphan_selection(&orphans, false);
        if !selected.is_empty() {
            check_pacman_lock();
            OrphanManager::execute_removal(&selected);
        }
        let remaining: Vec<String> = orphans
            .iter()
            .map(|o| o.name.clone())
            .filter(|name| !selected.contains(name))
            .collect();
        OrphanManager::save_known(&remaining);
    } else {
        let all: Vec<String> = orphans.iter().map(|o| o.name.clone()).collect();
        OrphanManager::save_known(&all);
        println!(
            "\n{}",
            "Use 'pacpin orphans -c' (or 'pin autoremove') to remove selected unneeded dependencies.".dimmed()
        );
    }
}


pub fn is_command_available(cmd: &str) -> bool {
    if let Ok(path) = env::var("PATH") {
        for dir in env::split_paths(&path) {
            if dir.join(cmd).is_file() {
                return true;
            }
        }
    }
    false
}

pub fn find_matching_pin(
    pkg: &str,
    pins: &BTreeMap<String, String>,
) -> Option<(String, String, bool)> {
    if let Some(repo) = pins.get(pkg) {
        return Some((pkg.to_string(), repo.clone(), true));
    }
    for (pat, repo) in pins {
        if Pattern::new(pat).map(|p| p.matches(pkg)).unwrap_or(false) {
            return Some((pat.clone(), repo.clone(), false));
        }
    }
    None
}

fn check_and_prompt_smart_unpin(mut config: Config, removed_pkgs: &[String], noconfirm: bool) {
    let mut config_dirty = false;
    let interactive = io::stdin().is_terminal();

    for pkg in removed_pkgs {
        if let Some((pattern, repo, is_exact)) = find_matching_pin(pkg, &config.pins) {
            let should_remove = if noconfirm {
                true
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
                config_dirty = true;
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
                config_dirty = true;
            }
        }
    }

    if config_dirty {
        if let Err(e) = save_config(&config) {
            eprintln!("{}", format!("Warning: Failed to save updated config: {}", e).yellow());
        } else {
            println!("{}", ":: Configuration updated successfully.".dimmed());
        }
    }
}

fn cmd_search(config: &Config, query_args: &[String]) {
    if query_args.is_empty() {
        eprintln!("{}", "Error: No search query provided.".red());
        eprintln!("Usage: pacpin search <query...> or pacpin -Ss <query...>");
        exit(1);
    }

    let helper = &config.options.helper;
    let status = if is_command_available(helper) {
        Command::new(helper).arg("-Ss").args(query_args).status()
    } else {
        Command::new("pacman").arg("-Ss").args(query_args).status()
    };

    match status {
        Ok(s) => {
            if !s.success() {
                exit(s.code().unwrap_or(1));
            }
        }
        Err(e) => {
            eprintln!("{}", format!("Search execution failed: {}", e).red());
            exit(1);
        }
    }
}

fn cmd_info(config: &Config, pkg_args: &[String]) {
    if pkg_args.is_empty() {
        eprintln!("{}", "Error: No package specified.".red());
        eprintln!("Usage: pacpin info <pkg...> or pacpin -Si <pkg...>");
        exit(1);
    }

    let helper = &config.options.helper;
    let status = if is_command_available(helper) {
        Command::new(helper).arg("-Si").args(pkg_args).status()
    } else {
        Command::new("pacman").arg("-Si").args(pkg_args).status()
    };

    match status {
        Ok(s) => {
            if !s.success() {
                exit(s.code().unwrap_or(1));
            }
        }
        Err(e) => {
            eprintln!("{}", format!("Info query failed: {}", e).red());
            exit(1);
        }
    }
}

fn cmd_clean(config: &Config, extra_args: &[String]) {
    check_pacman_lock();
    let helper = &config.options.helper;
    let status = if is_command_available(helper) {
        println!("{} Cleaning package cache via {} -Sc...", "::".cyan(), helper);
        Command::new(helper).arg("-Sc").args(extra_args).status()
    } else {
        println!("{} Cleaning package cache via sudo pacman -Sc...", "::".cyan());
        Command::new("sudo").arg("pacman").arg("-Sc").args(extra_args).status()
    };

    match status {
        Ok(s) => {
            if !s.success() {
                exit(s.code().unwrap_or(1));
            }
        }
        Err(e) => {
            eprintln!("{}", format!("Cache clean failed: {}", e).red());
            exit(1);
        }
    }

    let ext_providers = IntegrationsManager::get_active_providers(config);
    if !ext_providers.is_empty() {
        IntegrationsManager::execute_cleanups(&ext_providers);
    }
}

fn cmd_remove(config: Config, args: &[String], is_friendly: bool) {
    check_pacman_lock();

    let mut flags = Vec::new();
    let mut targets = Vec::new();

    if is_friendly {
        flags.push("-Rns".to_string());
        for a in args {
            if a.starts_with('-') {
                flags.push(a.clone());
            } else {
                targets.push(a.clone());
            }
        }
    } else {
        for a in args {
            if a.starts_with('-') {
                flags.push(a.clone());
            } else {
                targets.push(a.clone());
            }
        }
    }

    if targets.is_empty() {
        eprintln!("{}", "Error: No package targets specified for removal.".red());
        eprintln!("Usage: pacpin remove <pkg...> or pacpin -Rns <pkg...>");
        exit(1);
    }

    let mut cmd_args = Vec::new();
    cmd_args.extend(flags.iter().map(|s| s.as_str()));
    cmd_args.extend(targets.iter().map(|s| s.as_str()));

    println!(
        "{} Removing package(s) via sudo pacman {}...",
        "::".cyan(),
        cmd_args.join(" ")
    );

    let status = Command::new("sudo")
        .arg("pacman")
        .args(&cmd_args)
        .status();

    match status {
        Ok(s) if s.success() => {
            let noconfirm = flags.iter().any(|f| f == "--noconfirm");
            check_and_prompt_smart_unpin(config, &targets, noconfirm);
        }
        Ok(s) => exit(s.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("{}", format!("Failed to run sudo pacman: {}", e).red());
            exit(1);
        }
    }
}

fn cmd_reset(force: bool) {
    let path = config::get_config_path();
    if !path.exists() {
        println!("{}", "No configuration file found to reset.".yellow());
        return;
    }

    if !force {
        print!("{}", "Are you sure you want to reset all configurations and pins to default? [y/N] ".bold());
        io::stdout().flush().unwrap();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            println!("\nAborted.");
            return;
        }
        let r = input.trim().to_lowercase();
        if r != "y" && r != "yes" {
            println!("Aborted.");
            return;
        }
    }

    // Backup current configuration
    let now = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let bak_path = path.with_extension(format!("toml.bak.{}", now));
    if let Err(e) = std::fs::copy(&path, &bak_path) {
        eprintln!("{}", format!("Warning: Could not create backup file: {}", e).yellow());
    } else {
        println!("  {}", format!("📦 Backup created at {}", bak_path.display()).dimmed());
    }

    let default_cfg = Config::default();
    if let Err(e) = config::save_config(&default_cfg) {
        eprintln!("{}", format!("Error resetting configuration: {}", e).red());
        exit(1);
    }

    println!("{}", "✔ All configurations, pins, exclusions, and delays have been reset to default.".green().bold());
    println!("  Run 'pacpin init' to configure preferences from scratch.\n");
}

fn print_help() {
    print_banner();
    println!("\nUsage:");
    println!("  pacpin check                 Check pending updates & verify custom pin protections");
    println!("  pacpin upgrade [flags]       Execute safe upgrade with True Resolver");
    println!("    Flags:");
    println!("      -n, --dry-run            Show transaction summary without executing");
    println!("      -y, --refresh            Refresh sync databases (sudo pacman -Sy) first");
    println!("      -c, --clean              Prompt to clean all orphans during upgrade");
    println!("      --noconfirm              Bypass interactive confirmation prompt");
    println!("  pacpin -S [flags] [repo/]pkg Install packages (auto-pins repo/pkg with companion cascade)");
    println!("  pacpin remove <pkg...>       Remove package(s) and unneeded dependencies (-Rns + smart unpin)");
    println!("  pacpin search <query...>     Search official repositories and AUR simultaneously");
    println!("  pacpin info <pkg...>         Show detailed package information (-Si / AUR)");
    println!("  pacpin clean                 Clean pacman and AUR build cache (-Sc)");
    println!("  pacpin orphans [-c]          Inspect or remove orphaned dependencies (-c to clean)");
    println!("  pacpin autoremove            Alias for 'pacpin orphans -c'");
    println!("  pacpin keep <pkg...>         Mark package(s) as explicitly installed (silences orphan warnings)");
    println!("  pacpin pin <pkg> <repo>      Lock a package pattern to a designated repository");
    println!("  pacpin unpin <pkg>           Remove a pin rule");
    println!("  pacpin delay <pkg> <days>    Set a stability delay buffer on a package");
    println!("  pacpin undelay <pkg>         Remove a package stability delay rule");
    println!("  pacpin history [id]          View transaction history timeline or inspect a transaction");
    println!("  pacpin rollback [id] [-n]    Restore previous package versions from cache");
    println!("  pacpin list                  List active pins, exclusions, and matching packages");
    println!("  pacpin reset [-f]            Reset all configurations, pins, and delays to default (-f force)");
    println!("  pacpin init [--reset]        Interactive onboarding wizard (use --reset for clean slate)");
    println!("  pacpin --version             Show version and GPLv3 license notice");
    println!();
    println!("Pacman & AUR Drop-in Aliases:");
    println!("  pacpin -Syu                  Alias for 'pacpin upgrade -y'");
    println!("  pacpin -Qu                   Alias for 'pacpin check'");
    println!("  pacpin -Ss <query...>        Search repositories and AUR");
    println!("  pacpin -Si <pkg...>          Show remote/AUR package metadata");
    println!("  pacpin -Qi <pkg...>          Show locally installed package info");
    println!("  pacpin -R / -Rns <pkg...>    Remove packages (with smart pin cleanup)");
    println!("  pacpin -Q / -Qo / -Ql        Pacman query and package inspection");
    println!("  pacpin -U <pkg.tar.zst>      Install local package archive");
    println!("  pacpin -F / -Fy              Pacman files database operations");
    println!();
}

fn print_version() {
    println!("pacpin 3.1.0");
    println!("Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>");
    println!("License GPLv3+: GNU GPL version 3 or later <https://gnu.org/licenses/gpl.html>");
    println!("This is free software: you are free to change and redistribute it.");
    println!("There is NO WARRANTY, to the extent permitted by law.");
}


fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.first().map(|s| s == "-v" || s == "-V" || s == "--version").unwrap_or(false) {
        print_version();
        exit(0);
    }

    if args.first().map(|s| s == "-h" || s == "--help" || s == "help").unwrap_or(false) {
        print_help();
        exit(0);
    }

    if args.first().map(|s| s == "reset" || s == "config-reset").unwrap_or(false)
        || (args.len() >= 2 && args[0] == "config" && args[1] == "reset")
    {
        let force = args.iter().any(|a| a == "-f" || a == "--force" || a == "-y");
        cmd_reset(force);
        exit(0);
    }

    if args.first().map(|s| s == "init" || s == "setup").unwrap_or(false) {
        let reset = args.iter().any(|a| a == "-r" || a == "--reset");
        wizard::run_first_launch_wizard(true, reset);
        exit(0);
    }

    // Auto-launch wizard if no config exists and running in an interactive terminal
    let (config, just_initialized) = if !config::config_exists() && io::stdin().is_terminal() {
        if let Some(cfg) = wizard::run_first_launch_wizard(false, false) {
            (cfg, true)
        } else {
            (load_config(), false)
        }
    } else {
        (load_config(), false)
    };

    if just_initialized && args.is_empty() {
        exit(0);
    }

    if args.is_empty() {
        print_help();
        exit(0);
    }

    let cmd = &args[0];
    if cmd == "check" || cmd == "-Qu" {
        cmd_check(&config);
    } else if cmd == "upgrade"
        || cmd == "-Syu"
        || cmd == "up"
        || (cmd.starts_with("-S")
            && (cmd.contains('u') || cmd.contains('y'))
            && !cmd.starts_with("-Ss")
            && !cmd.starts_with("-Si")
            && !cmd.starts_with("-Sc")
            && !cmd.starts_with("-Sw")
            && !cmd.starts_with("-Sg")
            && !args.iter().skip(1).any(|a| !a.starts_with('-')))
    {
        let dry_run = args.iter().any(|a| a == "-n" || a == "--dry-run");
        let refresh = args.iter().any(|a| a == "-y" || a == "--refresh") || cmd.contains('y');
        let noconfirm = args.iter().any(|a| a == "--noconfirm");
        let autoremove = args.iter().any(|a| a == "-c" || a == "--clean" || a == "--autoremove") || cmd.contains('c');
        cmd_upgrade(&config, dry_run, refresh, noconfirm, autoremove);
    } else if cmd == "search" || cmd == "-Ss" || (cmd.starts_with("-S") && cmd.contains('s')) {
        let query_args: Vec<String> = if cmd.starts_with("-S") {
            let mut q = Vec::new();
            if cmd.len() > 3 {
                q.push(cmd[3..].to_string());
            }
            q.extend(args.iter().skip(1).cloned());
            q
        } else {
            args.iter().skip(1).cloned().collect()
        };
        cmd_search(&config, &query_args);
    } else if cmd == "info" || cmd == "-Si" || (cmd.starts_with("-S") && cmd.contains('i')) {
        let pkg_args: Vec<String> = if cmd.starts_with("-S") {
            let mut p = Vec::new();
            if cmd.len() > 3 {
                p.push(cmd[3..].to_string());
            }
            p.extend(args.iter().skip(1).cloned());
            p
        } else {
            args.iter().skip(1).cloned().collect()
        };
        cmd_info(&config, &pkg_args);
    } else if cmd == "clean"
        || cmd == "clean-cache"
        || cmd == "-Sc"
        || cmd == "-Scc"
        || (cmd.starts_with("-S") && cmd.contains('c') && !cmd.contains('u') && !args.iter().skip(1).any(|a| !a.starts_with('-')))
    {
        let extra_args: Vec<String> = args.iter().skip(1).cloned().collect();
        cmd_clean(&config, &extra_args);
    } else if cmd.starts_with("-Sw") {
        check_pacman_lock();
        let status = Command::new("sudo").arg("pacman").args(&args).status();
        match status {
            Ok(s) => exit(s.code().unwrap_or(0)),
            Err(e) => {
                eprintln!("{}", format!("Failed to run sudo pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd.starts_with("-Sg") {
        let status = Command::new("pacman").args(&args).status();
        match status {
            Ok(s) => exit(s.code().unwrap_or(0)),
            Err(e) => {
                eprintln!("{}", format!("Failed to run pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd == "-S" || cmd == "install" || (cmd.starts_with("-S") && args.iter().any(|a| !a.starts_with('-'))) {
        let mut targets = Vec::new();
        let mut flags = Vec::new();
        if cmd.len() > 2 && cmd.starts_with("-S") {
            let subflags = &cmd[2..];
            if subflags.contains('y') {
                flags.push("-y".to_string());
            }
            if subflags.contains('n') {
                flags.push("-n".to_string());
            }
        }
        for a in &args[1..] {
            if a.starts_with('-') {
                flags.push(a.clone());
            } else {
                targets.push(a.clone());
            }
        }
        cmd_install(config, &targets, &flags);
    } else if cmd == "remove" || cmd == "rm" || cmd.starts_with("-R") {
        let is_friendly = cmd == "remove" || cmd == "rm";
        let sub_args = if is_friendly {
            args[1..].to_vec()
        } else {
            args.clone()
        };
        cmd_remove(config, &sub_args, is_friendly);
    } else if cmd.starts_with("-Q") {
        let status = Command::new("pacman").args(&args).status();
        match status {
            Ok(s) => exit(s.code().unwrap_or(0)),
            Err(e) => {
                eprintln!("{}", format!("Failed to run pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd.starts_with("-F") {
        let needs_sudo = args.iter().any(|a| a.contains('y') || a == "--refresh");
        let status = if needs_sudo {
            check_pacman_lock();
            Command::new("sudo").arg("pacman").args(&args).status()
        } else {
            Command::new("pacman").args(&args).status()
        };
        match status {
            Ok(s) => exit(s.code().unwrap_or(0)),
            Err(e) => {
                eprintln!("{}", format!("Failed to run pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd.starts_with("-U") {
        check_pacman_lock();
        let status = Command::new("sudo").arg("pacman").args(&args).status();
        match status {
            Ok(s) => exit(s.code().unwrap_or(0)),
            Err(e) => {
                eprintln!("{}", format!("Failed to run sudo pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd.starts_with("-D") {
        check_pacman_lock();
        let status = Command::new("sudo").arg("pacman").args(&args).status();
        match status {
            Ok(s) => exit(s.code().unwrap_or(0)),
            Err(e) => {
                eprintln!("{}", format!("Failed to run sudo pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd == "history" {
        let tx_id = args.get(1).and_then(|s| s.parse::<usize>().ok());
        cmd_history(tx_id);
    } else if cmd == "rollback" {
        let mut tx_id = None;
        let dry_run = args.iter().any(|a| a == "-n" || a == "--dry-run");
        for a in &args[1..] {
            if let Ok(id) = a.parse::<usize>() {
                tx_id = Some(id);
            }
        }
        TransactionJournal::rollback(tx_id, dry_run);
    } else if cmd == "delay" {
        if args.len() < 3 || args[2].parse::<u32>().is_err() {
            eprintln!("{}", "Error: 'pacpin delay' requires <pkg> and <days>.".red());
            eprintln!("Usage: pacpin delay <pkg> <days> (e.g. pacpin delay openssl 3)");
            exit(1);
        }
        cmd_delay(config, &args[1], args[2].parse::<u32>().unwrap());
    } else if cmd == "undelay" {
        if args.len() < 2 {
            eprintln!("{}", "Error: 'pacpin undelay' requires <pkg>.".red());
            eprintln!("Usage: pacpin undelay <pkg>");
            exit(1);
        }
        cmd_undelay(config, &args[1]);
    } else if cmd == "list" {
        cmd_list(&config);
    } else if cmd == "orphans" || cmd == "autoremove" {
        let clean = args.iter().any(|a| a == "-c" || a == "--clean") || cmd == "autoremove";
        cmd_orphans(clean);
    } else if cmd == "keep" || cmd == "adopt" {
        if args.len() < 2 {
            eprintln!("{}", "Error: 'pacpin keep' requires at least one package name.".red());
            eprintln!("Usage: pacpin keep <pkg1> [pkg2...]");
            exit(1);
        }
        let pkgs = &args[1..];
        println!("{} Marking {} package(s) as explicitly installed (sudo pacman -D --asexplicit)...", "::".cyan(), pkgs.len());
        let status = Command::new("sudo")
            .arg("pacman")
            .arg("-D")
            .arg("--asexplicit")
            .args(pkgs)
            .status();
        match status {
            Ok(s) if s.success() => {
                println!("{}", "✔ Package(s) successfully marked as explicit! They will no longer appear as orphans.".green());
            }
            Ok(s) => exit(s.code().unwrap_or(1)),
            Err(e) => {
                eprintln!("{}", format!("Failed to run pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd == "pin" {
        if args.len() < 3 {
            eprintln!("{}", "Error: 'pacpin pin' requires <pattern> and <repo>.".red());
            eprintln!("Usage: pacpin pin <pattern> <repo> (e.g. pacpin pin 'linux-firmware*' core)");
            exit(1);
        }
        cmd_pin(config, args[1].clone(), args[2].clone());
    } else if cmd == "unpin" {
        if args.len() < 2 {
            eprintln!("{}", "Error: 'pacpin unpin' requires a pattern.".red());
            eprintln!("Usage: pacpin unpin <pattern> (e.g. pacpin unpin 'linux-firmware*')");
            exit(1);
        }
        cmd_unpin(config, &args[1]);
    } else if cmd.starts_with('-') {
        let helper = &config.options.helper;
        let status = if is_command_available(helper) {
            Command::new(helper).args(&args).status()
        } else {
            Command::new("pacman").args(&args).status()
        };
        match status {
            Ok(s) => exit(s.code().unwrap_or(0)),
            Err(e) => {
                eprintln!("{}", format!("Failed to run command: {}", e).red());
                exit(1);
            }
        }
    } else {
        eprintln!("{}", format!("Unknown command: {}", cmd).red());
        eprintln!("Did you mean:");
        eprintln!("  pacpin search {}  (search repositories and AUR)", cmd);
        eprintln!("  pacpin -S {}      (install package)", cmd);
        eprintln!("Run 'pacpin --help' for full command usage.");
        exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_command_available() {
        assert!(is_command_available("sh") || is_command_available("bash"));
        assert!(!is_command_available("this_binary_does_not_exist_xyz123"));
    }

    #[test]
    fn test_find_matching_pin_exact_and_glob() {
        let mut pins = BTreeMap::new();
        pins.insert("mesa".to_string(), "core".to_string());
        pins.insert("linux-firmware*".to_string(), "cachyos".to_string());

        assert_eq!(
            find_matching_pin("mesa", &pins),
            Some(("mesa".to_string(), "core".to_string(), true))
        );
        assert_eq!(
            find_matching_pin("linux-firmware-intel", &pins),
            Some(("linux-firmware*".to_string(), "cachyos".to_string(), false))
        );
        assert_eq!(find_matching_pin("git", &pins), None);
    }
}
