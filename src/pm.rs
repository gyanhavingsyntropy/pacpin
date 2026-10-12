//! Centralized Package Management Engine for pacpin.
//!
//! Encapsulates all subprocess interactions with pacman and AUR helpers,
//! enforcing database locks, repository priority, delay buffers, companion
//! cascades, and transaction journaling.

use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal, Write};
use std::process::{Command, Stdio};

mod continuation;
mod workspace;
mod rebuild;
mod orphans;
pub use orphans::remove_orphans;
pub use rebuild::{validate_rebuild, rebuild_aur};
pub use continuation::{execute_aur, ensure_independent, continuation_archive_check, AurBuildFailurePermit, AurOperation};

/// Keep filesystem/cache inspection in the native boundary. Only archives
/// actually missing from the selected cache contribute to download progress.
pub fn with_download_progress<T>(preview: &crate::preview::TransactionPreview, flags: &[&str], work: impl FnOnce() -> T) -> T {
    let mut caches = Vec::new();
    let mut args = flags.iter();
    while let Some(flag) = args.next() {
        if *flag == "--cachedir" {
            if let Some(path) = args.next() { caches.push(std::path::PathBuf::from(path)); }
        } else if let Some(path) = flag.strip_prefix("--cachedir=") { caches.push(path.into()); }
    }
    if caches.is_empty() { caches = TransactionJournal::get_cache_dirs(); }
    crate::progress::native_downloads(download_targets(preview, &caches), work)
}

fn download_targets(preview: &crate::preview::TransactionPreview, caches: &[std::path::PathBuf]) -> Vec<(String, u64)> {
    preview.adds.iter().filter(|p| p.download_size > 0 && !p.filename.is_empty())
        .filter(|p| !caches.iter().any(|dir| std::fs::metadata(dir.join(&p.filename)).is_ok_and(|m| m.is_file() && m.len() == p.download_size as u64)))
        .map(|p| (p.filename.split(".pkg.tar").next().unwrap_or(&p.filename).to_string(), p.download_size as u64)).collect()
}

pub fn refresh_databases(config: &Config) -> io::Result<std::process::ExitStatus> {
    check_lock().map_err(io::Error::other)?;
    crate::progress::run(
        crate::privilege::command(&config.options.privilege_tool).args(["pacman", "-Sy"]),
        crate::progress::Manager::Refresh, "Refreshing repositories", || {},
    )
}

pub fn authorize_downloads(config: &Config) -> io::Result<std::process::ExitStatus> {
    if config.options.privilege_tool != "sudo" {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "Background repository downloads require sudo authorization"));
    }
    crate::progress::run(
        crate::privilege::command("sudo").arg("-v"),
        crate::progress::Manager::Review, "Authorizing repository downloads", || {},
    )
}

pub fn mark_dependencies(config: &Config, names: &[&str]) -> io::Result<std::process::ExitStatus> {
    check_lock().map_err(io::Error::other)?;
    if names.is_empty() || names.iter().any(|name| !crate::utils::is_safe_pkg_name(name)) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid dependency targets"));
    }
    crate::progress::run(
        crate::privilege::command(&config.options.privilege_tool).args(["pacman", "-D", "--asdeps", "--"]).args(names),
        crate::progress::Manager::Record, "Recording dependency reasons", || {},
    )
}

pub fn validate_aur_scratch(config: &Config, flags: &[String]) -> Result<(), String> {
    workspace::validate(&config.options.helper, crate::scratch::policy(), flags)
}
use std::time::{SystemTime, UNIX_EPOCH};

use colored::*;

