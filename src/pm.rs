//! Centralized Package Management Engine for pacpin.
//!
//! Encapsulates all subprocess interactions with pacman and AUR helpers,
//! enforcing database locks, repository priority, delay buffers, companion
//! cascades, and transaction journaling.

use std::collections::{HashMap, HashSet};
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
    pub sysupgrade: bool,
    pub dry_run: bool,
    pub forwarded_flags: Vec<String>,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            needed: true,
            noconfirm: false,
            refresh: false,
            sysupgrade: false,
            dry_run: false,
            forwarded_flags: Vec::new(),
        }
    }
}

pub fn build_install_commands(
    pacman_targets: &[String],
    aur_targets: &[String],
    helper: &str,
    options: &InstallOptions,
    sysupgrade: bool,
) -> (Vec<String>, &'static str, &'static str) {
    let pacman_op = match (options.refresh, sysupgrade) {
        (true, true) => "-Syu",
        (false, true) => "-Su",
        _ => "-S",
    };

    let mut command_strs = Vec::new();
    if options.dry_run && options.refresh && !sysupgrade {
        command_strs.push("sudo pacman -Sy".to_string());
    }

    if !pacman_targets.is_empty() {
        let mut cmd = vec!["sudo", "pacman", pacman_op];
        if options.needed {
            cmd.push("--needed");
        }
        if options.noconfirm {
            cmd.push("--noconfirm");
        }
        for f in &options.forwarded_flags {
            cmd.push(f.as_str());
        }
        cmd.push("--");
        let mut s = cmd.join(" ");
        s.push(' ');
        let quoted_targets: Vec<String> = pacman_targets
            .iter()
            .map(|t| {
                if t.contains(' ') || t.contains('\'') || t.contains('"') {
                    crate::utils::shell_quote(t)
                } else {
                    t.clone()
                }
            })
            .collect();
        s.push_str(&quoted_targets.join(" "));
        command_strs.push(s);
    }

    let aur_op = if sysupgrade && pacman_targets.is_empty() {
        if options.refresh { "-Syu" } else { "-Su" }
    } else {
        "-S"
    };

    if !aur_targets.is_empty() {
        let mut cmd = vec![helper, aur_op, "--aur"];
        if options.needed {
            cmd.push("--needed");
        }
        if options.noconfirm {
            cmd.push("--noconfirm");
        }
        for f in &options.forwarded_flags {
            cmd.push(f.as_str());
        }
        cmd.push("--");
        let mut s = cmd.join(" ");
        s.push(' ');
        let quoted_targets: Vec<String> = aur_targets
            .iter()
            .map(|t| {
                if t.contains(' ') || t.contains('\'') || t.contains('"') {
                    crate::utils::shell_quote(t)
                } else {
                    t.clone()
                }
            })
            .collect();
        s.push_str(&quoted_targets.join(" "));
        command_strs.push(s);
    }

    (command_strs, pacman_op, aur_op)
}

