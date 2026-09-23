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

use std::io::{self, IsTerminal, Write};
use std::process::{exit, Command};
use std::thread;

use colored::Colorize;

use crate::commands::check_pacman_lock;
use crate::config::Config;
use crate::db::{AlpmManager, OrphanPackage};
use crate::integrations::{self, IntegrationsManager};
use crate::journal::TransactionJournal;
use crate::orphans::OrphanManager;
use crate::pm;
use crate::resolver::{ResolvedPackage, ResolverEngine};
use crate::restart;
use crate::ui::{self, print_banner, render_transaction_view};

pub fn cmd_upgrade(
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

        let ext_refresh_providers = ext_providers.clone();
        let ext_refresh_handle = if !ext_refresh_providers.is_empty() {
            Some(thread::spawn(move || {
                IntegrationsManager::refresh_all_parallel(&ext_refresh_providers);
            }))
        } else {
            None
        };

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

        if let Some(h) = ext_refresh_handle {
            let _ = h.join();
        }
    }

    let ext_providers_for_check = ext_providers.clone();
    let ext_handle = thread::spawn(move || {
        IntegrationsManager::check_updates_parallel(&ext_providers_for_check)
    });

    let manager = match AlpmManager::with_repo_order(&config.repo_order) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", format!("Error initializing ALPM: {}", e).red());
            exit(1);
        }
    };
    for warn in manager.verify_syncdbs() {
        eprintln!("{}", format!(":: Warning: {}", warn).yellow());
    }
    let resolver = ResolverEngine::new(&manager);
    let res = resolver.resolve_all(config);
    let orphans = if config.features.smart_orphans {
        manager.get_orphans(&res.updates)
    } else {
        Vec::new()
    };
    let external_updates = ext_handle.join().unwrap_or_default();
    let updates = render_transaction_view(&res, &config.options.helper, &external_updates);
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
                    .filter(|o| o.is_pure())
                    .map(|o| o.name.clone())
                    .collect()
            } else {
                Vec::new()
            };

            if !selected_orphans.is_empty() {
                OrphanManager::execute_removal(&selected_orphans);
            }
            let remaining: Vec<OrphanPackage> = orphans
                .iter()
                .filter(|o| !selected_orphans.contains(&o.name))
                .cloned()
                .collect();
            OrphanManager::save_known(&remaining);
        } else if !dry_run && OrphanManager::load_known().is_none() {
            OrphanManager::save_known(&orphans);
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

    let mut command_strs = Vec::new();

    if !pacman_targets.is_empty() {
        let mut p_cmd = vec!["sudo", "pacman", "-S", "--needed", "--noconfirm"];
        p_cmd.extend(forwarded_pacman_flags.iter().copied());
        let mut s = p_cmd.join(" ");
        s.push(' ');
        s.push_str(&pacman_targets.join(" "));
        command_strs.push(s);
    }

    if !aur_targets.is_empty() {
        let helper = &config.options.helper;
        let mut a_cmd = vec![helper.as_str(), "-S", "--needed", "--aur", "--noconfirm"];
        a_cmd.extend(forwarded_pacman_flags.iter().copied());
        let mut s = a_cmd.join(" ");
        s.push(' ');
        s.push_str(&aur_targets.join(" "));
        command_strs.push(s);
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

    let conflicts = pm::check_package_conflicts(&manager, &pacman_targets);
    if !conflicts.is_empty() {
        println!("{}", ":: Note: Potential package conflict(s) detected:".yellow().bold());
        for (cand, inst) in &conflicts {
            println!(
                "   - '{}' conflicts with installed package '{}' (may require replacement)",
                cand.cyan(),
                inst.yellow()
            );
        }
        println!();
    }

    println!("{}", ":: Synthesized Upgrade Command:".cyan());
    println!("  {}\n", full_cmd.bold());

    let mut selected_orphans = Vec::new();

    if !noconfirm {
        if !io::stdin().is_terminal() {
            eprintln!(
                "{}",
                "Error: Cannot prompt for confirmation in non-interactive environment. Use --noconfirm to proceed."
                    .red()
            );
            exit(1);
        }
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
            .filter(|o| o.is_pure())
            .map(|o| o.name.clone())
            .collect();
    }

    let mut applied_updates: Vec<ResolvedPackage> = Vec::new();
    let mut executed_commands: Vec<String> = Vec::new();
    let mut exit_status = Ok(std::process::ExitStatus::default());
    let mut all_success = true;

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
        let remaining_orphans: Vec<OrphanPackage> = orphans
            .iter()
            .filter(|o| !selected_orphans.contains(&o.name))
            .cloned()
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

pub fn parse_install_flags(flags: &[String]) -> pm::InstallOptions {
    let dry_run = flags.iter().any(|f| f == "-n" || f == "--dry-run")
        || flags
            .iter()
            .any(|f| f.starts_with('-') && !f.starts_with("--") && f[1..].contains('n'));
    let refresh = flags.iter().any(|f| f == "-y" || f == "--refresh" || f == "-yy")
        || flags
            .iter()
            .any(|f| f.starts_with('-') && !f.starts_with("--") && f[1..].contains('y'));
    let sysupgrade = flags.iter().any(|f| f == "-u" || f == "--sysupgrade" || f == "-uu")
        || flags
            .iter()
            .any(|f| f.starts_with('-') && !f.starts_with("--") && f[1..].contains('u'));
    let noconfirm = flags.iter().any(|f| f == "--noconfirm");
    let needed = true;

    let forwarded_flags: Vec<String> = flags
        .iter()
        .filter(|f| {
            let s = f.as_str();
            if s == "--refresh"
                || s == "--sysupgrade"
                || s == "--dry-run"
                || s == "--noconfirm"
                || s == "--needed"
            {
                return false;
            }
            if s.starts_with('-') && !s.starts_with("--") {
                let chars = s[1..].chars();
                if chars.clone().all(|c| c == 'y' || c == 'u' || c == 'n') {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect();

    pm::InstallOptions {
        needed,
        noconfirm,
        refresh,
        sysupgrade,
        dry_run,
        forwarded_flags,
    }
}

pub fn cmd_install(mut config: Config, targets: &[String], flags: &[String]) {
    if targets.is_empty() {
        eprintln!("{}", "Error: No package targets specified.".red());
        eprintln!("Usage: pacpin -S [flags] [repo/]package ...");
        exit(1);
    }

    check_pacman_lock();
    print_banner();

    let options = parse_install_flags(flags);

    match pm::install(&mut config, targets, &options) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{}", format!("Error: {}", e).red());
            exit(1);
        }
    }
}
