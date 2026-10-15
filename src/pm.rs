//! Centralized Package Management Engine for pacpin.
//!
//! Stable boundary for native and AUR operations. Implementation modules own
//! installation, cache cleanup, lock inspection, validation and helper execution.
//! Legacy forwarding/removal paths are recorded in docs/architecture.md.

use std::io;
use std::process::Command;

use colored::Colorize;

use crate::config::Config;
use crate::db::AlpmManager;
use crate::journal::TransactionJournal;

mod cache;
mod continuation;
mod install;
mod lock;
mod orphans;
mod pinning;
mod rebuild;
mod validation;
mod workspace;
#[cfg(test)]
mod tests;
pub use cache::{
    CacheScope, cache_scope, clean, clean_after_transaction, clean_preview, clean_scoped,
    helper_privilege_args, transaction_cache_cleanup,
};
pub use install::{InstallOptions, build_install_commands, install};
pub use lock::{LockInfo, check_lock, get_lock_info, wait_for_lock};
pub use orphans::remove_orphans;
pub use rebuild::{validate_rebuild, rebuild_aur};
pub use validation::{check_package_conflicts, check_resolved_package_conflicts};
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

pub fn check_aur_options(config: &Config, policy: crate::scratch::Policy, flags: &[String]) -> Result<(), crate::runtime::OptionError> {
    use crate::runtime::OptionError;
    crate::config::validate_privilege_tool(&config.options.privilege_tool)
        .map_err(|error| OptionError::Invalid(error.to_string()))?;
    workspace::check_options(&config.options.helper, policy, flags)?;
    if config.options.privilege_tool != "sudo" && !workspace::is_paru(&config.options.helper) {
        return Err(OptionError::Unsupported(format!("AUR helper '{}' cannot use the selected {} privilege override", config.options.helper, config.options.privilege_tool)));
    }
    Ok(())
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