use crate::aur;
use crate::config::{mutate_config, Config};
use crate::db::AlpmManager;
use crate::journal::TransactionJournal;
use crate::resolver::{ResolvedPackage, ResolverEngine};
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

    if options.dry_run {
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

    let native_preview = if !pacman_targets.is_empty() && !sysupgrade {
        let preview = crate::preview::prepare_native_preview(&mut manager, &pacman_targets)?;
        crate::preview::check_native_capacity(&preview)?;
        Some(preview)
    } else { None };
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

    if !installed_targets.is_empty() {
        let mut confirmed_pins = Vec::new();
        for (pkg_name, repo_name) in pending_pins {
            let was_installed = installed_targets
                .iter()
                .any(|t| t == &pkg_name || t == &format!("{}/{}", repo_name, pkg_name));
            if was_installed {
                config.pins.insert(pkg_name.clone(), repo_name.clone());
                confirmed_pins.push((pkg_name, repo_name));
            }
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

/// Removes packages via pacman with journal logging and smart unpin prompt.
pub fn remove(config: &mut Config, targets: &[String], flags: &[String]) -> Result<(), String> {
    if targets.is_empty() {
        return Err("No package targets specified for removal.".to_string());
    }

    if let Some(bad) = targets.iter().find(|t| !crate::utils::is_safe_install_target(t)) {
        return Err(format!("Refusing unsafe package target for removal: '{}'.", bad));
    }

    // `-n` / `--dry-run` only ever previews here: forwarded to `pacman -R`
    // it would mean `--nosave`, so it is consumed and never forwarded.
    // (Bundled short flags such as `-Rns` keep their pacman meaning.)
    let dry_run = flags.iter().any(|f| f == "-n" || f == "--dry-run");
    let forwarded: Vec<&str> = flags
        .iter()
        .filter(|f| f.as_str() != "-n" && f.as_str() != "--dry-run")
        .map(|s| s.as_str())
        .collect();

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
        if dry_run {
            println!("{}", ":: Dry-run: skipping lock wait; no changes will be made.".cyan());
        } else {
            wait_for_lock(secs)?;
        }
    } else if !dry_run {
        check_lock()?;
    }

    let mut cmd_args = Vec::new();
    cmd_args.extend(forwarded.iter().copied());
    cmd_args.push("--");
    cmd_args.extend(targets.iter().map(|s| s.as_str()));

    if dry_run {
        println!(
            "{}",
            "Dry-Run: Synthesized Removal Command (nothing will be removed):".bold()
        );
        println!("  ➔ {}", format!("{} pacman {}", config.options.privilege_tool, cmd_args.join(" ")).cyan());
        return Ok(());
    }

    println!(
        "{} Removing package(s) via {} pacman {}...",
        "::".cyan(),
        config.options.privilege_tool,
        cmd_args.join(" ")
    );

    let status = crate::privilege::command(&config.options.privilege_tool).arg("pacman").args(&cmd_args).status();

    match status {
        Ok(s) if s.success() => {
            let noconfirm = flags.iter().any(|f| f == "--noconfirm");
            crate::check_and_prompt_smart_unpin(config.clone(), targets, noconfirm);

            let packages = targets
                .iter()
                .map(|name| serde_json::json!({ "name": name, "target": name }))
                .collect();
            let command = format!("{} pacman {}", config.options.privilege_tool, cmd_args.join(" "));
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
        Ok(s) => Ok(s.code().unwrap_or(1)),
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
        Ok(s) => Ok(s.code().unwrap_or(1)),
        Err(e) => Err(format!("Info query failed: {}", e)),
    }
}

/// Cleans package cache via AUR helper or pacman, with support for scoping to native-only or AUR-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheScope { All, Native, Aur }

pub fn cache_scope(extra_args: &[String]) -> Result<CacheScope, String> {
    let native = extra_args.iter().any(|arg| arg == "--repo");
    let aur = extra_args.iter().any(|arg| arg == "--aur" || arg == "-a");
    match (native, aur) {
        (true, true) => Err("Cache cleanup cannot combine --repo with --aur".into()),
        (true, false) => Ok(CacheScope::Native),
        (false, true) => Ok(CacheScope::Aur),
        (false, false) => Ok(CacheScope::All),
    }
}

pub fn clean_scoped(
    config: &Config,
    full: bool,
    extra_args: &[String],
    force_native_only: bool,
) -> Result<i32, String> {
    // AUR helpers must never inherit a root pacpin process (e.g. `pkexec pin clean`).
    if unsafe { libc::geteuid() } == 0 {
        return Err("Do not run pacpin as root. Run `pin clean` as your user and set [options] privilege_tool = \"pkexec\" if needed.".into());
    }
    crate::config::validate_privilege_tool(&config.options.privilege_tool)?;
    check_lock()?;
    let is_aur_only = cache_scope(extra_args)? == CacheScope::Aur;
    if !is_aur_only {
        prune_download_directories(config, full)?;
    }
    let helper = &config.options.helper;
    let use_helper = !force_native_only && crate::is_command_available(helper);
    let flag = if full { "-Scc" } else { "-Sc" };
    let mut cmd = build_clean_command(config, full, extra_args, use_helper)?;
    if use_helper {
        if is_aur_only {
            println!(
                "{} Cleaning AUR build cache via {} {} --aur...",
                "::".cyan(),
                helper,
                flag
            );
        } else {
            println!(
                "{} Cleaning package cache via {} {}...",
                "::".cyan(),
                helper,
                flag
            );
        }
    } else {
        println!(
            "{} Cleaning native package cache via {} pacman {}...",
            "::".cyan(), config.options.privilege_tool, flag
        );
    }
    // `pacman -Scc --noconfirm` answers its destructive [y/N] prompt with No.
    // Answer only the known cache-clean questions after pacpin's own approval.
    let input = if full {
        // Queue the bounded answers before spawning. Writing to a child's
        // pipe after it fails early could terminate pacpin with SIGPIPE,
        // hiding the actual cleanup status and skipping restart advice.
        let input = cache_input(&cache_answers(TransactionJournal::get_cache_dirs().len(), is_aur_only))
            .map_err(|error| format!("Could not prepare cache-clean answers: {error}"))?;
        Stdio::from(std::os::fd::OwnedFd::from(input))
    } else { Stdio::inherit() };
    let label = if is_aur_only { "Cleaning AUR build cache" } else if force_native_only || cache_scope(extra_args)? == CacheScope::Native { "Cleaning native package cache" } else { "Cleaning package caches" };
    let status = crate::progress::run_with_stdin(&mut cmd, crate::progress::Manager::Cache, label, input);
    match status {
        Ok(s) => {
            if s.success() { crate::progress::completed(crate::progress::Manager::Cache); }
            Ok(s.code().unwrap_or(1))
        }
        Err(e) => Err(format!("Clean execution failed: {}", e)),
    }
}

pub fn clean(config: &Config, full: bool, extra_args: &[String]) -> Result<i32, String> {
    clean_scoped(config, full, extra_args, false)
}

fn cache_answers(native_dirs: usize, aur_only: bool) -> Vec<u8> {
    if aur_only { return b"y\n".repeat(64); }
    let mut answers = b"y\n".repeat(native_dirs);
    // Cache cleanup must not also remove sync databases.
    answers.extend_from_slice(b"n\n");
    answers.extend_from_slice(&b"y\n".repeat(64));
    answers
}

fn cache_input(answers: &[u8]) -> std::io::Result<std::os::unix::net::UnixStream> {
    if answers.len() > 8192 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Too many cache-clean prompts"));
    }
    let (input, mut output) = std::os::unix::net::UnixStream::pair()?;
    output.write_all(answers)?;
    output.shutdown(std::net::Shutdown::Write)?;
    Ok(input)
}

pub fn clean_preview(config: &Config, full: bool, extra_args: &[String]) -> Result<String, String> {
    let flags: Vec<_> = extra_args.iter().filter(|s| s.as_str() != "--dry-run" && s.as_str() != "-n").cloned().collect();
    let command = build_clean_command(config, full, &flags, crate::is_command_available(&config.options.helper))?;
    Ok(std::iter::once(command.get_program()).chain(command.get_args()).map(|s| crate::utils::shell_quote(&s.to_string_lossy())).collect::<Vec<_>>().join(" "))
}

pub fn helper_privilege_args<'a>(helper: &str, tool: &'a str) -> Result<Vec<&'a str>, String> {
    crate::config::validate_privilege_tool(tool)?;
    if tool == "sudo" { return Ok(Vec::new()); }
    if std::path::Path::new(helper).file_name().is_some_and(|name| name == "paru") {
        Ok(vec!["--sudo", tool])
    } else {
        Err(format!("Cannot configure {tool} for AUR helper '{helper}'; paru supports --sudo, other helpers require their own configuration"))
    }
}

