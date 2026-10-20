//! Native/source/AUR installation orchestration behind the pm facade.
use std::io::{self, IsTerminal, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use colored::Colorize;

use super::{
    AurOperation, check_lock, check_package_conflicts, execute_aur, helper_privilege_args,
    transaction_cache_cleanup, validate_aur_scratch, wait_for_lock, with_download_progress,
};
use super::validation::verify_target_and_dependencies_delay;
use super::pinning::RepositoryPinPlan;
use crate::aur;
use crate::config::{Config, mutate_config};
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
    pub channel: Option<crate::sources::Channel>,
    pub no_cache: bool,
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
            channel: None,
            no_cache: false,
        }
    }
}

pub fn build_install_commands(
    pacman_targets: &[String],
    aur_targets: &[String],
    helper: &str,
    privilege_tool: &str,
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
        command_strs.push(format!("{privilege_tool} pacman -Sy"));
    }

    if !pacman_targets.is_empty() {
        let mut cmd = vec![privilege_tool, "pacman", pacman_op];
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
        if privilege_tool != "sudo" && std::path::Path::new(helper).file_name().is_some_and(|name| name == "paru") {
            cmd.extend(["--sudo", privilege_tool]);
        }
        if options.needed {
            cmd.push("--needed");
        }
        if options.noconfirm {
            cmd.push("--noconfirm");
        }
        if options.no_cache {
            cmd.push("--cleanafter");
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

    let direct_targets: Vec<_> = targets.iter().filter(|target| crate::sources::is_direct_syntax(target)).collect();
    if !direct_targets.is_empty() {
        if direct_targets.len() != targets.len() || direct_targets.len() != 1 {
            return Err("Install one direct upstream source separately from native, AUR, or other source targets".into());
        }
        let result = crate::sources::install_direct(config, direct_targets[0], options.channel.clone().unwrap_or(crate::sources::Channel::Stable), options);
        transaction_cache_cleanup(config, options.no_cache, None, None);
        return result;
    }
    if options.channel.is_some() {
        return Err("--channel is only valid with a direct github:/gitlab: source target".into());
    }
    if let Some(bad) = targets.iter().find(|t| !crate::utils::is_safe_install_target(t)) {
        return Err(format!("Refusing unsafe package target '{}'.", bad));
    }

    let source_targets: Vec<String> = targets.iter().filter_map(|target| {
        if target.split_once('/').is_some_and(|(alias, _)| config.sources.contains_key(alias)) {
            Some(target.clone())
        } else if !target.contains('/') {
            crate::sources::owner(config, target).map(|alias| format!("{alias}/{target}"))
        } else { None }
    }).collect();
    if !source_targets.is_empty() {
        if source_targets.len() != targets.len() {
            return Err("Install upstream-source targets separately from native/AUR targets (transactions have different verification policies)".into());
        }
        let result = crate::sources::install_targets(config, &source_targets, options);
        transaction_cache_cleanup(config, options.no_cache, None, None);
        return result;
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
            format!(":: Refreshing package databases ({} pacman -Sy)...", config.options.privilege_tool).cyan()
        );
        let status = crate::privilege::command(&config.options.privilege_tool).args(["pacman", "-Sy"]).status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                return Err(format!(
                    "Database refresh failed with exit code {}.",
                    s.code().unwrap_or(1)
                ));
            }
            Err(e) => {
                return Err(format!("Failed to run {} pacman: {}", config.options.privilege_tool, e));
            }
        }
    }

    let mut manager = AlpmManager::with_repo_order(&config.repo_order)
        .map_err(|e| format!("Error initializing ALPM: {}", e))?;

    let mut pacman_targets = Vec::new();
    let mut aur_targets = Vec::new();
    let mut pin_plan = RepositoryPinPlan::from_requests(targets);

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

            // Native repository names stay case-sensitive (pacman semantics);
            // only the pseudo-repository `aur` folds case, matching the
            // routing below.
            let repo_known = known_repos.iter().any(|r| r == repo)
                || (repo.eq_ignore_ascii_case("aur")
                    && known_repos.iter().any(|r| r == "aur"));
            if !repo_known {
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
                    pin_plan.select_companions("aur", &selected);
                    to_install_repo.extend(selected);
                }
                for p in to_install_repo {
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
                    pin_plan.select_companions(repo, &selected);
                    to_install_repo.extend(selected);
                }

                for p in to_install_repo {
                    pacman_targets.push(format!("{}/{}", repo, p));
                }
            }
        } else {
            let exists_in_sync = manager.has_repository_target(target);

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
                "   - '{}' conflicts with package '{}' (may require replacement)",
                cand.cyan(),
                inst.yellow()
            );
        }
    }

    let helper_flags = if aur_targets.is_empty() { Vec::new() } else { helper_privilege_args(&config.options.helper, &config.options.privilege_tool)? };
    if !aur_targets.is_empty() { validate_aur_scratch(config, &options.forwarded_flags)?; }
    if !aur_targets.is_empty() && !options.dry_run {
        crate::preview::check_package_root_capacity(0, 0)?;
    }
    let (command_strs, pacman_op, aur_op) = build_install_commands(
        &pacman_targets,
        &aur_targets,
        &config.options.helper,
        &config.options.privilege_tool,
        options,
        sysupgrade,
    );

    let full_cmd = command_strs.join(" && ");

    let native_preview = if !pacman_targets.is_empty() && !sysupgrade {
        let preview = crate::preview::prepare_native_preview_with_needed(&mut manager, &pacman_targets, options.needed)?;
        if !options.dry_run { crate::preview::check_native_capacity(&preview)?; }
        Some(preview)
    } else { None };

    if options.dry_run {
        if let Some(preview) = &native_preview {
            let explicit_names: std::collections::HashSet<String> = pacman_targets.iter()
                .filter_map(|target| crate::db::find_sync_package(manager.handle(), target).ok())
                .map(|package| package.name().to_string()).collect();
            println!("\nDry-Run: Native package targets:");
            for package in preview.adds.iter().filter(|package| explicit_names.contains(&package.name)) {
                println!("  • {}/{} {}", package.repo, package.name, package.version);
            }
            crate::ui::render_preview_boxes(preview, &explicit_names);
        } else if sysupgrade && !pacman_targets.is_empty() {
            println!("Dry-Run: Full native system transaction is delegated to pacman; dependencies are not expanded here.");
        }
        if let Some(description) = crate::scratch::describe_plan(false, !aur_targets.is_empty()) { println!("Dry-Run: {description}"); }
        println!("\n{}", "Dry-Run: Synthesized Installation Commands:".bold());
        println!("  ➔ {}", full_cmd.cyan());
        if options.no_cache {
            println!("{}", "Dry-Run: Cache Cleanup Action (--no-cache):".bold());
            let has_native = !pacman_targets.is_empty();
            let has_aur = !aur_targets.is_empty();
            if has_native && has_aur {
                println!("  ➔ Purge both native pacman cache and AUR build cache\n");
            } else if has_native {
                println!("  ➔ Purge native pacman cache only ({} pacman -Scc); AUR cache preserved\n", config.options.privilege_tool);
            } else if has_aur {
                println!("  ➔ Purge AUR build cache only ({} -Scc --aur); native pacman cache preserved\n", config.options.helper);
            } else {
                println!("  ➔ No pacman or AUR caches to purge\n");
            }
        }
        return Ok(targets.to_vec());
    }

    let mut planned_managers = Vec::new();
    if !pacman_targets.is_empty() { planned_managers.push(crate::progress::Manager::Native); }
    if !aur_targets.is_empty() { planned_managers.push(crate::progress::Manager::Aur); }
    let _pending_plan = crate::progress::pending(&planned_managers);
    println!("\n{} {}", ":: Executing:".cyan(), full_cmd.bold());
    let mut installed_targets: Vec<String> = Vec::new();
    let mut executed_commands: Vec<String> = Vec::new();
    let mut exit_status = Ok(std::process::ExitStatus::default());
    let mut all_success = true;
    let mut native_ok = false;
    let mut aur_ok = false;

    if !pacman_targets.is_empty() {
        let mut args = vec!["pacman", pacman_op];
        args.extend(crate::progress::pacman_progress_args());
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
        let run = || crate::progress::run(
            crate::privilege::command(&config.options.privilege_tool).args(&args),
            crate::progress::Manager::Native,
            "Resolving / installing repository packages",
            || {},
        );
        let flags: Vec<&str> = options.forwarded_flags.iter().map(String::as_str).collect();
        let status = if let Some(preview) = &native_preview { with_download_progress(preview, &flags, run) } else { run() };
        match status {
            Ok(s) if s.success() => {
                native_ok = true;
                crate::progress::completed(crate::progress::Manager::Native);
                installed_targets.extend(pacman_targets.clone());
                let mut cmd_parts = vec![config.options.privilege_tool.as_str(), "pacman", pacman_op];
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
        let operation = match aur_op { "-Su" => AurOperation::Upgrade, "-Syu" => AurOperation::RefreshUpgrade, _ => AurOperation::Install };
        let status = execute_aur(config, &aur_targets, options, operation, false).map(|report| report.status);
        match status {
            Ok(s) if s.success() => {
                aur_ok = true;
                crate::progress::completed(crate::progress::Manager::Aur);
                installed_targets.extend(aur_targets.clone());
                let mut cmd_parts = vec![helper.as_str(), aur_op, "--aur"];
                cmd_parts.extend(helper_flags.iter().copied());
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

    if !all_success { crate::progress::stop_pending(); }
    if !installed_targets.is_empty() {
        let mut confirmed_pins = Vec::new();
        for (pkg_name, repo_name) in pin_plan.confirmed(&installed_targets) {
            config.pins.insert(pkg_name.clone(), repo_name.clone());
            confirmed_pins.push((pkg_name, repo_name));
        }

        if !confirmed_pins.is_empty() && !options.dry_run {
            if let Err(e) = mutate_config(|current| {
                current.pins.extend(confirmed_pins);
                Ok(())
            }) {
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

    let cleanup_ok = transaction_cache_cleanup(config, options.no_cache,
        (!pacman_targets.is_empty()).then_some(native_ok),
        if !aur_targets.is_empty() && (pacman_targets.is_empty() || native_ok) { Some(aur_ok) } else { None });

    if !all_success {
        match exit_status {
            Ok(s) => Err(format!(
                "Installation command exited with code {}.",
                s.code().unwrap_or(1)
            )),
            Err(e) => Err(format!("Installation execution failed: {}", e)),
        }
    } else if !cleanup_ok {
        Err("Packages installed, but requested cache cleanup failed.".into())
    } else {
        Ok(installed_targets)
    }
}
