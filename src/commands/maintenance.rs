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
use std::process::exit;

use colored::Colorize;

use crate::config::{self, Config};
use crate::integrations::IntegrationsManager;
use crate::pm;

pub fn cmd_clean(config: &Config, full: bool, extra_args: &[String]) {
    let aur_only = extra_args.iter().any(|s| s == "--aur" || s == "-a");
    let providers = if aur_only { Vec::new() } else { IntegrationsManager::get_active_providers(config) };
    if extra_args.iter().any(|s| s == "--dry-run" || s == "-n") {
        match pm::clean_preview(config, full, extra_args) {
            Ok(command) => println!(":: Dry-run cache cleanup: {command}"),
            Err(error) => { eprintln!("Error: {error}"); exit(1); }
        }
        for provider in providers.iter().filter(|p| p.supports_clean()) {
            println!(":: Dry-run {} cleanup: {}", provider.name(), provider.clean_command_str());
        }
        return;
    }
    let noconfirm = extra_args.iter().any(|arg| arg == "--noconfirm");
    if !noconfirm {
        if !io::stdin().is_terminal() {
            eprintln!("Clean requires confirmation on a terminal; use --noconfirm for scripts.");
            exit(1);
        }
        let scope = if full { "ALL cached packages (including installed versions)" } else { "unused cached packages" };
        print!(":: Remove {scope}{}? [y/N] ", if aur_only { " from AUR" } else if providers.iter().any(|p| p.supports_clean()) { " and clean enabled integrations" } else { "" });
        let _ = io::stdout().flush();
        let mut answer = String::new();
        if io::stdin().read_line(&mut answer).is_err() || !answer.trim().eq_ignore_ascii_case("y") {
            println!(":: Cache clean cancelled.");
            return;
        }
    }
    match pm::clean(config, full, extra_args) {
        Ok(code) => {
            if code != 0 {
                exit(code);
            }
        }
        Err(e) => {
            eprintln!("{}", format!("Cache clean failed: {}", e).red());
            exit(1);
        }
    }

    if !IntegrationsManager::execute_cleanups(&providers) {
        exit(1);
    }
}

pub fn cmd_remove(mut config: Config, args: &[String], is_friendly: bool) {
    let mut flags = Vec::new();
    let mut targets = Vec::new();

    if is_friendly {
        flags.push("-Rns".to_string());
    }
    for a in args {
        if a.starts_with('-') {
            flags.push(a.clone());
        } else {
            targets.push(a.clone());
        }
    }

    match pm::remove(&mut config, &targets, &flags) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{}", format!("Error: {}", e).red());
            exit(1);
        }
    }
}

fn validate_keep_targets(packages: &[String]) -> Result<(), String> {
    if packages.is_empty() {
        return Err("'pacpin keep' requires at least one package name.".into());
    }
    if let Some(bad) = packages.iter().find(|name| !crate::utils::is_safe_pkg_name(name)) {
        return Err(format!("Refusing unsafe package name '{}'.", bad));
    }
    Ok(())
}

/// `pacman -D` never prompts, so `--noconfirm` is accepted as a no-op for
/// script compatibility; every other flag is rejected as a package name.
fn keep_targets(packages: &[String]) -> Vec<String> {
    packages
        .iter()
        .filter(|p| p.as_str() != "--noconfirm")
        .cloned()
        .collect()
}

pub fn cmd_keep(config: &Config, packages: &[String]) {
    let targets = keep_targets(packages);
    if let Err(error) = validate_keep_targets(&targets) {
        eprintln!("{}", format!("Error: {}", error).red());
        eprintln!("Usage: pacpin keep <pkg1> [pkg2...]");
        exit(1);
    }
    if let Err(error) = pm::check_lock() {
        eprintln!("{}", format!("Error: {}", error).red());
        exit(1);
    }
    println!(
        "{} Marking {} package(s) as explicitly installed ({} pacman -D --asexplicit)...",
        "::".cyan(),
        targets.len(), config.options.privilege_tool
    );
    let status = crate::privilege::command(&config.options.privilege_tool)
        .args(["pacman", "-D", "--asexplicit", "--"])
        .args(&targets)
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
}

pub fn cmd_reset(force: bool, noconfirm: bool) {
    let path = config::get_config_path();
    if !path.exists() {
        println!("{}", "No configuration file found to reset.".yellow());
        return;
    }

    if !force && !noconfirm {
        if !io::stdin().is_terminal() {
            eprintln!(
                "{}",
                "Error: Cannot prompt for confirmation in non-interactive environment. Use --noconfirm or -f/--force."
                    .red()
            );
            exit(1);
        }
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

#[cfg(test)]
mod keep_tests {
    use super::{keep_targets, validate_keep_targets};

    #[test]
    fn keep_rejects_flag_injection() {
        assert!(validate_keep_targets(&["git".into()]).is_ok());
        assert!(validate_keep_targets(&["--config".into()]).is_err());
        assert!(validate_keep_targets(&[]).is_err());
    }

    #[test]
    fn keep_accepts_noconfirm_as_noop() {
        assert_eq!(
            keep_targets(&["nano".into(), "--noconfirm".into()]),
            vec!["nano".to_string()]
        );
        assert!(keep_targets(&["--noconfirm".into()]).is_empty());
        // Short flags keep their pacman meaning and are still rejected.
        assert_eq!(keep_targets(&["-n".into()]), vec!["-n".to_string()]);
    }
}