/// Cleans only the caches affected by the completed transaction.
/// If only AUR packages were installed, native pacman cache is untouched.
/// If only native packages were installed, AUR cache is untouched.
/// If neither was installed (e.g. only sources), neither cache is purged.
pub fn clean_after_transaction(config: &Config, has_native: bool, has_aur: bool) -> Result<(), String> {
    if !has_native && !has_aur {
        return Ok(());
    }

    let clean_args = vec!["--noconfirm".to_string()];

    if has_native {
        println!("{}", "\n:: Purging native package cache (--no-cache)...".cyan());
        cleanup_status("Native", clean_scoped(config, true, &clean_args, true)?)?;
        println!("✔ Native cache cleaned successfully.");
    }
    if has_aur {
        let helper = &config.options.helper;
        if crate::is_command_available(helper) {
            println!("{}", "\n:: Purging AUR build cache (--no-cache)...".cyan());
            let aur_clean_args = vec!["--aur".to_string(), "--noconfirm".to_string()];
            cleanup_status("AUR", clean_scoped(config, true, &aur_clean_args, false)?)?;
            println!("✔ AUR cache cleaned successfully.");
        } else {
            return Err(format!("Cannot clean AUR cache: helper '{helper}' is unavailable"));
        }
    }

    Ok(())
}

fn cleanup_status(manager: &str, code: i32) -> Result<(), String> {
    if code == 0 { Ok(()) } else { Err(format!("{manager} cache cleanup exited with code {code}")) }
}

pub fn transaction_cache_cleanup(config: &Config, requested: bool, native: Option<bool>, aur: Option<bool>) -> bool {
    if !requested { return true; }
    let mut success = true;
    for (manager, outcome) in [("Native", native), ("AUR", aur)] {
        if outcome == Some(false) {
            println!(":: {manager} cache cleanup skipped: transaction failed; cached packages preserved for recovery.");
        }
        if outcome == Some(true) {
            if let Err(error) = clean_after_transaction(config, manager == "Native", manager == "AUR") {
                eprintln!(":: Warning: {manager} cache cleanup failed: {error}");
                success = false;
            }
        }
    }
    if native.is_none() && aur.is_none() {
        println!(":: --no-cache: no native or AUR cache cleanup needed for this transaction.");
    }
    success
}