/// Installs packages with repository resolution, companion cascading, and delay tree verification.
pub fn install(
    config: &mut Config,
    targets: &[String],
    options: &InstallOptions,
) -> Result<Vec<String>, String> {
    if targets.is_empty() {
        return Err("No package targets specified.".to_string());
    }

    if let Some(bad) = targets.iter().find(|t| !crate::utils::is_safe_install_target(t)) {
        return Err(format!("Refusing unsafe package target '{}'.", bad));
    }

    let wait_secs = options
        .forwarded_flags
        .iter()
        .enumerate()
        .find_map(|(idx, f)| {
            if f.starts_with("--wait=") {
                f.strip_prefix("--wait=").and_then(|s| s.parse::<u64>().ok())
            } else if f == "--wait" {
                if let Some(next) = options.forwarded_flags.get(idx + 1) {
                    if let Ok(secs) = next.parse::<u64>() {
                        return Some(secs);
                    }
                }
                Some(600)
            } else {
                None
            }
        });

    if let Some(secs) = wait_secs {
        wait_for_lock(secs)?;
    } else {
        check_lock()?;
    }

    let mut sysupgrade = options.sysupgrade;

    if options.refresh && !sysupgrade && !options.dry_run {
        println!(
            "{}",
            ":: Warning: Installing packages with '-y' (database refresh) without performing a full system upgrade".yellow().bold()
        );
        println!(
            "{}",
            "   causes a partial upgrade and can break your system (dependency and ABI mismatches).".yellow()
        );

        if io::stdin().is_terminal() && !options.noconfirm {
            print!(
                "{}",
                ":: Would you like to perform a full system upgrade (-Syu) instead? [Y/n] ".bold()
            );
            io::stdout().flush().ok();
            let mut input = String::new();
            if io::stdin().read_line(&mut input).is_ok() {
                let trimmed = input.trim().to_lowercase();
                if trimmed.is_empty() || trimmed == "y" || trimmed == "yes" {
                    sysupgrade = true;
                    println!("{}", ":: Elevating to full system upgrade (-Syu)...".cyan());
                } else {
                    print!(
                        "{}",
                        ":: Proceed with unsupported partial upgrade? [y/N] ".red().bold()
                    );
                    io::stdout().flush().ok();
                    let mut confirm = String::new();
                    if io::stdin().read_line(&mut confirm).is_ok() {
                        let c_trimmed = confirm.trim().to_lowercase();
                        if c_trimmed != "y" && c_trimmed != "yes" {
                            return Err("Aborted partial upgrade to prevent system breakage.".to_string());
                        }
                        println!(
                            "{}",
                            ":: Warning: Proceeding with partial upgrade at user request.".yellow()
                        );
                    } else {
                        return Err("Aborted.".to_string());
                    }
                }
            } else {
                return Err("Aborted.".to_string());
            }
        } else if !io::stdin().is_terminal() && !options.noconfirm {
            return Err(
                "Error: Partial upgrade requested ('-Sy' with targets) in non-interactive environment without --noconfirm. Use '-Syu' for a full safe upgrade, or provide --noconfirm.".to_string()
            );
        } else {
            println!(
                "{}",
                ":: Warning: Proceeding with partial upgrade due to --noconfirm.".yellow()
            );
        }
    } else if options.refresh && !sysupgrade && options.dry_run {
        println!(
            "{}",
            ":: Warning: Installing packages with '-y' (database refresh) without performing a full system upgrade".yellow().bold()
        );
        println!(
            "{}",
            "   causes a partial upgrade and can break your system (dependency and ABI mismatches).".yellow()
        );
    }

    if options.refresh && !options.dry_run {
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

            // Verify stability delay on the package and its uninstalled dependency tree
            verify_target_and_dependencies_delay(
                pkg,
                Some(repo),
                config,
                &glob_delays,
                &manager,
                now_epoch,
                options,
            )?;

            if repo.eq_ignore_ascii_case("aur") {
                let companions = if io::stdin().is_terminal() && !options.noconfirm {
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
                let companions = if io::stdin().is_terminal() && !options.noconfirm {
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
            let exists_in_sync = manager.handle().syncdbs().into_iter().any(|db| {
                db.pkg(target.as_str()).is_ok()
                    || db.pkgs().find_satisfier(target.as_str()).is_some()
            });

            if exists_in_sync {
                verify_target_and_dependencies_delay(
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
                // Query AUR fallback
                let aur_query = vec![target.clone()];
                let aur_map = aur::query_aur(&aur_query);
                if aur_map.contains_key(target) {
                    verify_target_and_dependencies_delay(
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
                    pacman_targets.push(target.clone());
                }
            }
        }
    }

    // Inspect candidate packages against installed packages for potential conflicts
    let conflicts = check_package_conflicts(&manager, &pacman_targets);
    if !conflicts.is_empty() {
        println!("{}", ":: Note: Potential package conflict(s) detected:".yellow().bold());
        for (cand, inst) in &conflicts {
            println!(
                "   - '{}' conflicts with installed package '{}' (may require replacement)",
                cand.cyan(),
                inst.yellow()
            );
        }
    }

    let (command_strs, pacman_op, aur_op) = build_install_commands(
        &pacman_targets,
        &aur_targets,
        &config.options.helper,
        options,
        sysupgrade,
    );

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
        let mut args = vec!["pacman", pacman_op];
        if options.needed {
            args.push("--needed");
        }
        if options.noconfirm {
            args.push("--noconfirm");
        }
        for f in &options.forwarded_flags {
            args.push(f.as_str());
        }
        args.push("--");
        args.extend(pacman_targets.iter().map(|s| s.as_str()));
        let status = Command::new("sudo").args(&args).status();
        match status {
            Ok(s) if s.success() => {
                installed_targets.extend(pacman_targets.clone());
                let mut cmd_parts = vec!["sudo", "pacman", pacman_op];
                if options.needed {
                    cmd_parts.push("--needed");
                }
                if options.noconfirm {
                    cmd_parts.push("--noconfirm");
                }
                for f in &options.forwarded_flags {
                    cmd_parts.push(f.as_str());
                }
                cmd_parts.push("--");
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
        let mut args = vec![aur_op, "--aur"];
        if options.needed {
            args.push("--needed");
        }
        if options.noconfirm {
            args.push("--noconfirm");
        }
        for f in &options.forwarded_flags {
            args.push(f.as_str());
        }
        args.push("--");
        args.extend(aur_targets.iter().map(|s| s.as_str()));
        let status = Command::new(helper).args(&args).status();
        match status {
            Ok(s) if s.success() => {
                installed_targets.extend(aur_targets.clone());
                let mut cmd_parts = vec![helper.as_str(), aur_op, "--aur"];
                if options.needed {
                    cmd_parts.push("--needed");
                }
                if options.noconfirm {
                    cmd_parts.push("--noconfirm");
                }
                for f in &options.forwarded_flags {
                    cmd_parts.push(f.as_str());
                }
                cmd_parts.push("--");
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

/// Removes packages via pacman with journal logging and smart unpin prompt.
pub fn remove(config: &mut Config, targets: &[String], flags: &[String]) -> Result<(), String> {
    if targets.is_empty() {
        return Err("No package targets specified for removal.".to_string());
    }

    if let Some(bad) = targets.iter().find(|t| !crate::utils::is_safe_install_target(t)) {
        return Err(format!("Refusing unsafe package target for removal: '{}'.", bad));
    }

    let wait_secs = flags
        .iter()
        .enumerate()
        .find_map(|(idx, f)| {
            if f.starts_with("--wait=") {
                f.strip_prefix("--wait=").and_then(|s| s.parse::<u64>().ok())
            } else if f == "--wait" {
                if let Some(next) = flags.get(idx + 1) {
                    if let Ok(secs) = next.parse::<u64>() {
                        return Some(secs);
                    }
                }
                Some(600)
            } else {
                None
            }
        });

    if let Some(secs) = wait_secs {
        wait_for_lock(secs)?;
    } else {
        check_lock()?;
    }

    let mut cmd_args = Vec::new();
    cmd_args.extend(flags.iter().map(|s| s.as_str()));
    cmd_args.push("--");
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
            crate::check_and_prompt_smart_unpin(config.clone(), targets, noconfirm);

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
            Ok(())
        }
        Ok(s) => Err(format!("Removal exited with code {}.", s.code().unwrap_or(1))),
        Err(e) => Err(format!("Failed to run sudo pacman: {}", e)),
    }
}

/// Searches packages via AUR helper or pacman.
pub fn search(config: &Config, query_args: &[String]) -> Result<i32, String> {
    if query_args.is_empty() {
        return Err("No search query provided.".to_string());
    }

    let helper = &config.options.helper;
    let status = if crate::is_command_available(helper) {
        Command::new(helper).arg("-Ss").args(query_args).status()
    } else {
        Command::new("pacman").arg("-Ss").args(query_args).status()
    };

    match status {
        Ok(s) => Ok(s.code().unwrap_or(0)),
        Err(e) => Err(format!("Search execution failed: {}", e)),
    }
}

/// Queries package information via AUR helper or pacman.
pub fn info(config: &Config, pkg_args: &[String]) -> Result<i32, String> {
    if pkg_args.is_empty() {
        return Err("No package specified.".to_string());
    }

    let helper = &config.options.helper;
    let status = if crate::is_command_available(helper) {
        Command::new(helper).arg("-Si").args(pkg_args).status()
    } else {
        Command::new("pacman").arg("-Si").args(pkg_args).status()
    };

    match status {
        Ok(s) => Ok(s.code().unwrap_or(0)),
        Err(e) => Err(format!("Info query failed: {}", e)),
    }
}

/// Cleans package cache via AUR helper or pacman.
pub fn clean(config: &Config, extra_args: &[String]) -> Result<i32, String> {
    check_lock()?;
    let helper = &config.options.helper;
    let status = if crate::is_command_available(helper) {
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
        Ok(s) => Ok(s.code().unwrap_or(0)),
        Err(e) => Err(format!("Clean execution failed: {}", e)),
    }
}

/// Forwards arbitrary commands directly to pacman with database lock protection.
pub fn forward_pacman(args: &[String], needs_sudo: bool) -> Result<i32, String> {
    if needs_sudo {
        check_lock()?;
        let status = Command::new("sudo").arg("pacman").args(args).status();
        match status {
            Ok(s) => Ok(s.code().unwrap_or(0)),
            Err(e) => Err(format!("Failed to run sudo pacman: {}", e)),
        }
    } else {
        let status = Command::new("pacman").args(args).status();
        match status {
            Ok(s) => Ok(s.code().unwrap_or(0)),
            Err(e) => Err(format!("Failed to run pacman: {}", e)),
        }
    }
}

/// Information regarding the pacman database lock.
#[derive(Debug, Clone)]
pub struct LockInfo {
    pub path: std::path::PathBuf,
    pub pid: Option<i32>,
    pub process_name: Option<String>,
    pub is_alive: bool,
}

/// Inspects the pacman database lock file and the process holding it (if any).
pub fn get_lock_info() -> Option<LockInfo> {
    let lock_path = AlpmManager::get_dbpath().join("db.lck");
    get_lock_info_at(lock_path)
}

fn get_lock_info_at(lock_path: std::path::PathBuf) -> Option<LockInfo> {
    if !lock_path.exists() {
        return None;
    }

    let mut pid_opt = None;
    if let Ok(content) = std::fs::read_to_string(&lock_path) {
        if let Some(token) = content.split_whitespace().next() {
            if let Ok(pid) = token.parse::<i32>() {
                if pid > 0 {
                    pid_opt = Some(pid);
                }
            }
        }
    }

    let (is_alive, process_name) = if let Some(pid) = pid_opt {
        #[cfg(unix)]
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        #[cfg(not(unix))]
        let alive = true;

        let name = if alive {
            std::fs::read_to_string(format!("/proc/{}/comm", pid))
                .ok()
                .map(|s| s.trim().to_string())
        } else {
            None
        };
        (alive, name)
    } else {
        // Pacman creates db.lck with mode 0000 and 0 bytes without writing a PID.
        // On Linux, scan /proc for active package manager processes.
        let mut active_pm_found = None;
        let my_pid = std::process::id() as i32;

        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if let Ok(pid) = name.parse::<i32>() {
                        if pid == my_pid {
                            continue;
                        }
                        if let Ok(comm) = std::fs::read_to_string(format!("/proc/{}/comm", pid)) {
                            let trimmed = comm.trim();
                            if matches!(
                                trimmed,
                                "pacman"
                                    | "paru"
                                    | "yay"
                                    | "pamac-daemon"
                                    | "packagekitd"
                                    | "pamac"
                                    | "pacpin"
                                    | "pin"
                            ) {
                                active_pm_found = Some((pid, trimmed.to_string()));
                                break;
                            }
                        }
                    }
                }
            }
        }

        if let Some((pid, name)) = active_pm_found {
            pid_opt = Some(pid);
            (true, Some(name))
        } else {
            // db.lck exists, but no package manager process is running on the system!
            // It is definitively a stale lock.
            (false, None)
        }
    };

    Some(LockInfo {
        path: lock_path,
        pid: pid_opt,
        process_name,
        is_alive,
    })
}

/// Checks if pacman database is currently locked.
pub fn check_lock() -> Result<(), String> {
    if let Some(info) = get_lock_info() {
        if !info.is_alive {
            if let Some(pid) = info.pid {
                return Err(format!(
                    "Pacman database lock exists ({}), but process PID {} is no longer running (stale lock).\n\
                    If no other package manager is active, remove it with: sudo rm {}",
                    info.path.display(),
                    pid,
                    info.path.display()
                ));
            } else {
                return Err(format!(
                    "Pacman database lock exists ({}), but no active package manager process (pacman, paru, yay) is running (stale lock).\n\
                    If no other package manager is active, remove it with: sudo rm {}",
                    info.path.display(),
                    info.path.display()
                ));
            }
        }

        if let Some(ref name) = info.process_name {
            if let Some(pid) = info.pid {
                return Err(format!(
                    "Pacman database is locked ({}). Process '{}' (PID {}) is currently running.",
                    info.path.display(),
                    name,
                    pid
                ));
            } else {
                return Err(format!(
                    "Pacman database is locked ({}). Process '{}' is currently running.",
                    info.path.display(),
                    name
                ));
            }
        }

        if let Some(pid) = info.pid {
            return Err(format!(
                "Pacman database is locked ({}). Process PID {} is currently running.",
                info.path.display(),
                pid
            ));
        }

        return Err(format!(
            "Pacman database is locked ({}). Another package management process is currently running.",
            info.path.display()
        ));
    }
    Ok(())
}

/// Waits for pacman database lock to be released, or times out.
pub fn wait_for_lock(max_wait_secs: u64) -> Result<(), String> {
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(max_wait_secs);
    let mut warned = false;

    loop {
        match check_lock() {
            Ok(()) => return Ok(()),
            Err(e) => {
                // If it's a STALE lock, waiting won't help; fail immediately!
                if let Some(info) = get_lock_info() {
                    if !info.is_alive {
                        return Err(e);
                    }
                }
                if start.elapsed() >= timeout {
                    return Err(format!(
                        "Timed out waiting for pacman database lock after {}s: {}",
                        max_wait_secs, e
                    ));
                }
                if !warned {
                    eprintln!(
                        "{}",
                        format!(
                            ":: Waiting for pacman database lock to be released (timeout: {}s)...",
                            max_wait_secs
                        )
                        .yellow()
                    );
                    warned = true;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
    }
}

/// Inspects candidate packages against installed packages to identify potential conflicts.
pub fn check_package_conflicts(manager: &AlpmManager, targets: &[String]) -> Vec<(String, String)> {
    let local_db = manager.handle().localdb();
    let mut conflicts = Vec::new();
    let mut conflict_set: HashSet<(String, String)> = HashSet::new();

    // Pre-index conflicts declared by installed packages:
    // conflict_dep_name -> list of installed package names declaring it
    let mut installed_conflicts: HashMap<String, Vec<String>> = HashMap::new();
    for inst in local_db.pkgs() {
        for conflict_dep in inst.conflicts() {
            installed_conflicts
                .entry(conflict_dep.name().to_string())
                .or_default()
                .push(inst.name().to_string());
        }
    }

    for target in targets {
        let (requested_repo, pkg_clean) = match target.split_once('/') {
            Some((repo, pkg)) => (Some(repo), pkg),
            None => (None, target.as_str()),
        };

        let pkg_opt = manager
            .handle()
            .syncdbs()
            .into_iter()
            .filter(|db| requested_repo.is_none_or(|repo| db.name() == repo))
            .find_map(|db| {
                db.pkg(pkg_clean)
                    .ok()
                    .or_else(|| db.pkgs().find_satisfier(pkg_clean))
            });

        if let Some(pkg) = pkg_opt {
            let pkg_name = pkg.name();

            // 1. Check if candidate package declares conflicts with any installed package
            for conflict_dep in pkg.conflicts() {
                if let Some(inst) = local_db.pkgs().find_satisfier(conflict_dep.to_string()) {
                    if inst.name() != pkg_name
                        && conflict_set.insert((pkg_name.to_string(), inst.name().to_string()))
                    {
                        conflicts.push((pkg_name.to_string(), inst.name().to_string()));
                    }
                }
            }

            // 2. Check if any installed package declares a conflict with candidate package or what it provides
            if let Some(inst_names) = installed_conflicts.get(pkg_name) {
                for inst_name in inst_names {
                    if inst_name != pkg_name
                        && conflict_set.insert((pkg_name.to_string(), inst_name.clone()))
                    {
                        conflicts.push((pkg_name.to_string(), inst_name.clone()));
                    }
                }
            }
            for pr in pkg.provides() {
                if let Some(inst_names) = installed_conflicts.get(pr.name()) {
                    for inst_name in inst_names {
                        if inst_name != pkg_name
                            && conflict_set.insert((pkg_name.to_string(), inst_name.clone()))
                        {
                            conflicts.push((pkg_name.to_string(), inst_name.clone()));
                        }
                    }
                }
            }
        }
    }
    conflicts
}

/// Verifies stability delay for the target package and its uninstalled dependency graph.
fn verify_target_and_dependencies_delay(
    target_pkg: &str,
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

    // 1. Check target package itself
    check_single_pkg_delay(
        target_pkg,
        target_pkg,
        repo,
        false,
        config,
        glob_delays,
        manager,
        now_epoch,
        options,
    )?;

    // 2. Discover uninstalled direct dependencies and check their delay buffers
    let mut direct_deps = Vec::new();
    if let Some(r) = repo {
        if r.eq_ignore_ascii_case("aur") {
            let aur_map = aur::query_aur(&[target_pkg.to_string()]);
            if let Some(item) = aur_map.get(target_pkg) {
                for d in &item.depends {
                    direct_deps.push(aur::clean_dep_name(d).to_string());
                }
            }
        } else if let Some(db) = manager.handle().syncdbs().iter().find(|d| d.name() == r) {
            let pkg_obj = db
                .pkg(target_pkg)
                .ok()
                .or_else(|| db.pkgs().find_satisfier(target_pkg));
            if let Some(pkg) = pkg_obj {
                for d in pkg.depends() {
                    direct_deps.push(d.name().to_string());
                }
            }
        }
    } else {
        for db in manager.handle().syncdbs() {
            let pkg_obj = db
                .pkg(target_pkg)
                .ok()
                .or_else(|| db.pkgs().find_satisfier(target_pkg));
            if let Some(pkg) = pkg_obj {
                for d in pkg.depends() {
                    direct_deps.push(d.name().to_string());
                }
                break;
            }
        }
    }

    let local_db = manager.handle().localdb();
    let mut checked_deps = HashSet::new();

    for dep_name in direct_deps {
        if !checked_deps.insert(dep_name.clone()) {
            continue;
        }

        // If dependency is already satisfied locally, delay buffer is irrelevant
        if local_db.pkgs().find_satisfier(&*dep_name).is_some() {
            continue;
        }

        check_single_pkg_delay(
            &dep_name,
            target_pkg,
            None,
            true,
            config,
            glob_delays,
            manager,
            now_epoch,
            options,
        )?;
    }

    Ok(())
}

/// Evaluates a single package against delay rules.
#[allow(clippy::too_many_arguments)]
fn check_single_pkg_delay(
    pkg: &str,
    root_target: &str,
    repo: Option<&str>,
    is_dependency: bool,
    config: &Config,
    glob_delays: &[(String, glob::Pattern, u32)],
    manager: &AlpmManager,
    now_epoch: i64,
    options: &InstallOptions,
) -> Result<(), String> {
    // If already installed locally or satisfied by virtual provide, skip
    let local_db = manager.handle().localdb();
    if local_db.pkg(pkg).is_ok() || local_db.pkgs().find_satisfier(pkg).is_some() {
        return Ok(());
    }

    let req_days = match ResolverEngine::match_delay_days(pkg, &config.delay, glob_delays) {
        Some(d) => d,
        None => return Ok(()),
    };

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
            let context_label = if is_dependency {
                format!("Dependency '{}' (required by '{}')", pkg, root_target)
            } else {
                format!("Package '{}'", pkg)
            };

            if options.noconfirm || !io::stdin().is_terminal() {
                println!(
                    "{} {} was released {:.1} days ago (within your {}d stability delay buffer, {}d remaining). Proceeding in non-interactive mode.",
                    ":: Warning:".yellow().bold(),
                    context_label,
                    age_days.max(0.0),
                    req_days,
                    days_left
                );
            } else {
                print!(
                    "\n{} {} was released {:.1} days ago (within your {}d stability delay buffer, {}d remaining).\n   Arch mirrors only host the latest version. Proceed with installation? [Y/n] ",
                    ":: Notice:".yellow().bold(),
                    context_label,
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
                            "Installation cancelled: {} violates active {}-day stability delay policy ({}d remaining).",
                            context_label, req_days, days_left
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
        assert!(!opts.sysupgrade);
        assert!(!opts.dry_run);
        assert!(opts.forwarded_flags.is_empty());
    }

    #[test]
    fn test_empty_targets_rejected() {
        let mut config = Config::default();
        let opts = InstallOptions::default();
        let res = install(&mut config, &[], &opts);
        assert!(res.is_err());
        assert_eq!(res.unwrap_err(), "No package targets specified.");
    }

    #[test]
    fn test_unknown_repo_rejected() {
        let lock_path = AlpmManager::get_dbpath().join("db.lck");
        let mut config = Config::default();
        let opts = InstallOptions::default();
        let res = install(&mut config, &["nonexistent_repo/foobar".to_string()], &opts);
        assert!(res.is_err());
        if !lock_path.exists() {
            assert!(res.unwrap_err().contains("Repository '[nonexistent_repo]' is not recognized"));
        }
    }

    #[test]
    fn test_check_lock_when_no_lock() {
        let lock_path = AlpmManager::get_dbpath().join("db.lck");
        if !lock_path.exists() {
            assert!(check_lock().is_ok());
            assert!(get_lock_info().is_none());
        }
    }

    #[test]
    fn test_lock_inspection_live_and_stale_pids() {
        let lock_path = std::env::temp_dir().join(format!("pacpin-lock-test-{}", std::process::id()));
        std::fs::write(&lock_path, format!("{}\n", std::process::id())).unwrap();
        let live = get_lock_info_at(lock_path.clone()).unwrap();
        assert_eq!(live.pid, Some(std::process::id() as i32));
        assert!(live.is_alive);

        std::fs::write(&lock_path, format!("{}\n", i32::MAX)).unwrap();
        let stale = get_lock_info_at(lock_path.clone()).unwrap();
        assert_eq!(stale.pid, Some(i32::MAX));
        assert!(!stale.is_alive);
        std::fs::remove_file(lock_path).unwrap();
    }

    #[test]
    fn test_wait_for_lock_succeeds_when_unlocked() {
        let lock_path = AlpmManager::get_dbpath().join("db.lck");
        if !lock_path.exists() {
            assert!(wait_for_lock(1).is_ok());
        }
    }

    #[test]
    fn test_wait_flag_parsing() {
        let flags1 = ["--wait".to_string(), "42".to_string()];
        let secs1 = flags1
            .iter()
            .enumerate()
            .find_map(|(idx, f)| {
                if f.starts_with("--wait=") {
                    f.strip_prefix("--wait=").and_then(|s| s.parse::<u64>().ok())
                } else if f == "--wait" {
                    if let Some(next) = flags1.get(idx + 1) {
                        if let Ok(secs) = next.parse::<u64>() {
                            return Some(secs);
                        }
                    }
                    Some(600)
                } else {
                    None
                }
            });
        assert_eq!(secs1, Some(42));

        let flags2 = ["--wait=15".to_string()];
        let secs2 = flags2
            .iter()
            .enumerate()
            .find_map(|(idx, f)| {
                if f.starts_with("--wait=") {
                    f.strip_prefix("--wait=").and_then(|s| s.parse::<u64>().ok())
                } else if f == "--wait" {
                    if let Some(next) = flags2.get(idx + 1) {
                        if let Ok(secs) = next.parse::<u64>() {
                            return Some(secs);
                        }
                    }
                    Some(600)
                } else {
                    None
                }
            });
        assert_eq!(secs2, Some(15));
    }

    #[test]
    fn test_build_install_commands() {
        let targets = vec!["ollama".to_string()];
        let no_aur = Vec::new();

        // 1. Dry run with refresh only (-Sy): includes sudo pacman -Sy and sudo pacman -S
        let opts_dry_sy = InstallOptions {
            needed: true,
            noconfirm: false,
            refresh: true,
            sysupgrade: false,
            dry_run: true,
            forwarded_flags: Vec::new(),
        };
        let (cmds, op, _) = build_install_commands(&targets, &no_aur, "paru", &opts_dry_sy, false);
        assert_eq!(op, "-S");
        assert_eq!(
            cmds,
            vec![
                "sudo pacman -Sy".to_string(),
                "sudo pacman -S --needed -- ollama".to_string()
            ]
        );

        // 2. Real execution with refresh only (sysupgrade false, dry_run false):
        // Does NOT duplicate "sudo pacman -Sy" in execution string!
        let opts_sy = InstallOptions {
            needed: true,
            noconfirm: false,
            refresh: true,
            sysupgrade: false,
            dry_run: false,
            forwarded_flags: Vec::new(),
        };
        let (cmds, op, _) = build_install_commands(&targets, &no_aur, "paru", &opts_sy, false);
        assert_eq!(op, "-S");
        assert_eq!(cmds, vec!["sudo pacman -S --needed -- ollama".to_string()]);

        // 3. Sysupgrade (-Syu): atomic full upgrade, no duplicate -Sy
        let opts_syu = InstallOptions {
            needed: true,
            noconfirm: false,
            refresh: true,
            sysupgrade: true,
            dry_run: false,
            forwarded_flags: Vec::new(),
        };
        let (cmds, op, _) = build_install_commands(&targets, &no_aur, "paru", &opts_syu, true);
        assert_eq!(op, "-Syu");
        assert_eq!(cmds, vec!["sudo pacman -Syu --needed -- ollama".to_string()]);

        // 4. Sysupgrade without refresh (-Su):
        let opts_su = InstallOptions {
            needed: true,
            noconfirm: false,
            refresh: false,
            sysupgrade: true,
            dry_run: false,
            forwarded_flags: Vec::new(),
        };
        let (cmds, op, _) = build_install_commands(&targets, &no_aur, "paru", &opts_su, true);
        assert_eq!(op, "-Su");
        assert_eq!(cmds, vec!["sudo pacman -Su --needed -- ollama".to_string()]);

        // 5. AUR targets with sysupgrade when no repo targets:
        let aur_targets = vec!["google-chrome".to_string()];
        let (cmds, _op, aur_op) = build_install_commands(&[], &aur_targets, "paru", &opts_syu, true);
        assert_eq!(aur_op, "-Syu");
        assert_eq!(cmds, vec!["paru -Syu --aur --needed -- google-chrome".to_string()]);
    }

    #[test]
    fn test_install_unsafe_target_rejected() {
        let mut cfg = Config::default();
        let opts = InstallOptions::default();

        let res_flag = install(&mut cfg, &["--config=/tmp/bad".to_string()], &opts);
        assert!(res_flag.is_err());
        assert!(res_flag.unwrap_err().contains("Refusing unsafe package target"));

        let res_hyphen = install(&mut cfg, &["-badpkg".to_string()], &opts);
        assert!(res_hyphen.is_err());

        let res_remove = remove(&mut cfg, &["--dbpath=/tmp/bad".to_string()], &[]);
        assert!(res_remove.is_err());
        assert!(res_remove.unwrap_err().contains("Refusing unsafe package target for removal"));
    }

    #[test]
    fn test_check_package_conflicts_runs_safely() {
        if let Ok(manager) = AlpmManager::new() {
            // Verify empty targets returns empty conflicts without error
            let empty = check_package_conflicts(&manager, &[]);
            assert!(empty.is_empty());

            // Non-existent target returns no conflicts
            let non_existent = check_package_conflicts(&manager, &["nonexistent_pkg_xyz_12345".to_string()]);
            assert!(non_existent.is_empty());
        }
    }
}
