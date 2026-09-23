use std::io::{self, IsTerminal, Write};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use colored::*;

use crate::aur;
use crate::config::{save_config, Config};
use crate::db::AlpmManager;
use crate::journal::TransactionJournal;
use crate::resolver::ResolverEngine;
use crate::ui::prompt_multiselect;

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub needed: bool,
    pub noconfirm: bool,
    pub refresh: bool,
    pub dry_run: bool,
    pub forwarded_flags: Vec<String>,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            needed: true,
            noconfirm: false,
            refresh: false,
            dry_run: false,
            forwarded_flags: Vec::new(),
        }
    }
}

/// Centralized package installation engine for pacpin.
///
/// Handles target normalization, repository priority routing, AUR fallback,
/// stability delay checks on new installs, companion package detection,
/// sudo pacman / AUR helper execution, and transaction journaling.
pub fn install_packages(
    config: &mut Config,
    targets: &[String],
    options: &InstallOptions,
) -> Result<Vec<String>, String> {
    if targets.is_empty() {
        return Err("No package targets specified.".to_string());
    }

    let lock_path = AlpmManager::get_dbpath().join("db.lck");
    if lock_path.exists() {
        return Err(format!(
            "Pacman database is locked ({}). Another package management process is currently running.",
            lock_path.display()
        ));
    }

    if options.refresh && !options.dry_run {
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
                return Err(format!(
                    "Database refresh failed with exit code {}.",
                    s.code().unwrap_or(1)
                ));
            }
            Err(e) => {
                return Err(format!("Failed to run sudo pacman: {}", e));
            }
        }
    }

    let manager = AlpmManager::with_repo_order(&config.repo_order)
        .map_err(|e| format!("Error initializing ALPM: {}", e))?;

    let mut pacman_targets = Vec::new();
    let mut aur_targets = Vec::new();
    let mut pending_pins: Vec<(String, String)> = Vec::new();

    let mut known_repos = manager.repos().to_vec();
    known_repos.push("aur".to_string());

    let glob_delays = if config.features.stability_delays {
        ResolverEngine::compile_glob_delays(&config.delay)
    } else {
        Vec::new()
    };

    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    for target in targets {
        if target.contains('/') {
            let parts: Vec<&str> = target.splitn(2, '/').collect();
            let repo = parts[0];
            let pkg = parts[1];

            if !known_repos.iter().any(|r| r == repo) {
                return Err(format!(
                    "Repository '[{}]' is not recognized. Configured repositories: {}",
                    repo,
                    known_repos.join(", ")
                ));
            }

            // Check stability delay on new install target
            check_target_delay(
                pkg,
                Some(repo),
                config,
                &glob_delays,
                &manager,
                now_epoch,
                options,
            )?;

            if repo.eq_ignore_ascii_case("aur") {
                let companions = if io::stdin().is_terminal() {
                    manager.find_companions(pkg, "aur", &config.pins)
                } else {
                    Vec::new()
                };
                let mut to_install_repo = vec![pkg.to_string()];
                if !companions.is_empty() {
                    let title = format!(
                        "'{}' has companion packages in [aur] to avoid version mismatches",
                        pkg
                    );
                    let selected = prompt_multiselect(&title, "aur", &companions);
                    to_install_repo.extend(selected);
                }
                for p in to_install_repo {
                    pending_pins.push((p.clone(), "aur".to_string()));
                    aur_targets.push(p);
                }
            } else {
                let companions = if io::stdin().is_terminal() {
                    manager.find_companions(pkg, repo, &config.pins)
                } else {
                    Vec::new()
                };
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
                check_target_delay(
                    target,
                    None,
                    config,
                    &glob_delays,
                    &manager,
                    now_epoch,
                    options,
                )?;
                pacman_targets.push(target.clone());
            } else {
                // Query the AUR only after ruling out every configured sync repository
                let aur_query = vec![target.clone()];
                let aur_map = aur::query_aur(&aur_query);
                if aur_map.contains_key(target) {
                    check_target_delay(
                        target,
                        Some("aur"),
                        config,
                        &glob_delays,
                        &manager,
                        now_epoch,
                        options,
                    )?;
                    aur_targets.push(target.clone());
                    pending_pins.push((target.clone(), "aur".to_string()));
                } else {
                    // Preserve pacman's normal error reporting for packages unknown to both
                    pacman_targets.push(target.clone());
                }
            }
        }
    }

    let mut command_strs = Vec::new();
    if options.refresh {
        command_strs.push("sudo pacman -Sy".to_string());
    }

    if !pacman_targets.is_empty() {
        let mut cmd = vec!["sudo", "pacman", "-S"];
        if options.needed {
            cmd.push("--needed");
        }
        if options.noconfirm {
            cmd.push("--noconfirm");
        }
        for f in &options.forwarded_flags {
            cmd.push(f.as_str());
        }
        let mut s = cmd.join(" ");
        s.push(' ');
        s.push_str(&pacman_targets.join(" "));
        command_strs.push(s);
    }

    if !aur_targets.is_empty() {
        let helper = &config.options.helper;
        let mut cmd = vec![helper.as_str(), "-S", "--aur"];
        if options.needed {
            cmd.push("--needed");
        }
        if options.noconfirm {
            cmd.push("--noconfirm");
        }
        for f in &options.forwarded_flags {
            cmd.push(f.as_str());
        }
        let mut s = cmd.join(" ");
        s.push(' ');
        s.push_str(&aur_targets.join(" "));
        command_strs.push(s);
    }

    let full_cmd = command_strs.join(" && ");

    if options.dry_run {
        println!("\n{}", "Dry-Run: Synthesized Installation Commands:".bold());
        println!("  ➔ {}", full_cmd.cyan());
        return Ok(targets.to_vec());
    }

    println!("\n{} {}", ":: Executing:".cyan(), full_cmd.bold());
    let mut installed_targets: Vec<String> = Vec::new();
    let mut executed_commands: Vec<String> = Vec::new();
    let mut exit_status = Ok(std::process::ExitStatus::default());
    let mut all_success = true;

    if !pacman_targets.is_empty() {
        let mut args = vec!["pacman", "-S"];
        if options.needed {
            args.push("--needed");
        }
        if options.noconfirm {
            args.push("--noconfirm");
        }
        for f in &options.forwarded_flags {
            args.push(f.as_str());
        }
        args.extend(pacman_targets.iter().map(|s| s.as_str()));
        let status = Command::new("sudo").args(&args).status();
        match status {
            Ok(s) if s.success() => {
                installed_targets.extend(pacman_targets.clone());
                let mut cmd_parts = vec!["sudo", "pacman", "-S"];
                if options.needed {
                    cmd_parts.push("--needed");
                }
                if options.noconfirm {
                    cmd_parts.push("--noconfirm");
                }
                for f in &options.forwarded_flags {
                    cmd_parts.push(f.as_str());
                }
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
        if options.needed {
            args.push("--needed");
        }
        if options.noconfirm {
            args.push("--noconfirm");
        }
        for f in &options.forwarded_flags {
            args.push(f.as_str());
        }
        args.extend(aur_targets.iter().map(|s| s.as_str()));
        let status = Command::new(helper).args(&args).status();
        match status {
            Ok(s) if s.success() => {
                installed_targets.extend(aur_targets.clone());
                let mut cmd_parts = vec![helper.as_str(), "-S", "--aur"];
                if options.needed {
                    cmd_parts.push("--needed");
                }
                if options.noconfirm {
                    cmd_parts.push("--noconfirm");
                }
                for f in &options.forwarded_flags {
                    cmd_parts.push(f.as_str());
                }
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

        if confirmed_pins_added && !options.dry_run {
            if let Err(e) = save_config(config) {
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
            Ok(s) => Err(format!(
                "Installation command exited with code {}.",
                s.code().unwrap_or(1)
            )),
            Err(e) => Err(format!("Installation execution failed: {}", e)),
        }
    } else {
        Ok(installed_targets)
    }
}

/// Checks whether an uninstalled package target violates an active stability delay rule.
///
/// Because Arch Linux official mirrors only retain the single latest version of any package,
/// a brand-new package cannot fetch older versions from mirrors. If the package was released
/// more recently than the configured delay buffer, this function provides an informative
/// advisory and prompts the user before proceeding.
fn check_target_delay(
    pkg: &str,
    repo: Option<&str>,
    config: &Config,
    glob_delays: &[(String, glob::Pattern, u32)],
    manager: &AlpmManager,
    now_epoch: i64,
    options: &InstallOptions,
) -> Result<(), String> {
    if !config.features.stability_delays {
        return Ok(());
    }

    // Delay buffers only apply to uninstalled targets; already-installed packages
    // are governed during system upgrade evaluations.
    if manager.handle().localdb().pkg(pkg).is_ok() {
        return Ok(());
    }

    let req_days = match ResolverEngine::match_delay_days(pkg, &config.delay, glob_delays) {
        Some(d) => d,
        None => return Ok(()),
    };

    // Determine candidate build date
    let build_date: Option<i64> = if let Some(r) = repo {
        if r.eq_ignore_ascii_case("aur") {
            let aur_map = aur::query_aur(&[pkg.to_string()]);
            aur_map.get(pkg).and_then(|item| item.last_modified)
        } else {
            manager
                .handle()
                .syncdbs()
                .iter()
                .find(|d| d.name() == r)
                .and_then(|d| d.pkg(pkg).ok())
                .map(|p| p.build_date())
        }
    } else {
        manager
            .repos()
            .iter()
            .find_map(|r| {
                manager
                    .handle()
                    .syncdbs()
                    .iter()
                    .find(|d| d.name() == r.as_str())
                    .and_then(|d| d.pkg(pkg).ok())
            })
            .map(|p| p.build_date())
    };

    if let Some(bdate) = build_date {
        let age_days = (now_epoch - bdate) as f64 / 86400.0;
        let req_days_f = req_days as f64;
        if age_days < req_days_f {
            let days_left = ((req_days_f - age_days + 0.99) as u32).max(1);
            if options.noconfirm || !io::stdin().is_terminal() {
                println!(
                    "{} Package '{}' was released {:.1} days ago (within your {}d stability delay buffer, {}d remaining). Proceeding in non-interactive mode.",
                    ":: Warning:".yellow().bold(),
                    pkg,
                    age_days.max(0.0),
                    req_days,
                    days_left
                );
            } else {
                print!(
                    "\n{} Package '{}' was released {:.1} days ago (within your {}d stability delay buffer, {}d remaining).\n   Arch mirrors only host the latest version. Proceed with installation? [Y/n] ",
                    ":: Notice:".yellow().bold(),
                    pkg,
                    age_days.max(0.0),
                    req_days,
                    days_left
                );
                let _ = io::stdout().flush();
                let mut input = String::new();
                if io::stdin().read_line(&mut input).is_ok() {
                    let trimmed = input.trim().to_lowercase();
                    if trimmed == "n" || trimmed == "no" {
                        return Err(format!(
                            "Installation of '{}' cancelled due to active {}-day stability delay policy ({}d remaining).",
                            pkg, req_days, days_left
                        ));
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_install_options_default() {
        let opts = InstallOptions::default();
        assert!(opts.needed);
        assert!(!opts.noconfirm);
        assert!(!opts.refresh);
        assert!(!opts.dry_run);
        assert!(opts.forwarded_flags.is_empty());
    }

    #[test]
    fn test_empty_targets_rejected() {
        let mut config = Config::default();
        let opts = InstallOptions::default();
        let res = install_packages(&mut config, &[], &opts);
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "No package targets specified.");
    }

    #[test]
    fn test_unknown_repo_rejected() {
        let mut config = Config::default();
        let opts = InstallOptions::default();
        let res = install_packages(&mut config, &["nonexistent_repo/foobar".to_string()], &opts);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("Repository '[nonexistent_repo]' is not recognized"));
    }
}