fn prune_download_directories(config: &Config, full: bool) -> Result<(), String> {
    let output = crate::progress::capture_output(Command::new("pacman-conf").arg("CacheDir"))
        .map_err(|error| format!("Cannot inspect native cache directories: {error}"))?;
    if !output.status.success() { return Err("Native cache directory lookup failed".into()); }
    let mut empty = Vec::new();
    let mut stale = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let now = SystemTime::now();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let path = std::path::Path::new(line);
        if !path.is_absolute() { return Err("Native cache path must be absolute".into()); }
        let cache_dir = match path.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Cannot inspect native cache {}: {error}", path.display())),
        };
        if !seen.insert(cache_dir.clone()) { continue; }
        let entries = std::fs::read_dir(&cache_dir).map_err(|error| format!("Cannot scan native cache {}: {error}", cache_dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("Cannot scan native cache: {error}"))?;
            if !entry.file_name().to_string_lossy().starts_with("download-") || !entry.file_type().is_ok_and(|kind| kind.is_dir()) { continue; }
            if full && is_stale_download_directory(&entry, now) { stale.push(entry.path()); }
            else { empty.push(entry.path()); }
        }
    }
    for (paths, recursive) in [(&empty, false), (&stale, true)] {
        if paths.is_empty() { continue; }
        check_lock()?;
        let mut command = crate::privilege::command(&config.options.privilege_tool);
        if recursive {
            // Full native cleanup only: canonical cache children, no symlink
            // traversal or crossing filesystem boundaries. Recent/non-full
            // directories are handled only by rmdir, which cannot erase files.
            command.args(["/usr/bin/rm", "-rf", "--one-file-system", "--preserve-root=all"]);
        } else { command.arg("/usr/bin/rmdir"); }
        command.arg("--").args(paths);
        let status = crate::progress::run(&mut command, crate::progress::Manager::Cache, "Clearing native download directories", || {})
            .map_err(|error| format!("Download directory cleanup failed: {error}"))?;
        if !status.success() { return Err(format!("Download directory cleanup exited with {status}; recent or non-empty directories were preserved where removal was unsafe")); }
        for path in paths {
            match path.symlink_metadata() {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {},
                Err(error) => return Err(format!("Cannot verify download directory cleanup {}: {error}", path.display())),
                Ok(_) => return Err(format!("Download directory remains after cleanup: {}", path.display())),
            }
            println!(":: Removed inactive ALPM download directory {}", path.display());
        }
    }
    Ok(())
}

fn is_stale_download_directory(entry: &std::fs::DirEntry, now: SystemTime) -> bool {
    entry.file_name().to_string_lossy().starts_with("download-")
        && entry.file_type().is_ok_and(|kind| kind.is_dir())
        && entry.metadata().ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age.as_secs() >= 120)
}

fn build_clean_command(config: &Config, full: bool, extra_args: &[String], use_helper: bool) -> Result<Command, String> {
    let privilege_tool = crate::config::validate_privilege_tool(&config.options.privilege_tool)?;
    if extra_args.iter().any(|arg| arg == "--confirm") {
        return Err("--confirm conflicts with pacpin's single-confirmation clean flow".into());
    }
    let has_sudo_override = extra_args.iter().any(|arg| arg == "--sudo" || arg.starts_with("--sudo="));
    if !use_helper && has_sudo_override {
        return Err("--sudo is an AUR-helper option; set [options] privilege_tool for pacman fallback".into());
    }
    let flag = if full { "-Scc" } else { "-Sc" };
    let is_aur_only = cache_scope(extra_args)? == CacheScope::Aur;
    if is_aur_only && !use_helper { return Err("AUR cache cleanup requires an available AUR helper".into()); }
    let mut cmd = if use_helper {
        let mut cmd = Command::new(&config.options.helper);
        cmd.arg(flag);
        if !is_aur_only && !has_sudo_override && privilege_tool != "sudo" {
            let is_paru = std::path::Path::new(&config.options.helper).file_name()
                .is_some_and(|name| name == "paru");
            if !is_paru {
                return Err(format!(
                    "Cannot configure {privilege_tool} for helper '{}'; only paru's --sudo option is supported. Configure the helper itself or select paru.",
                    config.options.helper
                ));
            }
            cmd.args(["--sudo", privilege_tool]);
        }
        cmd
    } else {
        let mut cmd = crate::privilege::command(privilege_tool);
        cmd.arg("/usr/bin/pacman").arg(flag);
        cmd
    };
    cmd.args(extra_args.iter().filter(|arg| arg.as_str() != "--noconfirm" && (use_helper || arg.as_str() != "--repo")));
    if !full { cmd.arg("--noconfirm"); }
    Ok(cmd)
}

