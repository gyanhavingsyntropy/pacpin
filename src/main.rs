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
mod repo_menu;
mod resolver;
mod restart;
mod sandbox;
mod tui_select;
mod ui;
mod utils;
mod wizard;

use colored::Colorize;
use config::{load_config, save_config, Config};
use db::AlpmManager;
use integrations::IntegrationsManager;
use journal::TransactionJournal;
use orphans::OrphanManager;
use resolver::{ResolvedPackage, ResolverEngine};
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::{exit, Command};
use ui::{print_banner, prompt_multiselect, render_transaction_view};

fn check_pacman_lock() {
    let lock_path = AlpmManager::get_dbpath().join("db.lck");
    if lock_path.exists() {
        eprintln!(
            "{}",
            format!(
                "Error: Pacman database is locked ({}).",
                lock_path.display()
            )
            .red()
        );
        eprintln!("Another package management process is currently running. Exiting.");
        exit(1);
    }
}

fn cmd_check(config: &Config) {
    print_banner();
    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };
    let resolver = ResolverEngine::new(&manager);

    let alpm = manager.handle();
    let local_pkgs = alpm.localdb().pkgs();

    let glob_pins = ResolverEngine::compile_glob_pins(&config.pins);
    let syncdb_map: HashMap<&str, &alpm::Db> =
        alpm.syncdbs().into_iter().map(|d| (d.name(), d)).collect();

    let mut custom_pkgs = Vec::new();
    for p in local_pkgs {
        if let Some(target_repo) =
            ResolverEngine::match_pinned_package(p.name(), &config.pins, &glob_pins)
        {
            let mut cand_ver = "pinned".to_string();
            if let Some(&db) = syncdb_map.get(target_repo.as_str()) {
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

    if config.features.vendor_stickiness {
        println!(
            "\n{}",
            "ℹ Vendor Stickiness: ACTIVE (packages stay bound to originating repository unless pinned)".blue()
        );
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

fn cmd_upgrade(
    config: &Config,
    dry_run: bool,
    refresh: bool,
    noconfirm: bool,
    autoremove: bool,
    extra_flags: &[String],
) {
    check_pacman_lock();
    print_banner();

    let ext_providers = IntegrationsManager::get_active_providers(config);

    if refresh && !dry_run {
        println!(
            "{}",
            ":: Refreshing package databases (sudo pacman -Sy)...".cyan()
        );
        let status = Command::new("sudo").args(["pacman", "-Sy"]).status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!(
                    "{}",
                    format!(
                        "Error: Database refresh failed with exit code {}.",
                        s.code().unwrap_or(1)
                    )
                    .red()
                );
                exit(s.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("{}", format!("Failed to run sudo pacman: {}", e).red());
                exit(1);
            }
        }

        if !ext_providers.is_empty() {
            IntegrationsManager::refresh_all_parallel(&ext_providers);
        }
    }

    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };
    let resolver = ResolverEngine::new(&manager);
    let res = resolver.resolve_all(config);
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
            } else if autoremove {
                orphans
                    .iter()
                    .filter(|o| o.optional_for.is_empty())
                    .map(|o| o.name.clone())
                    .collect()
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
        let p_updates: Vec<integrations::ExternalUpdate> = external_updates
            .iter()
            .filter(|u| u.runner == p.name())
            .cloned()
            .collect();
        if !p_updates.is_empty() {
            command_strs.push(p.upgrade_command_str(&p_updates));
        }
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
            println!(
                "  ➔ {}\n",
                format!("sudo pacman -Rns --noconfirm {}", names.join(" ")).cyan()
            );
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
    } else if autoremove && !orphans.is_empty() {
        selected_orphans = orphans
            .iter()
            .filter(|o| o.optional_for.is_empty())
            .map(|o| o.name.clone())
            .collect();
    }

    let mut applied_updates: Vec<ResolvedPackage> = Vec::new();
    let mut executed_commands: Vec<String> = Vec::new();
    let mut exit_status = Ok(std::process::ExitStatus::default());
    let mut all_success = true;

    let forwarded_pacman_flags: Vec<&str> = extra_flags
        .iter()
        .filter(|f| {
            let s = f.as_str();
            s != "-y"
                && s != "--refresh"
                && s != "-yy"
                && s != "-u"
                && s != "--sysupgrade"
                && s != "-uu"
                && s != "-c"
                && s != "--clean"
                && s != "--autoremove"
                && s != "-n"
                && s != "--dry-run"
                && s != "--noconfirm"
                && s != "--needed"
        })
        .map(|s| s.as_str())
        .collect();

    if !pacman_targets.is_empty() {
        let mut args = vec!["pacman", "-S", "--needed", "--noconfirm"];
        args.extend(forwarded_pacman_flags.iter().cloned());
        args.extend(pacman_targets.iter().map(|s| s.as_str()));
        let status = Command::new("sudo").args(&args).status();
        match status {
            Ok(s) if s.success() => {
                for u in &updates {
                    if let Some(cand) = &u.candidate {
                        if !cand.is_aur {
                            applied_updates.push(u.clone());
                        }
                    }
                }
                let mut cmd_parts = vec!["sudo", "pacman", "-S", "--needed", "--noconfirm"];
                cmd_parts.extend(forwarded_pacman_flags.iter().cloned());
                let mut full_cmd_s = cmd_parts.join(" ");
                full_cmd_s.push(' ');
                full_cmd_s.push_str(&pacman_targets.join(" "));
                executed_commands.push(full_cmd_s);
            }
            Ok(s) => {
                exit_status = Ok(s);
                all_success = false;
            }
            Err(e) => {
                exit_status = Err(e);
                all_success = false;
            }
        }
    }

    if all_success && !aur_targets.is_empty() {
        let helper = &config.options.helper;
        let mut args = vec!["-S", "--needed", "--aur", "--noconfirm"];
        args.extend(forwarded_pacman_flags.iter().cloned());
        args.extend(aur_targets.iter().map(|s| s.as_str()));
        let status = Command::new(helper).args(&args).status();
        match status {
            Ok(s) if s.success() => {
                for u in &updates {
                    if let Some(cand) = &u.candidate {
                        if cand.is_aur {
                            applied_updates.push(u.clone());
                        }
                    }
                }
                let mut cmd_parts = vec![helper.as_str(), "-S", "--needed", "--aur", "--noconfirm"];
                cmd_parts.extend(forwarded_pacman_flags.iter().cloned());
                let mut full_cmd_s = cmd_parts.join(" ");
                full_cmd_s.push(' ');
                full_cmd_s.push_str(&aur_targets.join(" "));
                executed_commands.push(full_cmd_s);
            }
            Ok(s) => {
                exit_status = Ok(s);
                all_success = false;
            }
            Err(e) => {
                exit_status = Err(e);
                all_success = false;
            }
        }
    }

    if all_success && !external_updates.is_empty() {
        let outcomes = IntegrationsManager::execute_upgrades(&ext_providers, &external_updates);
        for (_p_name, cmd_str, success) in outcomes {
            if success {
                executed_commands.push(cmd_str);
            } else {
                all_success = false;
            }
        }
    }

    // Journal any packages that were successfully applied to disk
    if !applied_updates.is_empty() {
        let tx_packages: Vec<serde_json::Value> = applied_updates
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
        let cmd_to_record = if executed_commands.is_empty() {
            full_cmd.clone()
        } else {
            executed_commands.join(" && ")
        };
        let _ = TransactionJournal::record_transaction("upgrade", tx_packages, &cmd_to_record);

        // Inspect kernel and running services for post-upgrade restart advisory on applied packages
        restart::RestartInspector::print_restart_advisory(&applied_updates);
    }

    if all_success {
        // Execute orphan removal ONLY AFTER full successful upgrade
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
            ":: Warning: Installing packages with '-y' (database refresh) without performing a full system upgrade".yellow().bold()
        );
        println!(
            "{}",
            "   can lead to partial upgrades and dependency breakage on Arch Linux.".yellow()
        );
        println!(
            "{}",
            ":: Refreshing package databases (sudo pacman -Sy)...".cyan()
        );
        let status = Command::new("sudo").args(["pacman", "-Sy"]).status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!(
                    "{}",
                    format!(
                        "Error: Database refresh failed with exit code {}.",
                        s.code().unwrap_or(1)
                    )
                    .red()
                );
                exit(s.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("{}", format!("Failed to run sudo pacman: {}", e).red());
                exit(1);
            }
        }
    }

    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    let mut pacman_targets = Vec::new();
    let mut aur_targets = Vec::new();
    let mut pending_pins: Vec<(String, String)> = Vec::new();

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
                pending_pins.push((pkg.to_string(), "aur".to_string()));
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
                    pending_pins.push((p.clone(), repo.to_string()));
                    pacman_targets.push(format!("{}/{}", repo, p));
                }
            }
        } else {
            let exists_in_sync = manager
                .handle()
                .syncdbs()
                .into_iter()
                .any(|db| db.pkg(target.as_str()).is_ok());
            if exists_in_sync {
                pacman_targets.push(target.clone());
            } else {
                // Query the AUR only after ruling out every configured sync
                // repository, so unqualified AUR packages work like they do in
                // modern Arch helpers without changing official-package routing.
                let aur_query = vec![target.clone()];
                if aur::query_aur(&aur_query).contains_key(target) {
                    aur_targets.push(target.clone());
                    pending_pins.push((target.clone(), "aur".to_string()));
                } else {
                    // Preserve pacman's normal error reporting for packages
                    // unknown to both the configured repos and the AUR.
                    pacman_targets.push(target.clone());
                }
            }
        }
    }

    let forwarded_flags: Vec<&str> = flags
        .iter()
        .filter(|f| {
            let s = f.as_str();
            s != "-y"
                && s != "--refresh"
                && s != "-yy"
                && s != "-n"
                && s != "--dry-run"
                && s != "--noconfirm"
                && s != "--needed"
        })
        .map(|s| s.as_str())
        .collect();

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
        cmd.extend(forwarded_flags.iter().cloned());
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
        cmd.extend(forwarded_flags.iter().cloned());
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
    let mut installed_targets: Vec<String> = Vec::new();
    let mut executed_commands: Vec<String> = Vec::new();
    let mut exit_status = Ok(std::process::ExitStatus::default());
    let mut all_success = true;

    if !pacman_targets.is_empty() {
        let mut args = vec!["pacman", "-S"];
        if needed {
            args.push("--needed");
        }
        if noconfirm {
            args.push("--noconfirm");
        }
        args.extend(forwarded_flags.iter().cloned());
        args.extend(pacman_targets.iter().map(|s| s.as_str()));
        let status = Command::new("sudo").args(&args).status();
        match status {
            Ok(s) if s.success() => {
                installed_targets.extend(pacman_targets.clone());
                let mut cmd_parts = vec!["sudo", "pacman", "-S"];
                if needed {
                    cmd_parts.push("--needed");
                }
                if noconfirm {
                    cmd_parts.push("--noconfirm");
                }
                cmd_parts.extend(forwarded_flags.iter().cloned());
                let mut cmd_str = cmd_parts.join(" ");
                cmd_str.push(' ');
                cmd_str.push_str(&pacman_targets.join(" "));
                executed_commands.push(cmd_str);
            }
            Ok(s) => {
                exit_status = Ok(s);
                all_success = false;
            }
            Err(e) => {
                exit_status = Err(e);
                all_success = false;
            }
        }
    }

    if all_success && !aur_targets.is_empty() {
        let helper = &config.options.helper;
        let mut args = vec!["-S", "--aur"];
        if needed {
            args.push("--needed");
        }
        if noconfirm {
            args.push("--noconfirm");
        }
        args.extend(forwarded_flags.iter().cloned());
        args.extend(aur_targets.iter().map(|s| s.as_str()));
        let status = Command::new(helper).args(&args).status();
        match status {
            Ok(s) if s.success() => {
                installed_targets.extend(aur_targets.clone());
                let mut cmd_parts = vec![helper.as_str(), "-S", "--aur"];
                if needed {
                    cmd_parts.push("--needed");
                }
                if noconfirm {
                    cmd_parts.push("--noconfirm");
                }
                cmd_parts.extend(forwarded_flags.iter().cloned());
                let mut cmd_str = cmd_parts.join(" ");
                cmd_str.push(' ');
                cmd_str.push_str(&aur_targets.join(" "));
                executed_commands.push(cmd_str);
            }
            Ok(s) => {
                exit_status = Ok(s);
                all_success = false;
            }
            Err(e) => {
                exit_status = Err(e);
                all_success = false;
            }
        }
    }

    if !installed_targets.is_empty() {
        let mut confirmed_pins_added = false;
        for (pkg_name, repo_name) in pending_pins {
            let was_installed = installed_targets
                .iter()
                .any(|t| t == &pkg_name || t == &format!("{}/{}", repo_name, pkg_name));
            if was_installed {
                config.pins.insert(pkg_name, repo_name);
                confirmed_pins_added = true;
            }
        }

        if confirmed_pins_added && !dry_run {
            if let Err(e) = save_config(&config) {
                eprintln!(
                    "{}",
                    format!("Warning: Failed to save pins to configuration: {}", e).yellow()
                );
            } else {
                println!(
                    "{}",
                    "✔ Configuration updated with confirmed repository pins.".green()
                );
            }
        }
        let tx_packages: Vec<serde_json::Value> = installed_targets
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t,
                    "target": t
                })
            })
            .collect();
        let cmd_to_record = if executed_commands.is_empty() {
            full_cmd.clone()
        } else {
            executed_commands.join(" && ")
        };
        let _ = TransactionJournal::record_transaction("install", tx_packages, &cmd_to_record);
    }

    if !all_success {
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

fn cmd_repos(mut config: Config) {
    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };
    let current_repos = manager.repos();
    if let Some(new_order) = repo_menu::run_repo_menu(current_repos) {
        config.repo_order = new_order.clone();
        if let Err(e) = save_config(&config) {
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

fn cmd_pin(mut config: Config, repo: String, patterns: Vec<String>) {
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
        if !crate::utils::is_safe_pattern(pattern) || glob::Pattern::new(pattern).is_err() {
            eprintln!(
                "{}",
                format!("Error: Invalid package pattern '{}'. Patterns must be valid glob expressions without shell metacharacters or whitespace.", pattern).red().bold()
            );
            exit(1);
        }
    }

    let mut to_pin = patterns.clone();
    if repo != "aur" {
        let mut all_companions = Vec::new();
        let existing_set: std::collections::HashSet<String> = to_pin.iter().cloned().collect();
        for pat in &patterns {
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

fn cmd_unpin(mut config: Config, pattern: &str) {
    print_banner();
    if config.pins.remove(pattern).is_some() {
        if let Err(e) = save_config(&config) {
            eprintln!("{}", format!("Failed to save config: {}", e).red());
            exit(1);
        }
        println!("{}", format!("✔ Unpinned '{}'.", pattern).green());
    } else {
        println!(
            "{}",
            format!("Pattern '{}' was not pinned.", pattern).yellow()
        );
    }
}

fn cmd_delay(mut config: Config, pkg: &str, days: u32) {
    print_banner();

    if !crate::utils::is_safe_pattern(pkg) {
        eprintln!(
            "{}",
            format!("Error: Invalid package or pattern '{}'.", pkg)
                .red()
                .bold()
        );
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

    if !pkg.contains('*') && !pkg.contains('?') && !pkg.contains('[') {
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
}

fn cmd_undelay(mut config: Config, pkg: &str) {
    print_banner();
    if config.delay.remove(pkg).is_some() {
        if let Err(e) = save_config(&config) {
            eprintln!("{}", format!("Failed to save config: {}", e).red());
            exit(1);
        }
        println!(
            "{}",
            format!("✔ Removed stability delay on '{}'.", pkg).green()
        );
    } else {
        println!(
            "{}",
            format!("Package '{}' did not have a delay rule.", pkg).yellow()
        );
    }
}

fn cmd_history(tx_id: Option<usize>) {
    print_banner();
    if let Some(id) = tx_id {
        let tx = match TransactionJournal::get_transaction(id) {
            Some(t) => t,
            None => {
                eprintln!(
                    "{}",
                    format!("Error: Transaction #{} not found in history.", id).red()
                );
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

        println!(
            "{}",
            format!("Packages Involved ({}):", tx.packages.len()).bold()
        );
        for p in &tx.packages {
            let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
            let from_repo = p.get("from_repo").and_then(|v| v.as_str()).unwrap_or("?");
            let from_ver = p
                .get("from_version")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let to_repo = p.get("to_repo").and_then(|v| v.as_str()).unwrap_or("?");
            let to_ver = p.get("to_version").and_then(|v| v.as_str()).unwrap_or("?");
            let state = p.get("state").and_then(|v| v.as_str()).unwrap_or("default");
            let state_badge = match state {
                "custom" => "(custom)".cyan(),
                "sticky" => "(sticky)".blue(),
                _ => "(default)".dimmed(),
            };

            if from_ver != "?" && to_ver != "?" {
                println!(
                    "  • {:<28} : [{}] {} ➔ [{}] {} {}",
                    name.bold(),
                    from_repo,
                    from_ver,
                    to_repo,
                    to_ver,
                    state_badge
                );
            } else if let Some(restored) = p.get("restored_version").and_then(|v| v.as_str()) {
                println!("  • {:<28} : restored to {}", name.bold(), restored.green());
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

    println!(
        "\n{}",
        format!("Transaction Journal (Last {}):", txs.len()).bold()
    );
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

fn cmd_orphans(clean: bool, noconfirm: bool) {
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
        println!(
            "\n{}",
            "✔ No orphaned packages found on your system.".green()
        );
        return;
    }

    ui::render_orphans_summary(&orphans);

    if clean {
        let selected = if !noconfirm {
            ui::prompt_orphan_selection(&orphans, false)
        } else {
            orphans
                .iter()
                .filter(|o| o.optional_for.is_empty())
                .map(|o| o.name.clone())
                .collect()
        };
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
    let p = Path::new(cmd);
    if p.is_absolute() {
        return crate::utils::is_executable_file(p);
    }

    if let Ok(path) = env::var("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join(cmd);
            if crate::utils::is_executable_file(&candidate) {
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
    let glob_pins = ResolverEngine::compile_glob_pins(pins);
    ResolverEngine::find_matching_pin_rule(pkg, pins, &glob_pins)
}

fn check_and_prompt_smart_unpin(mut config: Config, removed_pkgs: &[String], noconfirm: bool) {
    let mut config_dirty = false;
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
            eprintln!(
                "{}",
                format!("Warning: Failed to save updated config: {}", e).yellow()
            );
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
        println!(
            "{} Cleaning package cache via {} -Sc...",
            "::".cyan(),
            helper
        );
        Command::new(helper).arg("-Sc").args(extra_args).status()
    } else {
        println!(
            "{} Cleaning package cache via sudo pacman -Sc...",
            "::".cyan()
        );
        Command::new("sudo")
            .arg("pacman")
            .arg("-Sc")
            .args(extra_args)
            .status()
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
        eprintln!(
            "{}",
            "Error: No package targets specified for removal.".red()
        );
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

    let status = Command::new("sudo").arg("pacman").args(&cmd_args).status();

    match status {
        Ok(s) if s.success() => {
            let noconfirm = flags.iter().any(|f| f == "--noconfirm");
            check_and_prompt_smart_unpin(config, &targets, noconfirm);
            let packages = targets
                .iter()
                .map(|name| serde_json::json!({ "name": name, "target": name }))
                .collect();
            let command = format!("sudo pacman {}", cmd_args.join(" "));
            if let Err(e) = TransactionJournal::record_transaction("remove", packages, &command) {
                eprintln!(
                    "{}",
                    format!("Warning: Failed to record removal transaction: {}", e).yellow()
                );
            }
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
        print!(
            "{}",
            "Are you sure you want to reset all configurations and pins to default? [y/N] ".bold()
        );
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
        eprintln!(
            "{}",
            format!("Warning: Could not create backup file: {}", e).yellow()
        );
    } else {
        println!(
            "  {}",
            format!("📦 Backup created at {}", bak_path.display()).dimmed()
        );
    }

    let default_cfg = Config::default();
    if let Err(e) = config::save_config(&default_cfg) {
        eprintln!("{}", format!("Error resetting configuration: {}", e).red());
        exit(1);
    }

    println!(
        "{}",
        "✔ All configurations, pins, exclusions, and delays have been reset to default."
            .green()
            .bold()
    );
    println!("  Run 'pacpin init' to configure preferences from scratch.\n");
}

fn cmd_print_uris(config: &Config, targets: &[String]) {
    if targets.is_empty() {
        eprintln!("{}", "Error: No targets specified.".red());
        eprintln!("Usage: pacpin -Sp [repo/]pkg ...");
        exit(1);
    }
    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    let mut stdout = io::stdout().lock();
    for target in targets {
        let (repo, pkg) = sandbox::parse_target(target);
        match manager.resolve_download_urls(repo, pkg) {
            Ok(urls) => {
                for target_pkg in urls {
                    if writeln!(stdout, "{}", target_pkg.url).is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                eprintln!("{}", format!("Error resolving '{}': {}", target, e).red());
                exit(1);
            }
        }
    }
}

fn cmd_sync_list(config: &Config, repos: &[String]) {
    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    let alpm = manager.handle();
    let local_db = alpm.localdb();

    let filter_repos: std::collections::HashSet<&str> = repos.iter().map(|s| s.as_str()).collect();
    let mut stdout = io::stdout().lock();

    for db in alpm.syncdbs() {
        if !filter_repos.is_empty() && !filter_repos.contains(db.name()) {
            continue;
        }
        for pkg in db.pkgs() {
            let res = if let Ok(local_pkg) = local_db.pkg(pkg.name()) {
                if local_pkg.version() == pkg.version() {
                    writeln!(
                        stdout,
                        "{} {} {} [installed]",
                        db.name(),
                        pkg.name(),
                        pkg.version()
                    )
                } else {
                    writeln!(
                        stdout,
                        "{} {} {} [installed: {}]",
                        db.name(),
                        pkg.name(),
                        pkg.version(),
                        local_pkg.version()
                    )
                }
            } else {
                writeln!(stdout, "{} {} {}", db.name(), pkg.name(), pkg.version())
            };
            if res.is_err() {
                return;
            }
        }
    }
}

fn cmd_sync_groups(config: &Config, groups: &[String]) {
    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };

    let alpm = manager.handle();
    let filter_groups: std::collections::HashSet<&str> =
        groups.iter().map(|s| s.as_str()).collect();
    let mut stdout = io::stdout().lock();

    if filter_groups.is_empty() {
        let mut all_groups = std::collections::BTreeSet::new();
        for db in alpm.syncdbs() {
            for pkg in db.pkgs() {
                for grp in pkg.groups() {
                    all_groups.insert(grp.to_string());
                }
            }
        }
        for grp in all_groups {
            if writeln!(stdout, "{}", grp).is_err() {
                return;
            }
        }
    } else {
        for db in alpm.syncdbs() {
            for pkg in db.pkgs() {
                for grp in pkg.groups() {
                    if filter_groups.contains(grp) {
                        if writeln!(stdout, "{} {}", grp, pkg.name()).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

fn print_help() {
    print_banner();
    println!("\nUsage: pacpin <command> [options]");
    println!();
    println!("{}", "Core Package Operations:".bold());
    println!("  check, -Qu                   Check pending updates & verify pin protections");
    println!("  upgrade, -Syu [flags]        Execute safe upgrade with True Resolver");
    println!("    Flags:");
    println!("      -n, --dry-run            Show transaction summary without executing");
    println!("      -y, --refresh            Refresh sync databases (sudo pacman -Sy) first");
    println!("      -c, --clean              Prompt to clean all orphans during upgrade");
    println!("      --noconfirm              Bypass interactive confirmation prompt");
    println!("  -S [flags] [repo/]pkg        Install packages (auto-pins repo/pkg with companion cascade)");
    println!("  -Sp, --print-uris <pkg...>   Print package and dependency download URIs");
    println!("  -Sl, --list [repo...]        List packages in sync repositories");
    println!("  -Sg, --groups [group...]     List package groups or packages in a group");
    println!("  remove, rm <pkg...>          Remove package(s) and unneeded dependencies (-Rns + smart unpin)");
    println!("  search, -Ss <query...>       Search official repositories and AUR simultaneously");
    println!("  info, -Si <pkg...>           Show detailed package information (-Si / AUR)");
    println!("  clean, -Sc                   Clean pacman and AUR build cache");
    println!();
    println!("{}", "Declarative Rules & Configuration:".bold());
    println!(
        "  repos                        Interactive BIOS-style repository search priority menu"
    );
    println!(
        "  pin <repo> <pkg...>          Lock package(s) or wildcards to a designated repository"
    );
    println!(
        "  pin <repo> -f <file>         Batch pin packages from a text file (one package per line)"
    );
    println!("  unpin <pkg>                  Remove a repository pin rule");
    println!("  delay <pkg> <days>           Set a stability delay buffer on a package");
    println!("  undelay <pkg>                Remove a package stability delay rule");
    println!(
        "  list                         List active pins, exclusions, delays, and repo priority"
    );
    println!("  reset [-f]                   Reset all configurations, pins, and delays to default (-f force)");
    println!("  init [--reset]               Interactive onboarding wizard (use --reset for clean slate)");
    println!();
    println!("{}", "System Maintenance & Hygiene:".bold());
    println!(
        "  orphans [-c]                 Inspect or remove orphaned dependencies (-c to clean)"
    );
    println!("  autoremove                   Alias for 'pacpin orphans -c'");
    println!("  keep, adopt <pkg...>         Mark package(s) as explicitly installed (silences orphan warnings)");
    println!("  needrestart                  Inspect processes holding outdated libraries or kernel in RAM");
    println!();
    println!("{}", "Power Tools & Ephemeral Execution:".bold());
    println!("  run [options] [repo/]pkg     Run package directly on host without installing (like 'nix run')");
    println!("  try [options] [repo/]pkg     Run package in isolated ephemeral sandbox (air-gapped bwrap)");
    println!(
        "  history [id]                 View transaction history timeline or inspect a transaction"
    );
    println!("  rollback [id] [-n]           Restore previous package versions from cache");
    println!();
    println!("{}", "Pacman & AUR Drop-in Aliases:".bold());
    println!("  pacpin -Syu                  Alias for 'pacpin upgrade -y'");
    println!("  pacpin -Qu                   Alias for 'pacpin check'");
    println!("  pacpin -Ss <query...>        Search repositories and AUR");
    println!("  pacpin -Si <pkg...>          Show remote/AUR package metadata");
    println!("  pacpin -Qi <pkg...>          Show locally installed package info");
    println!("  pacpin -R / -Rns <pkg...>    Remove packages (with smart pin cleanup)");
    println!("  pacpin -Q / -Qo / -Ql        Pacman query and package inspection");
    println!("  pacpin -U <pkg.tar.zst>      Install local package archive");
    println!("  pacpin -F / -Fy              Pacman files database operations");
    println!("  pacpin -T <deps...>          Check dependency requirements");
    println!();
}

fn print_version() {
    println!("pacpin {}", env!("CARGO_PKG_VERSION"));
    println!("Copyright (C) 2026 Gyan <330976822+gyanhavingsyntropy@users.noreply.github.com>");
    println!("License GPLv3+: GNU GPL version 3 or later <https://gnu.org/licenses/gpl.html>");
    println!("This is free software: you are free to change and redistribute it.");
    println!("There is NO WARRANTY, to the extent permitted by law.");
}

pub fn parse_pacman_cli_args(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut targets = Vec::new();
    let mut flags = Vec::new();
    let mut after_double_dash = false;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        if after_double_dash {
            targets.push(arg.clone());
            i += 1;
            continue;
        }

        if arg == "--" {
            after_double_dash = true;
            i += 1;
            continue;
        }

        let takes_arg = arg == "--ignore"
            || arg == "--ignoregroup"
            || arg == "--config"
            || arg == "--cachedir"
            || arg == "--root"
            || arg == "-r"
            || arg == "--dbpath"
            || arg == "-b"
            || arg == "--logfile"
            || arg == "--gpgdir"
            || arg == "--hookdir"
            || arg == "--overwrite"
            || arg == "--assume-installed"
            || arg == "--color"
            || arg == "--arch"
            || arg == "--print-format";

        if takes_arg {
            flags.push(arg.clone());
            if i + 1 < args.len() {
                flags.push(args[i + 1].clone());
                i += 1;
            }
        } else if arg.starts_with("--ignore=")
            || arg.starts_with("--ignoregroup=")
            || arg.starts_with("--config=")
            || arg.starts_with("--cachedir=")
            || arg.starts_with("--root=")
            || arg.starts_with("--dbpath=")
            || arg.starts_with("--logfile=")
            || arg.starts_with("--gpgdir=")
            || arg.starts_with("--hookdir=")
            || arg.starts_with("--overwrite=")
            || arg.starts_with("--assume-installed=")
            || arg.starts_with("--color=")
            || arg.starts_with("--arch=")
            || arg.starts_with("--print-format=")
            || arg.starts_with('-')
        {
            flags.push(arg.clone());
        } else {
            targets.push(arg.clone());
        }

        i += 1;
    }

    (targets, flags)
}

pub fn is_upgrade_invocation(args: &[String]) -> bool {
    if args.is_empty() {
        return false;
    }
    let first = &args[0];
    if first == "upgrade" || first == "up" {
        return true;
    }
    if !first.starts_with("-S") {
        return false;
    }

    let (targets, flags_list) = parse_pacman_cli_args(&args[1..]);
    if !targets.is_empty() {
        return false;
    }

    // Collect all short flag characters across first arg and flags_list
    let mut flags = std::collections::HashSet::new();
    for a in std::iter::once(first).chain(flags_list.iter()) {
        if a.starts_with('-') && !a.starts_with("--") {
            for c in a[1..].chars() {
                flags.insert(c);
            }
        }
    }

    // Reject if other -S sub-operations are requested:
    // s (search), i (info), w (downloadonly), p (print-uris), l (list), g (groups), c (clean without u)
    if flags.contains(&'s')
        || flags.contains(&'i')
        || flags.contains(&'w')
        || flags.contains(&'p')
        || flags.contains(&'l')
        || flags.contains(&'g')
        || (flags.contains(&'c') && !flags.contains(&'u'))
        || flags_list.iter().any(|a| {
            a == "--search"
                || a == "--info"
                || a == "--downloadonly"
                || a == "--print"
                || a == "--print-uris"
                || a == "--list"
                || a == "--groups"
        })
    {
        return false;
    }

    // It's an upgrade if flags contains 'u' or 'y' or long options
    flags.contains(&'u')
        || flags.contains(&'y')
        || flags_list
            .iter()
            .any(|a| a == "--sysupgrade" || a == "--refresh")
}

pub fn pacman_files_needs_sudo(args: &[String]) -> bool {
    let (_, flags) = parse_pacman_cli_args(args);
    flags.iter().any(|a| {
        if a == "--refresh" {
            true
        } else if a.starts_with('-') && !a.starts_with("--") {
            a.contains('y')
        } else {
            false
        }
    })
}

fn main() {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    if std::env::var_os("NO_COLOR").is_some()
        || (!io::stdout().is_terminal() && std::env::var_os("CLICOLOR_FORCE").is_none())
    {
        colored::control::set_override(false);
    }

    let args: Vec<String> = env::args().skip(1).collect();

    if args
        .first()
        .map(|s| s == "-v" || s == "-V" || s == "--version")
        .unwrap_or(false)
    {
        print_version();
        exit(0);
    }

    if args
        .first()
        .map(|s| s == "-h" || s == "--help" || s == "help")
        .unwrap_or(false)
    {
        print_help();
        exit(0);
    }

    if args
        .first()
        .map(|s| s == "reset" || s == "config-reset")
        .unwrap_or(false)
        || (args.len() >= 2 && args[0] == "config" && args[1] == "reset")
    {
        let force = args
            .iter()
            .any(|a| a == "-f" || a == "--force" || a == "-y");
        cmd_reset(force);
        exit(0);
    }

    if args
        .first()
        .map(|s| s == "init" || s == "setup")
        .unwrap_or(false)
    {
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
    if cmd == "repos" || cmd == "repo-order" || cmd == "priority" {
        cmd_repos(config);
        exit(0);
    } else if cmd == "check" || cmd == "-Qu" {
        cmd_check(&config);
    } else if is_upgrade_invocation(&args) {
        let (_targets, extra_flags) = parse_pacman_cli_args(&args[1..]);
        let dry_run = args.iter().any(|a| a == "-n" || a == "--dry-run")
            || args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a[1..].contains('n'));
        let refresh = args.iter().any(|a| a == "-y" || a == "--refresh")
            || args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a[1..].contains('y'));
        let noconfirm = args.iter().any(|a| a == "--noconfirm");
        let autoremove = args
            .iter()
            .any(|a| a == "-c" || a == "--clean" || a == "--autoremove")
            || args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a[1..].contains('c'));
        cmd_upgrade(
            &config,
            dry_run,
            refresh,
            noconfirm,
            autoremove,
            &extra_flags,
        );
    } else if cmd == "search"
        || cmd == "-Ss"
        || (cmd.starts_with("-S")
            && (cmd.contains('s') || args.iter().any(|a| a == "-s" || a == "--search")))
    {
        let query_args: Vec<String> = if cmd.starts_with("-S") {
            let mut q = Vec::new();
            if cmd.len() > 3 && cmd.starts_with("-Ss") {
                q.push(cmd[3..].to_string());
            }
            q.extend(
                args.iter()
                    .skip(1)
                    .filter(|a| *a != "-s" && *a != "--search" && !a.starts_with("-Ss"))
                    .cloned(),
            );
            q
        } else {
            args.iter().skip(1).cloned().collect()
        };
        cmd_search(&config, &query_args);
    } else if cmd == "info"
        || cmd == "-Si"
        || (cmd.starts_with("-S")
            && (cmd.contains('i') || args.iter().any(|a| a == "-i" || a == "--info")))
    {
        let pkg_args: Vec<String> = if cmd.starts_with("-S") {
            let mut p = Vec::new();
            if cmd.len() > 3 && cmd.starts_with("-Si") {
                p.push(cmd[3..].to_string());
            }
            p.extend(
                args.iter()
                    .skip(1)
                    .filter(|a| *a != "-i" && *a != "--info" && !a.starts_with("-Si"))
                    .cloned(),
            );
            p
        } else {
            args.iter().skip(1).cloned().collect()
        };
        cmd_info(&config, &pkg_args);
    } else if cmd == "clean"
        || cmd == "clean-cache"
        || cmd == "-Sc"
        || cmd == "-Scc"
        || (cmd.starts_with("-S")
            && (cmd.contains('c') || args.iter().any(|a| a == "-c" || a == "--clean"))
            && !is_upgrade_invocation(&args)
            && !args.iter().skip(1).any(|a| !a.starts_with('-')))
    {
        let extra_args: Vec<String> = args
            .iter()
            .skip(1)
            .filter(|a| *a != "-c" && *a != "--clean" && !a.starts_with("-Sc"))
            .cloned()
            .collect();
        cmd_clean(&config, &extra_args);
    } else if cmd.starts_with("-Sw")
        || (cmd.starts_with("-S") && args.iter().any(|a| a == "-w" || a == "--downloadonly"))
    {
        check_pacman_lock();
        let status = Command::new("sudo").arg("pacman").args(&args).status();
        match status {
            Ok(s) => exit(s.code().unwrap_or(0)),
            Err(e) => {
                eprintln!("{}", format!("Failed to run sudo pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd == "-Sp"
        || (cmd.starts_with("-S") && cmd.contains('p'))
        || (cmd.starts_with("-S")
            && args
                .iter()
                .any(|a| a == "-p" || a == "--print-uris" || a == "--print"))
    {
        let targets: Vec<String> = args
            .iter()
            .skip(1)
            .filter(|a| !a.starts_with('-'))
            .cloned()
            .collect();
        cmd_print_uris(&config, &targets);
        exit(0);
    } else if cmd == "-Sl"
        || (cmd.starts_with("-S") && cmd.contains('l'))
        || (cmd.starts_with("-S") && args.iter().any(|a| a == "-l" || a == "--list"))
    {
        let repos: Vec<String> = args
            .iter()
            .skip(1)
            .filter(|a| !a.starts_with('-'))
            .cloned()
            .collect();
        cmd_sync_list(&config, &repos);
        exit(0);
    } else if cmd == "-Sg"
        || (cmd.starts_with("-S") && cmd.contains('g'))
        || (cmd.starts_with("-S") && args.iter().any(|a| a == "-g" || a == "--groups"))
    {
        let groups: Vec<String> = args
            .iter()
            .skip(1)
            .filter(|a| !a.starts_with('-'))
            .cloned()
            .collect();
        cmd_sync_groups(&config, &groups);
        exit(0);
    } else if cmd.starts_with("-T") {
        let status = Command::new("pacman").args(&args).status();
        match status {
            Ok(s) => exit(s.code().unwrap_or(0)),
            Err(e) => {
                eprintln!("{}", format!("Failed to run pacman: {}", e).red());
                exit(1);
            }
        }
    } else if cmd == "-S"
        || cmd == "install"
        || (cmd.starts_with("-S") && {
            let (t, _) = parse_pacman_cli_args(&args[1..]);
            !t.is_empty()
        })
    {
        let (targets, mut flags) = parse_pacman_cli_args(&args[1..]);
        if cmd.len() > 2 && cmd.starts_with("-S") {
            let subflags = &cmd[2..];
            if subflags.contains('y') {
                flags.push("-y".to_string());
            }
            if subflags.contains('n') {
                flags.push("-n".to_string());
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
        let needs_sudo = pacman_files_needs_sudo(&args);
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
        let allow_partial = args
            .iter()
            .any(|a| a == "--allow-partial" || a == "--partial" || a == "-f" || a == "--force");
        for a in &args[1..] {
            if let Ok(id) = a.parse::<usize>() {
                tx_id = Some(id);
            }
        }
        TransactionJournal::rollback(tx_id, dry_run, allow_partial);
    } else if cmd == "run" {
        let mut opts = sandbox::TryOptions::for_run();
        let mut positional = Vec::new();
        let mut pass_through = Vec::new();
        let mut after_delimiter = false;

        let mut i = 1;
        while i < args.len() {
            let a = &args[i];
            if after_delimiter {
                pass_through.push(a.clone());
                i += 1;
                continue;
            }
            if a == "--" {
                after_delimiter = true;
                i += 1;
                continue;
            }
            if a == "--sandbox" || a == "--sandboxed" {
                opts.no_sandbox = false;
                opts.is_run_mode = false;
            } else if a == "--allow-unverified" {
                opts.allow_unverified = true;
            } else if a == "--bin" {
                if i + 1 < args.len() {
                    opts.bin = Some(args[i + 1].clone());
                    i += 1;
                }
            } else if a.starts_with("--bin=") {
                opts.bin = Some(a["--bin=".len()..].to_string());
            } else if positional.is_empty() && !a.starts_with('-') {
                positional.push(a.clone());
            } else {
                pass_through.push(a.clone());
            }
            i += 1;
        }

        if positional.is_empty() {
            eprintln!(
                "{}",
                "Error: 'pacpin run' requires a package target."
                    .red()
                    .bold()
            );
            eprintln!("Usage: pacpin run [options] [repo/]package [arguments...]");
            eprintln!("Runs an official, AUR, Flatpak, or Nix package directly on host without permanent installation.");
            eprintln!();
            eprintln!("Options:");
            eprintln!(
                "  --sandbox              Run in Bubblewrap container (unsandboxed by default)"
            );
            eprintln!("  --bin <name>           Specify exact executable binary name if package provides multiple");
            eprintln!("  --allow-unverified     Proceed even if repository metadata lacks a SHA256 checksum");
            eprintln!();
            eprintln!("Examples:");
            eprintln!("  pacpin run fastfetch");
            eprintln!("  pacpin run yt-dlp 'https://youtube.com/watch?v=...'");
            eprintln!("  pacpin run ffmpeg -i screencast.mkv output.mp4");
            eprintln!("  pacpin run jq . package.json");
            exit(1);
        }

        let pkg = &positional[0];
        sandbox::cmd_try(pkg, &pass_through, &opts);
    } else if cmd == "try" {
        let mut opts = sandbox::TryOptions::default();
        let mut positional = Vec::new();
        let mut pass_through = Vec::new();
        let mut after_delimiter = false;

        let mut i = 1;
        while i < args.len() {
            let a = &args[i];
            if after_delimiter {
                pass_through.push(a.clone());
                i += 1;
                continue;
            }
            if a == "--" {
                after_delimiter = true;
                i += 1;
                continue;
            }
            if a == "--no-sandbox" || a == "--bare" || a == "--unsandboxed" {
                opts.no_sandbox = true;
            } else if a == "--net" || a == "--network" || a == "--share-net" {
                opts.share_net = true;
            } else if a == "--rw" || a == "--rw-cwd" || a == "--write-cwd" {
                opts.rw_cwd = true;
            } else if a == "--gui" {
                opts.gui = true;
            } else if a == "--audio" {
                opts.audio = true;
            } else if a == "--allow-unverified" {
                opts.allow_unverified = true;
            } else if a == "--bin" {
                if i + 1 < args.len() {
                    opts.bin = Some(args[i + 1].clone());
                    i += 1;
                }
            } else if a.starts_with("--bin=") {
                opts.bin = Some(a["--bin=".len()..].to_string());
            } else if positional.is_empty() && !a.starts_with('-') {
                positional.push(a.clone());
            } else {
                pass_through.push(a.clone());
            }
            i += 1;
        }

        if positional.is_empty() {
            eprintln!(
                "{}",
                "Error: 'pacpin try' requires a package target."
                    .red()
                    .bold()
            );
            eprintln!("Usage: pacpin try [options] [repo/]package [arguments...]");
            eprintln!("Runs a package inside an isolated, air-gapped Bubblewrap container.");
            eprintln!();
            eprintln!("Options:");
            eprintln!("  --no-sandbox, --bare   Bypass Bubblewrap containerization and run directly on host");
            eprintln!("  --net, --network       Share host network access (unshared by default)");
            eprintln!("  --rw, --write-cwd      Mount current working directory read-write (read-only by default)");
            eprintln!("  --gui                  Grant X11/Wayland and DRI access for graphical applications");
            eprintln!("  --audio                Grant /dev/snd access for audio playback");
            eprintln!("  --bin <name>           Specify exact executable binary name if package provides multiple");
            eprintln!("  --allow-unverified     Proceed even if repository metadata lacks a SHA256 checksum");
            eprintln!();
            eprintln!("Examples:");
            eprintln!("  pacpin try jq . foo.json");
            eprintln!("  pacpin try --gui flatpak/org.gnome.Calculator");
            eprintln!("  pacpin try --net nix/ripgrep -i 'foo'");
            exit(1);
        }

        let pkg = &positional[0];
        sandbox::cmd_try(pkg, &pass_through, &opts);
    } else if cmd == "needrestart" || cmd == "restart-check" {
        restart::RestartInspector::print_restart_advisory(&[]);
        exit(0);
    } else if cmd == "delay" {
        if args.len() < 3 || args[2].parse::<u32>().is_err() {
            eprintln!(
                "{}",
                "Error: 'pacpin delay' requires <pkg> and <days>.".red()
            );
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
        let noconfirm = args.iter().any(|a| a == "--noconfirm");
        cmd_orphans(clean, noconfirm);
    } else if cmd == "keep" || cmd == "adopt" {
        if args.len() < 2 {
            eprintln!(
                "{}",
                "Error: 'pacpin keep' requires at least one package name.".red()
            );
            eprintln!("Usage: pacpin keep <pkg1> [pkg2...]");
            exit(1);
        }
        let pkgs = &args[1..];
        println!(
            "{} Marking {} package(s) as explicitly installed (sudo pacman -D --asexplicit)...",
            "::".cyan(),
            pkgs.len()
        );
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
    } else if cmd == "pin" || cmd == "add-pin" || cmd == "add-pins" {
        let mut known_repos = AlpmManager::discover_repos();
        known_repos.push("aur".to_string());

        match parse_pin_args(&args[1..], &known_repos) {
            Ok((repo, patterns)) => cmd_pin(config, repo, patterns),
            Err(e) => {
                eprintln!("{}", format!("Error: {}", e).red());
                eprintln!("Usage: pacpin pin <repo> <pkg1> [pkg2...]");
                eprintln!("       pacpin pin <repo> -f <packages.txt>");
                eprintln!("       pacpin pin <pkg> <repo>");
                exit(1);
            }
        }
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

fn parse_pin_args(
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
        return Err("'pacpin pin' requires a repository name.".to_string());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_command_available() {
        assert!(is_command_available("sh") || is_command_available("bash"));
        assert!(!is_command_available("this_binary_does_not_exist_xyz123"));
    }

    #[test]
    fn test_is_command_available_permissions() {
        let temp_dir = std::env::temp_dir();
        let non_exec = temp_dir.join(format!("pacpin_non_exec_test_{}", std::process::id()));
        std::fs::write(&non_exec, b"#!/bin/sh\necho hi\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&non_exec).unwrap().permissions();
            perms.set_mode(0o644); // No executable bit
            std::fs::set_permissions(&non_exec, perms).unwrap();
        }
        assert!(!is_command_available(&non_exec.to_string_lossy()));
        let _ = std::fs::remove_file(&non_exec);
    }

    #[test]
    fn test_find_matching_pin_exact_and_glob() {
        let mut pins = BTreeMap::new();
        pins.insert("mesa".to_string(), "core".to_string());
        pins.insert("linux-*".to_string(), "extra".to_string());
        pins.insert("linux-firmware*".to_string(), "cachyos".to_string());

        assert_eq!(
            find_matching_pin("mesa", &pins),
            Some(("mesa".to_string(), "core".to_string(), true))
        );
        // Specificity tie-break: "linux-firmware*" must beat "linux-*" despite ASCII key order
        assert_eq!(
            find_matching_pin("linux-firmware-intel", &pins),
            Some(("linux-firmware*".to_string(), "cachyos".to_string(), false))
        );
        assert_eq!(
            find_matching_pin("linux-zen", &pins),
            Some(("linux-*".to_string(), "extra".to_string(), false))
        );
        assert_eq!(find_matching_pin("git", &pins), None);
    }

    #[test]
    fn test_parse_pin_args_legacy_syntax() {
        let known_repos = vec!["core".to_string(), "extra".to_string(), "aur".to_string()];
        let args = vec!["linux-firmware*".to_string(), "core".to_string()];
        let (repo, patterns) = parse_pin_args(&args, &known_repos).unwrap();
        assert_eq!(repo, "core");
        assert_eq!(patterns, vec!["linux-firmware*"]);
    }

    #[test]
    fn test_parse_pin_args_repo_first() {
        let known_repos = vec!["core".to_string(), "extra".to_string(), "aur".to_string()];
        let args = vec!["core".to_string(), "linux-firmware*".to_string()];
        let (repo, patterns) = parse_pin_args(&args, &known_repos).unwrap();
        assert_eq!(repo, "core");
        assert_eq!(patterns, vec!["linux-firmware*"]);
    }

    #[test]
    fn test_parse_pin_args_batch() {
        let known_repos = vec!["core".to_string(), "extra".to_string(), "aur".to_string()];
        let args = vec![
            "core".to_string(),
            "pkg1".to_string(),
            "pkg2".to_string(),
            "pkg3".to_string(),
        ];
        let (repo, patterns) = parse_pin_args(&args, &known_repos).unwrap();
        assert_eq!(repo, "core");
        assert_eq!(patterns, vec!["pkg1", "pkg2", "pkg3"]);
    }

    #[test]
    fn test_parse_pin_args_file() {
        let known_repos = vec!["core".to_string(), "extra".to_string(), "aur".to_string()];
        let tmp_file = std::env::temp_dir().join("pacpin_test_pkgs.txt");
        std::fs::write(&tmp_file, "pkgA\n# comment\npkgB\n\npkgC\n").unwrap();

        let args = vec![
            "core".to_string(),
            "-f".to_string(),
            tmp_file.to_str().unwrap().to_string(),
        ];
        let (repo, patterns) = parse_pin_args(&args, &known_repos).unwrap();
        assert_eq!(repo, "core");
        assert_eq!(patterns, vec!["pkgA", "pkgB", "pkgC"]);

        let _ = std::fs::remove_file(&tmp_file);
    }

    #[test]
    fn test_pure_orphan_filtering() {
        use crate::db::OrphanPackage;
        let orphans = vec![
            OrphanPackage {
                name: "pure-orphan".to_string(),
                version: "1.0".to_string(),
                desc: "test".to_string(),
                isize: 100,
                optional_for: Vec::new(),
                dropped_by: Vec::new(),
                is_projected: false,
            },
            OrphanPackage {
                name: "opt-plugin".to_string(),
                version: "1.0".to_string(),
                desc: "test plugin".to_string(),
                isize: 200,
                optional_for: vec!["ffmpeg".to_string()],
                dropped_by: Vec::new(),
                is_projected: false,
            },
        ];

        let pure: Vec<String> = orphans
            .iter()
            .filter(|o| o.optional_for.is_empty())
            .map(|o| o.name.clone())
            .collect();

        assert_eq!(pure, vec!["pure-orphan"]);
    }

    #[test]
    fn test_is_upgrade_invocation() {
        let to_vec = |slice: &[&str]| slice.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Valid upgrade forms
        assert!(is_upgrade_invocation(&to_vec(&["upgrade"])));
        assert!(is_upgrade_invocation(&to_vec(&["up"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Syu"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Suy"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Su"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Sy"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Syuu"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-y", "-u"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-u", "-y"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-u"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-y"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "-u", "-n"])));
        assert!(is_upgrade_invocation(&to_vec(&["-Syu", "--noconfirm"])));
        assert!(is_upgrade_invocation(&to_vec(&["-S", "--sysupgrade"])));

        // Non-upgrade forms (should not match upgrade)
        assert!(!is_upgrade_invocation(&to_vec(&["-S"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-S", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Syu", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Ss", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-S", "-s", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Si", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-S", "-i", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Sc"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-S", "-c"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Sw", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Sp", "ripgrep"])));
        assert!(!is_upgrade_invocation(&to_vec(&["check"])));
        assert!(!is_upgrade_invocation(&to_vec(&["-Qu"])));

        // Upgrade with option-consuming flags
        assert!(is_upgrade_invocation(&to_vec(&[
            "-Syu", "--ignore", "linux"
        ])));
        assert!(is_upgrade_invocation(&to_vec(&["-Syu", "--ignore=linux"])));
        assert!(is_upgrade_invocation(&to_vec(&[
            "-S",
            "-u",
            "--overwrite",
            "/usr/*"
        ])));
        assert!(is_upgrade_invocation(&to_vec(&[
            "-Syu",
            "--assume-installed",
            "foo:1.0"
        ])));
    }

    #[test]
    fn test_parse_pacman_cli_args() {
        let to_vec = |slice: &[&str]| slice.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Separates option arguments from targets
        let (targets, flags) =
            parse_pacman_cli_args(&to_vec(&["--ignore", "linux", "--noconfirm", "neovim"]));
        assert_eq!(targets, vec!["neovim"]);
        assert_eq!(flags, vec!["--ignore", "linux", "--noconfirm"]);

        // Handles --ignore=linux equals syntax
        let (targets, flags) = parse_pacman_cli_args(&to_vec(&["--ignore=linux", "ripgrep"]));
        assert_eq!(targets, vec!["ripgrep"]);
        assert_eq!(flags, vec!["--ignore=linux"]);

        // Handles double dash -- to treat everything after as targets
        let (targets, flags) = parse_pacman_cli_args(&to_vec(&["-S", "--", "--ignore", "foo"]));
        assert_eq!(flags, vec!["-S"]);
        assert_eq!(targets, vec!["--ignore", "foo"]);

        // Handles config, dbpath, root, overwrite flags consuming arguments
        let (targets, flags) = parse_pacman_cli_args(&to_vec(&[
            "--config",
            "/etc/pacman.conf",
            "-b",
            "/var/lib/pacman",
            "--overwrite",
            "/usr/share/*",
            "git",
        ]));
        assert_eq!(targets, vec!["git"]);
        assert_eq!(
            flags,
            vec![
                "--config",
                "/etc/pacman.conf",
                "-b",
                "/var/lib/pacman",
                "--overwrite",
                "/usr/share/*"
            ]
        );
    }

    #[test]
    fn test_pacman_files_needs_sudo() {
        let to_vec = |slice: &[&str]| slice.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Read-only queries should NOT require sudo
        assert!(!pacman_files_needs_sudo(&to_vec(&["-F", "python"])));
        assert!(!pacman_files_needs_sudo(&to_vec(&["-Fl", "ripgrep"])));
        assert!(!pacman_files_needs_sudo(&to_vec(&["-Fs", "libssl.so"])));
        assert!(!pacman_files_needs_sudo(&to_vec(&[
            "-F",
            "-b",
            "/var/lib/pacman",
            "python"
        ])));

        // Sync / refresh actions DO require sudo
        assert!(pacman_files_needs_sudo(&to_vec(&["-Fy"])));
        assert!(pacman_files_needs_sudo(&to_vec(&["-Fyy"])));
        assert!(pacman_files_needs_sudo(&to_vec(&["-F", "-y"])));
        assert!(pacman_files_needs_sudo(&to_vec(&["-F", "--refresh"])));
        assert!(pacman_files_needs_sudo(&to_vec(&["-Fy", "python"])));
    }

    #[test]
    fn test_find_matching_pin_exact_and_wildcard() {
        let mut pins = std::collections::BTreeMap::new();
        pins.insert("linux-cachyos*".to_string(), "cachyos".to_string());
        pins.insert("neovim".to_string(), "extra".to_string());

        let res_exact = find_matching_pin("neovim", &pins);
        assert_eq!(res_exact, Some(("neovim".to_string(), "extra".to_string(), true)));

        let res_wildcard = find_matching_pin("linux-cachyos-headers", &pins);
        assert_eq!(res_wildcard, Some(("linux-cachyos*".to_string(), "cachyos".to_string(), false)));

        let res_none = find_matching_pin("ripgrep", &pins);
        assert_eq!(res_none, None);
    }
}