#[cfg(test)]
mod clean_tests {
    use super::*;
    #[test]
    fn cache_scope_is_explicit_and_native_fallback_consumes_helper_selector() {
        assert_eq!(cache_scope(&[]).unwrap(), CacheScope::All);
        assert_eq!(cache_scope(&["--repo".into()]).unwrap(), CacheScope::Native);
        assert_eq!(cache_scope(&["-a".into()]).unwrap(), CacheScope::Aur);
        assert!(cache_scope(&["--repo".into(), "--aur".into()]).is_err());
        let config = Config::default();
        let native = build_clean_command(&config, true, &["--repo".into()], false).unwrap();
        assert_eq!(argv(&native), ["sudo", "/usr/bin/pacman", "-Scc"]);
        assert!(build_clean_command(&config, true, &["--aur".into()], false).is_err());
    }
    #[test]
    fn aur_privilege_override_is_explicit_and_helper_specific() {
        assert_eq!(helper_privilege_args("/usr/bin/paru", "pkexec").unwrap(), ["--sudo", "pkexec"]);
        assert!(helper_privilege_args("yay", "pkexec").is_err());
        assert!(helper_privilege_args("yay", "sudo").unwrap().is_empty());
    }
    #[test]
    fn cache_confirmation_preserves_sync_databases() {
        let answers = cache_answers(2, false);
        assert!(answers.starts_with(b"y\ny\nn\n"));
        assert!(!cache_answers(2, true).contains(&b'n'));
    }

    #[test]
    fn cache_input_has_complete_answers_and_eof_before_a_child_exists() {
        use std::io::Read;
        let answers = cache_answers(2, false);
        let mut input = cache_input(&answers).unwrap();
        let mut read = Vec::new();
        input.read_to_end(&mut read).unwrap();
        assert_eq!(read, answers);
        assert!(cache_input(&vec![0; 8193]).is_err());
    }

    #[test]
    fn failed_cleanup_exit_status_is_never_success() {
        assert!(cleanup_status("Native", 0).is_ok());
        assert!(cleanup_status("Native", 255).unwrap_err().contains("255"));
    }

    fn argv(command: &Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn normal_clean_uses_configured_escalation_and_no_second_prompt() {
        let mut config = Config::default();
        config.options.privilege_tool = "pkexec".into();
        let helper = build_clean_command(&config, false, &[], true).unwrap();
        assert_eq!(argv(&helper), ["paru", "-Sc", "--sudo", "pkexec", "--noconfirm"]);
        let pacman = build_clean_command(&config, false, &[], false).unwrap();
        assert_eq!(argv(&pacman), ["pkexec", "/usr/bin/pacman", "-Sc", "--noconfirm"]);
    }

    #[test]
    fn full_clean_preserves_scc_and_does_not_choose_default_no() {
        let config = Config::default();
        let flags = vec!["--noconfirm".to_string()];
        let helper = build_clean_command(&config, true, &flags, true).unwrap();
        assert_eq!(argv(&helper), ["paru", "-Scc"]);
        let pacman = build_clean_command(&config, true, &flags, false).unwrap();
        assert_eq!(argv(&pacman), ["sudo", "/usr/bin/pacman", "-Scc"]);
    }

    #[test]
    fn aur_clean_does_not_use_privilege_escalation() {
        let mut config = Config::default();
        config.options.privilege_tool = "pkexec".into();
        let flags = vec!["--aur".to_string(), "--noconfirm".to_string()];
        let helper = build_clean_command(&config, true, &flags, true).unwrap();
        assert_eq!(argv(&helper), ["paru", "-Scc", "--aur"]);
    }

    #[test]
    fn native_only_clean_runs_pacman_directly() {
        let mut config = Config::default();
        config.options.privilege_tool = "pkexec".into();
        let flags = vec!["--noconfirm".to_string()];
        let pacman = build_clean_command(&config, true, &flags, false).unwrap();
        assert_eq!(argv(&pacman), ["pkexec", "/usr/bin/pacman", "-Scc"]);
    }

    #[test]
    fn clean_rejects_conflicting_prompt_and_escalation_flags() {
        let config = Config::default();
        assert!(build_clean_command(&config, false, &["--confirm".into()], true).is_err());
        assert!(build_clean_command(&config, false, &["--sudo=pkexec".into()], false).is_err());
        assert!(crate::config::validate_privilege_tool("sudo --bad-flag").is_err());
    }

    #[test]
    fn stale_download_dir_filter_ignores_new_dirs_and_files() {
        let root = std::env::temp_dir().join(format!("pacpin_clean_dirs_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(root.join("download-old")).unwrap();
        std::fs::create_dir_all(root.join("ordinary")).unwrap();
        std::fs::write(root.join("download-file"), b"not a directory").unwrap();
        let entries = std::fs::read_dir(&root).unwrap().map(Result::unwrap).collect::<Vec<_>>();
        let now = SystemTime::now();
        assert!(entries.iter().all(|entry| !is_stale_download_directory(entry, now)));
        let later = now + std::time::Duration::from_secs(121);
        let matched = entries.iter().filter(|entry| is_stale_download_directory(entry, later))
            .map(|entry| entry.file_name().to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(matched, ["download-old"]);
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// Forwards arbitrary commands directly to pacman with database lock protection.
pub fn forward_pacman(config: &Config, args: &[String], needs_sudo: bool) -> Result<i32, String> {
    if needs_sudo {
        check_lock()?;
        let status = crate::privilege::command(&config.options.privilege_tool).arg("pacman").args(args).status();
        match status {
            Ok(s) => Ok(s.code().unwrap_or(1)),
            Err(e) => Err(format!("Failed to run {} pacman: {}", config.options.privilege_tool, e)),
        }
    } else {
        let status = Command::new("pacman").args(args).status();
        match status {
            Ok(s) => Ok(s.code().unwrap_or(1)),
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
    let candidates: Vec<&alpm::Package> = targets.iter().filter_map(|target| {
        let (requested_repo, pkg_clean) = match target.split_once('/') {
            Some((repo, pkg)) => (Some(repo), pkg),
            None => (None, target.as_str()),
        };

        // Let ALPM search all repositories for an exact package before considering
        // providers. A provider in an earlier repository is not the upgrade target.
        let syncdbs = manager.handle().syncdbs();
        if let Some(repo) = requested_repo {
            syncdbs.into_iter().find(|db| db.name() == repo)
                .and_then(|db| db.pkgs().find_satisfier(pkg_clean))
        } else {
            syncdbs.find_satisfier(pkg_clean)
        }
    }).collect();
    check_candidate_conflicts(manager, &candidates)
}

/// Checks the exact candidates in the upgrade plan without resolving their names again.
pub fn check_resolved_package_conflicts(
    manager: &AlpmManager,
    updates: &[ResolvedPackage],
) -> Vec<(String, String)> {
    let candidates: Vec<&alpm::Package> = updates.iter().filter_map(|update| {
        let candidate = update.candidate.as_ref().filter(|candidate| !candidate.is_aur)?;
        manager.handle().syncdbs().into_iter()
            .find(|db| db.name() == candidate.repo)
            .and_then(|db| db.pkg(candidate.name.as_str()).ok())
    }).collect();
    check_candidate_conflicts(manager, &candidates)
}

fn check_candidate_conflicts(
    manager: &AlpmManager,
    candidates: &[&alpm::Package],
) -> Vec<(String, String)> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let candidate_names: HashSet<&str> = candidates.iter().map(|pkg| pkg.name()).collect();
    // Inspect the proposed versions, not old versions of packages being upgraded.
    let packages: Vec<&alpm::Package> = candidates.iter().copied().chain(
        manager.handle().localdb().pkgs().into_iter()
            .filter(|pkg| !candidate_names.contains(pkg.name()))
    ).collect();
    let mut satisfiers: HashMap<&str, Vec<&alpm::Package>> = HashMap::new();
    let mut reverse_conflicts: HashMap<&str, Vec<&alpm::Package>> = HashMap::new();
    for pkg in &packages {
        satisfiers.entry(pkg.name()).or_default().push(pkg);
        for provide in pkg.provides() {
            satisfiers.entry(provide.name()).or_default().push(pkg);
        }
        for conflict in pkg.conflicts() {
            reverse_conflicts.entry(conflict.name()).or_default().push(pkg);
        }
    }

    let mut checked = HashSet::new();
    let mut conflicts = Vec::new();
    for candidate in candidates {
        // Name indexes only narrow the possible pairs. ALPM alone decides whether
        // versions, epochs and virtual provides satisfy the actual conflict.
        let mut potential: HashMap<&str, &alpm::Package> = HashMap::new();
        for conflict in candidate.conflicts() {
            if let Some(packages) = satisfiers.get(conflict.name()) {
                for pkg in packages {
                    potential.insert(pkg.name(), pkg);
                }
            }
        }
        for name in std::iter::once(candidate.name()).chain(candidate.provides().into_iter().map(|p| p.name())) {
            if let Some(packages) = reverse_conflicts.get(name) {
                for pkg in packages {
                    potential.insert(pkg.name(), pkg);
                }
            }
        }
        for other in potential.values() {
            let name = candidate.name();
            let other_name = other.name();
            if name == other_name {
                continue;
            }
            let key = if name < other_name { (name, other_name) } else { (other_name, name) };
            if !checked.insert(key) {
                continue;
            }
            let pair = [*candidate, *other];
            if !manager.handle().check_conflicts(pair.iter()).is_empty() {
                let pair = if candidate_names.contains(other_name) {
                    key
                } else {
                    (name, other_name)
                };
                conflicts.push((pair.0.to_string(), pair.1.to_string()));
            }
        }
    }
    conflicts.sort();
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
    fn download_progress_excludes_complete_cached_archives() {
        let cache = std::env::temp_dir().join(format!("pacpin-download-progress-test-{}", std::process::id()));
        std::fs::create_dir(&cache).unwrap();
        let package = |name: &str, size| crate::preview::PreviewPackage {
            name: name.into(), version: "2-1".into(), repo: "fixture".into(), old_version: Some("1-1".into()),
            new_isize: 0, old_isize: 0, download_size: size, filename: format!("{name}-2-1-x86_64.pkg.tar.zst"),
        };
        let preview = crate::preview::TransactionPreview { adds: vec![package("cached", 10), package("partial", 20), package("missing", 30)], ..Default::default() };
        std::fs::write(cache.join(&preview.adds[0].filename), [0; 10]).unwrap();
        std::fs::write(cache.join(&preview.adds[1].filename), [0; 5]).unwrap();
        assert_eq!(download_targets(&preview, std::slice::from_ref(&cache)), [("partial-2-1-x86_64".into(), 20), ("missing-2-1-x86_64".into(), 30)]);
        std::fs::remove_dir_all(cache).unwrap();
    }

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
            channel: None,
            no_cache: false,
        };
        let (cmds, op, _) = build_install_commands(&targets, &no_aur, "paru", "sudo", &opts_dry_sy, false);
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
            channel: None,
            no_cache: false,
        };
        let (cmds, op, _) = build_install_commands(&targets, &no_aur, "paru", "sudo", &opts_sy, false);
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
            channel: None,
            no_cache: false,
        };
        let (cmds, op, _) = build_install_commands(&targets, &no_aur, "paru", "sudo", &opts_syu, true);
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
            channel: None,
            no_cache: false,
        };
        let (cmds, op, _) = build_install_commands(&targets, &no_aur, "paru", "sudo", &opts_su, true);
        assert_eq!(op, "-Su");
        assert_eq!(cmds, vec!["sudo pacman -Su --needed -- ollama".to_string()]);

        // 5. AUR targets with sysupgrade when no repo targets:
        let aur_targets = vec!["google-chrome".to_string()];
        let (cmds, _op, aur_op) = build_install_commands(&[], &aur_targets, "paru", "sudo", &opts_syu, true);
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

    #[test]
    fn test_versioned_reverse_conflicts() {
        use crate::db::test_support::{AlpmFixture, FixturePackage};
        for (version, count) in [("37-1", 2), ("38-1", 0), ("42.1-1", 0)] {
            let mut cryptsetup = FixturePackage::new("cryptsetup", "1-1");
            cryptsetup.conflicts = &["mkinitcpio<38-1"];
            let mut mdadm = FixturePackage::new("mdadm", "1-1");
            mdadm.conflicts = &["mkinitcpio<38"];
            let fixture = AlpmFixture::new(
                &[cryptsetup, mdadm],
                &[("core", &[FixturePackage::new("mkinitcpio", version)])],
            );
            assert_eq!(check_package_conflicts(&fixture.manager, &["mkinitcpio".into()]).len(), count);
        }
    }

    fn firefox_provider_fixture() -> crate::db::test_support::AlpmFixture {
        use crate::db::test_support::{AlpmFixture, FixturePackage};
        let mut provider = FixturePackage::new("firefox-pure", "2.0-1");
        provider.provides = &["firefox", "firefox=2.0"];
        provider.conflicts = &["firefox"];
        AlpmFixture::new(
            &[FixturePackage::new("firefox", "1.0-1")],
            &[
                ("first", &[provider]),
                ("second", &[FixturePackage::new("firefox", "2.0-1")]),
            ],
        )
    }

    #[test]
    fn upgrade_conflicts_inspect_exact_package_before_earlier_repo_provider() {
        let fixture = firefox_provider_fixture();
        let result = ResolverEngine::new(&fixture.manager).resolve_with_options(&Config::default(), true);
        assert_eq!(result.updates.len(), 1);
        let candidate = result.updates[0].candidate.as_ref().unwrap();
        assert_eq!(candidate.name, "firefox");
        assert_eq!(candidate.repo, "second");
        assert!(check_package_conflicts(&fixture.manager, &["firefox".into()]).is_empty());
        assert!(check_resolved_package_conflicts(&fixture.manager, &result.updates).is_empty());
    }

    #[test]
    fn explicit_provider_conflicts_and_repository_scope_are_preserved() {
        let fixture = firefox_provider_fixture();
        let expected = vec![("firefox-pure".to_string(), "firefox".to_string())];
        assert_eq!(check_package_conflicts(&fixture.manager, &["firefox-pure".into()]), expected);
        assert_eq!(check_package_conflicts(&fixture.manager, &["first/firefox".into()]), expected);
        assert!(check_package_conflicts(&fixture.manager, &["second/firefox".into()]).is_empty());
        assert!(check_package_conflicts(&fixture.manager, &["missing/firefox".into()]).is_empty());
    }

    #[test]
    fn virtual_target_still_uses_provider_when_no_exact_sync_package_exists() {
        use crate::db::test_support::{AlpmFixture, FixturePackage};
        let mut provider = FixturePackage::new("firefox-pure", "2.0-1");
        provider.provides = &["firefox"];
        provider.conflicts = &["firefox"];
        let fixture = AlpmFixture::new(
            &[FixturePackage::new("firefox", "1.0-1")],
            &[("first", &[provider])],
        );
        assert_eq!(
            check_package_conflicts(&fixture.manager, &["firefox".into()]),
            vec![("firefox-pure".to_string(), "firefox".to_string())],
        );
    }

    #[test]
    fn planned_repository_is_used_instead_of_resolving_the_name_again() {
        use crate::db::test_support::{AlpmFixture, FixturePackage};
        let mut first_firefox = FixturePackage::new("firefox", "2-1");
        first_firefox.conflicts = &["plugin"];
        let fixture = AlpmFixture::new(
            &[FixturePackage::new("firefox", "1-1"), FixturePackage::new("plugin", "1-1")],
            &[
                ("first", &[first_firefox]),
                ("second", &[FixturePackage::new("firefox", "3-1")]),
            ],
        );
        let mut config = Config::default();
        config.pins.insert("firefox".into(), "second".into());
        let result = ResolverEngine::new(&fixture.manager).resolve_with_options(&config, true);
        assert_eq!(result.updates.len(), 1);
        assert_eq!(result.updates[0].candidate.as_ref().unwrap().repo, "second");
        assert_eq!(check_package_conflicts(&fixture.manager, &["firefox".into()]).len(), 1);
        assert!(check_resolved_package_conflicts(&fixture.manager, &result.updates).is_empty());
    }

    #[test]
    fn native_conflict_rules_handle_versioned_and_unversioned_provides() {
        use crate::db::test_support::{AlpmFixture, FixturePackage};
        for (conflict, provides, expected) in [
            (&["api>=1:2.0"][..], &["api=1:2.0"][..], 1),
            (&["api>=1:2.0"][..], &["api=2.1"][..], 0),
            (&["api>=1:2.0"][..], &["api"][..], 0),
            (&["api"][..], &["api"][..], 1),
        ] {
            let mut consumer = FixturePackage::new("consumer", "1-1");
            consumer.conflicts = conflict;
            let mut provider = FixturePackage::new("provider", "99-1");
            provider.provides = provides;
            let fixture = AlpmFixture::new(&[consumer], &[("repo", &[provider])]);
            assert_eq!(check_package_conflicts(&fixture.manager, &["provider".into()]).len(), expected);
        }
    }

    #[test]
    fn outdated_installed_conflict_is_removed_when_both_packages_are_upgraded() {
        use crate::db::test_support::{AlpmFixture, FixturePackage};
        let mut old_consumer = FixturePackage::new("consumer", "1-1");
        old_consumer.conflicts = &["engine>=2"];
        let fixture = AlpmFixture::new(
            &[old_consumer, FixturePackage::new("engine", "1-1")],
            &[("repo", &[FixturePackage::new("consumer", "2-1"), FixturePackage::new("engine", "2-1")])],
        );
        assert_eq!(check_package_conflicts(&fixture.manager, &["engine".into()]).len(), 1);
        assert!(check_package_conflicts(&fixture.manager, &["engine".into(), "consumer".into()]).is_empty());
    }

    #[test]
    fn all_installed_providers_are_checked_and_self_upgrades_are_excluded() {
        use crate::db::test_support::{AlpmFixture, FixturePackage};
        let mut first = FixturePackage::new("plugin-a", "1-1");
        first.provides = &["api"];
        let mut second = FixturePackage::new("plugin-b", "1-1");
        second.provides = &["api"];
        let mut candidate = FixturePackage::new("plugin-a", "2-1");
        candidate.provides = &["api"];
        candidate.conflicts = &["api"];
        let fixture = AlpmFixture::new(&[first, second], &[("repo", &[candidate])]);
        assert_eq!(
            check_package_conflicts(&fixture.manager, &["plugin-a".into()]),
            vec![("plugin-a".to_string(), "plugin-b".to_string())],
        );
    }

    #[test]
    fn conflicts_between_selected_candidates_are_reported_once() {
        use crate::db::test_support::{AlpmFixture, FixturePackage};
        let mut first = FixturePackage::new("first", "1-1");
        first.conflicts = &["second"];
        let mut second = FixturePackage::new("second", "1-1");
        second.conflicts = &["first"];
        let fixture = AlpmFixture::new(&[], &[("repo", &[first, second])]);
        assert!(check_package_conflicts(&fixture.manager, &["first".into()]).is_empty());
        assert_eq!(
            check_package_conflicts(&fixture.manager, &["second".into(), "first".into()]),
            vec![("first".to_string(), "second".to_string())],
        );
    }

    #[test]
    fn remove_dry_run_never_executes() {
        // `-n` / `--dry-run` only preview; forwarded to `pacman -R` they
        // would mean `--nosave`, so they must be consumed, never forwarded.
        let mut config = crate::config::Config::default();
        let missing = ["definitely-not-a-real-package-xyz".to_string()];
        assert!(remove(&mut config, &missing, &["-n".to_string()]).is_ok());
        assert!(remove(&mut config, &missing, &["--dry-run".to_string()]).is_ok());
        assert!(remove(&mut config, &["--config".to_string()], &["-n".to_string()]).is_err());
    }
}
